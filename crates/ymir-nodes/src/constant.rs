//! Constant: a flat field at a chosen value.
//!
//! Fills the `height` layer with a single greyscale value everywhere. A generator by arity (no
//! inputs, one output): the simplest source, for a fixed level to blend against, offset or scale
//! with, threshold, or stand in as a uniform control field. `value` is the working `[0, 1]`
//! greyscale, the same range you read off the preview, though like all height it is not
//! hard-clamped downstream. Resolution- and world-independent, so `NO_WORLD` and byte-identical at
//! any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.constant";

/// Default value: mid grey, so the node shows a neutral level out of the box.
const DEFAULT_VALUE: f64 = 0.5;

/// Constant generator: no inputs, one output. Writes a uniform value to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Constant;

impl Operator for Constant {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: vec![],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "value",
                ParamKind::Float { min: 0.0, max: 1.0 },
                ParamValue::Float(DEFAULT_VALUE),
            )],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Independent of every world global and of resolution: the value is the same everywhere, so no
    /// world-setting slider invalidates this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let value = params.get_f64("value", DEFAULT_VALUE) as f32;
        let layer = Layer::filled(ctx.width, ctx.height, value);
        let field = Field::new(ctx.width, ctx.height, ctx.region)
            .with_layer(layers::HEIGHT, Arc::new(layer));
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Constant) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "source", sort: 51 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
    }

    fn eval(value: f64) -> Field {
        let params = Params::new().with("value", ParamValue::Float(value));
        Constant
            .eval(Inputs::required_only(&[]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    #[test]
    fn fills_the_field_with_the_value() {
        let out = eval(0.3);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (v - 0.3).abs() < 1e-6),
            "every cell should hold the value"
        );
    }

    #[test]
    fn defaults_to_mid_grey() {
        let params = Params::new();
        let out = Constant
            .eval(Inputs::required_only(&[]), &params, &ctx())
            .unwrap()
            .remove(0);
        assert_eq!(out.layer(layers::HEIGHT).unwrap().get(8, 8).unwrap(), 0.5);
    }

    #[test]
    fn output_matches_the_context_resolution() {
        let out = eval(0.5);
        assert_eq!(out.width(), 16);
        assert_eq!(out.height(), 16);
    }

    #[test]
    fn is_byte_identical_across_runs() {
        assert_eq!(eval(0.42).content_hash(), eval(0.42).content_hash());
    }
}
