//! The node-editor canvas: `egui-snarl` rendered as a pure view over the core
//! graph (GUI step 5, issue #6), following the reconciliation policy confirmed by
//! the spike (issue #5).
//!
//! The snarl node type is a [`Handle`] (a core `stable_id`), so node data lives
//! only in the canonical [`Graph`]. The [`SnarlViewer`] pulls a node's title and
//! pins from the graph, and the `connect`/`disconnect` hooks validate an attempt
//! against core (the DAG rule; arity is already guaranteed by the pins) and apply
//! accepted edges to core first, mutating snarl's wires only to mirror what core
//! holds. Node add and delete sync to core the same way.

use eframe::egui::{self, Pos2};
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlViewer};
use egui_snarl::{InPin, NodeId as SnarlNodeId, OutPin, Snarl};

use ymir_core::{Graph, NodeId, Params, registry};
use ymir_nodes::tr;

/// A node's handle in the canvas: its persistent core `stable_id`. Storing the
/// stable id (not the runtime [`NodeId`]) keeps canvas view-state valid across a
/// reload, and keeps the snarl structure a pure view with no copy of node data.
pub(crate) type Handle = u64;

/// A [`SnarlViewer`] that borrows the core graph and renders it. Every display
/// detail is pulled from the graph, and every edit is validated and applied to the
/// graph before snarl is touched, so core stays the single source of truth.
pub(crate) struct GraphViewer<'a> {
    pub(crate) graph: &'a mut Graph,
    /// The currently selected node, for the header highlight. Input, read-only.
    pub(crate) selected: Option<Handle>,
    /// Each node's final rect with its handle, collected during rendering. These
    /// are in the canvas's local (graph) space, not screen space. The canvas
    /// resolves a plain click to a node from these after the frame, rather than
    /// registering a competing interaction — so the collapse chevron, the pins, and
    /// node dragging all keep their own input. Output.
    pub(crate) node_rects: Vec<(Handle, egui::Rect)>,
    /// The canvas pan/zoom transform (local graph space to screen). snarl reports
    /// it each frame; the canvas uses its inverse to map a screen click back into
    /// the local space the node rects live in. Output.
    pub(crate) to_global: egui::emath::TSTransform,
    /// The previewed node's handle and its preview-status colour, drawn as a small
    /// dot at the left of that node's header. Only the previewed (selected) node has
    /// a status, since the preview evaluates a single target. Input, read-only.
    pub(crate) status: Option<(Handle, egui::Color32)>,
}

impl<'a> GraphViewer<'a> {
    /// A viewer for graph-structure tests that do not exercise selection.
    #[cfg(test)]
    fn for_test(graph: &'a mut Graph) -> Self {
        Self {
            graph,
            selected: None,
            node_rects: Vec::new(),
            to_global: egui::emath::TSTransform::IDENTITY,
            status: None,
        }
    }
}

impl GraphViewer<'_> {
    /// Resolves a canvas handle (a `stable_id`) to the live core node.
    fn core_id(&self, handle: Handle) -> Option<NodeId> {
        self.graph.node_id_of(handle)
    }

    /// Resolves a snarl node to the core node it stands for.
    fn core_id_of_snarl(&self, snarl: &Snarl<Handle>, node: SnarlNodeId) -> Option<NodeId> {
        snarl
            .get_node(node)
            .and_then(|&handle| self.core_id(handle))
    }

    /// The display label for an input or output port: the port's schema name. Port
    /// names are short ids (`"in"`, `"out"`); a localized label layer can wrap this
    /// later without changing the canvas.
    fn port_label(
        &self,
        snarl: &Snarl<Handle>,
        node: SnarlNodeId,
        input: bool,
        index: usize,
    ) -> Option<String> {
        let id = self.core_id_of_snarl(snarl, node)?;
        let spec = self.graph.spec(id)?;
        let ports = if input { &spec.inputs } else { &spec.outputs };
        ports.get(index).map(|p| p.name.clone())
    }
}

impl SnarlViewer<Handle> for GraphViewer<'_> {
    fn title(&mut self, node: &Handle) -> String {
        match self.core_id(*node).and_then(|id| self.graph.spec(id)) {
            Some(spec) => tr(&format!("node-{}", spec.type_id)).to_string(),
            None => "<missing>".to_string(),
        }
    }

    fn show_header(
        &mut self,
        node: SnarlNodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<Handle>,
    ) {
        let handle = snarl.get_node(node).copied();
        let title = handle
            .and_then(|h| self.core_id(h))
            .and_then(|id| self.graph.spec(id))
            .map_or_else(
                || "<missing>".to_string(),
                |spec| tr(&format!("node-{}", spec.type_id)).to_string(),
            );
        // The title is purely visual; selection is handled over the whole node in
        // `final_node_rect`. Selection shows as bold accent text. `selectable(false)`
        // keeps the title from being text-selectable, so it shows the normal cursor
        // (not a text I-beam) and reads as a node title, not editable text.
        let is_selected = handle.is_some() && handle == self.selected;
        let text = if is_selected {
            egui::RichText::new(title)
                .strong()
                .color(ui.visuals().selection.stroke.color)
        } else {
            egui::RichText::new(title)
        };
        ui.horizontal(|ui| {
            // Preview-status dot, left of the title, only for the previewed node.
            if let Some((status_handle, color)) = self.status
                && handle == Some(status_handle)
            {
                let diameter = ui.text_style_height(&egui::TextStyle::Body) * 0.55;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), diameter * 0.5, color);
            }
            ui.add(egui::Label::new(text).selectable(false));
        });
    }

    fn final_node_rect(
        &mut self,
        node: SnarlNodeId,
        rect: egui::Rect,
        _ui: &mut egui::Ui,
        snarl: &mut Snarl<Handle>,
    ) {
        // Record the node's rect for post-frame click resolution. Deliberately no
        // interaction here: registering one would sit on top of snarl's own widgets
        // (the collapse chevron, the pins) and swallow their clicks.
        if let Some(handle) = snarl.get_node(node).copied() {
            self.node_rects.push((handle, rect));
        }
    }

    fn current_transform(
        &mut self,
        to_global: &mut egui::emath::TSTransform,
        _snarl: &mut Snarl<Handle>,
    ) {
        // Capture (do not change) the pan/zoom transform, so a screen click can be
        // mapped into the local space the node rects are recorded in.
        self.to_global = *to_global;
    }

    fn inputs(&mut self, node: &Handle) -> usize {
        self.core_id(*node)
            .and_then(|id| self.graph.spec(id))
            .map_or(0, |spec| spec.inputs.len())
    }

    fn outputs(&mut self, node: &Handle) -> usize {
        self.core_id(*node)
            .and_then(|id| self.graph.spec(id))
            .map_or(0, |spec| spec.outputs.len())
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<Handle>,
    ) -> impl SnarlPin + 'static {
        if let Some(label) = self.port_label(snarl, pin.id.node, true, pin.id.input) {
            ui.label(label);
        }
        PinInfo::circle()
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<Handle>,
    ) -> impl SnarlPin + 'static {
        if let Some(label) = self.port_label(snarl, pin.id.node, false, pin.id.output) {
            ui.label(label);
        }
        PinInfo::circle()
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<Handle>) {
        let Some(source) = self.core_id_of_snarl(snarl, from.id.node) else {
            return;
        };
        let Some(dest) = self.core_id_of_snarl(snarl, to.id.node) else {
            return;
        };
        // Core is the validity authority: refuse a wire that would form a loop
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
            // source. Mirror that: drop the old wire into this input, then add the
            // accepted one, so the view shows exactly the edges core holds.
            snarl.drop_inputs(to.id);
            snarl.connect(from.id, to.id);
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<Handle>) {
        let Some(dest) = self.core_id_of_snarl(snarl, to.id.node) else {
            return;
        };
        if self.graph.disconnect(dest, to.id.input).is_ok() {
            snarl.disconnect(from.id, to.id);
        }
    }

    fn has_node_menu(&mut self, _node: &Handle) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: SnarlNodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<Handle>,
    ) {
        if ui.button("Delete node").clicked() {
            remove_snarl_node(self.graph, snarl, node);
            ui.close();
        }
    }
}

/// Adds a node to core (assigning a new `stable_id`) and hands the canvas its
/// handle at `pos`. Returns the core id, or `None` if `type_id` is unregistered;
/// in that case neither structure is touched.
pub(crate) fn add_node(
    graph: &mut Graph,
    snarl: &mut Snarl<Handle>,
    type_id: &str,
    pos: Pos2,
) -> Option<NodeId> {
    let operator = registry::make(type_id)?;
    let id = graph.add_op(operator, Params::default());
    let handle = graph.stable_id(id)?;
    snarl.insert_node(pos, handle);
    Some(id)
}

/// Removes a canvas node from core (cascading its edges) and from snarl (cascading
/// its wires), keeping the two in step.
pub(crate) fn remove_snarl_node(graph: &mut Graph, snarl: &mut Snarl<Handle>, node: SnarlNodeId) {
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
    fn pins(snarl: &Snarl<Handle>, from: SnarlNodeId, to: SnarlNodeId) -> (OutPin, InPin) {
        use egui_snarl::{InPinId, OutPinId};
        let out = snarl.out_pin(OutPinId {
            node: from,
            output: 0,
        });
        let inp = snarl.in_pin(InPinId { node: to, input: 0 });
        (out, inp)
    }

    /// Number of snarl wires feeding `node`'s input 0.
    fn wires_into(snarl: &Snarl<Handle>, node: SnarlNodeId) -> usize {
        snarl
            .wires()
            .filter(|(_, in_id)| in_id.node == node && in_id.input == 0)
            .count()
    }

    /// The snarl node id whose handle is `core`'s `stable_id`.
    fn snarl_id(snarl: &Snarl<Handle>, graph: &Graph, core: NodeId) -> SnarlNodeId {
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

    /// The full sync invariant: every snarl node has a backing `stable_id` in core,
    /// and every snarl wire corresponds to an accepted core edge.
    fn assert_in_sync(graph: &Graph, snarl: &Snarl<Handle>) {
        for (_, &handle) in snarl.node_ids() {
            assert!(
                graph.node_id_of(handle).is_some(),
                "snarl node {handle} has no backing core node"
            );
        }
        for (out_id, in_id) in snarl.wires() {
            let source = graph
                .node_id_of(*snarl.get_node(out_id.node).expect("out handle"))
                .expect("source in core");
            let dest = graph
                .node_id_of(*snarl.get_node(in_id.node).expect("in handle"))
                .expect("dest in core");
            assert_eq!(
                graph.input_source(dest, in_id.input),
                Some((source, out_id.output)),
                "snarl wire has no matching core edge"
            );
        }
    }

    #[test]
    fn node_data_is_pulled_from_core_not_copied() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");

        let head_handle = graph.stable_id(head).expect("handle");
        let modr_handle = graph.stable_id(modr).expect("handle");
        let mut viewer = GraphViewer::for_test(&mut graph);

        assert_eq!(viewer.title(&head_handle), tr("node-generator.fbm"));
        assert_eq!(viewer.inputs(&head_handle), 0);
        assert_eq!(viewer.outputs(&head_handle), 1);
        assert_eq!(viewer.inputs(&modr_handle), 1);
        assert_eq!(viewer.outputs(&modr_handle), 1);
    }

    #[test]
    fn accepted_connection_lands_in_core_and_in_snarl() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (s_head, s_modr) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, s_head, s_modr);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);

        assert!(edge_exists(&graph, head, modr));
        assert_eq!(wires_into(&snarl, s_modr), 1);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn cycle_forming_connection_is_rejected_everywhere() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let a = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("a");
        let b = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("b");
        let (sa, sb) = (snarl_id(&snarl, &graph, a), snarl_id(&snarl, &graph, b));

        let (out, inp) = pins(&snarl, sa, sb);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        assert!(edge_exists(&graph, a, b));

        // b -> a would close a loop: refused in core and never wired in snarl.
        let (out, inp) = pins(&snarl, sb, sa);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        assert!(!edge_exists(&graph, b, a));
        assert_eq!(wires_into(&snarl, sa), 0);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn second_connection_to_an_input_overwrites_in_core_and_view() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let g1 = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("g1");
        let g2 = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("g2");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (s1, s2, sm) = (
            snarl_id(&snarl, &graph, g1),
            snarl_id(&snarl, &graph, g2),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, s1, sm);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        let (out, inp) = pins(&snarl, s2, sm);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);

        assert!(edge_exists(&graph, g2, modr));
        assert!(!edge_exists(&graph, g1, modr));
        assert_eq!(wires_into(&snarl, sm), 1);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn disconnect_clears_core_and_view() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (sh, sm) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );

        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer::for_test(&mut graph).disconnect(&out, &inp, &mut snarl);

        assert!(!edge_exists(&graph, head, modr));
        assert_eq!(wires_into(&snarl, sm), 0);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn delete_cascades_through_core_and_snarl() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let modr = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("thermal");
        let (sh, sm) = (
            snarl_id(&snarl, &graph, head),
            snarl_id(&snarl, &graph, modr),
        );
        let (out, inp) = pins(&snarl, sh, sm);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);

        remove_snarl_node(&mut graph, &mut snarl, sh);
        assert_eq!(graph.node_count(), 1);
        assert_eq!(snarl.nodes().count(), 1);
        assert!(!edge_exists(&graph, head, modr));
        assert_eq!(wires_into(&snarl, sm), 0);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn add_node_rejects_unknown_type_and_touches_nothing() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        assert!(add_node(&mut graph, &mut snarl, "no.such.node", Pos2::ZERO).is_none());
        assert_eq!(graph.node_count(), 0);
        assert_eq!(snarl.nodes().count(), 0);
    }
}
