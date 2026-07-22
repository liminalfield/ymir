//! Normalize: fit the height layer's actual range to `[0, 1]`.
//!
//! Remaps the layer's actual `[min, max]` onto `[0, 1]`: the lowest cell becomes 0, the highest 1,
//! the rest linearly between. The one-click "fit to range" companion to Levels, which sets the
//! window by hand — for pulling a selector's raw measure (slope in degrees, curvature in RMS units)
//! or a height that drifted out of range back into the working greyscale. A flat layer has no range
//! to fit, so it passes through unchanged.
//!
//! Mask-aware per the convention: the normalized height is composited over the original through the
//! `mask` layer. A pure per-cell remap after a deterministic range read, so `NO_WORLD` and
//! byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, Params, PortSpec, Result,
    layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.normalize";

/// Normalize modifier: one input, one output.
#[derive(Clone)]
pub struct Normalize;

impl Operator for Normalize {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![],
        }
    }

    /// A pure per-cell remap of the height value: it reads no world global, so no world-setting
    /// slider invalidates this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, _params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        // The actual range to fit. A deterministic reduction, so the node stays byte-exact.
        let (min, max) = h.value_range();
        let span = max - min;

        let shaped = Layer::from_par_fn(width, height, |x, y| {
            let v = h.get(x, y).unwrap_or(0.0);
            // A flat layer (zero span) has no range to fit, so leave the value as it is.
            let normalized = if span > f32::EPSILON {
                (v - min) / span
            } else {
                v
            };
            let m = mask.get(x, y).unwrap_or(1.0);
            v + (normalized - v) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Normalize) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
    }

    fn eval(input: &Field) -> Field {
        Normalize
            .eval(Inputs::required_only(&[input]), &Params::new(), &ctx())
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
    fn fits_the_actual_range_to_unit() {
        // A [0.2, 0.8] ramp stretches so the min cell reads 0 and the max cell reads 1.
        let out = eval(&ramp(16, 0.2, 0.8));
        assert!((at(&out, 0, 8) - 0.0).abs() < 1e-6, "min maps to 0");
        assert!((at(&out, 15, 8) - 1.0).abs() < 1e-6, "max maps to 1");
        assert!(
            (at(&out, 8, 8) - 8.0 / 15.0).abs() < 1e-6,
            "midpoint stays proportional"
        );
    }

    #[test]
    fn negative_and_over_range_values_come_into_unit() {
        let out = eval(&ramp(16, -0.5, 1.5));
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (-1e-6..=1.0 + 1e-6).contains(&v)),
            "everything lands in [0, 1]"
        );
    }

    #[test]
    fn a_flat_field_passes_through() {
        let flat = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, 0.37)));
        assert!(
            eval(&flat)
                .layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (v - 0.37).abs() < 1e-6),
            "a constant field has no range to fit and is unchanged"
        );
    }

    #[test]
    fn mask_gates_the_remap() {
        let mut input = ramp(16, 0.2, 0.8);
        input.set_layer(
            layers::MASK,
            Arc::new(Layer::from_fn(16, 16, |_, y| if y < 8 { 0.0 } else { 1.0 })),
        );
        let out = eval(&input);
        // Masked-out row keeps the original ramp value; unmasked row is normalized.
        assert!((at(&out, 0, 0) - 0.2).abs() < 1e-6, "mask 0 keeps original");
        assert!(
            (at(&out, 0, 12) - 0.0).abs() < 1e-6,
            "mask 1 normalizes (min -> 0)"
        );
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(16, 0.2, 0.8);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.7)));
        assert_eq!(eval(&input).layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn is_byte_identical_across_runs() {
        let input = ramp(16, 0.2, 0.8);
        assert_eq!(eval(&input).content_hash(), eval(&input).content_hash());
    }
}
