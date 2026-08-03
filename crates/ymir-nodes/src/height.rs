//! Height selector: selects a band of elevation.
//!
//! Output is a `[0, 1]` selection on the **`height`** layer, high where the input
//! elevation falls in `[min, max]` and falling off to zero over `falloff` beyond each
//! edge. Elevation is the normalized `[0, 1]` height (0 lowest, 1 highest), which is
//! exactly the grayscale you see in the preview, so the band is set in terms you can
//! read directly off screen. (When a vertical scale lands these flip to meters of real
//! elevation, the way Slope's degrees will become a true angle.)
//!
//! The pointwise sibling of the Slope selector and the small-node successor to the old
//! Mask's `height` mode: it selects a range, leaving freeform shaping to a downstream
//! Curve and application to an effect's mask input.
//!
//! The `output` param switches between the selection and the raw **measure** — the elevation itself —
//! for numeric probing or a downstream Histogram-Scan.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.height";

/// Default band: a mid-elevation selection that shows the band shape out of the box.
const DEFAULT_MIN: f64 = 100.0;
const DEFAULT_MAX: f64 = 180.0;
const DEFAULT_FALLOFF: f64 = 25.0;

/// Height selector: one input, one output. Writes the band selection to
/// [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Height;

impl Operator for Height {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out").selection()],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_MIN),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "max",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_MAX),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_FALLOFF),
                )
                .with_unit(Unit::Meters),
                crate::selector::output_param(),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    /// The band is stated in metres of elevation, converted against the world's vertical scale,
    /// so a change to world height moves the band and must invalidate this node (#377).
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_HEIGHT
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        // Declared as elevations in metres; the layer they are compared against is normalized,
        // so they convert here (#377). `falloff` is a span of elevation and converts the same way.
        let min = ctx.height_from_meters(params.get_f64("min", DEFAULT_MIN));
        let max = ctx.height_from_meters(params.get_f64("max", DEFAULT_MAX));
        let falloff = ctx.height_from_meters(params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0));

        let measure = crate::selector::is_measure(params);
        let selection = Layer::from_fn(width, height, |x, y| {
            let elevation = h.get(x, y).unwrap_or(0.0);
            // Measure mode emits the raw elevation; selection maps it to a band.
            if measure {
                return elevation;
            }
            // Fully selected in [min, max], softening to zero over `falloff` beyond each
            // edge. The product of a rising lower edge and a falling upper edge gives the
            // band; min > max simply yields an empty selection.
            let lower = smoothstep(min - falloff, min, elevation);
            let upper = 1.0 - smoothstep(max, max + falloff, elevation);
            (lower * upper).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
/// `low == high` degrades to a hard step at that threshold (a zero-width falloff).
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Height) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    /// A world with a real vertical scale. Left at the default of one metre, metres and
    /// normalized height are the same number and nothing below would notice the band being
    /// converted at all, which is exactly how a conversion bug survives a green suite (#377).
    const TEST_WORLD_HEIGHT: f64 = 256.0;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0).with_world_height(TEST_WORLD_HEIGHT)
    }

    fn flat(size: usize, value: f32) -> Field {
        Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(size, size, value)))
    }

    /// A field whose height ramps left-to-right from 0 to 1.
    fn ramp(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                x as f32 / (size - 1) as f32
            })),
        )
    }

    /// Selects a band given as *fractions* of the world's vertical range.
    ///
    /// The parameters are metres, so this converts. Stating the tests in fractions keeps what
    /// they mean readable while still exercising the conversion: break it and every one fails.
    fn select(input: &Field, min: f32, max: f32, falloff: f32) -> Field {
        let metres = |v: f32| ParamValue::Float(f64::from(v) * TEST_WORLD_HEIGHT);
        let params = Params::new()
            .with("min", metres(min))
            .with("max", metres(max))
            .with("falloff", metres(falloff));
        Height
            .eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn measure_mode_emits_the_elevation() {
        // Measure mode passes the raw elevation through: a 0->1 ramp reads its own values.
        let input = ramp(16);
        let params = Params::new().with("output", ParamValue::Text("measure".into()));
        let out = Height
            .eval(Inputs::required_only(&[&input]), &params, &ctx())
            .unwrap()
            .remove(0);
        for x in 0..16 {
            let expected = x as f32 / 15.0;
            assert!((at(&out, x, 8) - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn an_elevation_inside_the_band_is_selected() {
        let out = select(&flat(8, 0.55), 0.4, 0.7, 0.1);
        assert!(at(&out, 4, 4) > 0.99, "in-band elevation selects ~1");
    }

    #[test]
    fn elevations_outside_the_band_are_excluded() {
        // Below the lower falloff (0.1 < 0.4 - 0.1) and above the upper falloff
        // (0.95 > 0.7 + 0.1) both fall to zero.
        assert!(at(&select(&flat(8, 0.1), 0.4, 0.7, 0.1), 4, 4) < 0.01);
        assert!(at(&select(&flat(8, 0.95), 0.4, 0.7, 0.1), 4, 4) < 0.01);
    }

    #[test]
    fn the_band_sits_in_the_middle_of_a_ramp() {
        // On a 0->1 ramp the band selects the middle elevations: low and high ends are
        // excluded, the centre is selected.
        let out = select(&ramp(32), 0.4, 0.7, 0.1);
        assert_eq!(at(&out, 0, 0), 0.0, "low elevation excluded");
        assert!(at(&out, 16, 0) > 0.9, "mid elevation selected");
        assert!(at(&out, 31, 0) < 0.01, "high elevation excluded");
    }

    #[test]
    fn falloff_softens_the_edge() {
        // Just past the upper edge: partial with a falloff, fully excluded without one.
        let soft = at(&select(&flat(8, 0.75), 0.4, 0.7, 0.1), 4, 4);
        let hard = at(&select(&flat(8, 0.75), 0.4, 0.7, 0.0), 4, 4);
        assert!(
            soft > 0.05 && soft < 0.95,
            "edge partial under falloff: {soft}"
        );
        assert_eq!(hard, 0.0, "no falloff is a hard cutoff");
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = select(&flat(8, 0.55), 0.4, 0.7, 0.1);
        assert!(
            out.layer(layers::MASK).is_none(),
            "no mask layer is written"
        );
        assert!(at(&out, 4, 4) > 0.0, "the selection is on the height layer");
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = flat(8, 0.55);
        input.set_layer("flow", Arc::new(Layer::filled(8, 8, 0.7)));
        let out = select(&input, 0.4, 0.7, 0.1);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = select(&ramp(16), 0.4, 0.7, 0.1);
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
        let input = ramp(16);
        assert_eq!(
            select(&input, 0.4, 0.7, 0.1).content_hash(),
            select(&input, 0.4, 0.7, 0.1).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        let out = select(&ramp(16), 0.4, 0.7, 0.1);
        assert_eq!(out.content_hash().to_u64(), 0x67e6_3d07_9808_aad1);
    }
}
