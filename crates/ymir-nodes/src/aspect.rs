//! Aspect selector: selects slopes that face a chosen compass direction.
//!
//! The third of the local selector trio, alongside [`crate::slope`] and [`crate::curvature`].
//! Output is a `[0, 1]` selection on the **`height`** layer (so the rest of the toolset shapes and
//! applies it), high where the terrain faces the `direction` and falling to zero as the facing swings
//! `falloff` degrees away. This is the knob behind sun/wind-facing effects, snow on poleward slopes,
//! and directional weathering.
//!
//! Like the other derived selectors, it is *measured at a scale set by an upstream Blur*: being a
//! gradient operator it amplifies sub-cell sharpness (crease noise, thin erosion ridges) into hard
//! lines, so a Blur ahead of it selects the aspect of landforms rather than of pixel-scale noise.
//!
//! A cell's *aspect* is the compass direction of steepest descent (the way the slope faces): the
//! negated height gradient. Its deviation from the target `direction` drives the selection, so the
//! result is `cos(aspect - direction)` reshaped by `falloff`. Flats have no aspect, so a `slope_weight`
//! term suppresses gentle ground (weighted by the real slope angle), keeping the selection on genuine
//! faces rather than noise on the flats.
//!
//! The gradient direction is independent of the vertical:horizontal scale (a uniform scale cancels),
//! so the *aspect* is resolution- and world-independent; the scale enters only the slope-angle used
//! for `slope_weight`. `SLOPE` context-deps accordingly.
//!
//! The `output` param switches between the selection and the raw **measure** — the compass aspect in
//! degrees `[0, 360)` — for probing or a downstream Histogram-Scan.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.aspect";

/// Default target facing, the angular half-width of the facing cone, and how strongly flats are
/// suppressed. The defaults show a clear directional wedge out of the box.
const DEFAULT_DIRECTION: f64 = 0.0;
const DEFAULT_FALLOFF: f64 = 90.0;
const DEFAULT_SLOPE_WEIGHT: f64 = 1.0;

/// Slope angle (degrees) at which a cell counts as a full face for `slope_weight`; gentler ground
/// ramps down toward flat, so the aspect of near-level terrain never dominates.
const SLOPE_REF: f32 = 20.0;

/// Aspect selector: one input, one output. Writes the facing selection to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Aspect;

impl Operator for Aspect {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "direction",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(DEFAULT_DIRECTION),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float {
                        min: 1.0,
                        max: 180.0,
                    },
                    ParamValue::Float(DEFAULT_FALLOFF),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "slope_weight",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_SLOPE_WEIGHT),
                ),
                crate::selector::output_param(),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Slope-aware (the `slope_weight` term reads the real slope angle): world height and extent, not
    /// the sea level, so the sea-level slider never invalidates this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::SLOPE
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let direction = params.get_f64("direction", DEFAULT_DIRECTION) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(1e-4) as f32;
        let slope_weight = params
            .get_f64("slope_weight", DEFAULT_SLOPE_WEIGHT)
            .clamp(0.0, 1.0) as f32;

        // The target facing as a unit vector in image space (+x right, +y down), matching how the
        // gradient below is taken from x/y neighbours: 0 degrees points +x, 90 points +y.
        let (sin, cos) = direction.to_radians().sin_cos();
        let (tx, ty) = (cos, sin);

        // The scale turns a normalized-height delta into a true slope, for the slope-angle weight. It
        // cancels out of the aspect *direction* (a uniform scale on both axes), which is why aspect is
        // world-independent.
        let scale = ctx.real_slope_scale() as f32;
        let measure = crate::selector::is_measure(params);

        let selection = Layer::from_fn(width, height, |x, y| {
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(width - 1);
            let ym = y.saturating_sub(1);
            let yp = (y + 1).min(height - 1);
            let gx = (h.get(xp, y).unwrap_or(0.0) - h.get(xm, y).unwrap_or(0.0)) * scale
                / (xp - xm) as f32;
            let gy = (h.get(x, yp).unwrap_or(0.0) - h.get(x, ym).unwrap_or(0.0)) * scale
                / (yp - ym) as f32;
            let mag = (gx * gx + gy * gy).sqrt();
            if mag < 1e-6 {
                // A flat cell has no facing direction, so it is never selected.
                return 0.0;
            }

            // Aspect is the downhill direction (the negated gradient); its angular deviation from the
            // target drives the selection, softening to zero over `falloff` degrees.
            let (fx, fy) = (-gx / mag, -gy / mag);
            // Measure mode emits the raw aspect: the compass direction faced, in degrees [0, 360).
            if measure {
                return fy.atan2(fx).to_degrees().rem_euclid(360.0);
            }
            let cos_dev = (fx * tx + fy * ty).clamp(-1.0, 1.0);
            let dev = cos_dev.acos().to_degrees();
            let directional = 1.0 - smoothstep(0.0, falloff, dev);

            // Suppress flats: full weight for a real face, ramping to none as the slope flattens.
            let slope_factor = smoothstep(0.0, SLOPE_REF, mag.atan().to_degrees());
            let weight = 1.0 - slope_weight + slope_weight * slope_factor;
            (directional * weight).clamp(0.0, 1.0)
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
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Aspect) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0)
    }

    fn select(input: &Field, direction: f64, falloff: f64, slope_weight: f64) -> Field {
        let params = Params::new()
            .with("direction", ParamValue::Float(direction))
            .with("falloff", ParamValue::Float(falloff))
            .with("slope_weight", ParamValue::Float(slope_weight));
        Aspect
            .eval(
                Inputs::required_only(&[input]),
                &params,
                &ctx(input.width()),
            )
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// Height rising toward +x, so the surface faces downhill toward -x everywhere.
    fn slopes_west(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                x as f32 / (size - 1) as f32
            })),
        )
    }

    /// A dome (height falls with distance from the centre), so the surface faces radially outward and
    /// every aspect is present.
    fn dome(size: usize) -> Field {
        let c = (size as f32 - 1.0) * 0.5;
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                let (dx, dy) = (x as f32 - c, y as f32 - c);
                1.0 - (dx * dx + dy * dy).sqrt() / c
            })),
        )
    }

    fn total(field: &Field) -> f32 {
        let layer = field.layer(layers::HEIGHT).unwrap();
        (0..layer.width())
            .flat_map(|x| (0..layer.height()).map(move |y| (x, y)))
            .map(|(x, y)| at(field, x, y))
            .sum()
    }

    #[test]
    fn measure_mode_emits_the_aspect_angle() {
        // Measure mode outputs the raw compass aspect in degrees. A west-facing slope (height rising
        // toward +x) faces -x, which is 180 degrees.
        let input = slopes_west(16);
        let params = Params::new().with("output", ParamValue::Text("measure".into()));
        let out = Aspect
            .eval(Inputs::required_only(&[&input]), &params, &ctx(16))
            .unwrap()
            .remove(0);
        let a = at(&out, 8, 8);
        assert!(
            (a - 180.0).abs() < 2.0,
            "west-facing aspect should be ~180 degrees: {a}"
        );
    }

    #[test]
    fn selects_the_faced_direction_not_the_opposite() {
        // A west-facing slope: direction west (180) selects it strongly; east (0) barely at all.
        let input = slopes_west(16);
        let west = at(&select(&input, 180.0, 90.0, 1.0), 8, 8);
        let east = at(&select(&input, 0.0, 90.0, 1.0), 8, 8);
        assert!(
            west > 0.9,
            "west-facing slope should select for a west target: {west}"
        );
        assert!(
            east < 0.1,
            "west-facing slope should not select for an east target: {east}"
        );
    }

    #[test]
    fn flats_are_never_selected() {
        // A flat field has no aspect: nothing is selected for any direction.
        let flat = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.5)));
        let out = select(&flat, 45.0, 90.0, 1.0);
        assert!(total(&out) < 1e-3, "a flat field should select nothing");
    }

    #[test]
    fn narrower_falloff_selects_less() {
        // On a dome (all aspects present), a tighter facing cone picks a smaller wedge.
        let input = dome(48);
        let wide = total(&select(&input, 0.0, 120.0, 0.0));
        let narrow = total(&select(&input, 0.0, 40.0, 0.0));
        assert!(
            wide > narrow,
            "a wider falloff should select more: {wide} vs {narrow}"
        );
        assert!(narrow > 0.0, "a narrow cone still selects the faced wedge");
    }

    #[test]
    fn slope_weight_suppresses_gentle_ground() {
        // Two west-facing ramps, one gentle and one steep, both facing the target. With full
        // slope_weight the gentle one selects less; with none they match (both fully faced).
        let gentle = slopes_west_at(16, 0.05);
        let steep = slopes_west_at(16, 1.0);
        let weighted = |f: &Field| at(&select(f, 180.0, 90.0, 1.0), 8, 8);
        let unweighted = |f: &Field| at(&select(f, 180.0, 90.0, 0.0), 8, 8);
        assert!(
            weighted(&gentle) < weighted(&steep),
            "slope weight should favour the steeper face"
        );
        assert!(
            (unweighted(&gentle) - unweighted(&steep)).abs() < 1e-3,
            "with no slope weight, facing alone decides"
        );
    }

    /// Height rising toward +x at gradient `k`, facing west, with a settable steepness.
    fn slopes_west_at(size: usize, k: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                k * x as f32 / (size - 1) as f32
            })),
        )
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = slopes_west(16);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.9)));
        let out = select(&input, 180.0, 90.0, 1.0);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn is_deterministic() {
        let input = dome(24);
        assert_eq!(
            select(&input, 30.0, 75.0, 0.8).content_hash(),
            select(&input, 30.0, 75.0, 0.8).content_hash()
        );
    }
}
