//! Ymir's node editor and viewport.
//!
//! Step 2 (issue #3): the pane model. Each region of the UI is a self-contained
//! pane function drawing into a passed [`egui::Ui`] from the app state, with no
//! knowledge of where it is mounted. Pane kinds self-register by stable id (the
//! same `inventory` pattern the engine uses for operators and categories), and
//! the default layout is data naming which pane fills each slot, realised by a
//! fixed-panel backend.
//!
//! These two seams — mount-agnostic panes and a pane-kind registry — are the
//! whole v1 commitment to customizable workspaces later. The serializable
//! workspace tree and a swappable docking backend are deferred (see `DESIGN.md`).

use std::sync::Arc;

use eframe::egui;
use ymir_core::{Field, Layer, Region, layers};

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

/// The data panes draw from. Panes receive `&mut AppState`, never the app shell,
/// so they stay mount-agnostic.
struct AppState {
    /// A hardcoded field standing in for graph output until the canvas and
    /// evaluator are wired (steps 5 and 6).
    field: Field,
    /// The 2D preview texture, uploaded once on first use.
    preview: Option<egui::TextureHandle>,
}

impl AppState {
    fn new() -> Self {
        Self {
            field: placeholder_field(),
            preview: None,
        }
    }
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

fn ribbon_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.strong("Ribbon");
        ui.separator();
        ui.weak("category tabs · node search · Build  (placeholder)");
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

fn canvas_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.centered_and_justified(|ui| {
        ui.weak("Node canvas — placeholder (step 5)");
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
    fn default_layout_references_registered_panes() {
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
    }

    #[test]
    fn pane_kind_ids_are_unique_and_complete() {
        let mut ids: Vec<&str> = inventory::iter::<PaneKind>().map(|p| p.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate pane-kind id");
        assert!(
            total >= 6,
            "expected at least the six default panes, found {total}"
        );
    }

    #[test]
    fn pane_lookup_handles_unknown() {
        assert!(pane_kind("canvas").is_some());
        assert!(pane_kind("nope").is_none());
    }

    #[test]
    fn gray8_maps_and_clamps() {
        assert_eq!(gray8(0.0), 0);
        assert_eq!(gray8(1.0), 255);
        assert_eq!(gray8(-0.5), 0);
        assert_eq!(gray8(1.5), 255);
        assert_eq!(gray8(0.5), 128); // 0.5 * 255 = 127.5, rounds up
    }

    #[test]
    fn field_to_image_matches_field_size() {
        let image = field_to_image(&placeholder_field());
        assert_eq!(image.size, [256, 256]);
        assert_eq!(image.pixels.len(), 256 * 256);
    }
}
