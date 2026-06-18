//! Ymir's node editor and viewport.
//!
//! Step 4 (issue #4): the ribbon and palette. The ribbon's category tabs come
//! from the registered `CategoryDef`s, the per-tab node toolbar from the operator
//! registry filtered by `NodeSpec.category`, and a search field fuzzy-matches over
//! node names and tags — all generated from the registries, never a hand-kept
//! list, and labelled through `tr(key)`. Clicking a node adds it to the core
//! graph. The canvas and evaluator (steps 5 and 6) render and run that graph; for
//! now the canvas shows the node count and the 2D preview shows a placeholder.

use std::sync::Arc;

use eframe::egui;
use ymir_core::registry;
use ymir_core::{Field, Graph, Layer, Params, Region, layers};
use ymir_nodes::{CategoryDef, categories, find_category, tr};

// The reconciliation spike (issue #5): a headless proof that egui-snarl can be a
// pure view over the core graph, gating the real canvas in step 5. It is test-only
// (the deliverable is the confirmed policy, not shipped canvas code), so it does
// not enter the running binary.
#[cfg(test)]
mod spike;

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

/// The data panes draw from. Panes receive `&mut AppState`, never the app shell,
/// so they stay mount-agnostic.
struct AppState {
    /// The graph being composed. Rendered by the canvas (step 5) and evaluated
    /// (step 6); for now nodes are added but not yet wired or displayed.
    graph: Graph,
    /// The selected palette tab (defaults to the first category on first draw).
    active_tab: Option<ActiveTab>,
    /// The node-search query; when non-empty it overrides the active tab.
    search: String,
    /// A hardcoded field for the 2D preview until the evaluator is wired (step 6).
    field: Field,
    /// The 2D preview texture, uploaded once on first use.
    preview: Option<egui::TextureHandle>,
}

impl AppState {
    fn new() -> Self {
        Self {
            graph: Graph::new(),
            active_tab: None,
            search: String::new(),
            field: placeholder_field(),
            preview: None,
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

/// Instantiates `type_id` via the registry and adds it to `graph`. Returns the
/// new node's id, or `None` if the type is unregistered.
fn add_node(graph: &mut Graph, type_id: &str) -> Option<ymir_core::NodeId> {
    let operator = registry::make(type_id)?;
    Some(graph.add_op(operator, Params::default()))
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
            // Build runs the graph; wired in step 6.
            ui.add_enabled(false, egui::Button::new("Build"));
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
                add_node(&mut state.graph, entry.type_id);
            }
        }
    });
}
inventory::submit! { PaneKind { id: "ribbon", draw: ribbon_pane } }

fn params_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.heading("Parameters");
    ui.weak("(node / global parameter tabs — placeholder)");
}
inventory::submit! { PaneKind { id: "params", draw: params_pane } }

fn preview_2d_pane(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("2D preview");

    // Upload the preview texture once (disjoint field borrows: `field` is read
    // while `preview` is filled).
    let texture = {
        let field = &state.field;
        state.preview.get_or_insert_with(|| {
            ui.ctx().load_texture(
                "preview",
                field_to_image(field),
                egui::TextureOptions::LINEAR,
            )
        })
    };
    let width = ui.available_width();
    let sized = egui::load::SizedTexture::new(texture.id(), texture.size_vec2());
    ui.add(
        egui::Image::new(sized)
            .max_width(width)
            .maintain_aspect_ratio(true),
    );
}
inventory::submit! { PaneKind { id: "preview-2d", draw: preview_2d_pane } }

fn canvas_pane(ui: &mut egui::Ui, state: &mut AppState) {
    let count = state.graph.node_count();
    ui.centered_and_justified(|ui| {
        ui.weak(format!(
            "Node canvas — placeholder (step 5).  Graph: {count} node(s)"
        ));
    });
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

// ---- field -> preview helpers -----------------------------------------------

/// Maps a normalized height value to an 8-bit grayscale level, matching the PNG
/// export's mapping (clamp to `[0, 1]`, scale to `0..=255`, round).
fn gray8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Builds a grayscale image from a field's `height` layer for the 2D preview.
fn field_to_image(field: &Field) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let mut rgba = Vec::with_capacity(layer.len() * 4);
    for &value in layer.as_slice() {
        let g = gray8(value);
        rgba.extend_from_slice(&[g, g, g, 255]);
    }
    egui::ColorImage::from_rgba_unmultiplied([layer.width(), layer.height()], &rgba)
}

/// A hardcoded radial-dome field, standing in for graph output.
fn placeholder_field() -> Field {
    let size: usize = 256;
    let centre = (size - 1) as f32 / 2.0;
    let max_dist = (2.0 * centre * centre).sqrt();
    let dome = Layer::from_fn(size, size, |x, y| {
        let dx = x as f32 - centre;
        let dy = y as f32 - centre;
        1.0 - (dx * dx + dy * dy).sqrt() / max_dist
    });
    Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(dome))
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
        assert_eq!(ids, ["noise", "erosion", "output"]); // sort 0, 10, 90
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
    fn add_node_grows_the_graph_and_rejects_unknown() {
        let mut graph = Graph::new();
        assert!(add_node(&mut graph, "generator.fbm").is_some());
        assert_eq!(graph.node_count(), 1);
        assert!(add_node(&mut graph, "no.such.node").is_none());
        assert_eq!(graph.node_count(), 1);
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

    #[test]
    fn gray8_maps_and_clamps() {
        assert_eq!(gray8(0.0), 0);
        assert_eq!(gray8(1.0), 255);
        assert_eq!(gray8(-0.5), 0);
        assert_eq!(gray8(1.5), 255);
        assert_eq!(gray8(0.5), 128);
    }
}
