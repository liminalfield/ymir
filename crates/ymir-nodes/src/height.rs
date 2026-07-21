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
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.height";

/// Default band: a mid-elevation selection that shows the band shape out of the box.
const DEFAULT_MIN: f64 = 0.4;
const DEFAULT_MAX: f64 = 0.7;
const DEFAULT_FALLOFF: f64 = 0.1;

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
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MIN),
                ),
                ParamSpec::new(
                    "max",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MAX),
                ),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FALLOFF),
                ),
                crate::selector::output_param(),
            ],
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

        let min = params.get_f64("min", DEFAULT_MIN) as f32;
        let max = params.get_f64("max", DEFAULT_MAX) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0) as f32;

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

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
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

    fn select(input: &Field, min: f32, max: f32, falloff: f32) -> Field {
        let params = Params::new()
            .with("min", ParamValue::Float(f64::from(min)))
            .with("max", ParamValue::Float(f64::from(max)))
            .with("falloff", ParamValue::Float(f64::from(falloff)));
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
