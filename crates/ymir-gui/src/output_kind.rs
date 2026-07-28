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
//! So this walks upstream instead. A **selector** or a **Material** originates a selection whatever
//! went into it; a **generator** originates terrain; everything else inherits from its primary
//! input. `Slope -> Blur -> Blend -> Curve` is a selection the whole way down with nothing
//! configured, and `fBm -> Blur -> Erosion` is terrain.
//!
//! Blending a mask *into* terrain reads as terrain, because input 0 is the terrain and terrain is
//! what you are making. Blending two masks stays a mask. Following input 0 is what makes both come
//! out right, and it works because Ymir's convention is that input 0 is the main chain and a mask
//! arrives on a later, optional port.
//!
//! Nothing here asks which node it is looking at: it reads each node's category and arity from its
//! own spec and follows the wiring, so a new node needs no entry anywhere.

use ymir_core::{Graph, NodeId};

/// What a node's output is, for the purpose of showing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OutputKind {
    /// A heightfield. Shown as 3D relief with water.
    #[default]
    Terrain,
    /// A `[0, 1]` selection. Shown flat, at true scale, without water.
    Selection,
}

/// Categories whose nodes originate a selection regardless of what they read.
///
/// A selector answers a question *about* terrain (how steep, how high, which way facing) and its
/// answer is a mask. A material names a selection. Neither passes terrain through, so neither
/// inherits.
const SELECTION_SOURCES: [&str; 2] = ["selector", "material"];

/// A ceiling on the upstream walk.
///
/// The graph is validated as a DAG before evaluation, so a cycle should not reach here. This is
/// insurance against a malformed graph turning a display decision into a hang, which would be a
/// bad trade for something that only decides which view opens.
const MAX_DEPTH: usize = 64;

/// What `node` produces, derived by walking upstream.
pub(crate) fn of(graph: &Graph, node: NodeId) -> OutputKind {
    walk(graph, node, MAX_DEPTH)
}

fn walk(graph: &Graph, node: NodeId, budget: usize) -> OutputKind {
    let Some(spec) = graph.spec(node) else {
        return OutputKind::Terrain;
    };
    if SELECTION_SOURCES.contains(&spec.category) {
        return OutputKind::Selection;
    }
    // No inputs means nothing to inherit from: a generator, or a subgraph boundary marker standing
    // in for one. Terrain is the right answer for both, and the honest default for anything else
    // that reaches here.
    if spec.inputs.is_empty() || budget == 0 {
        return OutputKind::Terrain;
    }
    // Input 0 is the main chain by convention; a mask arrives on a later, optional port. Following
    // it is what makes "a mask blended into terrain" read as terrain while "two masks blended"
    // stays a mask.
    match graph.input_source(node, 0) {
        Some((source, _)) => walk(graph, source, budget - 1),
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
        assert_eq!(of(&graph, ids[0]), OutputKind::Terrain);
    }

    #[test]
    fn a_selector_is_a_selection_whatever_it_reads() {
        // It answers a question about terrain, and the answer is a mask, so it does not inherit.
        let (graph, ids) = graph_of(&[("generator.fbm", None), ("modifier.slope", Some(0))]);
        assert_eq!(of(&graph, ids[1]), OutputKind::Selection);
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
        assert_eq!(of(&graph, ids[3]), OutputKind::Selection);
    }

    #[test]
    fn a_terrain_branch_stays_terrain_through_the_same_nodes() {
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.blur", Some(0)),
            ("modifier.thermal_erosion", Some(1)),
        ]);
        assert_eq!(of(&graph, ids[2]), OutputKind::Terrain);
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
        assert_eq!(of(&graph, ids[2]), OutputKind::Terrain);
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
        assert_eq!(of(&graph, ids[3]), OutputKind::Selection);
    }

    #[test]
    fn a_material_is_a_selection() {
        let (graph, ids) = graph_of(&[
            ("generator.fbm", None),
            ("modifier.slope", Some(0)),
            ("modifier.material", Some(1)),
        ]);
        assert_eq!(of(&graph, ids[2]), OutputKind::Selection);
    }

    #[test]
    fn an_unwired_node_is_terrain() {
        // Nothing to inherit from yet. Terrain is the calmer default: it is the view the editor
        // already opens in, so an unfinished graph does not flip the viewport about.
        let (graph, ids) = graph_of(&[("modifier.blur", None)]);
        assert_eq!(of(&graph, ids[0]), OutputKind::Terrain);
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
            of(&graph, *ids.last().expect("nodes")),
            OutputKind::Selection
        );
    }
}
