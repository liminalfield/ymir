//! Invert: flips the `height` layer (`1 - height`). One job, no parameters.
//!
//! Deliberately atomic: a graph that inverts should *read* as an Invert node, not
//! as a remap with its output range swapped. Mask-aware per the convention — the
//! inverted height is composited over the original through the `mask` layer, so
//! `mask = 1` fully inverts and `mask = 0` is the original. Other layers pass
//! through.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{EvalContext, Field, Layer, NodeSpec, Operator, Params, PortSpec, Result, layers};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.invert";

/// Invert the height layer: one input, one output, no parameters.
#[derive(Clone)]
pub struct Invert;

impl Operator for Invert {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            tags: &["invert", "flip", "negate", "modifier"],
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: Vec::new(),
        }
    }

    fn eval(&self, inputs: &[&Field], _params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        let inverted = Layer::from_fn(width, height, |x, y| {
            let original = h.get(x, y).unwrap_or(0.0);
            // Composite the inverted value over the original through the mask.
            let m = mask.get(x, y).unwrap_or(1.0);
            original + ((1.0 - original) - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(inverted));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Invert) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    fn field_with(height: f32, mask: Option<f32>) -> Field {
        let mut f = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, height)));
        if let Some(m) = mask {
            f.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, m)));
        }
        f
    }

    fn invert(input: &Field) -> Field {
        Invert
            .eval(&[input], &Params::new(), &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn inverts_height() {
        assert!((at(&invert(&field_with(0.3, None)), 0, 0) - 0.7).abs() < 1e-6);
        assert!((at(&invert(&field_with(0.0, None)), 0, 0) - 1.0).abs() < 1e-6);
        assert!((at(&invert(&field_with(1.0, None)), 0, 0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn double_invert_is_identity() {
        let once = invert(&field_with(0.42, None));
        let twice = invert(&once);
        assert!((at(&twice, 0, 0) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn mask_modulates_the_invert() {
        // Half mask on 0.3: halfway between original (0.3) and inverted (0.7).
        assert!((at(&invert(&field_with(0.3, Some(0.5))), 0, 0) - 0.5).abs() < 1e-6);
        // Fully masked out: unchanged.
        assert!((at(&invert(&field_with(0.3, Some(0.0))), 0, 0) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = field_with(0.5, None);
        input.set_layer("flow", Arc::new(Layer::filled(8, 8, 0.9)));
        assert_eq!(
            invert(&input).layer("flow").unwrap().get(0, 0).unwrap(),
            0.9
        );
    }

    #[test]
    fn is_deterministic() {
        let input = field_with(0.4, None);
        assert_eq!(invert(&input).content_hash(), invert(&input).content_hash());
    }
}
