//! Reconciliation spike (issue #5): prove `egui-snarl` can act as a pure view
//! over `ymir-core`'s canonical graph, before the real canvas (step 5) is built.
//!
//! This module is `#[cfg(test)]`: it is the spike's executable proof, not shipped
//! canvas code. Step 5 builds the real, non-test viewer. The value here is the
//! confirmed policy and the headless tests that pin it.
//!
//! What it proves, against the real `egui-snarl` 0.10 API:
//!
//! - The snarl node type `T` is just a `stable_id` handle (`u64`). All node data
//!   (title, pin counts) is pulled from the core graph in the [`SnarlViewer`]
//!   methods, so snarl holds no second copy of node data.
//! - The `connect`/`disconnect` hooks validate an attempt (port arity is already
//!   guaranteed by the pins; the DAG/no-cycle rule is checked against core) and
//!   apply the accepted change to core. Snarl's wires are mutated only after core
//!   accepts, so they reflect exactly the edges core holds. A core input port
//!   holds one connection, so a connect into an occupied input overwrites in core
//!   and the matching old wire is dropped in snarl.
//! - Node lifecycle: adding creates the core node (new `stable_id`) and hands snarl
//!   the handle; removing deletes from core (cascading its edges) and from snarl
//!   (cascading its wires).
//!
//! Crucially, none of `title`/`inputs`/`outputs`/`connect`/`disconnect` take an
//! `egui::Ui`, so the whole policy is exercised here with no display. Only the
//! `show_*` pin renderers need a `Ui`; they are present for the trait and proven by
//! compilation, and step 5 fills them in.

use eframe::egui::{self, Pos2};
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlViewer};
use egui_snarl::{InPin, NodeId as SnarlNodeId, OutPin, Snarl};

use ymir_core::{Graph, NodeId, Params, registry};
use ymir_nodes::tr;

/// A [`SnarlViewer`] whose node data is only a `stable_id`. It borrows the core
/// graph and pulls every display detail from it, so the snarl structure is a pure
/// view. Wire edits are validated and applied to core first; snarl follows.
struct GraphViewer<'a> {
    graph: &'a mut Graph,
}

impl GraphViewer<'_> {
    /// Resolves a snarl handle (a `stable_id`) to the live core node.
    fn core_id(&self, handle: u64) -> Option<NodeId> {
        self.graph.node_id_of(handle)
    }

    /// Resolves a snarl node to the core node it stands for.
    fn core_id_of_snarl(&self, snarl: &Snarl<u64>, node: SnarlNodeId) -> Option<NodeId> {
        snarl
            .get_node(node)
            .and_then(|&handle| self.core_id(handle))
    }
}

impl SnarlViewer<u64> for GraphViewer<'_> {
    fn title(&mut self, node: &u64) -> String {
        match self.core_id(*node).and_then(|id| self.graph.spec(id)) {
            Some(spec) => tr(&format!("node-{}", spec.type_id)).to_string(),
            None => "<missing>".to_string(),
        }
    }

    fn inputs(&mut self, node: &u64) -> usize {
        self.core_id(*node)
            .and_then(|id| self.graph.spec(id))
            .map_or(0, |spec| spec.inputs.len())
    }

    fn outputs(&mut self, node: &u64) -> usize {
        self.core_id(*node)
            .and_then(|id| self.graph.spec(id))
            .map_or(0, |spec| spec.outputs.len())
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<u64>,
    ) -> impl SnarlPin + 'static {
        let _ = (pin, ui, snarl);
        PinInfo::circle()
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<u64>,
    ) -> impl SnarlPin + 'static {
        let _ = (pin, ui, snarl);
        PinInfo::circle()
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<u64>) {
        let Some(source) = self.core_id_of_snarl(snarl, from.id.node) else {
            return;
        };
        let Some(dest) = self.core_id_of_snarl(snarl, to.id.node) else {
            return;
        };
        // Core is the validity authority: reject a wire that would form a loop
        // before it is ever shown.
        if self.graph.would_create_cycle(source, dest) {
            return;
        }
        if self
            .graph
            .connect(source, from.id.output, dest, to.id.input)
            .is_ok()
        {
            // A core input holds one connection, so this overwrote any prior
            // source. Mirror that in the view: drop the old wire into this input,
            // then add the accepted one.
            snarl.drop_inputs(to.id);
            snarl.connect(from.id, to.id);
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<u64>) {
        let Some(dest) = self.core_id_of_snarl(snarl, to.id.node) else {
            return;
        };
        if self.graph.disconnect(dest, to.id.input).is_ok() {
            snarl.disconnect(from.id, to.id);
        }
    }
}

/// Adds a node to core (assigning a new `stable_id`) and hands snarl the handle.
/// Returns the core id, or `None` if `type_id` is unregistered.
fn add_node(graph: &mut Graph, snarl: &mut Snarl<u64>, type_id: &str, pos: Pos2) -> Option<NodeId> {
    let operator = registry::make(type_id)?;
    let id = graph.add_op(operator, Params::default());
    let handle = graph.stable_id(id)?;
    snarl.insert_node(pos, handle);
    Some(id)
}

/// Removes a snarl node from core (cascading its edges) and from snarl (cascading
/// its wires), keeping the two in step.
fn remove_node(graph: &mut Graph, snarl: &mut Snarl<u64>, node: SnarlNodeId) {
    if let Some(&handle) = snarl.get_node(node)
        && let Some(id) = graph.node_id_of(handle)
    {
        graph.remove_node(id);
    }
    snarl.remove_node(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FBM: &str = "generator.fbm";
    const THERMAL: &str = "modifier.thermal_erosion";

    /// Builds a snarl pin pair addressing `from`'s output 0 and `to`'s input 0.
    fn pins(snarl: &Snarl<u64>, from: SnarlNodeId, to: SnarlNodeId) -> (OutPin, InPin) {
        use egui_snarl::{InPinId, OutPinId};
        let out = snarl.out_pin(OutPinId {
            node: from,
            output: 0,
        });
        let inp = snarl.in_pin(InPinId { node: to, input: 0 });
        (out, inp)
    }

    /// Number of snarl wires feeding `node`'s input 0.
    fn wires_into(snarl: &Snarl<u64>, node: SnarlNodeId) -> usize {
        snarl
            .wires()
            .filter(|(_, in_id)| in_id.node == node && in_id.input == 0)
            .count()
    }

    #[test]
    fn node_data_is_pulled_from_core_not_copied() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");

        let head_handle = graph.stable_id(head).expect("handle");
        let modr_handle = graph.stable_id(modr).expect("handle");
        let mut viewer = GraphViewer { graph: &mut graph };

        // Titles resolve through tr from the core type id; pin counts from arity.
        assert_eq!(viewer.title(&head_handle), tr("node-generator.fbm"));
        assert_eq!(viewer.inputs(&head_handle), 0);
        assert_eq!(viewer.outputs(&head_handle), 1);
        assert_eq!(viewer.inputs(&modr_handle), 1);
        assert_eq!(viewer.outputs(&modr_handle), 1);
    }

    #[test]
    fn accepted_connection_lands_in_core_and_in_snarl() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (s_head, s_modr) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, s_head, s_modr);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);

        // Core holds the edge...
        assert!(edge_exists(&graph, head, modr));
        // ...and snarl reflects exactly one matching wire.
        assert_eq!(wires_into(&snarl, s_modr), 1);
    }

    #[test]
    fn cycle_forming_connection_is_rejected_everywhere() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let a = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("a");
        let b = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("b");
        let (sa, sb) = (snarl_id(&snarl, &graph, a), snarl_id(&snarl, &graph, b));

        // a -> b accepted.
        let (out, inp) = pins(&snarl, sa, sb);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);
        assert!(edge_exists(&graph, a, b));

        // b -> a would close a loop: rejected in core and never wired in snarl.
        let (out, inp) = pins(&snarl, sb, sa);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);
        assert!(!edge_exists(&graph, b, a));
        assert_eq!(wires_into(&snarl, sa), 0);
    }

    #[test]
    fn second_connection_to_an_input_overwrites_in_core_and_view() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let g1 = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("g1");
        let g2 = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("g2");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (s1, s2, sm) = (
            snarl_id(&snarl, &graph, g1),
            snarl_id(&snarl, &graph, g2),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, s1, sm);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);
        let (out, inp) = pins(&snarl, s2, sm);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);

        // Core's single input now points at g2; snarl shows exactly one wire.
        assert!(edge_exists(&graph, g2, modr));
        assert!(!edge_exists(&graph, g1, modr));
        assert_eq!(wires_into(&snarl, sm), 1);
    }

    #[test]
    fn disconnect_clears_core_and_view() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (sh, sm) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);
        assert!(edge_exists(&graph, head, modr));

        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer { graph: &mut graph }.disconnect(&out, &inp, &mut snarl);
        assert!(!edge_exists(&graph, head, modr));
        assert_eq!(wires_into(&snarl, sm), 0);
    }

    #[test]
    fn lifecycle_keeps_core_and_snarl_in_step() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<u64>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (sh, sm) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );
        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer { graph: &mut graph }.connect(&out, &inp, &mut snarl);

        assert_eq!(graph.node_count(), 2);
        assert_eq!(snarl.nodes().count(), 2);

        // Removing the generator cascades its edge out of both core and snarl.
        remove_node(&mut graph, &mut snarl, sh);
        assert_eq!(graph.node_count(), 1);
        assert_eq!(snarl.nodes().count(), 1);
        assert!(!edge_exists(&graph, head, modr));
        assert_eq!(wires_into(&snarl, sm), 0);

        // Unknown type ids do not touch either structure.
        assert!(add_node(&mut graph, &mut snarl, "no.such.node", Pos2::ZERO).is_none());
        assert_eq!(graph.node_count(), 1);
        assert_eq!(snarl.nodes().count(), 1);
    }

    // --- small helpers ------------------------------------------------------

    /// The snarl node id whose handle is `core`'s `stable_id`.
    fn snarl_id(snarl: &Snarl<u64>, graph: &Graph, core: NodeId) -> SnarlNodeId {
        let handle = graph.stable_id(core).expect("handle");
        snarl
            .node_ids()
            .find(|(_, h)| **h == handle)
            .map(|(id, _)| id)
            .expect("snarl node")
    }

    /// Whether core holds an edge feeding `source` into `dest`'s input 0.
    fn edge_exists(graph: &Graph, source: NodeId, dest: NodeId) -> bool {
        graph.input_source(dest, 0) == Some((source, 0))
    }
}
