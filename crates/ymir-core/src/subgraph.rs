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
use crate::error::{Error, Result};
use crate::eval::{EvalCache, EvalRequest};
use crate::field::Field;
use crate::graph::{Graph, NodeId};
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
/// Type id of the subgraph container.
pub(crate) const SUBGRAPH_TYPE_ID: &str = "subgraph";

/// Maximum subgraph nesting depth. Generous on purpose: nesting is finite by construction
/// (template instantiation, so a subgraph cannot contain itself), so this only guards a
/// pathologically deep but finite evaluation stack, turning a would-be overflow into a
/// reported [`Error::NestingTooDeep`] rather than constraining real use.
pub const MAX_NESTING_DEPTH: u32 = 64;

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

/// A subgraph container: a node holding an inner [`Graph`] whose ports are derived from the
/// inner graph's [`InputNode`]/[`OutputNode`] markers. Evaluating it runs the inner graph
/// with the boundary inputs bound to the input markers and reads the fields feeding the
/// output markers.
///
/// Template instantiation (see `docs/design/subgraphs.md`): it holds a concrete *copy* of
/// the inner graph, never a link to a shared definition, so two instances are independent
/// and editing one cannot disturb another.
#[derive(Clone)]
pub struct SubgraphNode {
    inner: Graph,
    /// Precomputed content hash of `inner`, returned from [`Operator::content_hash`] and
    /// folded into the node's cache key so editing the inside invalidates the cached output.
    /// Precomputed because the evaluator calls `content_hash` per key.
    inner_hash: u64,
}

impl SubgraphNode {
    /// Wraps an inner graph as a subgraph container, fingerprinting it for the cache key.
    #[must_use]
    pub fn new(inner: Graph) -> Self {
        let inner_hash = inner.content_hash();
        Self { inner, inner_hash }
    }

    /// An empty subgraph (no markers, so no ports): the registry default, which the editor
    /// fills in by diving into it.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Graph::new())
    }

    /// Builds the boundary ports for `marker_type`, ordered by `stable_id` and named from
    /// each marker's name override, falling back to a positional default (`in0`, `out1`).
    fn boundary_ports(&self, marker_type: &str, default_prefix: &str) -> Vec<PortSpec> {
        self.inner
            .nodes_of_type(marker_type)
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let name = self
                    .inner
                    .name(id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{default_prefix}{i}"));
                PortSpec::new(name)
            })
            .collect()
    }
}

impl Operator for SubgraphNode {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: SUBGRAPH_TYPE_ID,
            category: "utility",
            inputs: self.boundary_ports(INPUT_TYPE_ID, "in"),
            outputs: self.boundary_ports(OUTPUT_TYPE_ID, "out"),
            params: Vec::new(),
        }
    }

    fn content_hash(&self) -> Option<u64> {
        Some(self.inner_hash)
    }

    fn eval(&self, inputs: Inputs, _params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        // Stack-safety backstop. Nesting is finite by construction, so this only catches a
        // pathologically deep but finite graph, reporting instead of overflowing the stack.
        if ctx.depth() >= MAX_NESTING_DEPTH {
            return Err(Error::NestingTooDeep {
                limit: MAX_NESTING_DEPTH,
            });
        }

        // Bind each input marker (in port order) to the field on the matching port. Every
        // subgraph input port is required, so the evaluator guarantees `inputs[port]`.
        let input_markers = self.inner.nodes_of_type(INPUT_TYPE_ID);
        let mut bound: Vec<(NodeId, Field)> = Vec::with_capacity(input_markers.len());
        for (port, &marker) in input_markers.iter().enumerate() {
            bound.push((marker, inputs[port].clone()));
        }

        // Each output marker exposes the field feeding it; resolve that source edge. An
        // unwired output marker is a broken inner graph, surfaced as this node failing.
        let output_markers = self.inner.nodes_of_type(OUTPUT_TYPE_ID);
        let mut outputs: Vec<(NodeId, usize)> = Vec::with_capacity(output_markers.len());
        for &marker in &output_markers {
            let source = self
                .inner
                .input_source(marker, 0)
                .ok_or(Error::DisconnectedInput {
                    type_id: OUTPUT_TYPE_ID,
                    port: 0,
                })?;
            outputs.push(source);
        }

        // Run the inner graph one nesting level deeper, threading the world settings and
        // cancellation through. The inner cache is transient (see issue #125): the container
        // itself memoizes in the outer cache, so an unchanged subgraph is never re-run, and
        // within this one call the evaluator's working set pins the active path regardless of
        // cache capacity.
        let request = EvalRequest::new(ctx.width, ctx.height, ctx.region, ctx.seed)
            .with_world_extent(ctx.world_extent())
            .with_world_height(ctx.world_height())
            .with_cancel(ctx.cancel_token())
            .with_depth(ctx.depth() + 1);
        let mut cache = EvalCache::new(0);
        self.inner
            .evaluate_bound(&bound, &outputs, &request, &mut cache)
    }
}

inventory::submit! {
    OperatorEntry { type_id: SUBGRAPH_TYPE_ID, make: || Box::new(SubgraphNode::empty()) }
}

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

    use crate::eval::{EvalCache, EvalRequest};
    use crate::graph::{Graph, NodeId};

    /// A test-only generator emitting a uniform height, so a subgraph's output is a
    /// recognizable value. Built programmatically; never registered.
    #[derive(Clone)]
    struct ConstGen {
        value: f32,
    }

    impl Operator for ConstGen {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.const",
                category: "test",
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
            Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region).with_layer(
                    layers::HEIGHT,
                    Arc::new(Layer::filled(ctx.width, ctx.height, self.value)),
                ),
            ])
        }
    }

    /// An inner graph of Input -> Output: a passthrough whose single output is its single
    /// input. Returns the graph and its two marker ids.
    fn identity_inner() -> (Graph, NodeId, NodeId) {
        let mut inner = Graph::new();
        let input = inner.add_op(Box::new(InputNode), Params::new());
        let output = inner.add_op(Box::new(OutputNode), Params::new());
        inner
            .connect(input, 0, output, 0)
            .expect("wire input to output");
        (inner, input, output)
    }

    #[test]
    fn ports_derive_from_markers_in_stable_id_order_with_names() {
        let mut inner = Graph::new();
        let a = inner.add_op(Box::new(InputNode), Params::new());
        let b = inner.add_op(Box::new(InputNode), Params::new());
        inner.set_name(a, Some("base".to_string())).expect("name a");
        let out = inner.add_op(Box::new(OutputNode), Params::new());
        inner.connect(b, 0, out, 0).expect("wire");

        let spec = SubgraphNode::new(inner).spec();
        assert_eq!(spec.inputs.len(), 2);
        // a has the smaller stable_id, so it is the first port, named from its override;
        // b falls back to the positional default.
        assert_eq!(spec.inputs[0].name, "base");
        assert_eq!(spec.inputs[1].name, "in1");
        assert_eq!(spec.outputs.len(), 1);
    }

    #[test]
    fn identity_subgraph_returns_its_input_field() {
        let (inner, _, _) = identity_inner();
        let sg = SubgraphNode::new(inner);

        let field = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, 0.42)));
        let ctx = EvalContext::new(8, 8, Region::UNIT, 0);
        let out = sg
            .eval(Inputs::required_only(&[&field]), &Params::default(), &ctx)
            .unwrap();

        assert_eq!(out.len(), 1);
        let v = out[0].layer(layers::HEIGHT).unwrap().as_slice()[0];
        assert!(
            (v - 0.42).abs() < 1e-6,
            "the bound input comes straight through"
        );
    }

    #[test]
    fn subgraph_evaluates_through_the_outer_evaluator() {
        // outer: const(0.7) -> subgraph(identity).
        let (inner, _, _) = identity_inner();
        let mut outer = Graph::new();
        let src_gen = outer.add_op(Box::new(ConstGen { value: 0.7 }), Params::new());
        let sg = outer.add_op(Box::new(SubgraphNode::new(inner)), Params::new());
        outer
            .connect(src_gen, 0, sg, 0)
            .expect("wire gen into subgraph");

        let mut cache = EvalCache::new(16);
        let out = outer
            .evaluate(sg, &EvalRequest::new(8, 8, Region::UNIT, 0), &mut cache)
            .unwrap();
        let v = out[0].layer(layers::HEIGHT).unwrap().as_slice()[0];
        assert!((v - 0.7).abs() < 1e-6);
    }

    #[test]
    fn content_hash_tracks_the_inner_graph() {
        let (inner, _, output) = identity_inner();
        let sg = SubgraphNode::new(inner.clone());
        // Same inner content hashes equal.
        assert_eq!(
            SubgraphNode::new(inner.clone()).content_hash(),
            sg.content_hash()
        );
        // An output-affecting inner edit (bypassing a node) changes the hash.
        let mut edited = inner;
        edited.set_bypassed(output, true).expect("bypass");
        assert_ne!(SubgraphNode::new(edited).content_hash(), sg.content_hash());
    }

    #[test]
    fn editing_the_inner_graph_changes_the_outer_cache_key() {
        let build = |bypass: bool| {
            let (inner, _, output) = identity_inner();
            let mut inner = inner;
            if bypass {
                inner.set_bypassed(output, true).expect("bypass");
            }
            let mut outer = Graph::new();
            let src_gen = outer.add_op(Box::new(ConstGen { value: 0.3 }), Params::new());
            let sg = outer.add_op(Box::new(SubgraphNode::new(inner)), Params::new());
            outer.connect(src_gen, 0, sg, 0).expect("wire");
            (outer, sg)
        };
        let (g1, t1) = build(false);
        let (g2, t2) = build(true);
        let req = EvalRequest::new(8, 8, Region::UNIT, 0);
        // The inner edit reaches the outer node's cache key via the content-hash hook.
        assert_ne!(
            g1.output_key(t1, &req).unwrap(),
            g2.output_key(t2, &req).unwrap()
        );
    }

    #[test]
    fn an_unwired_output_marker_fails_rather_than_panicking() {
        let mut inner = Graph::new();
        inner.add_op(Box::new(OutputNode), Params::new()); // output marker, nothing feeding it
        let sg = SubgraphNode::new(inner);
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0);
        let err = sg
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::DisconnectedInput { .. }));
    }

    #[test]
    fn nesting_depth_backstop_reports_instead_of_overflowing() {
        let (inner, _, _) = identity_inner();
        let sg = SubgraphNode::new(inner);
        let field = Field::new(4, 4, Region::UNIT);
        // A context already at the limit: evaluating one level deeper is refused.
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0).with_depth(MAX_NESTING_DEPTH);
        let err = sg
            .eval(Inputs::required_only(&[&field]), &Params::default(), &ctx)
            .unwrap_err();
        assert!(matches!(err, Error::NestingTooDeep { limit } if limit == MAX_NESTING_DEPTH));
    }

    #[test]
    fn subgraph_container_is_registered() {
        assert!(crate::registry::make(SUBGRAPH_TYPE_ID).is_some());
    }
}
