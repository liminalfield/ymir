//! The main viewport's 2D map mode (#134): the previewed field drawn flat and large,
//! with pan and zoom, for judging data maps (flow, wetness, masks) at a size the small
//! preview pane cannot afford.
//!
//! It shades the same field the 3D view meshes (build-quality when a Build is loaded,
//! else the live preview), on the GPU (see [`crate::viewport2d_gpu`]), so 2D and 3D show
//! the same data and differ only in projection. The field is uploaded only when it
//! changes; light, mode, scale, and water are uniforms, so steering the sun re-shades a
//! resident texture rather than recomputing the whole field on the CPU each frame (#167).
//! Panning and zooming reuse the shaded texture and cost nothing.

use eframe::egui;
use eframe::egui_wgpu;
use ymir_core::Field;

use crate::shade::{DEFAULT_LIGHT, HeightScale, ShadeMode};
use crate::viewport2d_gpu::{Gpu2d, ShadeParams};

/// What the caller asks the 2D map to draw: which output, the Auto/Fixed scale, and the water
/// overlay (sea level plus whether it is shown). The shading mode and light are the view's own
/// state. Bundled so [`View2d::show`] takes one parameter for these, not four.
pub(crate) struct MapDisplay {
    /// Which tapped output is shown.
    pub output: usize,
    /// The shared Auto/Fixed Height scale.
    pub scale: HeightScale,
    /// Sea level as a raw layer value.
    pub sea_level: f32,
    /// Whether to draw the water overlay.
    pub show_water: bool,
}

/// Which projection the main viewport draws.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    /// The 3D meshed relief (the original viewport).
    #[default]
    ThreeD,
    /// A flat 2D image of the field, pannable and zoomable.
    TwoD,
}

/// How fast the scroll wheel zooms: `smooth_scroll_delta.y` is in points, so a small
/// coefficient turns a wheel notch (~50 points) into a gentle ~8% zoom step.
const ZOOM_SPEED: f32 = 0.0015;
/// Zoom bounds over the fit-to-pane scale, so the map can neither shrink to a speck nor
/// blow up unboundedly.
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 64.0;

/// The 2D view's own state: the GPU shading resources (created lazily once the wgpu render state is
/// available), the relief light, and the pan/zoom transform.
///
/// `zoom` is a multiplier over the fit-to-pane scale (`1.0` = the whole map fits), and
/// `pan` is the screen-space offset of the image centre from the pane centre, in points.
/// Both reset to fit on a double-click. `light` is this view's own relief sun (independent of the
/// preview pane and the 3D light), ephemeral like the camera and not persisted.
pub(crate) struct View2d {
    gpu: Option<Gpu2d>,
    mode: ShadeMode,
    light: [f32; 3],
    zoom: f32,
    pan: egui::Vec2,
}

impl Default for View2d {
    fn default() -> Self {
        Self {
            gpu: None,
            mode: ShadeMode::Height,
            light: DEFAULT_LIGHT,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        }
    }
}

impl View2d {
    /// The current shading mode, for the HUD's Height/Relief toggle.
    pub(crate) fn shade_mode(&self) -> ShadeMode {
        self.mode
    }

    /// Sets the shading mode; the texture rebuilds on the next `show` if it changed.
    pub(crate) fn set_shade_mode(&mut self, mode: ShadeMode) {
        self.mode = mode;
    }

    /// This view's relief light as a unit image-space vector; the flyout's 2D-sun sliders read and
    /// write it through [`crate::sun::light_angles`] / [`crate::sun::light_from_angles`], now that
    /// the map steers its light from the Display flyout rather than an on-map dial.
    pub(crate) fn relief_light(&self) -> [f32; 3] {
        self.light
    }

    /// Sets the relief light; the texture rebuilds on the next `show` if it changed.
    pub(crate) fn set_relief_light(&mut self, light: [f32; 3]) {
        self.light = light;
    }

    /// Resets to fit-to-view (the whole map centred in the pane).
    pub(crate) fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
    }

    /// Draws the field flat over the pane, handling pan (drag), zoom (scroll about the
    /// cursor), and reset (double-click). `field` is the field the 3D view would mesh; `output`
    /// names which output it is; `scale` is the shared Auto/Fixed Height scale; `sea_level`/
    /// `show_water` mirror the World settings to draw the same water overlay the 3D plane shows.
    /// The field is shaded on the GPU via `render_state`; a black fill stands in when there is no
    /// field (or no GPU, in a headless build).
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        render_state: Option<&egui_wgpu::RenderState>,
        field: Option<&Field>,
        display: MapDisplay,
    ) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        if response.double_clicked() {
            self.reset_view();
        }
        if response.dragged() {
            self.pan += response.drag_delta();
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && response.hovered()
            && let Some(cursor) = response.hover_pos()
        {
            self.zoom_about(cursor, rect.center(), scroll);
        }

        // Shade the field on the GPU (a no-op re-shade when nothing but pan/zoom changed) and take
        // its egui texture id and pixel size, or nothing when there is no field or no GPU backend.
        let params = ShadeParams {
            output: display.output,
            mode: self.mode,
            scale: display.scale,
            light: self.light,
            sea_level: display.sea_level,
            show_water: display.show_water,
        };
        let shaded = match (render_state, field) {
            (Some(rs), Some(field)) if field.width() > 0 && field.height() > 0 => {
                let id = self
                    .gpu
                    .get_or_insert_with(|| Gpu2d::new(rs))
                    .shade(rs, field, params);
                Some((id, egui::vec2(field.width() as f32, field.height() as f32)))
            }
            _ => None,
        };

        // Clip to the pane so a panned or zoomed image never spills over the HUD or
        // neighbouring panes.
        let painter = ui.painter_at(rect);
        match shaded {
            Some((id, size)) => {
                let fit = fit_scale(size, rect.size());
                let draw = size * (fit * self.zoom);
                let image_rect = egui::Rect::from_center_size(rect.center() + self.pan, draw);
                painter.image(
                    id,
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
            }
        }
    }

    /// Zooms toward/away so the map point under `cursor` stays fixed: the offset of the
    /// image centre from the cursor scales by the same factor as the zoom, keeping what is
    /// under the pointer put.
    fn zoom_about(&mut self, cursor: egui::Pos2, pane_center: egui::Pos2, scroll: f32) {
        let new_zoom = (self.zoom * (scroll * ZOOM_SPEED).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
        let applied = new_zoom / self.zoom;
        let old_center = pane_center + self.pan;
        let new_center = cursor - (cursor - old_center) * applied;
        self.pan = new_center - pane_center;
        self.zoom = new_zoom;
    }
}

/// The scale that fits an image of size `img` inside a pane of size `pane` without
/// cropping (the smaller of the width and height ratios). Guards a zero-sized image.
fn fit_scale(img: egui::Vec2, pane: egui::Vec2) -> f32 {
    if img.x <= 0.0 || img.y <= 0.0 {
        return 1.0;
    }
    (pane.x / img.x).min(pane.y / img.y).max(f32::EPSILON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_scale_fits_within_pane() {
        // A 200x100 image in a 400x400 pane fits by width (the tighter ratio): 400/200 = 2.
        let s = fit_scale(egui::vec2(200.0, 100.0), egui::vec2(400.0, 400.0));
        assert!((s - 2.0).abs() < 1e-6);
        // Fitting never overflows either dimension.
        assert!(200.0 * s <= 400.0 + 1e-3 && 100.0 * s <= 400.0 + 1e-3);
    }

    #[test]
    fn fit_scale_guards_zero_size() {
        assert_eq!(
            fit_scale(egui::vec2(0.0, 0.0), egui::vec2(400.0, 400.0)),
            1.0
        );
    }

    #[test]
    fn reset_view_returns_to_fit() {
        let mut view = View2d {
            zoom: 4.0,
            pan: egui::vec2(50.0, -30.0),
            ..Default::default()
        };
        view.reset_view();
        assert_eq!(view.zoom, 1.0);
        assert_eq!(view.pan, egui::Vec2::ZERO);
    }

    #[test]
    fn zoom_about_keeps_cursor_point_fixed() {
        let mut view = View2d::default();
        let pane_center = egui::pos2(200.0, 200.0);
        let cursor = egui::pos2(260.0, 170.0);
        // The map point under the cursor, in image space relative to the image centre,
        // before zooming.
        let before = (cursor - (pane_center + view.pan)) / view.zoom;
        view.zoom_about(cursor, pane_center, 40.0);
        let after = (cursor - (pane_center + view.pan)) / view.zoom;
        // Same image point stays under the cursor after the zoom.
        assert!((before - after).length() < 1e-3);
        assert!(view.zoom > 1.0, "scrolling up zooms in");
    }

    #[test]
    fn zoom_is_clamped() {
        let mut view = View2d::default();
        let c = egui::pos2(0.0, 0.0);
        for _ in 0..1000 {
            view.zoom_about(c, c, 100.0);
        }
        assert!(view.zoom <= MAX_ZOOM);
        for _ in 0..1000 {
            view.zoom_about(c, c, -100.0);
        }
        assert!(view.zoom >= MIN_ZOOM);
    }
}
