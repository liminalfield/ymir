//! Ymir's node editor and viewport.
//!
//! Step 1 (issue #2): an egui + wgpu window with the menu bar and the default
//! fixed-panel layout, rendering a hardcoded `Field` in the 2D preview pane. No
//! interactivity yet — pixels on screen. The pane-kind registry, ribbon, canvas,
//! evaluation, and 3D viewport arrive in later steps; see `DESIGN.md`.

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

/// A hardcoded radial-dome field, standing in for graph output until the canvas
/// and evaluator are wired (steps 5 and 6).
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

struct YmirApp {
    field: Field,
    /// Lazily uploaded so the texture is created once, on the first frame.
    preview: Option<egui::TextureHandle>,
}

impl YmirApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            field: placeholder_field(),
            preview: None,
        }
    }
}

impl eframe::App for YmirApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu_bar").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for menu in ["File", "Edit", "View", "Graph", "Help"] {
                    ui.menu_button(menu, |ui| {
                        ui.weak("(empty)");
                    });
                }
            });
        });

        egui::Panel::top("ribbon").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Ribbon");
                ui.separator();
                ui.weak("node tabs · search · seed · resolution · Cook  (placeholder)");
            });
        });

        egui::Panel::right("right_column")
            .resizable(true)
            .default_size(300.0)
            .show_inside(ui, |ui| {
                ui.heading("Parameters");
                ui.weak("(parameter inspector — placeholder)");
                ui.separator();
                ui.heading("2D preview");

                // Upload the preview texture once (disjoint field borrows: `field`
                // is read while `preview` is filled).
                let texture = {
                    let field = &self.field;
                    self.preview.get_or_insert_with(|| {
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
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::Panel::right("viewport_3d")
                .resizable(true)
                .default_size(ui.available_width() * 0.4)
                .show_inside(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.weak("3D viewport — placeholder (step 7)");
                    });
                });
            ui.centered_and_justified(|ui| {
                ui.weak("Node canvas — placeholder (step 5)");
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
