//! Curvature selector: selects convex or concave ground from the second derivative.
//!
//! Output is a `[0, 1]` selection on the **`height`** layer (so the rest of the toolset
//! shapes and applies it), high where the surface curves the chosen way. Curvature is
//! the rate of change of slope, not slope itself: a planar ramp, however steep, has zero
//! curvature, while a ridge crest or a valley floor has a lot. That is what makes it the
//! third axis of the material algebra: `steep AND high AND convex` selects exposed rock,
//! `concave AND high-flow` a lush valley.
//!
//! `mode` picks **convex** (ridges, spurs, outcrops: ground that bulges up) or
//! **concave** (valleys, hollows, basins: ground that dishes in). `strength` is the
//! selection gain. Raw curvature magnitude varies enormously with feature sharpness and
//! resolution, so the curvature is self-calibrated against its own magnitude (the RMS
//! over the field) before `strength` is applied; `strength` therefore reads consistently
//! across terrains rather than saturating instantly. The scale at which curvature is
//! measured still comes from a Blur placed upstream: blur first to read large landforms,
//! leave it sharp to read fine detail.
//!
//! The `output` param switches between the selection and the raw **measure** — the signed convexity
//! in RMS units (positive convex, negative concave, independent of `mode`) — for probing or a
//! downstream Histogram-Scan.
//!
//! The second difference is taken in world units (scaled by the region span and
//! resolution), so the measure is stable across resolutions for a given feature scale
//! rather than drifting with cell size. Like the other selectors it is not mask-aware: it
//! derives a selection from scratch.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.curvature";

/// Mode ids (stored as the `mode` param's text value).
const MODE_CONVEX: &str = "convex";
const MODE_CONCAVE: &str = "concave";

/// Default gain in RMS units: selects ground curved a bit more than typical out of the
/// box. The curvature is self-calibrated, so this reads consistently across terrains.
const DEFAULT_STRENGTH: f64 = 1.0;

/// Curvature selector: one input, one output. Writes the selection to
/// [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Curvature;

impl Operator for Curvature {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "mode",
                    ParamKind::Enum {
                        options: &[MODE_CONVEX, MODE_CONCAVE],
                    },
                    ParamValue::Text(MODE_CONVEX.to_string()),
                ),
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 8.0 },
                    ParamValue::Float(DEFAULT_STRENGTH),
                ),
                crate::selector::output_param(),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        // Concave mode flips the sign, so the same convexity measure selects hollows.
        let concave = params.get_str("mode", MODE_CONVEX) == MODE_CONCAVE;
        let sign = if concave { -1.0 } else { 1.0 };
        let strength = params.get_f64("strength", DEFAULT_STRENGTH).max(0.0) as f32;

        // Cells per world unit; weights the x and y second differences so they are
        // comparable for a non-square grid (for a square grid this is a common factor
        // that the normalization below cancels).
        let region = input.region();
        let wx = (width as f64 / region.width().max(f64::EPSILON)) as f32;
        let wy = (height as f64 / region.height().max(f64::EPSILON)) as f32;

        // A noise floor for the self-calibration below: a flat or planar field has zero
        // curvature in exact arithmetic, leaving only float-precision noise in the second
        // difference. Without this, normalizing by the RMS would amplify that noise into a
        // full selection. The floor is the size of that numerical noise: f32 epsilon times
        // the height scale times the world-scaling.
        let (lo, hi) = h.value_range();
        let noise_floor = f32::EPSILON * lo.abs().max(hi.abs()) * (wx * wx + wy * wy) * 8.0;

        // First pass: convexity per cell (0 at the border, which has no two-sided
        // neighbours), accumulating its RMS over the interior. Curvature magnitude varies
        // wildly with feature sharpness and resolution, so a fixed gain would saturate on
        // detailed terrain and vanish on smooth terrain. Normalizing by the RMS makes
        // `strength` self-calibrating: it operates in the same range whatever the input.
        let mut convexity = vec![0.0_f32; width * height];
        let mut sum_sq = 0.0_f64;
        let mut interior = 0_u64;
        for y in 1..height.saturating_sub(1) {
            for x in 1..width.saturating_sub(1) {
                let c = h.get(x, y).unwrap_or(0.0);
                let cxx = (h.get(x + 1, y).unwrap_or(0.0) - 2.0 * c
                    + h.get(x - 1, y).unwrap_or(0.0))
                    * wx
                    * wx;
                let cyy = (h.get(x, y + 1).unwrap_or(0.0) - 2.0 * c
                    + h.get(x, y - 1).unwrap_or(0.0))
                    * wy
                    * wy;
                // -Laplacian: positive on convex ground (it bulges up), negative in
                // concave hollows (it dishes in).
                let v = -(cxx + cyy);
                convexity[y * width + x] = v;
                sum_sq += f64::from(v) * f64::from(v);
                interior += 1;
            }
        }
        // Row-major accumulation in a fixed order keeps the RMS (and thus the output)
        // deterministic.
        let rms = if interior > 0 {
            (sum_sq / interior as f64).sqrt() as f32
        } else {
            0.0
        };
        let scale = if rms > noise_floor { 1.0 / rms } else { 0.0 };

        // Second pass: map the normalized convexity into the selection. `strength` is the
        // gain in RMS units, so 1.0 selects ground curved more than typical. Measure mode emits
        // the raw signed convexity in those RMS units instead (positive convex, negative concave),
        // independent of `mode` and `strength` — pure data for probing or a downstream Histogram-Scan.
        let measure = crate::selector::is_measure(params);
        let selection = Layer::from_fn(width, height, |x, y| {
            let v = convexity[y * width + x] * scale;
            if measure {
                v
            } else {
                smoothstep(0.0, 1.0, sign * v * strength)
            }
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Curvature) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(32, 32, Region::UNIT, 0)
    }

    /// A paraboloid in world-normalized coordinates: `a > 0` is a convex hill (curves
    /// down from the centre), `a < 0` a concave bowl. Its world-scaled curvature is a
    /// clean constant, independent of resolution.
    fn dome(size: usize, a: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                let u = x as f32 / size as f32 - 0.5;
                let v = y as f32 / size as f32 - 0.5;
                -a * (u * u + v * v)
            })),
        )
    }

    /// A planar ramp: steep, but zero curvature everywhere.
    fn ramp(size: usize, k: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                k * x as f32 / (size - 1) as f32
            })),
        )
    }

    fn run(input: &Field, mode: &str, strength: f64) -> Field {
        let params = Params::new()
            .with("mode", ParamValue::Text(mode.to_string()))
            .with("strength", ParamValue::Float(strength));
        Curvature
            .eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn measure_mode_emits_signed_curvature() {
        // Measure mode outputs the signed convexity in RMS units, independent of `mode` and
        // `strength`: a convex hill reads positive, a concave bowl negative, and switching `mode`
        // (which only shapes the selection) does not change the measure. A paraboloid has constant
        // curvature, so its normalized convexity is ~1.
        let measure = |field: &Field, mode: &str| {
            let params = Params::new()
                .with("mode", ParamValue::Text(mode.to_string()))
                .with("output", ParamValue::Text("measure".into()));
            Curvature
                .eval(Inputs::required_only(&[field]), &params, &ctx())
                .unwrap()
                .remove(0)
        };
        let hill = dome(32, 1.0);
        let bowl = dome(32, -1.0);
        assert!(
            at(&measure(&hill, MODE_CONVEX), 16, 16) > 0.5,
            "convex hill reads positive"
        );
        assert!(
            at(&measure(&bowl, MODE_CONVEX), 16, 16) < -0.5,
            "concave bowl reads negative"
        );
        let (a, b) = (
            at(&measure(&hill, MODE_CONVEX), 16, 16),
            at(&measure(&hill, MODE_CONCAVE), 16, 16),
        );
        assert!(
            (a - b).abs() < 1e-6,
            "measure is independent of mode: {a} vs {b}"
        );
    }

    #[test]
    fn convex_mode_selects_a_convex_hill() {
        let hill = dome(32, 1.0);
        assert!(
            at(&run(&hill, MODE_CONVEX, 1.0), 16, 16) > 0.9,
            "convex ground should select high in convex mode"
        );
        // The same hill is not concave, so concave mode rejects it.
        assert!(at(&run(&hill, MODE_CONCAVE, 1.0), 16, 16) < 0.1);
    }

    #[test]
    fn concave_mode_selects_a_bowl() {
        let bowl = dome(32, -1.0);
        assert!(at(&run(&bowl, MODE_CONCAVE, 1.0), 16, 16) > 0.9);
        assert!(at(&run(&bowl, MODE_CONVEX, 1.0), 16, 16) < 0.1);
    }

    #[test]
    fn a_planar_ramp_has_no_curvature() {
        // The defining property: a steep ramp has slope but zero curvature, so it is not
        // selected in either mode. This is what makes curvature distinct from slope.
        let r = ramp(32, 1.0);
        assert!(at(&run(&r, MODE_CONVEX, 1.0), 16, 16) < 0.01);
        assert!(at(&run(&r, MODE_CONCAVE, 1.0), 16, 16) < 0.01);
    }

    #[test]
    fn a_flat_field_is_unselected() {
        let flat = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.5)));
        assert_eq!(at(&run(&flat, MODE_CONVEX, 1.0), 16, 16), 0.0);
    }

    #[test]
    fn curvature_is_resolution_stable() {
        // A hill's curvature is uniform, so it equals its own RMS; the normalized
        // selection at a non-saturating strength matches across resolutions.
        let lo = at(&run(&dome(32, 1.0), MODE_CONVEX, 0.5), 16, 16);
        let hi = at(&run(&dome(64, 1.0), MODE_CONVEX, 0.5), 32, 32);
        assert!(
            (lo - hi).abs() < 5e-3,
            "drifted with resolution: {lo} vs {hi}"
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = run(&dome(32, 1.0), MODE_CONVEX, 1.0);
        assert!(out.layer(layers::MASK).is_none());
        assert!(at(&out, 16, 16) > 0.0);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = dome(32, 1.0);
        input.set_layer("flow", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = run(&input, MODE_CONVEX, 1.0);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = run(&dome(32, 1.0), MODE_CONVEX, 2.0);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_deterministic() {
        let input = dome(32, 1.0);
        assert_eq!(
            run(&input, MODE_CONVEX, 1.0).content_hash(),
            run(&input, MODE_CONVEX, 1.0).content_hash()
        );
    }

    #[test]
    fn spec_is_a_modifier() {
        assert_eq!(Curvature.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(Curvature.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let out = run(&dome(16, 1.0), MODE_CONVEX, 0.5);
        assert_eq!(out.content_hash().to_u64(), 0x0b6c_ddc8_5c3e_1c71);
    }
}
