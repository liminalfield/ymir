//! Clamp: limit the height layer to a `[min, max]` range.
//!
//! Hard-clamps every cell into `[min, max]`: values below `min` become `min`, above `max` become
//! `max`, the rest pass through. The explicit, single-purpose companion to Levels, which can clip as
//! a side effect of its window — for capping a height that overshot, flooring a basin to a sea
//! bottom, or bounding a measure before it feeds something range-sensitive. `min` above `max` is
//! tolerated (the bounds are ordered), yielding a flat clamp at the lower of the two.
//!
//! Mask-aware per the convention: the clamped height is composited over the original through the
//! `mask` layer. A pure per-cell transform, so `NO_WORLD` and byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.clamp";

/// Default lower bound: the bottom of the working greyscale.
const DEFAULT_MIN: f64 = 0.0;
/// Default upper bound: the top of the working greyscale.
const DEFAULT_MAX: f64 = 1.0;

/// Clamp modifier: one input, one output.
#[derive(Clone)]
pub struct Clamp;

impl Operator for Clamp {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float {
                        min: -4.0,
                        max: 4.0,
                    },
                    ParamValue::Float(DEFAULT_MIN),
                ),
                ParamSpec::new(
                    "max",
                    ParamKind::Float {
                        min: -4.0,
                        max: 4.0,
                    },
                    ParamValue::Float(DEFAULT_MAX),
                ),
            ],
        }
    }

    /// A pure per-cell transform of the height value: it reads no world global, so no world-setting
    /// slider invalidates this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        let a = params.get_f64("min", DEFAULT_MIN) as f32;
        let b = params.get_f64("max", DEFAULT_MAX) as f32;
        // Order the bounds so `min` above `max` is a flat clamp rather than a panic in `f32::clamp`.
        let lo = a.min(b);
        let hi = a.max(b);

        let shaped = Layer::from_par_fn(width, height, |x, y| {
            let v = h.get(x, y).unwrap_or(0.0);
            let clamped = v.clamp(lo, hi);
            let m = mask.get(x, y).unwrap_or(1.0);
            v + (clamped - v) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Clamp) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
    }

    fn clamp(input: &Field, min: f64, max: f64) -> Field {
        let params = Params::new()
            .with("min", ParamValue::Float(min))
            .with("max", ParamValue::Float(max));
        Clamp
            .eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A field ramping across x between `lo` and `hi`.
    fn ramp(size: usize, lo: f32, hi: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                lo + (hi - lo) * x as f32 / (size - 1) as f32
            })),
        )
    }

    #[test]
    fn bounds_the_values() {
        // A [0, 1] ramp clamped to [0.25, 0.75]: ends flatten to the bounds, the middle passes.
        let out = clamp(&ramp(16, 0.0, 1.0), 0.25, 0.75);
        assert!(
            (at(&out, 0, 8) - 0.25).abs() < 1e-6,
            "below-min floors to min"
        );
        assert!(
            (at(&out, 15, 8) - 0.75).abs() < 1e-6,
            "above-max caps to max"
        );
        let mid = at(&out, 8, 8);
        assert!(
            (0.25..=0.75).contains(&mid),
            "in-range value passes through: {mid}"
        );
    }

    #[test]
    fn reversed_bounds_do_not_panic() {
        // min above max is ordered rather than panicking: a flat clamp at the lower bound.
        let out = clamp(&ramp(16, 0.0, 1.0), 0.75, 0.25);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.25..=0.75).contains(&v)),
            "reversed bounds still clamp into [0.25, 0.75]"
        );
    }

    #[test]
    fn mask_gates_the_clamp() {
        let mut input = ramp(16, 0.0, 1.0);
        input.set_layer(
            layers::MASK,
            Arc::new(Layer::from_fn(16, 16, |_, y| if y < 8 { 0.0 } else { 1.0 })),
        );
        let out = clamp(&input, 0.0, 0.5);
        // The high end of the ramp: masked-out row keeps the original, unmasked row is capped.
        assert!(
            (at(&out, 15, 0) - 1.0).abs() < 1e-6,
            "mask 0 keeps original"
        );
        assert!((at(&out, 15, 12) - 0.5).abs() < 1e-6, "mask 1 caps to max");
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(16, 0.0, 1.0);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.7)));
        assert_eq!(
            clamp(&input, 0.0, 0.5)
                .layer("flow")
                .unwrap()
                .get(0, 0)
                .unwrap(),
            0.7
        );
    }

    #[test]
    fn is_byte_identical_across_runs() {
        let input = ramp(16, 0.0, 1.0);
        assert_eq!(
            clamp(&input, 0.25, 0.75).content_hash(),
            clamp(&input, 0.25, 0.75).content_hash()
        );
    }
}
