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
use ymir_core::{EvalRequest, Graph, NodeId, ParamValue, Region};
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
// Off-thread full-resolution Build (#7).
mod build;
use build::BuildRunner;

/// Resolution of the interactive 2D preview. Low for responsiveness; it is an
/// approximation of the target-resolution build, never equal to it (erosion is
/// resolution-dependent). The build resolution is decided later (step 6c).
const PREVIEW_RES: usize = 256;

fn main() -> eframe::Result {
    // The window icon: `with_icon` is honoured on X11/Windows/macOS. On Wayland it is
    // ignored (no runtime icon protocol); there the icon comes from a `.desktop` entry
    // matched by `app_id`, so set a stable one. Falls back to eframe's default if the
    // PNG can't be decoded (it is cosmetic, never worth failing startup over).
    let viewport = egui::ViewportBuilder::default().with_app_id("ymir");
    let viewport = match app_icon() {
        Some(icon) => viewport.with_icon(icon),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Ymir",
        options,
        Box::new(|cc| Ok(Box::new(YmirApp::new(cc)))),
    )
}

/// The application's window icon, decoded from the embedded PNG into RGBA. `None` if
/// the PNG can't be decoded (then eframe uses its default icon). winit scales this
/// single image for the titlebar/taskbar, so one size suffices.
fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../../../ymir-icon-512.png").as_slice();
    let Ok(mut reader) = png::Decoder::new(bytes).read_info() else {
        return None;
    };
    let mut buf = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return None;
    };
    buf.truncate(info.buffer_size());
    // eframe wants RGBA; the icon is RGB, so give every pixel an opaque alpha.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Some(egui::IconData {
        rgba,
        width: info.width,
        height: info.height,
    })
}

// ---- application state ------------------------------------------------------

/// Which palette tab is active: a registered category, or the synthesized
/// "Uncategorized" group for nodes whose category has no `CategoryDef`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Category(&'static str),
    Uncategorized,
}

/// Which tab of the Parameters pane is showing: the selected node's inspector, or the
/// global world/build settings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamTab {
    Node,
    World,
}

/// Build-resolution presets offered in the world settings: UE5 landscape sizes
/// (component-based, `N·63 + 1`) and plain powers of two (for texture maps). The
/// field itself accepts any custom value; these are just shortcuts.
const BUILD_RES_PRESETS: &[usize] = &[256, 505, 512, 1009, 1024, 2017, 2048, 4033, 4096, 8129];

/// A sanity cap on how many outputs one Build evaluates, so a stray graph can't spawn
/// an unbounded run. Exceeding it is reported, never silently truncated.
const MAX_BUILD_OUTPUTS: usize = 64;

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

    /// Maps a screen position into graph space, falling back to the visible centre
    /// if the transform is degenerate, so a placement is never a non-finite point
    /// (a NaN position panics egui's layout). Used to drop a node at the cursor.
    fn graph_pos_finite(&self, screen: egui::Pos2) -> egui::Pos2 {
        let mapped = self.graph_pos(screen);
        if mapped.is_finite() {
            mapped
        } else {
            self.center()
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
    /// inspector edits. Refreshed each frame from the canvas selection. Drives the
    /// preview only when no node is pinned (see `preview_pin`).
    selected: Option<Handle>,
    /// The node pinned as the preview target, if any (GUI view-state, not core graph
    /// data — issue #39). When set and still previewable, the 2D preview shows this
    /// node instead of the selection, so selection can move upstream to edit while the
    /// pinned downstream result keeps updating (the Houdini display-flag idea).
    preview_pin: Option<Handle>,
    /// The global seed for evaluation, set by the ribbon control. Reseeds the whole
    /// world; each node stays internally stable across edits.
    seed: u64,
    /// Background preview evaluation: submits graph snapshots and shows results
    /// without ever blocking the UI thread.
    preview: PreviewEngine,
    /// The off-thread full-resolution Build (#7).
    build: BuildRunner,
    /// The selected palette tab (defaults to the first category on first draw).
    active_tab: Option<ActiveTab>,
    /// The node-search query; when non-empty it overrides the active tab.
    search: String,
    /// The cursor-anchored node-creation menu (issue #51), open only while the user
    /// is picking a node. `None` when closed.
    node_menu: Option<NodeMenu>,
    /// The rename dialog (#61), open while the user edits a node's display name.
    /// `None` when closed.
    rename: Option<RenameDialog>,
    /// A one-shot "zoom to graph" transform to apply on the next frame (#65). The
    /// fit is computed from this frame's node rects (collected during rendering) and
    /// applied via the canvas's `current_transform` override next frame.
    pending_view: Option<egui::emath::TSTransform>,
    /// Which Parameters-pane tab is showing (the node inspector or world settings).
    param_tab: ParamTab,
    /// The resolution a full Build evaluates at (square). Custom, since UE5 landscapes
    /// need specific sizes.
    build_res: usize,
    /// The interactive 2D preview resolution (square); low for responsiveness.
    preview_res: usize,
}

/// The node-rename dialog (#61): edits a node's display-name override.
struct RenameDialog {
    /// The node being renamed (its `stable_id`).
    target: Handle,
    /// The editable name buffer, seeded from the current override.
    text: String,
    /// True only on the frame the dialog opened, so its field grabs focus once.
    just_opened: bool,
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
            build: BuildRunner::new(),
            active_tab: None,
            search: String::new(),
            node_menu: None,
            preview_pin: None,
            rename: None,
            pending_view: None,
            param_tab: ParamTab::Node,
            build_res: 1024,
            preview_res: PREVIEW_RES,
        }
    }

    /// Whether `handle` resolves to a live node that produces an output, so it can be
    /// previewed (an endpoint or a deleted node cannot).
    fn is_previewable(&self, handle: Handle) -> bool {
        self.graph
            .node_id_of(handle)
            .and_then(|id| self.graph.spec(id))
            .is_some_and(|spec| !spec.outputs.is_empty())
    }

    /// The node whose output the 2D preview shows: the pinned node when one is set
    /// and still previewable, otherwise the selected node. Decouples the preview
    /// target from selection (#39).
    fn preview_target(&self) -> Option<Handle> {
        self.preview_pin
            .filter(|&h| self.is_previewable(h))
            .or_else(|| self.selected.filter(|&h| self.is_previewable(h)))
    }
}

/// The cursor-anchored node-creation menu (issue #51): opened with Space over the
/// canvas, it drops the chosen node at the cursor. An empty search browses the
/// categories (drilling into one to list its nodes); a non-empty search shows flat
/// results across all categories.
struct NodeMenu {
    /// Where the menu opened, in screen space; the created node lands here.
    anchor: egui::Pos2,
    /// The canvas view captured at open time, giving the screen-to-graph mapping for
    /// placement (stable even if the canvas re-lays-out while the menu is open).
    view: CanvasView,
    /// The filter query; non-empty overrides category browsing with flat results.
    search: String,
    /// The category drilled into; `None` is the top-level category list.
    drilled: Option<&'static str>,
    /// The keyboard-highlighted row (index into the current row list). Arrow keys
    /// move it; Enter activates it. Clamped to the row list each frame.
    highlight: usize,
    /// Set when the search field should grab keyboard focus next frame (on open and
    /// after a drill, so typing keeps working without a click).
    focus_search: bool,
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

/// One selectable line in the node menu. A flat row list is the single model both
/// the mouse and the keyboard act on, so a highlight index and a click hit the same
/// rows. Each variant carries only `'static` ids, so the list outlives the borrow of
/// the menu state it was built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuRow {
    /// Returns from a drilled-in category to the category list.
    Back,
    /// A category to drill into, by id.
    Category(&'static str),
    /// A node to create, by `type_id`.
    Node(&'static str),
}

/// The rows the node menu shows, in display order. A non-empty search wins (flat
/// node results across all categories); otherwise a drilled-in category lists a
/// `Back` row then its nodes, and the top level lists the categories. Pure, so the
/// navigation logic is unit-tested apart from the egui drawing.
fn menu_rows(entries: &[NodeEntry], search: &str, drilled: Option<&'static str>) -> Vec<MenuRow> {
    if !search.trim().is_empty() {
        visible_nodes(entries, None, search)
            .iter()
            .map(|e| MenuRow::Node(e.type_id))
            .collect()
    } else if let Some(cat) = drilled {
        std::iter::once(MenuRow::Back)
            .chain(
                visible_nodes(entries, Some(ActiveTab::Category(cat)), "")
                    .iter()
                    .map(|e| MenuRow::Node(e.type_id)),
            )
            .collect()
    } else {
        categories_sorted()
            .iter()
            .map(|c| MenuRow::Category(c.id))
            .collect()
    }
}

/// The display label for a menu row. `>` / `<` are plain ASCII so they always render
/// (the triangle glyphs are absent from egui's default fonts).
fn menu_row_label(row: MenuRow) -> String {
    match row {
        MenuRow::Back => "< back".to_string(),
        MenuRow::Category(id) => format!("{}  >", tr(&format!("category-{id}"))),
        MenuRow::Node(type_id) => tr(&format!("node-{type_id}")).to_string(),
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
            // Build the selected outputs at the world-tab resolution, off-thread.
            state.build.poll(ui.ctx());
            let building = state.build.is_building();
            if ui
                .add_enabled(!building, egui::Button::new("Build"))
                .clicked()
            {
                let targets = included_endpoints(state);
                if targets.is_empty() {
                    state
                        .build
                        .report("No outputs selected to build.".to_string());
                } else if targets.len() > MAX_BUILD_OUTPUTS {
                    state.build.report(format!(
                        "Too many outputs ({}); the limit is {MAX_BUILD_OUTPUTS}.",
                        targets.len()
                    ));
                } else {
                    let res = state.build_res;
                    let request = EvalRequest::new(res, res, Region::UNIT, state.seed);
                    state.build.start(state.graph.clone(), targets, request);
                }
            }
            state.build.show(ui);
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
                if let Some(id) =
                    canvas::add_node(&mut state.graph, &mut state.snarl, entry.type_id, pos)
                {
                    // Select the new node so the inspector shows it immediately (#62).
                    state.selected = state.graph.stable_id(id);
                }
            }
        }
    });
}
inventory::submit! { PaneKind { id: "ribbon", draw: ribbon_pane } }

/// The display-name override for some edited text: `None` when the text is empty or
/// whitespace (revert to the type name), else the raw text. Shared by the inspector
/// Name field and the Rename dialog (#59, #61).
fn name_override(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// The `stable_id`s of the output endpoints a Build should write: nodes with no
/// outputs whose `build` flag is on (default on). Reads from the canvas's snarl (it
/// holds every node handle) since the core graph has no node iterator.
fn included_endpoints(state: &AppState) -> Vec<u64> {
    state
        .snarl
        .node_ids()
        .filter_map(|(_, &handle)| state.graph.node_id_of(handle).map(|id| (handle, id)))
        .filter(|(_, id)| state.graph.spec(*id).is_some_and(|s| s.outputs.is_empty()))
        .filter(|(_, id)| {
            state
                .graph
                .params(*id)
                .is_none_or(|p| p.get_bool("build", true))
        })
        .map(|(handle, _)| handle)
        .collect()
}

/// A node's display name: its per-instance override if set (#59), else its type's
/// name via `tr`. Mirrors the canvas title for the preview header.
fn node_display_name(graph: &Graph, id: NodeId) -> String {
    if let Some(name) = graph.name(id) {
        return name.to_string();
    }
    graph.spec(id).map_or_else(
        || "?".to_string(),
        |spec| tr(&format!("node-{}", spec.type_id)).to_string(),
    )
}

fn params_pane(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.param_tab, ParamTab::Node, "Node");
        ui.selectable_value(&mut state.param_tab, ParamTab::World, "World");
    });
    ui.separator();
    match state.param_tab {
        ParamTab::Node => node_inspector(ui, state),
        ParamTab::World => world_settings(ui, state),
    }
}
inventory::submit! { PaneKind { id: "params", draw: params_pane } }

/// The selected node's inspector: its display-name override and parameter widgets.
fn node_inspector(ui: &mut egui::Ui, state: &mut AppState) {
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

    // Display-name override (#59): edit at the top. The type name is the hint, so an
    // empty field shows what the node falls back to, and a weak line below always
    // shows the underlying type even when renamed.
    let type_name = tr(&format!("node-{}", spec.type_id)).to_string();
    let mut name = state.graph.name(id).unwrap_or("").to_string();
    ui.horizontal(|ui| {
        ui.label("Name");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text(type_name.as_str())
                .desired_width(f32::INFINITY),
        );
        if resp.changed()
            && let Err(err) = state.graph.set_name(id, name_override(&name))
        {
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    });
    ui.weak(type_name.as_str());
    ui.separator();

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

/// The world/build settings: the global eval-request inputs (seed, resolutions) that
/// apply to the whole graph. Outputs selection and the Build action land here in
/// later steps.
fn world_settings(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label("Seed");
        ui.add(egui::DragValue::new(&mut state.seed).speed(1.0));
    });

    ui.separator();
    ui.label("Build resolution");
    ui.horizontal(|ui| {
        // Custom value (UE5 landscapes need specific sizes), with presets as
        // shortcuts.
        ui.add(
            egui::DragValue::new(&mut state.build_res)
                .speed(8.0)
                .range(16..=8192),
        );
        egui::ComboBox::from_id_salt("build-res-presets")
            .selected_text("presets")
            .show_ui(ui, |ui| {
                for &preset in BUILD_RES_PRESETS {
                    if ui.selectable_label(false, preset.to_string()).clicked() {
                        state.build_res = preset;
                    }
                }
            });
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Preview resolution");
        ui.add(
            egui::DragValue::new(&mut state.preview_res)
                .speed(4.0)
                .range(32..=1024),
        );
    });

    ui.separator();
    ui.label("Outputs");
    ui.weak("Endpoints a Build will write; tick to include.");
    // Endpoints are nodes with no outputs. Collect them first (releasing the snarl
    // borrow) before mutating params below.
    let endpoints: Vec<NodeId> = state
        .snarl
        .node_ids()
        .filter_map(|(_, &handle)| state.graph.node_id_of(handle))
        .filter(|&id| state.graph.spec(id).is_some_and(|s| s.outputs.is_empty()))
        .collect();
    if endpoints.is_empty() {
        ui.weak("No output nodes in the graph.");
        return;
    }
    for id in endpoints {
        let mut params = state.graph.params(id).cloned().unwrap_or_default();
        let mut include = params.get_bool("build", true);
        let name = node_display_name(&state.graph, id);
        let path = params.get_str("path", "").to_string();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut include, name).changed() {
                params.insert("build".to_string(), ParamValue::Bool(include));
                if let Err(err) = state.graph.set_params(id, params) {
                    ui.colored_label(ui.visuals().error_fg_color, err.to_string());
                }
            }
            if !path.is_empty() {
                ui.weak(path);
            }
        });
    }
}

fn preview_2d_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // Drop a pin left pointing at a deleted node, so it never sticks the preview on
    // nothing.
    if state
        .preview_pin
        .is_some_and(|h| state.graph.node_id_of(h).is_none())
    {
        state.preview_pin = None;
    }

    // The preview shows the pinned node if one is set, else the selection. Only nodes
    // with an output qualify; evaluating an endpoint would run its side effect.
    let Some(target) = state.preview_target() else {
        if state.selected.is_some() {
            ui.weak("This node has no output to preview.");
        } else {
            ui.weak("Select a node to preview its output.");
        }
        return;
    };
    let Some(id) = state.graph.node_id_of(target) else {
        ui.weak("Select a node to preview its output.");
        return;
    };
    let is_pinned = state.preview_pin == Some(target);

    // Row 1: status dot (colour = up-to-date/evaluating/error, words on hover), the
    // previewed node's name so it is always clear what is shown, a pinned marker, and
    // the Pin/Unpin toggle right-aligned.
    let name = node_display_name(&state.graph, id);
    ui.horizontal(|ui| {
        let color = state.preview.status_color(ui.visuals());
        let d = ui.text_style_height(&egui::TextStyle::Body) * 0.6;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), d * 0.5, color);
        resp.on_hover_text(state.preview.status_label());

        ui.label(name);
        if is_pinned {
            ui.weak("· pinned");
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if is_pinned {
                if ui.button("Unpin").clicked() {
                    state.preview_pin = None;
                }
            } else if ui.button("Pin").clicked() {
                state.preview_pin = Some(target);
            }
        });
    });

    // Row 2: shading toggle (relief makes subtle height changes legible, #40) and, in
    // relief, the light dial inline.
    let mut mode = state.preview.mode();
    ui.horizontal(|ui| {
        // Reserve the dial's height in both modes so the image never jumps when
        // toggling Height/Relief.
        ui.set_min_height(preview::LIGHT_DIAL_SIZE);
        ui.selectable_value(&mut mode, preview::ShadeMode::Height, "Height");
        ui.selectable_value(&mut mode, preview::ShadeMode::Relief, "Relief");
        if mode == preview::ShadeMode::Relief {
            state.preview.light_indicator(ui);
        }
    });
    state.preview.set_mode(mode);

    // Submit a snapshot for off-thread evaluation if the output changed, collect any
    // result, and render — none of which blocks the UI thread.
    let res = state.preview_res;
    let request = EvalRequest::new(res, res, Region::UNIT, state.seed);
    let now = ui.input(|i| i.time);
    state.preview.sync(&state.graph, id, request, now);
    state.preview.poll(ui.ctx());
    state.preview.show(ui);
}
inventory::submit! { PaneKind { id: "preview-2d", draw: preview_2d_pane } }

/// Draws the cursor-anchored node-creation menu when open, and applies its outcome:
/// drilling into a category, creating a node at the cursor, or closing. A no-op when
/// the menu is closed.
fn node_menu_ui(ui: &mut egui::Ui, state: &mut AppState) {
    // Esc closes without creating. Checked before borrowing the menu so it can clear
    // `node_menu` outright.
    if state.node_menu.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.node_menu = None;
    }
    let Some(menu) = state.node_menu.as_mut() else {
        return;
    };

    let entries = node_entries();
    let rows = menu_rows(&entries, &menu.search, menu.drilled);

    // Keyboard navigation: Up/Down move the highlight (wrapping), Enter activates it,
    // Left/Right step back and forth through the category hierarchy. Read before the
    // draw so the highlight shown is this frame's. A single-line search field ignores
    // Up/Down, so they are free; Left/Right are used for navigation only when the
    // search is empty (otherwise they edit the query's caret as usual).
    let (up, down, left, right, enter) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::Enter),
        )
    });
    if rows.is_empty() {
        menu.highlight = 0;
    } else {
        let n = rows.len();
        if down {
            menu.highlight = (menu.highlight + 1) % n;
        }
        if up {
            menu.highlight = (menu.highlight + n - 1) % n;
        }
        menu.highlight = menu.highlight.min(n - 1);
    }
    // The activated row: Enter takes the highlight; a click overrides with its row.
    let mut activated = (enter && !rows.is_empty()).then_some(menu.highlight);
    // Left/Right step through the hierarchy when not editing a query: Right drills
    // into the highlighted category, Left returns via the Back row (always row 0 of a
    // drilled-in list). Both reuse the row-activation path below.
    if menu.search.trim().is_empty() {
        if right && matches!(rows.get(menu.highlight), Some(MenuRow::Category(_))) {
            activated = Some(menu.highlight);
        }
        if left && menu.drilled.is_some() {
            activated = Some(0);
        }
    }

    let area = egui::Area::new(egui::Id::new("ymir-node-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(menu.anchor)
        .constrain(true);
    // A fixed menu width: rows fill it through a justified layout, so nothing
    // queries `available_width`.
    const MENU_WIDTH: f32 = 180.0;
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            ui.set_width(MENU_WIDTH);
            // Taller rows make calmer, less-overshootable targets (#64).
            ui.spacing_mut().button_padding = egui::vec2(8.0, 6.0);

            // A bare search field: a hint, not a label, so it costs no row after the
            // first use and vanishes the moment you type.
            let edit = egui::TextEdit::singleline(&mut menu.search)
                .hint_text("filter nodes")
                .desired_width(f32::INFINITY);
            let resp = ui.add(edit);
            if menu.focus_search {
                resp.request_focus();
                menu.focus_search = false;
            }
            // Editing the query resets the highlight to the first result.
            if resp.changed() {
                menu.highlight = 0;
            }
            ui.separator();

            // The list sizes to its content each frame. No scroll area: a scroll
            // area must size its viewport from the previous frame's content, so it
            // lags (and, while egui is idle, sticks) when the list shrinks then grows
            // across a drill-in/back. The current lists are short enough to fit; a
            // scroll area is the deliberate addition for when a category can overflow
            // the screen.
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                // The blue selection is the only row highlight; suppress egui's grey
                // hover fill so it never competes with the blue when the content
                // changes under a stationary pointer (e.g. just after drilling into a
                // category). Hover still drives the blue highlight (below) on movement.
                let hovered_vis = &mut ui.visuals_mut().widgets.hovered;
                hovered_vis.weak_bg_fill = egui::Color32::TRANSPARENT;
                hovered_vis.bg_fill = egui::Color32::TRANSPARENT;
                if rows.is_empty() {
                    ui.weak("no matches");
                }
                // Pointer movement makes hover drive the single (blue) highlight, so it
                // follows the mouse instead of a separate grey hover fighting the
                // keyboard highlight. When the pointer is still, arrow keys own the
                // highlight without hover stomping it.
                let pointer_moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                let mut hovered = None;
                for (i, &row) in rows.iter().enumerate() {
                    let resp = menu_row(ui, &menu_row_label(row), i == menu.highlight);
                    if resp.hovered() {
                        hovered = Some(i);
                    }
                    if resp.clicked() {
                        activated = Some(i);
                    }
                }
                if pointer_moved
                    && let Some(h) = hovered
                    && h != menu.highlight
                {
                    menu.highlight = h;
                    // Redraw so the highlight lands on the hovered row promptly.
                    ui.ctx().request_repaint();
                }
            });
        });
    });

    // The placement view and anchor are Copy; read them out so the menu borrow can
    // end before the state is mutated below.
    let anchor = menu.anchor;
    let view = menu.view;
    let menu_rect = area_resp.response.rect;

    // A primary press outside the menu dismisses it.
    let clicked_outside = ui.input(|i| {
        i.pointer.primary_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !menu_rect.contains(p))
    });

    if clicked_outside {
        state.node_menu = None;
    } else if let Some(&row) = activated.and_then(|i| rows.get(i)) {
        match row {
            // Drilling keeps the menu open: reset the highlight (skipping the Back row
            // when entering a category) and refocus the search so typing continues.
            MenuRow::Back => {
                if let Some(menu) = state.node_menu.as_mut() {
                    menu.drilled = None;
                    menu.highlight = 0;
                    menu.focus_search = true;
                }
            }
            MenuRow::Category(id) => {
                if let Some(menu) = state.node_menu.as_mut() {
                    menu.drilled = Some(id);
                    menu.highlight = 1;
                    menu.focus_search = true;
                }
            }
            MenuRow::Node(type_id) => {
                let pos = view.graph_pos_finite(anchor);
                if let Some(id) = canvas::add_node(&mut state.graph, &mut state.snarl, type_id, pos)
                {
                    // Select the new node so the inspector shows it immediately (#62).
                    state.selected = state.graph.stable_id(id);
                }
                state.node_menu = None;
            }
        }
    }
}

/// A menu row: a borderless selectable button that fills the row width under the
/// caller's justified layout (so it never queries `available_width`). `selected`
/// draws the keyboard highlight.
fn menu_row(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    ui.add(egui::Button::selectable(selected, text))
}

/// The pan/zoom transform that frames every node within `canvas` (#65): the
/// graph-space union of the node rects, scaled (with a little padding, clamped to the
/// zoom bounds) and centred in the canvas. `None` for an empty or degenerate graph.
fn fit_view(
    node_rects: &[(Handle, egui::Rect)],
    canvas: egui::Rect,
    min_scale: f32,
    max_scale: f32,
) -> Option<egui::emath::TSTransform> {
    let mut bounds: Option<egui::Rect> = None;
    for (_, rect) in node_rects {
        bounds = Some(bounds.map_or(*rect, |b| b.union(*rect)));
    }
    let bounds = bounds?;
    if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return None;
    }

    // Fit the bounds in the canvas with a little breathing room, within the zoom
    // clamp, then translate so the bounds centre lands at the canvas centre.
    let margin = 0.85;
    let scale = ((canvas.width() / bounds.width()).min(canvas.height() / bounds.height()) * margin)
        .clamp(min_scale, max_scale);
    let translation = canvas.center().to_vec2() - scale * bounds.center().to_vec2();
    Some(egui::emath::TSTransform::new(translation, scale))
}

/// A fresh node-creation menu anchored at `anchor` (screen space), placing into
/// `view`. Shared by the Space gesture and the right-click "Add node" (#51, #60).
fn open_node_menu(anchor: egui::Pos2, view: CanvasView) -> NodeMenu {
    NodeMenu {
        anchor,
        view,
        search: String::new(),
        drilled: None,
        highlight: 0,
        focus_search: true,
    }
}

fn canvas_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // While the node menu is open it owns the pointer: skip canvas selection so a
    // click on a menu row does not also select or clear under it.
    let menu_open = state.node_menu.is_some();

    // The previewed node's status dot, drawn on the node the preview is showing (the
    // pinned node if any, else the selection). The pinned node also gets a ring
    // marker. Both computed before the disjoint borrow below, from read-only fields.
    let status = state
        .preview_target()
        .map(|h| (h, state.preview.status_color(ui.visuals())));
    let pinned = state.preview_pin.filter(|&h| state.is_previewable(h));
    // A one-shot "zoom to graph" view to apply this frame (#65), Copy, read before
    // the disjoint borrow.
    let pending_view = state.pending_view;

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
        pinned,
        add_node_at: None,
        select_after: None,
        rename_request: None,
        pin_request: None,
        pending_view,
        frame_all_request: false,
        zoom: None,
    };
    // The canvas's screen rect comes from the ui, not snarl's response: snarl
    // returns an unbounded `EVERYTHING` rect, so it cannot be used for hit-testing
    // or to locate the visible centre.
    let canvas_rect = ui.max_rect();
    // Wires default to ~1px in a muted colour, which is hard to read; thicken them
    // and tie their colour (and the pin fill, which wires inherit) to the active
    // widget foreground so they stand out. Width/colour become user settings later
    // (#57).
    let wire_color = ui.visuals().widgets.active.fg_stroke.color;
    let style = egui_snarl::ui::SnarlStyle {
        wire_width: Some(2.5),
        pin_fill: Some(wire_color),
        // Clamp zoom so the graph can't shrink to an unfindable speck (snarl's
        // default min is 0.2 = 5x out); "zoom to graph" handles seeing a big graph
        // whole (#65).
        min_scale: Some(canvas::MIN_SCALE),
        max_scale: Some(canvas::MAX_SCALE),
        ..egui_snarl::ui::SnarlStyle::new()
    };
    // Ports stack in snarl's top-down pin layout, so the gap between them is the
    // canvas ui's vertical item spacing (~3px by default, which reads as jammed).
    // Roughly double it for breathing room between ports and node rows (#58). Scoped
    // to this ui, so it only affects the snarl widget, not other panes or the menu.
    ui.spacing_mut().item_spacing.y = 6.0;

    // Plain scroll wheel zooms the canvas about the cursor (#36). snarl's egui Scene
    // would scroll-pan instead, so suppress its scroll-pan and hand the zoom to the
    // viewer's `current_transform`. Only when the pointer is over the canvas, so other
    // panes keep normal scroll-to-pan.
    viewer.zoom = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| canvas_rect.contains(*p))
        .and_then(|cursor| {
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            (scroll != egui::Vec2::ZERO).then(|| {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                // Match egui Scene's exponential zoom feel (scroll_zoom_speed 1/200).
                let factor = ((scroll.x + scroll.y) / 200.0).exp();
                (factor, cursor)
            })
        });

    SnarlWidget::new()
        .style(style)
        .id_salt("ymir-canvas")
        .show(snarl, &mut viewer, ui);

    // Capture the view now (Copy data, no borrows); stored on the state at the end,
    // once the graph/snarl/selected borrows are released.
    let view = CanvasView {
        to_global: viewer.to_global,
        rect: canvas_rect,
    };
    // A graph-space spot from a right-click "Add node", if any (#60).
    let add_node_at = viewer.add_node_at;
    // A node the viewer asks to select (e.g. a duplicate, #61).
    let select_after = viewer.select_after;
    // A node the viewer asks to rename (context-menu "Rename", #61).
    let rename_request = viewer.rename_request;
    // A preview-pin change the viewer requests (context-menu Pin/Unpin, #39).
    let pin_request = viewer.pin_request;
    // If "zoom to graph" was chosen, compute the fit from this frame's node rects to
    // apply next frame (#65). Done here, while the viewer (and its node rects) is
    // still borrowed.
    let frame_all_fit = viewer.frame_all_request.then(|| {
        fit_view(
            &viewer.node_rects,
            canvas_rect,
            canvas::MIN_SCALE,
            canvas::MAX_SCALE,
        )
    });

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
    if let Some(screen_pos) = click
        .filter(|_| !menu_open)
        .filter(|p| canvas_rect.contains(*p))
    {
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

    // The pending "zoom to graph" view was consumed by this frame's render; replace
    // it with a freshly requested fit (or clear it). One-shot, so it does not fight
    // subsequent pan/zoom (#65).
    state.pending_view = frame_all_fit.flatten();

    // Apply a viewer-requested selection (e.g. a duplicate) after the click-selection
    // above, so it wins (#61).
    if let Some(handle) = select_after {
        state.selected = Some(handle);
    }

    // Apply a viewer-requested preview-pin change (context-menu Pin/Unpin, #39).
    if let Some(new_pin) = pin_request {
        state.preview_pin = new_pin;
    }

    // Open the rename dialog for a node the context menu asked to rename (#61),
    // seeding its field with the node's current override (empty if none).
    if let Some(handle) = rename_request
        && state.rename.is_none()
    {
        let text = state
            .graph
            .node_id_of(handle)
            .and_then(|id| state.graph.name(id))
            .map(str::to_string)
            .unwrap_or_default();
        state.rename = Some(RenameDialog {
            target: handle,
            text,
            just_opened: true,
        });
    }

    // Right-click "Add node" (snarl graph menu) opens the node menu at the clicked
    // graph spot, mapped back to screen for the anchor (#60).
    if state.node_menu.is_none()
        && let Some(graph_pos) = add_node_at
        && let Some(view) = state.canvas_view
    {
        let anchor = view.to_global * graph_pos;
        if anchor.is_finite() {
            state.node_menu = Some(open_node_menu(anchor, view));
        }
    }

    // Space over the canvas opens the node-creation menu at the cursor (issue #51),
    // unless a text field already has focus (so a space typed elsewhere never opens
    // it). The view captured above gives the screen-to-graph mapping for placement.
    if state.node_menu.is_none() && !ui.ctx().egui_wants_keyboard_input() {
        let anchor = ui
            .input(|i| {
                i.key_pressed(egui::Key::Space)
                    .then(|| i.pointer.hover_pos())
                    .flatten()
            })
            .filter(|p| canvas_rect.contains(*p));
        if let (Some(anchor), Some(view)) = (anchor, state.canvas_view) {
            state.node_menu = Some(open_node_menu(anchor, view));
        }
    }
    node_menu_ui(ui, state);
    rename_dialog_ui(ui, state);
}
inventory::submit! { PaneKind { id: "canvas", draw: canvas_pane } }

/// Draws the node-rename dialog when open, and applies its result. A no-op when the
/// dialog is closed (#61).
fn rename_dialog_ui(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(dialog) = state.rename.as_mut() else {
        return;
    };

    let mut apply = false;
    let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));

    egui::Window::new("Rename node")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label("Display name (empty reverts to the type name):");
            let resp = ui.add(egui::TextEdit::singleline(&mut dialog.text).desired_width(220.0));
            if dialog.just_opened {
                resp.request_focus();
                dialog.just_opened = false;
            }
            // Enter in the field confirms.
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                apply = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if apply {
        // Read out the Copy/clone values so the dialog borrow ends before the graph
        // and the dialog state are mutated.
        let target = dialog.target;
        let name = name_override(&dialog.text);
        if let Some(id) = state.graph.node_id_of(target)
            && let Err(err) = state.graph.set_name(id, name)
        {
            // Unreachable (the node existed when the menu opened); surface, never swallow.
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
        state.rename = None;
    } else if close {
        state.rename = None;
    }
}

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
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Calmer, more deliberate menus (#63, #64). Two global dials: a touch more
        // animation so hover highlights glide and popups ease open/closed instead of
        // snapping, and more menu inner margin so text is not jammed against the
        // border (where the pointer's corner sits). Menu row height is set per-menu
        // (button_padding) so the ribbon's buttons are not bloated.
        cc.egui_ctx.global_style_mut(|style| {
            style.animation_time = 0.15;
            style.spacing.menu_margin = egui::Margin::same(8);
        });
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
        // sort 0, 10, 20, 30, 40, 90
        assert_eq!(
            ids,
            [
                "generator",
                "selector",
                "adjust",
                "combine",
                "geology",
                "output"
            ]
        );
    }

    #[test]
    fn nodes_filter_by_category() {
        let entries = node_entries();
        let generators = visible_nodes(&entries, Some(ActiveTab::Category("generator")), "");
        assert!(generators.iter().all(|e| e.category == "generator"));
        assert!(generators.iter().any(|e| e.type_id == "generator.fbm"));
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
    fn graph_pos_finite_falls_back_when_degenerate() {
        // The node menu places at an arbitrary cursor point; a degenerate transform
        // must not yield a non-finite position (which would panic egui's layout).
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let degenerate = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(0.0, 0.0), 0.0),
            rect,
        };
        let p = degenerate.graph_pos_finite(egui::pos2(123.0, 456.0));
        assert!(p.is_finite());
        assert_eq!(p, degenerate.center());
        // A valid transform maps the point straight through.
        let view = CanvasView {
            to_global: egui::emath::TSTransform::IDENTITY,
            rect,
        };
        assert_eq!(
            view.graph_pos_finite(egui::pos2(123.0, 456.0)),
            egui::pos2(123.0, 456.0)
        );
    }

    #[test]
    fn menu_rows_browse_categories_then_drill_in() {
        let entries = node_entries();
        // Empty search, no drill: the top-level category list, in palette order.
        assert_eq!(
            menu_rows(&entries, "", None),
            [
                MenuRow::Category("generator"),
                MenuRow::Category("selector"),
                MenuRow::Category("adjust"),
                MenuRow::Category("combine"),
                MenuRow::Category("geology"),
                MenuRow::Category("output"),
            ]
        );
        // Drilled into a category: a Back row, then that category's nodes only.
        let drilled = menu_rows(&entries, "", Some("adjust"));
        assert_eq!(drilled[0], MenuRow::Back);
        assert!(drilled.contains(&MenuRow::Node("modifier.curve")));
        assert!(drilled.contains(&MenuRow::Node("modifier.invert")));
        // No node row outside the drilled category.
        assert!(!drilled.contains(&MenuRow::Node("generator.fbm")));
    }

    #[test]
    fn menu_rows_search_is_flat_and_ignores_drill() {
        let entries = node_entries();
        // A non-empty search overrides drill state with flat node results across all
        // categories, and shows no Back row.
        let rows = menu_rows(&entries, "fbm", Some("adjust"));
        assert!(rows.contains(&MenuRow::Node("generator.fbm")));
        assert!(!rows.contains(&MenuRow::Back));
    }

    #[test]
    fn menu_row_labels_use_ascii_affordances() {
        // The flyout/back affordances are plain ASCII (the triangle glyphs are absent
        // from egui's default fonts and render as tofu).
        assert_eq!(menu_row_label(MenuRow::Back), "< back");
        assert!(menu_row_label(MenuRow::Category("adjust")).ends_with("  >"));
        assert_eq!(menu_row_label(MenuRow::Node("modifier.invert")), "Invert");
    }

    #[test]
    fn preview_target_prefers_a_valid_pin_then_selection() {
        let mut state = AppState::new();
        let pos = egui::Pos2::ZERO;
        let gen_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        let out_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "endpoint.export", pos).unwrap();
        let generator = state.graph.stable_id(gen_id).unwrap();
        let endpoint = state.graph.stable_id(out_id).unwrap();

        // Nothing selected or pinned: no target.
        assert_eq!(state.preview_target(), None);

        // A previewable selection is the target; an endpoint (no output) is not.
        state.selected = Some(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.selected = Some(endpoint);
        assert_eq!(state.preview_target(), None);

        // A valid pin wins over the selection.
        state.preview_pin = Some(generator);
        state.selected = Some(endpoint);
        assert_eq!(state.preview_target(), Some(generator));

        // An invalid pin (an endpoint, or a missing node) is ignored, falling back to
        // the selection.
        state.preview_pin = Some(endpoint);
        state.selected = Some(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.preview_pin = Some(99_999);
        assert_eq!(state.preview_target(), Some(generator));
    }

    #[test]
    fn fit_view_centres_and_scales_the_node_bounds() {
        // Two 100x100 nodes spanning a 300x300 bounds centred at (150, 150).
        let rects = [
            (
                1u64,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
            ),
            (
                2u64,
                egui::Rect::from_min_size(egui::pos2(200.0, 200.0), egui::vec2(100.0, 100.0)),
            ),
        ];
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 600.0));
        let t = fit_view(&rects, canvas, 0.1, 4.0).expect("fit");

        // scale = (600/300) * 0.85 margin = 1.7, within the clamp.
        assert!((t.scaling - 1.7).abs() < 1e-4);
        // The bounds centre maps to the canvas centre.
        let mapped = t * egui::pos2(150.0, 150.0);
        assert!((mapped - egui::pos2(300.0, 300.0)).length() < 1e-3);

        // The clamp caps the scale for a tiny graph.
        let tiny = [(
            1u64,
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
        )];
        assert_eq!(fit_view(&tiny, canvas, 0.1, 2.0).expect("fit").scaling, 2.0);

        // No nodes: nothing to frame.
        assert!(fit_view(&[], canvas, 0.1, 4.0).is_none());
    }

    #[test]
    fn name_override_is_none_when_blank_else_the_raw_text() {
        assert_eq!(name_override(""), None);
        assert_eq!(name_override("   "), None);
        assert_eq!(
            name_override("Base Terrain"),
            Some("Base Terrain".to_string())
        );
        // Non-blank text is stored raw (surrounding spaces preserved, not trimmed).
        assert_eq!(name_override("  Hi "), Some("  Hi ".to_string()));
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

#[cfg(test)]
mod icon_test {
    #[test]
    fn app_icon_decodes_to_rgba() {
        let icon = super::app_icon().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }
}
