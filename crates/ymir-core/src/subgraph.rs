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
use crate::param::{ParamKind, ParamSpec, ParamValue, Params};
use crate::registry::OperatorEntry;
use crate::spec::{NodeSpec, PortSpec};

/// Type id of a subgraph input marker.
pub const INPUT_TYPE_ID: &str = "subgraph.input";
/// Type id of a subgraph output marker.
pub const OUTPUT_TYPE_ID: &str = "subgraph.output";
/// Type id of the subgraph container.
pub(crate) const SUBGRAPH_TYPE_ID: &str = "subgraph";

/// The default display label for a subgraph boundary marker and its derived container port,
/// 1-based (e.g. `"Input 1"`, `"Output 2"`). Shared by the container's port naming and the
/// editor's marker-node title so the two always read identically. `index` is the marker's
/// 0-based position among markers of its type, ordered by `stable_id`.
#[must_use]
pub fn marker_port_label(type_id: &str, index: usize) -> String {
    let kind = if type_id == OUTPUT_TYPE_ID {
        "Output"
    } else {
        "Input"
    };
    format!("{kind} {}", index + 1)
}

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
    /// each marker's name override, falling back to the shared default label
    /// ([`marker_port_label`], e.g. `"Input 1"`).
    fn boundary_ports(&self, marker_type: &str) -> Vec<PortSpec> {
        self.inner
            .nodes_of_type(marker_type)
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let name = self
                    .inner
                    .name(id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| marker_port_label(marker_type, i));
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
            inputs: self.boundary_ports(INPUT_TYPE_ID),
            outputs: self.boundary_ports(OUTPUT_TYPE_ID),
            // The subgraph's own seed, used as the *absolute* global seed for its inner
            // graph (not an offset to the host's world seed, as a generator's seed is).
            // That self-containment is what lets a shared subgraph reproduce the same
            // terrain in any project; reseeding here is the "vary this instance" switch.
            params: vec![ParamSpec::new(
                "seed",
                ParamKind::Int {
                    min: 0,
                    max: i64::from(i32::MAX),
                },
                ParamValue::Int(0),
            )],
        }
    }

    fn content_hash(&self) -> Option<u64> {
        Some(self.inner_hash)
    }

    fn nested(&self) -> Option<&Graph> {
        Some(&self.inner)
    }

    fn rebuild_nested(&self, inner: Graph) -> Box<dyn Operator> {
        Box::new(SubgraphNode::new(inner))
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
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

        // The inner graph runs under the subgraph's own captured seed (absolute, not the
        // host's world seed), so a shared subgraph reproduces the same terrain in any
        // project; 0 is the default. Reseeding this param is the "vary this instance" switch.
        let inner_seed = params.get_i64("seed", 0) as u64;

        // Run the inner graph one nesting level deeper, threading the world settings and
        // cancellation through. The inner cache is transient (see issue #125): the container
        // itself memoizes in the outer cache, so an unchanged subgraph is never re-run, and
        // within this one call the evaluator's working set pins the active path regardless of
        // cache capacity.
        let request = EvalRequest::new(ctx.width, ctx.height, ctx.region, inner_seed)
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

    /// A test-only generator whose uniform height is derived from the per-node seed, so a
    /// test can observe which seed the inner evaluation ran under. Never registered.
    #[derive(Clone)]
    struct SeedGen;

    impl Operator for SeedGen {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.seedgen",
                category: "test",
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
            let value = (ctx.seed % 1000) as f32 / 1000.0;
            Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region).with_layer(
                    layers::HEIGHT,
                    Arc::new(Layer::filled(ctx.width, ctx.height, value)),
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
        assert_eq!(spec.inputs[1].name, "Input 2"); // 1-based default label
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

    #[test]
    fn replacing_a_container_operator_refreshes_its_ports() {
        // A container with one input marker exposes one input port.
        let (inner, _, _) = identity_inner();
        let mut outer = Graph::new();
        let sg = outer.add_op(Box::new(SubgraphNode::new(inner.clone())), Params::new());
        assert_eq!(outer.spec(sg).unwrap().inputs.len(), 1);

        // Edit the inner graph (add a second input marker) and swap the operator in:
        // the container's derived ports follow.
        let mut edited = inner;
        edited.add_op(Box::new(InputNode), Params::new());
        outer
            .set_operator(sg, Box::new(SubgraphNode::new(edited)))
            .unwrap();
        assert_eq!(
            outer.spec(sg).unwrap().inputs.len(),
            2,
            "ports refresh from the edited inner graph"
        );
    }

    #[test]
    fn nested_and_set_nested_read_and_replace_the_inner_graph() {
        let (inner, _, _) = identity_inner();
        let mut outer = Graph::new();
        let sg = outer.add_op(Box::new(SubgraphNode::new(inner)), Params::new());
        let plain = outer.add_op(Box::new(SeedGen), Params::new());

        // nested() exposes a container's inner graph and is None for an ordinary node.
        assert!(outer.nested(sg).is_some());
        assert!(outer.nested(plain).is_none());

        // set_nested swaps the inner graph and refreshes the container's ports.
        let mut new_inner = Graph::new();
        let i1 = new_inner.add_op(Box::new(InputNode), Params::new());
        new_inner.add_op(Box::new(InputNode), Params::new()); // a second input marker
        let o = new_inner.add_op(Box::new(OutputNode), Params::new());
        new_inner.connect(i1, 0, o, 0).unwrap();
        outer.set_nested(sg, new_inner).unwrap();
        assert_eq!(
            outer.spec(sg).unwrap().inputs.len(),
            2,
            "ports follow the installed inner graph"
        );
        // set_nested on an ordinary node leaves it intact (default rebuild_nested ignores it).
        outer.set_nested(plain, Graph::new()).unwrap();
        assert!(outer.nested(plain).is_none());
    }

    #[test]
    fn a_subgraph_round_trips_through_a_document() {
        let (inner, _, _) = identity_inner();
        let mut outer = Graph::new();
        let sg = outer.add_op(Box::new(SubgraphNode::new(inner)), Params::new());
        let sid = outer.stable_id(sg).unwrap();

        let doc = outer.to_document();
        assert!(
            doc.nodes[0].subgraph.is_some(),
            "the inner graph is captured in the document"
        );

        let rebuilt = Graph::from_document(&doc).expect("rebuild");
        // Document equality proves the whole nested structure round-trips.
        assert_eq!(rebuilt.to_document(), doc);

        // The restored container has its derived port back and still runs as identity.
        let sg2 = rebuilt.node_id_of(sid).unwrap();
        assert_eq!(rebuilt.spec(sg2).unwrap().inputs.len(), 1);
        let field = Field::new(4, 4, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(4, 4, 0.9)));
        let out = rebuilt
            .node(sg2)
            .unwrap()
            .operator
            .eval(
                Inputs::required_only(&[&field]),
                &Params::default(),
                &EvalContext::new(4, 4, Region::UNIT, 0),
            )
            .unwrap();
        assert!((out[0].layer(layers::HEIGHT).unwrap().as_slice()[0] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn subgraph_declares_a_seed_param() {
        let spec = SubgraphNode::empty().spec();
        assert!(
            spec.params.iter().any(|p| p.name == "seed"),
            "the captured seed is a visible param"
        );
    }

    #[test]
    fn the_seed_param_is_absolute_so_a_shared_subgraph_reproduces() {
        // inner: a seed-driven generator -> Output. The subgraph's output depends on the
        // inner global seed, which is the subgraph's own seed param, not the host's.
        let mut inner = Graph::new();
        let g = inner.add_op(Box::new(SeedGen), Params::new());
        let out = inner.add_op(Box::new(OutputNode), Params::new());
        inner.connect(g, 0, out, 0).unwrap();
        let sg = SubgraphNode::new(inner);

        let value_at = |host_seed: u64, params: &Params| {
            let ctx = EvalContext::new(8, 8, Region::UNIT, host_seed);
            sg.eval(Inputs::required_only(&[]), params, &ctx).unwrap()[0]
                .layer(layers::HEIGHT)
                .unwrap()
                .as_slice()[0]
        };

        let default_seed = Params::new(); // seed defaults to 0
        // The host's world seed does not change the subgraph's output: it is self-contained,
        // so sharing it into any project reproduces the same terrain.
        assert!(
            (value_at(7, &default_seed) - value_at(999, &default_seed)).abs() < 1e-9,
            "output is independent of the host world seed"
        );
        // Changing the captured seed does change the output: the reseed/vary switch.
        let reseeded = Params::new().with("seed", ParamValue::Int(5));
        assert!(
            (value_at(7, &default_seed) - value_at(7, &reseeded)).abs() > 1e-9,
            "the captured seed selects the terrain"
        );
    }

    #[test]
    fn the_captured_seed_survives_a_round_trip() {
        let (inner, _, _) = identity_inner();
        let mut outer = Graph::new();
        let sg = outer.add_op(
            Box::new(SubgraphNode::new(inner)),
            Params::new().with("seed", ParamValue::Int(123)),
        );
        let sid = outer.stable_id(sg).unwrap();

        let rebuilt = Graph::from_document(&outer.to_document()).expect("rebuild");
        let sg2 = rebuilt.node_id_of(sid).unwrap();
        // The captured seed travels with the project (and so with a shared subgraph file).
        assert_eq!(rebuilt.params(sg2).unwrap().get_i64("seed", 0), 123);
    }

    #[test]
    fn extract_subgraph_preserves_a_wrapped_subgraph() {
        // src -> A (a subgraph with identity inner) -> sink.
        let (inner, _, _) = identity_inner();
        let mut g = Graph::new();
        let src = g.add_op(Box::new(SeedGen), Params::new());
        let a = g.add_op(Box::new(SubgraphNode::new(inner)), Params::new());
        let sink = g.add_op(Box::new(OutputNode), Params::new());
        g.connect(src, 0, a, 0).unwrap();
        g.connect(a, 0, sink, 0).unwrap();

        // Wrapping A must keep the copied A a container with its own inner graph intact
        // (operators are cloned, not rebuilt from type, so nesting survives).
        let container = g.extract_subgraph(&[a]).unwrap().container;
        let outer_inner = g.nested(container).expect("container inner");
        let wrapped_a = outer_inner
            .to_document()
            .nodes
            .iter()
            .filter_map(|nd| outer_inner.node_id_of(nd.stable_id))
            .find(|&id| outer_inner.nested(id).is_some())
            .expect("the wrapped subgraph survives");
        assert_eq!(
            outer_inner.nested(wrapped_a).unwrap().node_count(),
            2,
            "its identity inner (input + output marker) is intact"
        );
    }

    #[test]
    fn nested_subgraphs_round_trip() {
        // A subgraph whose inner graph contains another subgraph (Input -> sub -> Output).
        let (inner_inner, _, _) = identity_inner();
        let mut mid = Graph::new();
        let mi = mid.add_op(Box::new(InputNode), Params::new());
        let msub = mid.add_op(Box::new(SubgraphNode::new(inner_inner)), Params::new());
        let mo = mid.add_op(Box::new(OutputNode), Params::new());
        mid.connect(mi, 0, msub, 0).unwrap();
        mid.connect(msub, 0, mo, 0).unwrap();

        let mut outer = Graph::new();
        outer.add_op(Box::new(SubgraphNode::new(mid)), Params::new());

        let doc = outer.to_document();
        // Two levels of nesting are present in the document.
        let mid_doc = doc.nodes[0]
            .subgraph
            .as_ref()
            .expect("first level captured");
        assert!(
            mid_doc.nodes.iter().any(|n| n.subgraph.is_some()),
            "the second level is captured too"
        );

        let rebuilt = Graph::from_document(&doc).expect("rebuild");
        assert_eq!(rebuilt.to_document(), doc, "nested structure round-trips");
    }
}
