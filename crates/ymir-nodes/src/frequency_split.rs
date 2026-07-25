//! Frequency Split: separate the height layer into a low- and a high-frequency band.
//!
//! The low band is a Gaussian blur of the height at a world-unit cut radius (the same
//! scale mechanism as [`crate::blur`]); the high band is the residual, `input - low`.
//! The two recombine exactly to the input, so a graph can reshape or erode the large
//! forms on the low band and re-add the preserved fine grain from the high band. This
//! makes the "work the big forms, keep the surface detail" principle a visible wiring
//! rather than a trick buried in a node's parameters.
//!
//! One input, two outputs (`low`, `high`). Every other layer passes through unchanged on
//! both outputs (cheap via `Arc`).

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.frequency_split";

/// Default cut radius in world units (meters): coarse enough to separate broad landforms
/// from surface detail on a default-sized world, while staying a visible split out of the box.
const DEFAULT_RADIUS: f64 = 64.0;

/// Frequency-band split: one input, two outputs (`low`, `high`).
#[derive(Clone)]
pub struct FrequencySplit;

impl Operator for FrequencySplit {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("low"), PortSpec::new("high")],
            params: vec![
                ParamSpec::new(
                    "radius",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_RADIUS),
                )
                .with_unit(Unit::Meters),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads only the world horizontal extent (the cut radius is a world-unit length), so the
    /// world height and sea level never invalidate this node. Mirrors [`crate::blur`].
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        // The cut radius is a world-unit length; convert to a cell-space sigma through the world
        // extent so the same radius cuts at the same physical scale at any resolution.
        let radius_m = params.get_f64("radius", DEFAULT_RADIUS).max(0.0);
        let sigma = ctx.world_to_cells(radius_m);
        let low = crate::blur::gaussian_blur(h.as_slice(), width, height, sigma);

        // High = input - low: the residual detail, centred on zero and carrying negative values.
        // Not clamped, so `low + high` recovers the input exactly and the full range is preserved
        // through the graph (per the height-range convention).
        let high: Vec<f32> = h
            .as_slice()
            .iter()
            .zip(&low)
            .map(|(&hi, &lo)| hi - lo)
            .collect();

        let mut low_field = input.clone();
        low_field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_vec(width, height, low)),
        );
        let mut high_field = input.clone();
        high_field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_vec(width, height, high)),
        );
        Ok(vec![low_field, high_field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(FrequencySplit) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(res: usize, world_extent: f64) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0).with_world_extent(world_extent)
    }

    /// Runs the split and returns `(low, high)`.
    fn run(input: &Field, radius_m: f64, ctx: &EvalContext) -> (Field, Field) {
        let params = Params::new().with("radius", ParamValue::Float(radius_m));
        let mut out = FrequencySplit
            .eval(Inputs::required_only(&[input]), &params, ctx)
            .unwrap();
        assert_eq!(out.len(), 2, "expected low and high outputs");
        let high = out.remove(1);
        let low = out.remove(0);
        (low, high)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    fn constant(res: usize, v: f32) -> Field {
        Field::new(res, res, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(res, res, v)))
    }

    fn ramp(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                x as f32 / (res as f32 - 1.0)
            })),
        )
    }

    fn vertical_step(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                if x < res / 2 { 0.0 } else { 1.0 }
            })),
        )
    }

    #[test]
    fn bands_recombine_to_the_input() {
        // The defining property: low + high == input, cell for cell.
        let input = ramp(32);
        let (low, high) = run(&input, 6.0, &ctx(32, 32.0));
        for y in 0..32 {
            for x in 0..32 {
                let sum = at(&low, x, y) + at(&high, x, y);
                assert!(
                    (sum - at(&input, x, y)).abs() < 1e-4,
                    "low+high != input at {x},{y}: {sum} vs {}",
                    at(&input, x, y)
                );
            }
        }
    }

    #[test]
    fn low_smooths_and_high_carries_the_edge() {
        // A vertical step: the low band softens the boundary, the high band holds the residual
        // there and is ~0 across the flat interior far from the edge.
        let res = 32;
        let (low, high) = run(&vertical_step(res), 4.0, &ctx(res, res as f64));
        let mid = res / 2;

        // Low: boundary columns pulled off the hard 0/1.
        assert!(at(&low, mid - 1, mid) > 0.001 && at(&low, mid - 1, mid) < 0.5);
        assert!(at(&low, mid, mid) > 0.5 && at(&low, mid, mid) < 0.999);

        // High: non-zero right at the step, essentially zero deep in the flat regions.
        assert!(at(&high, mid, mid).abs() > 0.001, "high flat at the edge");
        assert!(
            at(&high, 0, mid).abs() < 0.01,
            "high not ~0 far from the edge"
        );
    }

    #[test]
    fn zero_radius_puts_everything_in_low() {
        // No blur: low is the input and high is exactly zero (an identity split).
        let input = ramp(16);
        let (low, high) = run(&input, 0.0, &ctx(16, 16.0));
        for y in 0..16 {
            for x in 0..16 {
                assert!((at(&low, x, y) - at(&input, x, y)).abs() < 1e-6);
                assert!(at(&high, x, y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_constant_field_has_no_detail() {
        // A flat field is entirely low-frequency: high is ~0 everywhere.
        let (low, high) = run(&constant(16, 0.42), 4.0, &ctx(16, 16.0));
        for y in 0..16 {
            for x in 0..16 {
                assert!((at(&low, x, y) - 0.42).abs() < 1e-5);
                assert!(at(&high, x, y).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn both_outputs_pass_through_other_layers() {
        let mut input = constant(16, 0.5);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.9)));
        let (low, high) = run(&input, 4.0, &ctx(16, 16.0));
        assert_eq!(low.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
        assert_eq!(high.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(24);
        let c = ctx(24, 24.0);
        let (l1, h1) = run(&input, 5.0, &c);
        let (l2, h2) = run(&input, 5.0, &c);
        assert_eq!(l1.content_hash(), l2.content_hash());
        assert_eq!(h1.content_hash(), h2.content_hash());
    }
}
