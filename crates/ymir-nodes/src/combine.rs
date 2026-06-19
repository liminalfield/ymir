//! Combine/blend: a two-input modifier that merges two height fields by a chosen
//! operation.
//!
//! Inputs are `a` (the base) and `b` (the overlay). The op is selectable: add,
//! subtract, multiply, min, max, or a mask-weighted mix (linear interpolation from
//! `a` to `b`). The base's non-height layers pass through, and its `mask` layer drives the
//! mix weight. Mask-aware per the soft-layer contract: with no mask present the mix
//! weight is uniform (`1.0`), so the node never gates on a mask.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.combine";

/// Combine operation ids, in dropdown order. These are the stored param values.
const OP_ADD: &str = "add";
const OP_SUBTRACT: &str = "subtract";
const OP_MULTIPLY: &str = "multiply";
const OP_MIN: &str = "min";
const OP_MAX: &str = "max";
const OP_MIX: &str = "mix";
const OPS: &[&str] = &[OP_ADD, OP_SUBTRACT, OP_MULTIPLY, OP_MIN, OP_MAX, OP_MIX];

/// Two-input combine/blend modifier: inputs `a` (base) and `b` (overlay), one
/// output.
#[derive(Clone)]
pub struct Combine;

impl Operator for Combine {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "combine",
            tags: &[
                "combine", "blend", "mix", "add", "subtract", "multiply", "modifier",
            ],
            inputs: vec![PortSpec::new("a"), PortSpec::new("b")],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "op",
                ParamKind::Enum { options: OPS },
                ParamValue::Text(OP_ADD.to_string()),
            )],
        }
    }

    fn eval(&self, inputs: &[&Field], params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        // The evaluator gathers every input slot before calling eval, so both are
        // present; an unwired port would have failed upstream.
        let a = inputs[0];
        let b = inputs[1];
        let width = a.width();
        let height = a.height();

        let base = a.layer_or(layers::HEIGHT, 0.0);
        let overlay = b.layer_or(layers::HEIGHT, 0.0);
        // Soft contract: mix reads the base's mask if present, else a uniform 1.0.
        let mask = a.layer_or(layers::MASK, 1.0);

        let op = params.get_str("op", OP_ADD);

        let combined = Layer::from_fn(width, height, |x, y| {
            let av = base.get(x, y).unwrap_or(0.0);
            let bv = overlay.get(x, y).unwrap_or(0.0);
            match op {
                OP_SUBTRACT => av - bv,
                OP_MULTIPLY => av * bv,
                OP_MIN => av.min(bv),
                OP_MAX => av.max(bv),
                OP_MIX => {
                    let t = mask.get(x, y).unwrap_or(1.0);
                    av + (bv - av) * t
                }
                // OP_ADD, and any unrecognized id, add: the safe, neutral default.
                _ => av + bv,
            }
        });

        let mut out = a.clone();
        out.set_layer(layers::HEIGHT, Arc::new(combined));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Combine) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        // Combine reads its input fields' dimensions, not the context's, so the
        // context's resolution is irrelevant here.
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    fn const_field(value: f32) -> Field {
        Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, value)))
    }

    fn combine(a: &Field, b: &Field, op: &str) -> Field {
        let params = Params::new().with("op", ParamValue::Text(op.to_string()));
        Combine.eval(&[a, b], &params, &ctx()).unwrap().remove(0)
    }

    fn height_at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn ops_combine_two_heights() {
        let a = const_field(0.6);
        let b = const_field(0.5);
        for (op, expected) in [
            (OP_ADD, 1.1_f32),
            (OP_SUBTRACT, 0.1),
            (OP_MULTIPLY, 0.30),
            (OP_MIN, 0.5),
            (OP_MAX, 0.6),
        ] {
            let out = combine(&a, &b, op);
            let v = height_at(&out, 0, 0);
            assert!((v - expected).abs() < 1e-6, "op {op}: {v} != {expected}");
        }
    }

    #[test]
    fn mix_lerps_from_a_to_b_by_the_mask() {
        let mut a = const_field(0.0);
        a.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, 0.25)));
        let b = const_field(1.0);
        // lerp(0, 1, 0.25) == 0.25.
        assert!((height_at(&combine(&a, &b, OP_MIX), 0, 0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn mix_without_a_mask_applies_uniformly() {
        let a = const_field(0.2);
        let b = const_field(0.8);
        let out = combine(&a, &b, OP_MIX);
        let layer = out.layer(layers::HEIGHT).unwrap();
        let v = layer.get(0, 0).unwrap();
        // Mask defaults to 1.0, so the mix is the overlay everywhere, uniformly.
        assert!((v - 0.8).abs() < 1e-6);
        assert!(layer.as_slice().iter().all(|&x| (x - v).abs() < 1e-6));
    }

    #[test]
    fn passes_through_the_bases_other_layers() {
        let mut a = const_field(0.3);
        a.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, 0.42)));
        let b = const_field(0.1);
        let out = combine(&a, &b, OP_ADD);
        // The base's mask survives unchanged on the output.
        assert_eq!(out.layer(layers::MASK).unwrap().get(0, 0).unwrap(), 0.42);
    }

    #[test]
    fn is_deterministic() {
        let a = const_field(0.3);
        let b = const_field(0.7);
        assert_eq!(
            combine(&a, &b, OP_MAX).content_hash(),
            combine(&a, &b, OP_MAX).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        // Two orthogonal gradients added: a fixed, textured output to pin behavior.
        let a = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |x, _| x as f32 / 15.0)),
        );
        let b = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |_, y| y as f32 / 15.0)),
        );
        let out = combine(&a, &b, OP_ADD);
        assert_eq!(out.content_hash().to_u64(), 0x4006_cb7a_67c5_0acd);
    }
}
