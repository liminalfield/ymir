//! The built-in starter graph the app opens with on a fresh session.
//!
//! Rather than a blank canvas, a new session comes up with a small, complete
//! pipeline already wired (an fBm generator feeding thermal erosion feeding a PNG
//! export endpoint), so there is something to preview, build, and edit from the
//! first frame. This is only the fallback: once the user saves a default of their
//! own (issue #76, step 2) that file opens in preference to this graph.
//!
//! The chain is built in both representations the app keeps in step: the core
//! [`Graph`] (the engine truth) and the `egui-snarl` view (node positions and
//! wires), exactly as the interactive node-creation and wiring paths do.

use eframe::egui;
use egui_snarl::{InPinId, NodeId as SnarlNodeId, OutPinId, Snarl};
use ymir_core::{Graph, NodeId};

use crate::canvas::{self, Handle};

/// Horizontal spacing between successive nodes in the starter layout, so the
/// chain reads left to right as a pipeline rather than stacking on one point. The
/// canvas frames to the whole graph on first render, so this is about relative
/// layout, not absolute placement.
const STARTER_NODE_GAP: f32 = 220.0;

/// The default starter graph: an fBm generator → thermal erosion → PNG export
/// chain, laid out left to right and pre-wired in core and on the canvas.
///
/// If a node type is unregistered (the link-time strip `CLAUDE.md` warns about)
/// or a connection is rejected, the chain cannot be built and the app opens to a
/// blank canvas instead of a partly wired graph. In a correctly linked build every
/// type is present, so the chain is always complete.
pub(crate) fn starter_graph() -> (Graph, Snarl<Handle>) {
    let mut graph = Graph::new();
    let mut snarl = Snarl::new();
    match build_chain(&mut graph, &mut snarl) {
        Some(()) => (graph, snarl),
        // Discard whatever partial chain was built; a fresh, empty pair is a clean
        // blank canvas rather than a half-formed graph.
        None => (Graph::new(), Snarl::new()),
    }
}

/// Builds the starter chain into `graph`/`snarl`, returning `None` if any node
/// type is unregistered or a connection is rejected. Split out so the happy path
/// reads as a straight line and the caller can discard a partial build.
fn build_chain(graph: &mut Graph, snarl: &mut Snarl<Handle>) -> Option<()> {
    let generator = add(graph, snarl, "generator.fbm", egui::pos2(0.0, 0.0))?;
    let erosion = add(
        graph,
        snarl,
        "modifier.thermal_erosion",
        egui::pos2(STARTER_NODE_GAP, 0.0),
    )?;
    let export = add(
        graph,
        snarl,
        "endpoint.export",
        egui::pos2(STARTER_NODE_GAP * 2.0, 0.0),
    )?;
    wire(graph, snarl, generator, erosion)?;
    wire(graph, snarl, erosion, export)?;
    Some(())
}

/// A node placed in the starter graph: its core id and its canvas (snarl) id, the
/// two handles wiring needs.
#[derive(Clone, Copy)]
struct Placed {
    core: NodeId,
    snarl: SnarlNodeId,
}

/// Adds a node to core and the canvas at `pos`, returning both ids. `None` if
/// `type_id` is unregistered (then neither structure is touched, per
/// [`canvas::add_node`]).
fn add(
    graph: &mut Graph,
    snarl: &mut Snarl<Handle>,
    type_id: &str,
    pos: egui::Pos2,
) -> Option<Placed> {
    let core = canvas::add_node(graph, snarl, type_id, pos)?;
    let handle = graph.stable_id(core)?;
    let snarl_id = snarl
        .node_ids()
        .find(|&(_, &h)| h == handle)
        .map(|(id, _)| id)?;
    Some(Placed {
        core,
        snarl: snarl_id,
    })
}

/// Wires `from`'s output 0 to `to`'s input 0 in both core (the validity authority)
/// and the canvas view, mirroring the interactive `connect` path. `None` if core
/// rejects the edge, so the caller falls back to a blank canvas.
fn wire(graph: &mut Graph, snarl: &mut Snarl<Handle>, from: Placed, to: Placed) -> Option<()> {
    if graph.connect(from.core, 0, to.core, 0).is_err() {
        return None;
    }
    snarl.connect(
        OutPinId {
            node: from.snarl,
            output: 0,
        },
        InPinId {
            node: to.snarl,
            input: 0,
        },
    );
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `type_id`s of every node in `graph`, sorted, resolved through the canvas
    /// handles so the test reads the same node set the user would see.
    fn type_ids(graph: &Graph, snarl: &Snarl<Handle>) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = snarl
            .node_ids()
            .filter_map(|(_, &h)| graph.node_id_of(h))
            .filter_map(|id| graph.spec(id))
            .map(|spec| spec.type_id)
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn starter_is_a_wired_generator_erosion_export_chain() {
        let (graph, snarl) = starter_graph();

        // Three nodes, present in both core and the canvas view.
        assert_eq!(graph.node_count(), 3);
        assert_eq!(snarl.node_ids().count(), 3);
        assert_eq!(
            type_ids(&graph, &snarl),
            [
                "endpoint.export",
                "generator.fbm",
                "modifier.thermal_erosion"
            ]
        );

        // Two wires on the canvas: generator → erosion → export.
        assert_eq!(snarl.wires().count(), 2);

        // The same two edges exist in core (the engine truth): the document lists a
        // connection on erosion and on export, and none on the head generator.
        let doc = graph.to_document();
        let connections: usize = doc.nodes.iter().map(|n| n.connections.len()).sum();
        assert_eq!(connections, 2);
    }

    #[test]
    fn starter_nodes_are_laid_out_left_to_right() {
        let (_graph, snarl) = starter_graph();
        let xs: Vec<f32> = {
            let mut v: Vec<f32> = snarl
                .node_ids()
                .filter_map(|(id, _)| snarl.get_node_info(id).map(|info| info.pos.x))
                .collect();
            v.sort_by(f32::total_cmp);
            v
        };
        // Three distinct columns, one gap apart, so the chain does not stack.
        assert_eq!(xs.len(), 3);
        assert!((xs[1] - xs[0] - STARTER_NODE_GAP).abs() < 1e-3);
        assert!((xs[2] - xs[1] - STARTER_NODE_GAP).abs() < 1e-3);
    }
}
