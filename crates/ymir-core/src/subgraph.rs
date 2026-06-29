//! Subgraph nesting: the boundary markers (and, later, the container node).
//!
//! These are the one deliberate exception to "`ymir-core` holds no concrete nodes"
//! (see the workspace layout in `CLAUDE.md`). They are generic graph machinery, not
//! terrain operators, and the container that builds on them must call the evaluator,
//! which lives in this crate; an operator in `ymir-nodes` could not, since the
//! dependency arrow points the other way. The additive invariant still holds: the
//! evaluator dispatches through `dyn Operator` and never names these types, and the
//! container recognises a marker structurally (by `type_id`), not by branching on what
//! a node "is".
//!
//! A subgraph (the container, added next) is a node holding an inner [`Graph`] whose
//! ports are derived from these markers: an [`InputNode`] marks an inner field fed from
//! outside, an [`OutputNode`] marks an inner field exposed outside. See
//! `docs/design/subgraphs.md`.

use std::sync::Arc;

use crate::context::EvalContext;
use crate::error::Result;
use crate::field::Field;
use crate::layer::Layer;
use crate::layers;
use crate::operator::{Inputs, Operator};
use crate::param::Params;
use crate::registry::OperatorEntry;
use crate::spec::{NodeSpec, PortSpec};

/// Type id of a subgraph input marker.
pub(crate) const INPUT_TYPE_ID: &str = "subgraph.input";
/// Type id of a subgraph output marker.
pub(crate) const OUTPUT_TYPE_ID: &str = "subgraph.output";

/// An input boundary marker: a source inside a subgraph whose single output stands in
/// for the field supplied to the matching container input port.
///
/// During a container run the evaluator binds this node's output to the boundary field
/// (via [`Graph::evaluate_bound`](crate::Graph::evaluate_bound)), so [`eval`](InputNode::eval)
/// is not called there. It runs only when the inner graph is evaluated on its own
/// (previewing it while editing), where it yields a flat zero height field as the
/// graceful stand-in for "nothing wired in yet".
#[derive(Clone)]
pub struct InputNode;

impl Operator for InputNode {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: INPUT_TYPE_ID,
            category: "utility",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: Vec::new(),
        }
    }

    fn eval(&self, _inputs: Inputs, _params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let layer = Layer::filled(ctx.width, ctx.height, 0.0);
        Ok(vec![
            Field::new(ctx.width, ctx.height, ctx.region)
                .with_layer(layers::HEIGHT, Arc::new(layer)),
        ])
    }
}

/// An output boundary marker: a sink inside a subgraph whose input is the field exposed
/// on the matching container output port.
///
/// It produces no field of its own. The container reads the field *feeding* it (through
/// the inner graph's connection) to fill its matching output port, so this body's result
/// is unused; like any endpoint it returns nothing.
#[derive(Clone)]
pub struct OutputNode;

impl Operator for OutputNode {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: OUTPUT_TYPE_ID,
            category: "utility",
            inputs: vec![PortSpec::new("in")],
            outputs: Vec::new(),
            params: Vec::new(),
        }
    }

    fn eval(&self, _inputs: Inputs, _params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        Ok(Vec::new())
    }
}

inventory::submit! { OperatorEntry { type_id: INPUT_TYPE_ID, make: || Box::new(InputNode) } }
inventory::submit! { OperatorEntry { type_id: OUTPUT_TYPE_ID, make: || Box::new(OutputNode) } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Region;
    use crate::spec::NodeKind;

    fn ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    #[test]
    fn input_marker_is_a_generator_yielding_a_zero_field() {
        let spec = InputNode.spec();
        assert_eq!(spec.kind(), NodeKind::Generator, "no inputs, one output");
        assert_eq!(spec.type_id, INPUT_TYPE_ID);

        let out = InputNode
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx())
            .unwrap();
        // The standalone stand-in is a flat zero height field at the requested resolution.
        let height = out[0].layer(layers::HEIGHT).expect("height layer");
        assert!(height.as_slice().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn output_marker_is_an_endpoint_producing_nothing() {
        let spec = OutputNode.spec();
        assert_eq!(spec.kind(), NodeKind::Endpoint, "one input, no outputs");
        assert_eq!(spec.type_id, OUTPUT_TYPE_ID);

        let input = Field::new(8, 8, Region::UNIT);
        let out = OutputNode
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        assert!(out.is_empty(), "a sink marker emits no field");
    }

    #[test]
    fn both_markers_are_registered() {
        assert!(
            crate::registry::make(INPUT_TYPE_ID).is_some(),
            "input marker is registered"
        );
        assert!(
            crate::registry::make(OUTPUT_TYPE_ID).is_some(),
            "output marker is registered"
        );
    }
}
