//! Null: a pass-through that returns its input unchanged.
//!
//! The no-op of the graph, modelled on Houdini's Null. It does nothing to the field — every
//! layer passes through untouched — so it earns its place as a stable, named point to wire
//! into rather than as an operation. Tap a byproduct (a `water` output, a selection) into a
//! Null and select it to view that field on its own; drop one as a reroute or organizing
//! anchor to keep a long graph legible; or use it as a fixed reference whose downstream
//! wiring stays put while the upstream is re-plumbed.

use ymir_core::registry::OperatorEntry;
use ymir_core::{EvalContext, Field, Inputs, NodeSpec, Operator, Params, PortSpec, Result};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.null";

/// Null (pass-through) modifier: one input, one output, no parameters.
#[derive(Clone)]
pub struct Null;

impl Operator for Null {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "utility",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: Vec::new(),
        }
    }

    fn eval(&self, inputs: Inputs, _params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        // Pass the input through unchanged: every layer, untouched (the field clone is cheap,
        // its layers are `Arc`). The memo cache makes this a no-cost relay in practice.
        Ok(vec![inputs[0].clone()])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Null) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{Layer, Region, layers};

    fn ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    /// A field carrying more than just height, to prove every layer survives.
    fn multi_layer_field() -> Field {
        let mut f = Field::new(8, 8, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(8, 8, |x, _| x as f32 / 7.0)),
        );
        f.set_layer(layers::WATER, Arc::new(Layer::filled(8, 8, 0.3)));
        f.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, 0.5)));
        f
    }

    #[test]
    fn passes_the_field_through_unchanged() {
        let input = multi_layer_field();
        let out = Null
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap()
            .remove(0);
        // Byte-identical: the whole field, every layer.
        assert_eq!(out.content_hash(), input.content_hash());
    }

    #[test]
    fn spec_is_a_modifier_in_utility() {
        let spec = Null.spec();
        assert_eq!(spec.kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(spec.type_id, TYPE_ID);
        assert_eq!(spec.category, "utility");
        assert!(spec.params.is_empty(), "Null takes no parameters");
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = multi_layer_field();
        let made = ymir_core::registry::make(TYPE_ID).expect("null operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let direct = Null
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        assert_eq!(via_registry[0].content_hash(), direct[0].content_hash());
    }
}
