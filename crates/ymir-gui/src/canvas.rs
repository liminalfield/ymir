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

use ymir_core::{EvalRequest, Graph, NodeId, Params, Region, registry};
use ymir_nodes::tr;

/// Constant width for the right-click context menus, so they do not resize with
/// their longest item (the wider node menu vs the narrow "Add node" graph menu).
const CONTEXT_MENU_WIDTH: f32 = 200.0;

/// Canvas zoom bounds (#65): the graph can't shrink to an unfindable speck or grow
/// unboundedly. Used for snarl's clamp and our scroll-zoom.
pub(crate) const MIN_SCALE: f32 = 0.4;
pub(crate) const MAX_SCALE: f32 = 2.0;

/// Zooms `to_global` by `factor` about `cursor` (screen space), keeping the graph
/// point under the cursor fixed, within the zoom bounds. Mirrors egui `Scene`'s zoom
/// so plain-scroll zoom (#36) matches ctrl-scroll zoom.
///
/// The `factor` is clamped *before* it is applied so the resulting scale lands within
/// `[min, max]`. Clamping the scale *after* the zoom (as a first cut did) would leave
/// the translation inconsistent — jumping the view — and push the scale out of range,
/// triggering snarl's own clamp-around-screen-centre on the next frame.
fn zoom_around(
    to_global: egui::emath::TSTransform,
    factor: f32,
    cursor: Pos2,
    min: f32,
    max: f32,
) -> egui::emath::TSTransform {
    use egui::emath::TSTransform;
    let factor = factor.clamp(min / to_global.scaling, max / to_global.scaling);
    let in_scene = to_global.inverse() * cursor;
    to_global
        * TSTransform::from_translation(in_scene.to_vec2())
        * TSTransform::from_scaling(factor)
        * TSTransform::from_translation(-in_scene.to_vec2())
}

/// Styles a right-click context-menu ui to match the node-creation menu: taller
/// rows, a constant width, and the blue selection highlight on hover (egui's default
/// is a muted grey). Pointing the hovered/active widget fills at `selection.bg_fill`
/// gives the same blue a `Button::selectable` shows when selected.
fn style_context_menu(ui: &mut egui::Ui) {
    ui.spacing_mut().button_padding = egui::vec2(8.0, 6.0);
    ui.set_min_width(CONTEXT_MENU_WIDTH);
    let selection = ui.visuals().selection;
    let widgets = &mut ui.visuals_mut().widgets;
    for state in [&mut widgets.hovered, &mut widgets.active] {
        state.weak_bg_fill = selection.bg_fill;
        state.bg_fill = selection.bg_fill;
        state.fg_stroke.color = selection.stroke.color;
    }
}

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
    /// dot at the left of that node's header. Only the previewed node has a status,
    /// since the preview evaluates a single target. Input, read-only.
    pub(crate) status: Option<(Handle, egui::Color32)>,
    /// The node pinned as the preview target, if any (#39). It gets a ring around its
    /// status dot so it reads as locked. Input, read-only.
    pub(crate) pinned: Option<Handle>,
    /// Set by the graph context menu ("Add node") to the graph-space position where
    /// the user asked to add a node; the canvas reads it after the frame to open the
    /// node menu there (#60). Output.
    pub(crate) add_node_at: Option<egui::Pos2>,
    /// A node the viewer asks the canvas to select after the frame (e.g. a duplicate),
    /// keeping selection logic in one place. Output.
    pub(crate) select_after: Option<Handle>,
    /// A node the viewer asks the canvas to rename (context-menu "Rename"); the canvas
    /// opens the rename dialog for it after the frame (#61). Output.
    pub(crate) rename_request: Option<Handle>,
    /// A preview-pin change the viewer requests (context-menu Pin/Unpin, #39): the
    /// inner value is the new pin (`Some(node)` to pin, `None` to unpin). Output.
    pub(crate) pin_request: Option<Option<Handle>>,
    /// A one-shot pan/zoom override to apply this frame ("zoom to graph", #65). Input.
    pub(crate) pending_view: Option<egui::emath::TSTransform>,
    /// Set when the graph context menu's "Zoom to graph" was chosen; the canvas
    /// computes the fit from the node rects after the frame (#65). Output.
    pub(crate) frame_all_request: bool,
    /// A scroll-wheel zoom to apply this frame: `(factor, cursor)` in screen space
    /// (#36). snarl's Scene only zooms on ctrl-scroll, so plain scroll is applied
    /// here instead of letting it pan. Input.
    pub(crate) zoom: Option<(f32, Pos2)>,
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
            pinned: None,
            add_node_at: None,
            select_after: None,
            rename_request: None,
            pin_request: None,
            pending_view: None,
            frame_all_request: false,
            zoom: None,
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

    /// Whether `id` is structurally broken (a disconnected required input or a cycle).
    /// A cheap check via the graph's output key, whose `Ok`/`Err` outcome is
    /// independent of resolution and seed, so a throwaway request suffices.
    fn is_broken(&self, id: NodeId) -> bool {
        self.graph
            .output_key(id, &EvalRequest::new(1, 1, Region::UNIT, 0))
            .is_err()
    }

    /// A node's display name: its per-instance override if set (#59), else its type's
    /// name resolved through [`tr`].
    fn display_label(&self, id: NodeId) -> String {
        if let Some(name) = self.graph.name(id) {
            return name.to_string();
        }
        self.graph.spec(id).map_or_else(
            || "<missing>".to_string(),
            |spec| tr(&format!("node-{}", spec.type_id)).to_string(),
        )
    }

    /// Duplicates `node` (same type and params, a fresh `stable_id`, unconnected) at
    /// a small offset, and asks the canvas to select it (#61). The name override is
    /// not copied, so the copy is distinguishable and does not share a label.
    fn duplicate_node(&mut self, node: SnarlNodeId, snarl: &mut Snarl<Handle>) {
        let Some(src) = self.core_id_of_snarl(snarl, node) else {
            return;
        };
        let Some(spec) = self.graph.spec(src) else {
            return;
        };
        let Some(operator) = registry::make(spec.type_id) else {
            return;
        };
        let params = self.graph.params(src).cloned().unwrap_or_default();
        let new_id = self.graph.add_op(operator, params);
        let Some(handle) = self.graph.stable_id(new_id) else {
            return;
        };
        let pos = snarl
            .get_node_info(node)
            .map_or(Pos2::ZERO, |info| info.pos + egui::vec2(30.0, 30.0));
        snarl.insert_node(pos, handle);
        self.select_after = Some(handle);
    }

    /// Disconnects every wire touching `node` (inputs and outputs), in core and snarl
    /// together so the two stay in sync (#61).
    fn disconnect_all(&mut self, node: SnarlNodeId, snarl: &mut Snarl<Handle>) {
        let touching: Vec<_> = snarl
            .wires()
            .filter(|(out_pin, in_pin)| out_pin.node == node || in_pin.node == node)
            .collect();
        for (out_pin, in_pin) in touching {
            // Core holds the edge on the destination input; drop it there, then mirror
            // into snarl, exactly as the per-wire `disconnect` hook does.
            if let Some(dest) = self.core_id_of_snarl(snarl, in_pin.node)
                && self.graph.disconnect(dest, in_pin.input).is_ok()
            {
                snarl.disconnect(out_pin, in_pin);
            }
        }
    }
}

impl SnarlViewer<Handle> for GraphViewer<'_> {
    fn title(&mut self, node: &Handle) -> String {
        match self.core_id(*node) {
            Some(id) => self.display_label(id),
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
            .map_or_else(|| "<missing>".to_string(), |id| self.display_label(id));
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
            // Always reserve the status dot's space so a node never changes width when
            // it becomes the previewed node (a layout jump is jarring). Paint the
            // colour only for the previewed node; the slot stays empty otherwise.
            let diameter = ui.text_style_height(&egui::TextStyle::Body) * 0.55;
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
            // The dot's colour: the previewed node shows the preview status; any other
            // structurally-broken node shows red, so a broken node (e.g. a Blend with
            // a disconnected input) is visible even while the preview is pinned
            // elsewhere (#43; a fuller per-node status is #44).
            let dot = handle.and_then(|h| {
                if let Some((status_handle, color)) = self.status
                    && status_handle == h
                {
                    Some(color)
                } else if self.core_id(h).is_some_and(|id| self.is_broken(id)) {
                    Some(ui.visuals().error_fg_color)
                } else {
                    None
                }
            });
            if let Some(color) = dot {
                ui.painter()
                    .circle_filled(rect.center(), diameter * 0.5, color);
            }
            // A ring around the dot marks the pinned node, so it reads as the locked
            // preview target even as selection moves elsewhere. Painted (not
            // allocated), so it never changes the node's width.
            if handle.is_some() && handle == self.pinned {
                ui.painter().circle_stroke(
                    rect.center(),
                    diameter * 0.5 + 1.5,
                    egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                );
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
        // Apply a one-shot "zoom to graph" view if requested (#65), overriding the
        // pan/zoom snarl computed this frame. snarl persists this transform, so the
        // framed view sticks until the user pans or zooms again. Otherwise apply a
        // scroll-wheel zoom about the cursor (#36), since snarl's Scene only zooms on
        // ctrl-scroll and we suppressed its plain-scroll pan.
        if let Some(view) = self.pending_view {
            *to_global = view;
        } else if let Some((factor, cursor)) = self.zoom {
            *to_global = zoom_around(*to_global, factor, cursor, MIN_SCALE, MAX_SCALE);
        }
        // Capture the pan/zoom transform, so a screen click can be mapped into the
        // local space the node rects are recorded in.
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
        style_context_menu(ui);
        if ui.button("Duplicate").clicked() {
            self.duplicate_node(node, snarl);
            ui.close();
        }
        if ui.button("Rename").clicked() {
            self.rename_request = snarl.get_node(node).copied();
            ui.close();
        }
        // Pin/Unpin the 2D preview to this node (#39). Only previewable nodes (those
        // with an output) qualify; an endpoint has nothing to preview.
        if let Some(handle) = snarl.get_node(node).copied()
            && self
                .core_id(handle)
                .and_then(|id| self.graph.spec(id))
                .is_some_and(|spec| !spec.outputs.is_empty())
        {
            let is_pinned = self.pinned == Some(handle);
            let label = if is_pinned {
                "Unpin preview"
            } else {
                "Pin to preview"
            };
            if ui.button(label).clicked() {
                self.pin_request = Some((!is_pinned).then_some(handle));
                ui.close();
            }
        }
        if ui.button("Delete all connections").clicked() {
            self.disconnect_all(node, snarl);
            ui.close();
        }
        if ui.button("Delete node").clicked() {
            remove_snarl_node(self.graph, snarl, node);
            ui.close();
        }
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<Handle>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut egui::Ui, _snarl: &mut Snarl<Handle>) {
        // Record the clicked spot; the canvas opens the node-creation menu there
        // after the frame, reusing the Space menu (#60).
        style_context_menu(ui);
        if ui.button("Add node").clicked() {
            self.add_node_at = Some(pos);
            ui.close();
        }
        if ui.button("Zoom to graph").clicked() {
            self.frame_all_request = true;
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
        // Thermal has a required `in` plus an optional `mask`, so two input ports.
        assert_eq!(viewer.inputs(&modr_handle), 2);
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
    fn title_uses_the_name_override_then_falls_back_to_the_type() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let head = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("fbm");
        let handle = graph.stable_id(head).expect("handle");
        graph
            .set_name(head, Some("Base Terrain".to_string()))
            .expect("set name");

        let mut viewer = GraphViewer::for_test(&mut graph);
        assert_eq!(viewer.title(&handle), "Base Terrain");
        // Clearing the override reverts to the type's name.
        viewer.graph.set_name(head, None).expect("clear name");
        assert_eq!(viewer.title(&handle), tr("node-generator.fbm"));
    }

    #[test]
    fn duplicate_clones_type_and_params_with_a_fresh_id() {
        use ymir_core::ParamValue;
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let src = add_node(&mut graph, &mut snarl, THERMAL, Pos2::new(10.0, 20.0)).expect("src");
        graph
            .set_params(src, Params::new().with("talus", ParamValue::Float(0.05)))
            .expect("set params");
        let s_src = snarl_id(&snarl, &graph, src);
        let src_handle = graph.stable_id(src).expect("handle");

        GraphViewer::for_test(&mut graph).duplicate_node(s_src, &mut snarl);

        assert_eq!(graph.node_count(), 2, "a node was added");
        assert_eq!(snarl.nodes().count(), 2);
        // The duplicate has a distinct handle, the same type, and the copied params.
        let dup_handle = snarl
            .node_ids()
            .map(|(_, h)| *h)
            .find(|&h| h != src_handle)
            .expect("duplicate handle");
        let dup_id = graph.node_id_of(dup_handle).expect("duplicate id");
        assert_eq!(
            graph.spec(dup_id).expect("spec").type_id,
            "modifier.thermal_erosion"
        );
        assert!((graph.params(dup_id).expect("params").get_f64("talus", 0.0) - 0.05).abs() < 1e-9);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn delete_all_connections_clears_every_wire_touching_a_node() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let a = add_node(&mut graph, &mut snarl, FBM, Pos2::ZERO).expect("a");
        let b = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("b");
        let c = add_node(&mut graph, &mut snarl, THERMAL, Pos2::ZERO).expect("c");
        let (sa, sb, sc) = (
            snarl_id(&snarl, &graph, a),
            snarl_id(&snarl, &graph, b),
            snarl_id(&snarl, &graph, c),
        );
        // a -> b -> c, so b has both an input and an output wire.
        let (out, inp) = pins(&snarl, sa, sb);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        let (out, inp) = pins(&snarl, sb, sc);
        GraphViewer::for_test(&mut graph).connect(&out, &inp, &mut snarl);
        assert!(edge_exists(&graph, a, b) && edge_exists(&graph, b, c));

        GraphViewer::for_test(&mut graph).disconnect_all(sb, &mut snarl);

        assert!(!edge_exists(&graph, a, b), "input wire dropped");
        assert!(!edge_exists(&graph, b, c), "output wire dropped");
        assert_eq!(wires_into(&snarl, sb), 0);
        assert_eq!(wires_into(&snarl, sc), 0);
        assert_in_sync(&graph, &snarl);
    }

    #[test]
    fn zoom_around_keeps_the_cursor_point_fixed_and_clamps() {
        use egui::emath::TSTransform;
        let cursor = Pos2::new(100.0, 50.0);
        let t = TSTransform::IDENTITY;
        let in_scene = t.inverse() * cursor;

        let zoomed = zoom_around(t, 2.0, cursor, 0.1, 4.0);
        assert!((zoomed.scaling - 2.0).abs() < 1e-6);
        // The graph point under the cursor stays under the cursor after zooming.
        assert!(((zoomed * in_scene) - cursor).length() < 1e-3);

        // Zoom in then out by the inverse returns to the original view (reversible).
        let back = zoom_around(
            zoom_around(t, 1.5, cursor, 0.1, 4.0),
            1.0 / 1.5,
            cursor,
            0.1,
            4.0,
        );
        assert!((back.scaling - 1.0).abs() < 1e-5);
        assert!((back.translation - t.translation).length() < 1e-3);

        // At the zoom limit the factor is clamped, so the scale stops at the bound and
        // the cursor point still stays fixed (no jump to centre).
        let at_max = zoom_around(t, 100.0, cursor, 0.1, 4.0);
        assert!((at_max.scaling - 4.0).abs() < 1e-6);
        assert!(((at_max * in_scene) - cursor).length() < 1e-3);
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
