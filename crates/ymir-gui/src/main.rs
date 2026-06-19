//! Ymir's node editor and viewport.
//!
//! The app shell mounts self-registered panes (ribbon, canvas, parameter
//! inspector, 2D preview) over a single canonical [`Graph`]. The ribbon and
//! palette are generated from the operator and category registries; the canvas
//! ([`canvas`]) is an `egui-snarl` pure view over the graph; the inspector
//! ([`param_ui`]) edits the selected node's params; and the 2D preview
//! ([`preview`]) evaluates the selected node's output on a worker thread. All
//! display strings resolve through `tr(key)`, so this crate holds no node prose.

use eframe::egui;
use egui_snarl::Snarl;
use egui_snarl::ui::SnarlWidget;
use ymir_core::registry;
use ymir_core::{EvalRequest, Graph, Region};
use ymir_nodes::{CategoryDef, categories, find_category, tr};

// The node-editor canvas (GUI step 5, issue #6): egui-snarl as a pure view over the
// core graph, per the policy confirmed by the spike (issue #5).
mod canvas;
use canvas::Handle;
// The parameter inspector: ParamSpec-driven widgets, no per-node code.
mod param_ui;
// The visual curve editor widget (GUI step A2), rendered for ParamKind::Curve.
mod curve_edit;
// Background preview evaluation (GUI step 6b): off-thread, latest-wins.
mod preview;
use preview::PreviewEngine;

/// Resolution of the interactive 2D preview. Low for responsiveness; it is an
/// approximation of the target-resolution build, never equal to it (erosion is
/// resolution-dependent). The build resolution is decided later (step 6c).
const PREVIEW_RES: usize = 256;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Ymir",
        options,
        Box::new(|cc| Ok(Box::new(YmirApp::new(cc)))),
    )
}

// ---- application state ------------------------------------------------------

/// Which palette tab is active: a registered category, or the synthesized
/// "Uncategorized" group for nodes whose category has no `CategoryDef`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Category(&'static str),
    Uncategorized,
}

/// The canvas's pan/zoom view, captured each frame so other panes (the ribbon
/// add) can place a node where the user is actually looking. The transform maps
/// the canvas's local graph space to screen; `rect` is the canvas area on screen.
/// This is the one shared "screen point to graph position" helper the node-creation
/// paths (ribbon add, future tab-menu) build on.
#[derive(Clone, Copy)]
struct CanvasView {
    to_global: egui::emath::TSTransform,
    rect: egui::Rect,
}

impl CanvasView {
    /// Maps a screen position into the canvas's local graph space.
    fn graph_pos(&self, screen: egui::Pos2) -> egui::Pos2 {
        self.to_global.inverse() * screen
    }

    /// The graph-space position at the centre of the visible canvas.
    ///
    /// A degenerate transform (an empty, not-yet-laid-out canvas) maps to a
    /// non-finite point; fall back to the screen-space centre, which is correct
    /// while the transform is the identity-like default it recovers to, and keeps
    /// placement finite (a NaN position panics egui's layout) and on the canvas.
    fn center(&self) -> egui::Pos2 {
        let mapped = self.graph_pos(self.rect.center());
        if mapped.is_finite() {
            mapped
        } else {
            self.rect.center()
        }
    }
}

/// The data panes draw from. Panes receive `&mut AppState`, never the app shell,
/// so they stay mount-agnostic.
struct AppState {
    /// The canonical graph being composed. The canvas renders it and edits flow
    /// back into it; the evaluator (step 6) runs it.
    graph: Graph,
    /// The canvas view over `graph`: snarl holds only node handles (`stable_id`)
    /// and view-state (positions), never a copy of node data.
    snarl: Snarl<Handle>,
    /// The canvas's pan/zoom view from the last frame, for placing new nodes in
    /// view. `None` until the canvas has drawn once.
    canvas_view: Option<CanvasView>,
    /// The node selected on the canvas (its `stable_id`), whose parameters the
    /// inspector edits and whose output the 2D preview shows. Refreshed each frame
    /// from the canvas selection.
    selected: Option<Handle>,
    /// The global seed for evaluation, set by the ribbon control. Reseeds the whole
    /// world; each node stays internally stable across edits.
    seed: u64,
    /// Background preview evaluation: submits graph snapshots and shows results
    /// without ever blocking the UI thread.
    preview: PreviewEngine,
    /// The selected palette tab (defaults to the first category on first draw).
    active_tab: Option<ActiveTab>,
    /// The node-search query; when non-empty it overrides the active tab.
    search: String,
}

impl AppState {
    fn new() -> Self {
        Self {
            graph: Graph::new(),
            snarl: Snarl::new(),
            canvas_view: None,
            selected: None,
            seed: 0,
            preview: PreviewEngine::new(),
            active_tab: None,
            search: String::new(),
        }
    }
}

// ---- palette: registry-driven node listing (pure, testable) -----------------

/// A node available in the palette, projected from its `NodeSpec`. All fields are
/// `'static` (they come from the spec's static strings); the display name is
/// resolved on demand via [`tr`] so the data layer stays prose-free.
struct NodeEntry {
    type_id: &'static str,
    category: &'static str,
    tags: &'static [&'static str],
}

/// Every registered operator, projected to a [`NodeEntry`]. Built from the
/// operator registry, never a hand-kept list.
fn node_entries() -> Vec<NodeEntry> {
    registry::entries()
        .map(|entry| {
            let spec = (entry.make)().spec();
            NodeEntry {
                type_id: spec.type_id,
                category: spec.category,
                tags: spec.tags,
            }
        })
        .collect()
}

/// The registered categories, sorted by `sort` then `id` for a stable palette.
fn categories_sorted() -> Vec<&'static CategoryDef> {
    let mut cats: Vec<_> = categories().collect();
    cats.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.id.cmp(b.id)));
    cats
}

/// Whether any operator declares a category with no registered `CategoryDef`
/// (which the palette shows under an "Uncategorized" tab).
fn has_uncategorized_nodes() -> bool {
    node_entries()
        .iter()
        .any(|e| find_category(e.category).is_none())
}

/// Whether a node matches a lowercased search query, over its display name and
/// its tags (a simple case-insensitive contains; can grow into true fuzzy later).
fn node_matches(entry: &NodeEntry, query: &str) -> bool {
    let name = tr(&format!("node-{}", entry.type_id)).to_lowercase();
    name.contains(query)
        || entry
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(query))
}

/// The nodes shown for the current tab/search selection.
fn visible_nodes<'a>(
    entries: &'a [NodeEntry],
    tab: Option<ActiveTab>,
    search: &str,
) -> Vec<&'a NodeEntry> {
    let query = search.trim().to_lowercase();
    if query.is_empty() {
        entries
            .iter()
            .filter(|e| match tab {
                Some(ActiveTab::Category(id)) => e.category == id,
                Some(ActiveTab::Uncategorized) => find_category(e.category).is_none(),
                None => false,
            })
            .collect()
    } else {
        entries.iter().filter(|e| node_matches(e, &query)).collect()
    }
}

/// A spawn position (in graph space) for the next node placed from the ribbon.
///
/// Anchored at the centre of the currently visible canvas so the node lands where
/// the user is looking, not at a fixed graph-space origin that may be off-screen
/// when panned or zoomed. A small per-add cascade keeps successive adds from
/// stacking exactly. Falls back to a fixed point before the canvas has drawn once.
fn spawn_pos(view: Option<CanvasView>, node_count: usize) -> egui::Pos2 {
    let step = (node_count % 12) as f32 * 24.0;
    let base = view.map_or(egui::pos2(40.0, 40.0), |v| v.center());
    // Offset up-left of centre so the node body sits roughly centred, then cascade.
    base + egui::vec2(step - 60.0, step - 24.0)
}

// ---- pane kinds (self-registered by id) -------------------------------------

/// A pane kind: a stable id plus a mount-agnostic draw function. Pane kinds
/// self-register with `inventory::submit!`, so adding one is a single
/// registration beside the pane and touches no layout code.
struct PaneKind {
    id: &'static str,
    draw: fn(&mut egui::Ui, &mut AppState),
}

inventory::collect!(PaneKind);

/// Looks up a registered pane kind by id.
fn pane_kind(id: &str) -> Option<&'static PaneKind> {
    inventory::iter::<PaneKind>().find(|p| p.id == id)
}

/// Draws the pane registered under `id`, or a visible placeholder if the id is
/// unknown — graceful degradation, never a panic.
fn draw_pane(id: &str, ui: &mut egui::Ui, state: &mut AppState) {
    match pane_kind(id) {
        Some(kind) => (kind.draw)(ui, state),
        None => {
            let error = ui.visuals().error_fg_color;
            ui.colored_label(error, format!("unknown pane: {id:?}"));
        }
    }
}

fn menu_bar_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in ["File", "Edit", "View", "Graph", "Help"] {
            ui.menu_button(menu, |ui| {
                ui.weak("(empty)");
            });
        }
    });
}
inventory::submit! { PaneKind { id: "menu-bar", draw: menu_bar_pane } }

fn ribbon_pane(ui: &mut egui::Ui, state: &mut AppState) {
    let cats = categories_sorted();
    if state.active_tab.is_none() {
        state.active_tab = cats.first().map(|c| ActiveTab::Category(c.id));
    }

    // Row 1: category tabs, search, and the Build action.
    ui.horizontal(|ui| {
        for cat in &cats {
            let key = format!("category-{}", cat.id);
            ui.selectable_value(
                &mut state.active_tab,
                Some(ActiveTab::Category(cat.id)),
                tr(&key),
            );
        }
        if has_uncategorized_nodes() {
            ui.selectable_value(
                &mut state.active_tab,
                Some(ActiveTab::Uncategorized),
                "Uncategorized",
            );
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .hint_text("search nodes")
                .desired_width(160.0),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Build is the full-resolution cook; wired in a later step (6c).
            ui.add_enabled(false, egui::Button::new("Build"));
            ui.separator();
            // Global seed: reseeds the whole world and re-evaluates the preview.
            ui.add(
                egui::DragValue::new(&mut state.seed)
                    .prefix("seed: ")
                    .speed(1),
            );
        });
    });

    ui.separator();

    // Row 2: the nodes for the active tab (or search), as buttons.
    let entries = node_entries();
    let shown = visible_nodes(&entries, state.active_tab, &state.search);
    ui.horizontal_wrapped(|ui| {
        for entry in shown {
            let key = format!("node-{}", entry.type_id);
            if ui.button(tr(&key)).clicked() {
                let pos = spawn_pos(state.canvas_view, state.graph.node_count());
                canvas::add_node(&mut state.graph, &mut state.snarl, entry.type_id, pos);
            }
        }
    });
}
inventory::submit! { PaneKind { id: "ribbon", draw: ribbon_pane } }

fn params_pane(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Parameters");

    // Resolve the selected handle to a live node; nothing selected (or it was
    // deleted) shows a hint, not an error.
    let Some(id) = state.selected.and_then(|h| state.graph.node_id_of(h)) else {
        ui.weak("Select a node to edit its parameters.");
        return;
    };
    let Some(spec) = state.graph.spec(id) else {
        ui.weak("Select a node to edit its parameters.");
        return;
    };

    ui.label(tr(&format!("node-{}", spec.type_id)));
    if spec.params.is_empty() {
        ui.weak("This node has no parameters.");
        return;
    }

    // Edit against a clone of the current params, then write back once if anything
    // changed. The graph stays the single source of truth.
    let mut params = state.graph.params(id).cloned().unwrap_or_default();
    let mut changed = false;
    for pspec in &spec.params {
        let current = param_ui::current_value(&params, pspec);
        if let Some(new_value) = param_ui::edit(ui, pspec, &current) {
            params.insert(pspec.name.clone(), new_value);
            changed = true;
        }
    }

    if changed && let Err(err) = state.graph.set_params(id, params) {
        // The node would have to vanish mid-frame to reach here; surface it rather
        // than swallow it.
        ui.colored_label(ui.visuals().error_fg_color, err.to_string());
    }
}
inventory::submit! { PaneKind { id: "params", draw: params_pane } }

fn preview_2d_pane(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("2D preview");
    ui.weak(format!(
        "{PREVIEW_RES}×{PREVIEW_RES} preview — an approximation, not the build"
    ));

    let Some(id) = state.selected.and_then(|h| state.graph.node_id_of(h)) else {
        ui.weak("Select a node to preview its output.");
        return;
    };
    // Only nodes with an output can be previewed; evaluating an endpoint would run
    // its side effect (export writing a file) just from selecting it.
    if state
        .graph
        .spec(id)
        .is_none_or(|spec| spec.outputs.is_empty())
    {
        ui.weak("This node has no output to preview.");
        return;
    }

    // Submit a snapshot for off-thread evaluation if the output changed, collect any
    // result, and render — none of which blocks the UI thread.
    let request = EvalRequest::new(PREVIEW_RES, PREVIEW_RES, Region::UNIT, state.seed);
    let now = ui.input(|i| i.time);
    state.preview.sync(&state.graph, id, request, now);
    state.preview.poll(ui.ctx());
    state.preview.show(ui);
}
inventory::submit! { PaneKind { id: "preview-2d", draw: preview_2d_pane } }

fn canvas_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // The previewed node's status dot, for its header. Only a previewable selected
    // node has one (the preview evaluates a single target). Computed before the
    // disjoint borrow below, from read-only fields.
    let status = state
        .selected
        .and_then(|h| state.graph.node_id_of(h).map(|id| (h, id)))
        .filter(|(_, id)| {
            state
                .graph
                .spec(*id)
                .is_some_and(|spec| !spec.outputs.is_empty())
        })
        .map(|(h, _)| (h, state.preview.status_color(ui.visuals())));

    // Disjoint borrows: the viewer holds the graph while snarl is rendered. Both
    // are distinct fields of the state, so this split is sound.
    let AppState {
        graph,
        snarl,
        selected,
        ..
    } = &mut *state;
    let mut viewer = canvas::GraphViewer {
        graph,
        selected: *selected,
        node_rects: Vec::new(),
        to_global: egui::emath::TSTransform::IDENTITY,
        status,
    };
    // The canvas's screen rect comes from the ui, not snarl's response: snarl
    // returns an unbounded `EVERYTHING` rect, so it cannot be used for hit-testing
    // or to locate the visible centre.
    let canvas_rect = ui.max_rect();
    SnarlWidget::new()
        .id_salt("ymir-canvas")
        .show(snarl, &mut viewer, ui);

    // Capture the view now (Copy data, no borrows); stored on the state at the end,
    // once the graph/snarl/selected borrows are released.
    let view = CanvasView {
        to_global: viewer.to_global,
        rect: canvas_rect,
    };

    // Resolve selection from a plain click (snarl 0.10 only selects on shift-click).
    // A click is a press-and-release without movement, so drags — wiring from or to
    // a pin, and node moves — are excluded automatically and keep their behavior.
    // Reading the pointer rather than registering an interaction leaves snarl's own
    // widgets (the collapse chevron, the pins) their clicks.
    let click = ui
        .ctx()
        .input(|i| {
            i.pointer
                .primary_clicked()
                .then(|| i.pointer.interact_pos())
        })
        .flatten();
    if let Some(screen_pos) = click.filter(|p| canvas_rect.contains(*p)) {
        // Node rects are in the canvas's local space; map the screen click back into
        // it through the inverse pan/zoom transform before hit-testing.
        let pos = viewer.to_global.inverse() * screen_pos;
        match viewer
            .node_rects
            .iter()
            .find(|(_, rect)| rect.contains(pos))
        {
            Some((handle, rect)) => {
                // The collapse chevron sits at the header's top-left and toggles the
                // node; a click there must not also select. Exclude that corner.
                let chevron = egui::Rect::from_min_size(
                    rect.min,
                    egui::Vec2::splat(ui.spacing().icon_width + 12.0),
                );
                if !chevron.contains(pos) {
                    *selected = Some(*handle);
                }
            }
            // A click on empty canvas clears the selection.
            None => *selected = None,
        }
    }

    // Keep the view as long as its rect is finite; a degenerate transform is handled
    // by CanvasView::center falling back to the screen centre, so placement stays
    // finite (a NaN position panics egui's layout) and on the canvas.
    state.canvas_view = view.rect.is_finite().then_some(view);
}
inventory::submit! { PaneKind { id: "canvas", draw: canvas_pane } }

fn viewport_3d_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.centered_and_justified(|ui| {
        ui.weak("3D viewport — placeholder (step 7)");
    });
}
inventory::submit! { PaneKind { id: "viewport-3d", draw: viewport_3d_pane } }

// ---- layout description + fixed-panel backend -------------------------------

/// Which pane kind (by id) fills each slot of the default layout. This is the
/// data the v1 backend reads; a future workspace tree and docking backend will
/// replace the fixed slots without touching any pane internals.
struct Layout {
    menu_bar: &'static str,
    ribbon: &'static str,
    params: &'static str,
    preview_2d: &'static str,
    canvas: &'static str,
    viewport_3d: &'static str,
}

fn default_layout() -> Layout {
    Layout {
        menu_bar: "menu-bar",
        ribbon: "ribbon",
        params: "params",
        preview_2d: "preview-2d",
        canvas: "canvas",
        viewport_3d: "viewport-3d",
    }
}

/// The v1 layout backend: mounts the panes named by `layout` into fixed,
/// resizable native egui panels.
fn mount(layout: &Layout, ui: &mut egui::Ui, state: &mut AppState) {
    egui::Panel::top("menu_bar").show_inside(ui, |ui| draw_pane(layout.menu_bar, ui, state));
    egui::Panel::top("ribbon").show_inside(ui, |ui| draw_pane(layout.ribbon, ui, state));

    egui::Panel::right("right_column")
        .resizable(true)
        .default_size(300.0)
        .show_inside(ui, |ui| {
            draw_pane(layout.params, ui, state);
            ui.separator();
            draw_pane(layout.preview_2d, ui, state);
        });

    egui::CentralPanel::default().show_inside(ui, |ui| {
        egui::Panel::right("viewport_3d")
            .resizable(true)
            .default_size(ui.available_width() * 0.4)
            .show_inside(ui, |ui| draw_pane(layout.viewport_3d, ui, state));
        draw_pane(layout.canvas, ui, state);
    });
}

// ---- app shell --------------------------------------------------------------

struct YmirApp {
    state: AppState,
}

impl YmirApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            state: AppState::new(),
        }
    }
}

impl eframe::App for YmirApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        mount(&default_layout(), ui, &mut self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registries_populate_the_palette() {
        // Proves ymir-nodes is linked: its operator and category registrations
        // are visible. If 0, the crate did not link.
        assert!(
            node_entries().len() >= 3,
            "operators not registered in the GUI"
        );
        assert!(
            categories_sorted().len() >= 3,
            "categories not registered in the GUI"
        );
    }

    #[test]
    fn categories_are_sorted_by_sort_then_id() {
        let ids: Vec<&str> = categories_sorted().iter().map(|c| c.id).collect();
        // sort 0, 5, 6, 7, 10, 90
        assert_eq!(
            ids,
            ["noise", "combine", "filter", "mask", "erosion", "output"]
        );
    }

    #[test]
    fn nodes_filter_by_category() {
        let entries = node_entries();
        let noise = visible_nodes(&entries, Some(ActiveTab::Category("noise")), "");
        assert!(noise.iter().all(|e| e.category == "noise"));
        assert!(noise.iter().any(|e| e.type_id == "generator.fbm"));
    }

    #[test]
    fn search_matches_name_and_tags() {
        let entries = node_entries();
        assert!(
            visible_nodes(&entries, None, "fbm")
                .iter()
                .any(|e| e.type_id == "generator.fbm")
        );
        assert!(
            visible_nodes(&entries, None, "talus")
                .iter()
                .any(|e| e.type_id == "modifier.thermal_erosion")
        );
        assert!(visible_nodes(&entries, None, "zzznotanode").is_empty());
    }

    #[test]
    fn all_operators_are_categorized() {
        assert!(!has_uncategorized_nodes());
    }

    #[test]
    fn canvas_view_maps_screen_to_graph() {
        // With a pan+zoom transform, the screen->graph map is the inverse: a 2x
        // zoom and a (100, 50) pan put graph-space (0,0) at screen (100, 50).
        let view = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(100.0, 50.0), 2.0),
            rect: egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(200.0, 200.0)),
        };
        assert_eq!(
            view.graph_pos(egui::pos2(100.0, 50.0)),
            egui::pos2(0.0, 0.0)
        );
        // The visible centre maps to the graph-space centre of the view.
        assert_eq!(view.center(), egui::pos2(50.0, 50.0));
    }

    #[test]
    fn a_degenerate_view_falls_back_to_the_screen_centre() {
        // A zero-scale transform inverts to non-finite coordinates; center() falls
        // back to the screen-space centre so placement stays finite (a NaN position
        // panics egui's layout) and on the canvas, not at a fixed off-screen origin.
        // Regression for the ribbon-add crash and the off-screen placement on a
        // fresh, not-yet-laid-out canvas.
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
        let view = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(0.0, 0.0), 0.0),
            rect,
        };
        assert!(view.center().is_finite());
        assert_eq!(view.center(), rect.center());
    }

    #[test]
    fn spawn_pos_anchors_on_the_visible_centre() {
        let view = CanvasView {
            to_global: egui::emath::TSTransform::IDENTITY,
            rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
        };
        // Near the visible centre (400, 300), not the fixed origin.
        let p = spawn_pos(Some(view), 0);
        assert!((p.x - (400.0 - 60.0)).abs() < 1e-3 && (p.y - (300.0 - 24.0)).abs() < 1e-3);
        // Without a view yet, falls back near the origin.
        let fallback = spawn_pos(None, 0);
        assert!(fallback.x < 100.0 && fallback.y < 100.0);
    }

    #[test]
    fn pane_kinds_are_registered_and_unique() {
        let l = default_layout();
        for id in [
            l.menu_bar,
            l.ribbon,
            l.params,
            l.preview_2d,
            l.canvas,
            l.viewport_3d,
        ] {
            assert!(pane_kind(id).is_some(), "pane {id:?} is not registered");
        }
        let mut ids: Vec<&str> = inventory::iter::<PaneKind>().map(|p| p.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate pane-kind id");
    }
}
