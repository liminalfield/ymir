//! Curve: reshapes the `height` layer through an editable transfer curve.
//!
//! This is height shaping done right (the remap/curve of #15): the transfer
//! function is a visual curve you draw, not a handful of opaque sliders. Each
//! height is mapped through the curve (with `[0, 1]` as the working domain; values
//! off the ends continue along the endpoint slope, so an identity curve passes
//! out-of-range height through untouched and never clips it into a plateau).
//! Mask-aware per the convention: the
//! shaped height is composited over the original through the `mask` layer, so
//! `mask = 1` is fully shaped and `mask = 0` is the original. Other layers pass
//! through.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Curve, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.curve";

/// Curve shaping modifier: one input, one output.
#[derive(Clone)]
pub struct CurveNode;

impl Operator for CurveNode {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "curve",
                ParamKind::Curve,
                ParamValue::Curve(Curve::identity()),
            )],
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
        let mask = input.layer_or(layers::MASK, 1.0);

        let identity = Curve::identity();
        let curve = params.get_curve("curve", &identity);
        // Precompute the curve's tangents once, then sample per cell.
        let sample = curve.sampler();

        let shaped = Layer::from_fn(width, height, |x, y| {
            let original = h.get(x, y).unwrap_or(0.0);
            let mapped = sample(original);
            let m = mask.get(x, y).unwrap_or(1.0);
            original + (mapped - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(CurveNode) }
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

    fn shape(input: &Field, curve: Curve) -> Field {
        let params = Params::new().with("curve", ParamValue::Curve(curve));
        CurveNode
            .eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn identity_curve_passes_height_through() {
        assert!((at(&shape(&field_with(0.4, None), Curve::identity()), 0, 0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn identity_curve_passes_out_of_range_height_through() {
        // Regression: an upstream Blend can push height above 1. An unadjusted (identity)
        // Curve must leave it alone, not clip it to a 1.0 plateau. The identity extends
        // y = x past the ends, so 1.4 stays 1.4 and a negative stays negative.
        assert!((at(&shape(&field_with(1.4, None), Curve::identity()), 0, 0) - 1.4).abs() < 1e-6);
        assert!(
            (at(&shape(&field_with(-0.2, None), Curve::identity()), 0, 0) - (-0.2)).abs() < 1e-6
        );
    }

    #[test]
    fn an_inverting_curve_inverts() {
        let inv = Curve::new([(0.0, 1.0), (1.0, 0.0)]);
        assert!((at(&shape(&field_with(0.3, None), inv), 0, 0) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn mask_modulates_the_shaping() {
        let inv = Curve::new([(0.0, 1.0), (1.0, 0.0)]);
        // Half mask on 0.3: halfway between original (0.3) and shaped (0.7).
        assert!((at(&shape(&field_with(0.3, Some(0.5)), inv), 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = field_with(0.5, None);
        input.set_layer("flow", Arc::new(Layer::filled(8, 8, 0.9)));
        let out = shape(&input, Curve::identity());
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn is_deterministic() {
        let inv = Curve::new([(0.0, 1.0), (0.5, 0.2), (1.0, 0.0)]);
        let input = field_with(0.6, None);
        assert_eq!(
            shape(&input, inv.clone()).content_hash(),
            shape(&input, inv).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        let input = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |x, _| x as f32 / 15.0)),
        );
        // A peak curve: 0 -> 0, 0.5 -> 1, 1 -> 0.
        let peak = Curve::new([(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
        let out = shape(&input, peak);
        assert_eq!(out.content_hash().to_u64(), 0x61ba_1b3b_65e0_e391);
    }
}
