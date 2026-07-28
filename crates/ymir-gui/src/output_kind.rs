//! Whether a node's output is terrain or a selection, and therefore how to show it (#339).
//!
//! Judging terrain and judging a selection want different views. Terrain wants the 3D relief, lit,
//! with water: the question is what shape it is. A selection wants a flat image at true scale with
//! no water: the question is where it applies and how strongly.
//!
//! Getting that wrong is not cosmetic. The 2D view defaults to auto range, which maps a layer's own
//! minimum and maximum to black and white, so a selection whose values only reach `0.03` renders as
//! a bright, confident shape while contributing almost nothing wherever it is used as a weight. The
//! picture says "strongly selected" and the number says "barely". Auto range is right for terrain,
//! where the shape is the question, and wrong for a selection, where the strength is.
//!
//! # It flows; it is not a property of the node
//!
//! Blur, Blend, Curve and Levels are generic: they are used on terrain and on masks alike, so their
//! type says nothing about which they are carrying. Marking each one by hand would mean walking
//! whole branches ticking boxes.
//!
//! So this walks upstream instead. Each **port declares what it carries** ([`Carries`]): a
//! selector's output is a selection, and so are erosion's `wear` and `flow` and Coastal's `shore`.
//! Everything else inherits from its primary input. `Slope -> Blur -> Blend -> Curve` is a
//! selection the whole way down with nothing configured, and `fBm -> Blur -> Erosion` is terrain.
//!
//! The declaration is per *port*, not per node, because a node can produce both: erosion emits a
//! heightfield beside its byproducts. Which output a wire left from is therefore carried through
//! the walk, so a Levels inserted after `wear` inherits the byproduct and not the heightfield next
//! to it. Position cannot stand in for the declaration either, since Frequency Split's two outputs
//! are both terrain.
//!
//! Blending a mask *into* terrain reads as terrain, because input 0 is the terrain and terrain is
//! what you are making. Blending two masks stays a mask. Following input 0 is what makes both come
//! out right, and it works because Ymir's convention is that input 0 is the main chain and a mask
//! arrives on a later, optional port.
//!
//! Nothing here asks which node it is looking at: it reads each node's own spec and follows the
//! wiring, so a new node declares its ports and needs no entry anywhere.

use ymir_core::{Carries, Graph, INPUT_TYPE_ID, NodeId, OUTPUT_TYPE_ID};

/// What a node's output is, for the purpose of showing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OutputKind {
    /// A heightfield. Shown as 3D relief with water.
    #[default]
    Terrain,
    /// A `[0, 1]` selection. Shown flat, at true scale, without water.
    Selection,
}

/// A ceiling on the upstream walk.
///
/// The graph is validated as a DAG before evaluation, so a cycle should not reach here. This is
/// insurance against a malformed graph turning a display decision into a hang, which would be a
/// bad trade for something that only decides which view opens.
const MAX_DEPTH: usize = 64;

/// What `node` produces on `port`, derived by walking upstream.
pub(crate) fn of(graph: &Graph, node: NodeId, port: usize) -> OutputKind {
    walk(graph, node, port, MAX_DEPTH, &[])
}

/// `bound` is what the enclosing container's input ports carry, in port order, and is empty at the
/// top level. It exists so an `Input` marker inside a subgraph can answer with whatever is wired to
/// the container outside, rather than reporting terrain because a marker has nothing upstream of it
/// (#343). Without it a subgraph that merely passes a mask through would come back terrain.
fn walk(
    graph: &Graph,
    node: NodeId,
    port: usize,
    budget: usize,
    bound: &[OutputKind],
) -> OutputKind {
    let Some(spec) = graph.spec(node) else {
        return OutputKind::Terrain;
    };
    // An Input marker is the subgraph boundary seen from inside: it produces whatever the container
    // was handed on the matching port. Answered before the port declaration below, because a marker
    // declares nothing and would otherwise fall through to "no inputs, so terrain".
    if spec.type_id == INPUT_TYPE_ID {
        return graph
            .nodes_of_type(INPUT_TYPE_ID)
            .iter()
            .position(|&id| id == node)
            .and_then(|i| bound.get(i).copied())
            .unwrap_or(OutputKind::Terrain);
    }
    // The port's own declaration wins. A node can produce both at once: erosion emits a
    // heightfield beside `wear` and `flow`, Coastal one beside `shore`, `beach` and `bluff`. Which
    // output a wire came from is the whole question, and position cannot answer it either, since
    // Frequency Split's two outputs are both terrain.
    if spec
        .outputs
        .get(port)
        .is_some_and(|out| out.carries == Carries::Selection)
    {
        return OutputKind::Selection;
    }
    // No inputs means nothing to inherit from: a generator, or a subgraph boundary marker standing
    // in for one. Terrain is the right answer for both, and the honest default for anything else
    // that reaches here.
    if spec.inputs.is_empty() || budget == 0 {
        return OutputKind::Terrain;
    }
    // A container declares nothing on its ports: they are built from the markers inside, which
    // carry no declaration either. Inheriting from input 0 would therefore call every subgraph
    // terrain, including one built precisely to turn terrain into a mask. So descend: resolve the
    // output port to the marker behind it and continue from whatever feeds it, carrying down what
    // this container's own inputs carry so the markers inside can resolve back outward.
    if let Some(inner) = graph.nested(node) {
        let bound_inner: Vec<OutputKind> = (0..spec.inputs.len())
            .map(|i| match graph.input_source(node, i) {
                Some((src, src_port)) => walk(graph, src, src_port, budget - 1, bound),
                None => OutputKind::Terrain,
            })
            .collect();
        let marker = inner.nodes_of_type(OUTPUT_TYPE_ID).get(port).copied();
        if let Some(marker) = marker {
            return match inner.input_source(marker, 0) {
                Some((src, src_port)) => walk(inner, src, src_port, budget - 1, &bound_inner),
                // A marker with nothing wired to it produces nothing useful yet.
                None => OutputKind::Terrain,
            };
        }
    }
    // Input 0 is the main chain by convention; a mask arrives on a later, optional port. Following
    // it is what makes "a mask blended into terrain" read as terrain while "two masks blended"
    // stays a mask. The source *port* is carried through, so a Levels inserted after an erosion
    // node's `wear` output inherits the byproduct rather than the heightfield beside it.
    match graph.input_source(node, 0) {
        Some((source, source_port)) => walk(graph, source, source_port, budget - 1, bound),
        // An unwired primary input produces nothing useful yet; terrain is the calmer default,
        // since it is the view the editor already opens in.
        None => OutputKind::Terrain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;

    /// Builds a graph from `(type_id, wired_to)` pairs, returning the node ids in order. Each entry
    /// wires its input 0 to an earlier node by index.
    fn graph_of(nodes: &[(&str, Option<usize>)]) -> (Graph, Vec<NodeId>) {
        let mut graph = Graph::new();
        let mut ids = Vec::new();
        for (type_id, from) in nodes {
            let op = registry::make(type_id).unwrap_or_else(|| panic!("{type_id} is registered"));
            let id = graph.add_op(op, ymir_core::Params::default());
            if let Some(from) = from {
                graph.connect(ids[*from], 0, id, 0).expect("connect");
            }
            ids.push(id);
        }
        (graph, ids)
    }

    #[test]
    fn a_generator_is_terrain() {
        let (graph, ids) = graph_of(&[("generator.fbm", None)]);
        assert_eq!(of(&graph, ids[0], 0), OutputKind::Terrain);
    }

    #[test]
    fn a_selector_is_a_selection_whatever_it_reads() {
        // It answers a question about terrain, and the answer is a mask, so it does not inherit.
        let (graph, ids) = graph_of(&[("generator.fbm", None), ("modifier.slope", Some(0))]);
        assert_eq!(of(&graph, ids[1], 0), OutputKind::Selection);
    }

    #[test]
    fn a_mask_branch_stays_a_mask_through_generic_nodes() {
        // The case that makes hand-marking untenable: Blur and Curve are used on both, so their
        // type says nothing and the branch has to carry the answer.
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.slope", Some(0)),
            ("modifier.blur", Some(1)),
            ("modifier.curve", Some(2)),
        ]);
        assert_eq!(of(&graph, ids[3], 0), OutputKind::Selection);
    }

    #[test]
    fn a_terrain_branch_stays_terrain_through_the_same_nodes() {
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.blur", Some(0)),
            ("modifier.thermal_erosion", Some(1)),
        ]);
        assert_eq!(of(&graph, ids[2], 0), OutputKind::Terrain);
    }

    #[test]
    fn a_mask_blended_into_terrain_reads_as_terrain() {
        // Input 0 is the terrain, and terrain is what you are making. This is the case that
        // decides why the walk follows the primary input rather than any input.
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.slope", Some(0)),
            ("modifier.blend", Some(0)),
        ]);
        let (mut graph, ids) = (graph, ids);
        graph
            .connect(ids[1], 0, ids[2], 1)
            .expect("mask into blend");
        assert_eq!(of(&graph, ids[2], 0), OutputKind::Terrain);
    }

    #[test]
    fn two_masks_blended_stay_a_mask() {
        let (mut graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.slope", Some(0)),
            ("modifier.height", Some(0)),
            ("modifier.blend", Some(1)),
        ]);
        graph.connect(ids[2], 0, ids[3], 1).expect("second mask");
        assert_eq!(of(&graph, ids[3], 0), OutputKind::Selection);
    }

    #[test]
    fn a_material_is_a_selection() {
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.slope", Some(0)),
            ("modifier.material", Some(1)),
        ]);
        assert_eq!(of(&graph, ids[2], 0), OutputKind::Selection);
    }

    #[test]
    fn an_unwired_node_is_terrain() {
        // Nothing to inherit from yet. Terrain is the calmer default: it is the view the editor
        // already opens in, so an unfinished graph does not flip the viewport about.
        let (graph, ids) = graph_of(&[("modifier.blur", None)]);
        assert_eq!(of(&graph, ids[0], 0), OutputKind::Terrain);
    }

    #[test]
    fn an_erosion_byproduct_is_a_selection_but_its_heightfield_is_not() {
        // The same node answers differently per port, which is why the declaration is on the port
        // and not the node.
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.thermal_erosion", Some(0)),
        ]);
        assert_eq!(of(&graph, ids[1], 0), OutputKind::Terrain, "heightfield");
        assert_eq!(of(&graph, ids[1], 1), OutputKind::Selection, "wear");
        assert_eq!(of(&graph, ids[1], 2), OutputKind::Selection, "debris");
    }

    #[test]
    fn a_node_inserted_after_a_byproduct_inherits_the_byproduct() {
        // Reported from use: a Levels dropped after an erosion node's `wear` output opened in 3D,
        // because the walk followed the source node and threw away which output it came from.
        let mut graph = Graph::new();
        let fbm = graph.add_op(
            registry::make("generator.fbm").expect("fbm"),
            ymir_core::Params::default(),
        );
        let erosion = graph.add_op(
            registry::make("modifier.thermal_erosion").expect("thermal"),
            ymir_core::Params::default(),
        );
        let levels = graph.add_op(
            registry::make("modifier.levels").expect("levels"),
            ymir_core::Params::default(),
        );
        graph.connect(fbm, 0, erosion, 0).expect("fbm -> erosion");
        // Output 1 is `wear`, not the heightfield.
        graph
            .connect(erosion, 1, levels, 0)
            .expect("wear -> levels");
        assert_eq!(of(&graph, levels, 0), OutputKind::Selection);
    }

    #[test]
    fn both_halves_of_a_frequency_split_are_terrain() {
        // The counter-example to "a later port means a selection": these are two bands of the same
        // heightfield, so position says nothing and only the declaration can.
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.frequency_split", Some(0)),
        ]);
        assert_eq!(of(&graph, ids[1], 0), OutputKind::Terrain, "low");
        assert_eq!(of(&graph, ids[1], 1), OutputKind::Terrain, "high");
    }

    /// Builds a container whose inner graph is `Input -> <chain> -> Output`, wired under an fBm in
    /// a fresh outer graph. Returns the outer graph and the container.
    fn subgraph_of(chain: &[&str]) -> (Graph, NodeId) {
        let mut inner = Graph::new();
        let input = inner.add_op(
            registry::make(ymir_core::INPUT_TYPE_ID).expect("input marker"),
            ymir_core::Params::default(),
        );
        let mut last = input;
        for type_id in chain {
            let id = inner.add_op(
                registry::make(type_id).unwrap_or_else(|| panic!("{type_id} is registered")),
                ymir_core::Params::default(),
            );
            inner.connect(last, 0, id, 0).expect("chain");
            last = id;
        }
        let output = inner.add_op(
            registry::make(ymir_core::OUTPUT_TYPE_ID).expect("output marker"),
            ymir_core::Params::default(),
        );
        inner.connect(last, 0, output, 0).expect("to output");

        let mut outer = Graph::new();
        let fbm = outer.add_op(
            registry::make("generator.fbm").expect("fbm"),
            ymir_core::Params::default(),
        );
        let container = outer.add_op(
            registry::make(ymir_core::SUBGRAPH_TYPE_ID).expect("container"),
            ymir_core::Params::default(),
        );
        outer.set_nested(container, inner).expect("nest");
        outer
            .connect(fbm, 0, container, 0)
            .expect("fbm -> container");
        (outer, container)
    }

    #[test]
    fn a_subgraph_that_makes_a_mask_is_a_selection() {
        // The reported case (#343): terrain in, mask out. Inheriting from input 0 called this
        // terrain, so a "steep slopes" subgraph opened in 3D with water over it.
        let (graph, container) = subgraph_of(&["modifier.slope"]);
        assert_eq!(of(&graph, container, 0), OutputKind::Selection);
    }

    #[test]
    fn a_subgraph_that_only_shapes_terrain_stays_terrain() {
        let (graph, container) = subgraph_of(&["modifier.blur"]);
        assert_eq!(of(&graph, container, 0), OutputKind::Terrain);
    }

    #[test]
    fn a_subgraph_passing_a_mask_through_stays_a_selection() {
        // Nothing inside says "selection": the chain is a plain Blur, and the answer has to come
        // from what is wired to the container from outside. This is what the boundary binding is
        // for; without it the Input marker has nothing upstream and reports terrain.
        let (mut graph, container) = subgraph_of(&["modifier.blur"]);
        let fbm = graph
            .node_ids()
            .into_iter()
            .find(|&id| graph.spec(id).is_some_and(|s| s.type_id == "generator.fbm"))
            .expect("fbm");
        let slope = graph.add_op(
            registry::make("modifier.slope").expect("slope"),
            ymir_core::Params::default(),
        );
        graph.connect(fbm, 0, slope, 0).expect("fbm -> slope");
        graph
            .connect(slope, 0, container, 0)
            .expect("slope -> container");
        assert_eq!(of(&graph, container, 0), OutputKind::Selection);
    }

    #[test]
    fn the_descent_resolves_through_two_levels() {
        // A container inside a container: the walk has to keep descending, and the boundary
        // binding has to be rebuilt at each level rather than only the outermost.
        let (inner_outer, inner_container) = subgraph_of(&["modifier.slope"]);
        let mut nested = Graph::new();
        let input = nested.add_op(
            registry::make(ymir_core::INPUT_TYPE_ID).expect("input"),
            ymir_core::Params::default(),
        );
        // Rebuild the mask-making container inside a second one.
        let deep = nested.add_op(
            registry::make(ymir_core::SUBGRAPH_TYPE_ID).expect("container"),
            ymir_core::Params::default(),
        );
        let inner_graph = inner_outer.nested(inner_container).expect("inner").clone();
        nested.set_nested(deep, inner_graph).expect("nest");
        nested.connect(input, 0, deep, 0).expect("in -> deep");
        let output = nested.add_op(
            registry::make(ymir_core::OUTPUT_TYPE_ID).expect("output"),
            ymir_core::Params::default(),
        );
        nested.connect(deep, 0, output, 0).expect("deep -> out");

        let mut outer = Graph::new();
        let fbm = outer.add_op(
            registry::make("generator.fbm").expect("fbm"),
            ymir_core::Params::default(),
        );
        let container = outer.add_op(
            registry::make(ymir_core::SUBGRAPH_TYPE_ID).expect("container"),
            ymir_core::Params::default(),
        );
        outer.set_nested(container, nested).expect("nest");
        outer
            .connect(fbm, 0, container, 0)
            .expect("fbm -> container");
        assert_eq!(of(&outer, container, 0), OutputKind::Selection);
    }

    #[test]
    fn an_unwired_output_marker_is_terrain() {
        // A half-built subgraph must not panic or index past its markers.
        let mut inner = Graph::new();
        inner.add_op(
            registry::make(ymir_core::OUTPUT_TYPE_ID).expect("output"),
            ymir_core::Params::default(),
        );
        let mut outer = Graph::new();
        let container = outer.add_op(
            registry::make(ymir_core::SUBGRAPH_TYPE_ID).expect("container"),
            ymir_core::Params::default(),
        );
        outer.set_nested(container, inner).expect("nest");
        assert_eq!(of(&outer, container, 0), OutputKind::Terrain);
    }

    #[test]
    fn a_long_chain_still_resolves() {
        // The walk is bounded, so a chain longer than a hand-written test should still reach its
        // source rather than giving up at the ceiling.
        let mut nodes: Vec<(&str, Option<usize>)> =
            vec![("generator.fbm", None), ("modifier.slope", Some(0))];
        for i in 2..40 {
            nodes.push(("modifier.blur", Some(i - 1)));
        }
        let (graph, ids) = graph_of(&nodes);
        assert_eq!(
            of(&graph, *ids.last().expect("nodes"), 0),
            OutputKind::Selection
        );
    }
}
