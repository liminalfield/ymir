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
    /// Set while exploring the field around the world: how many worlds across the shown field is.
    ///
    /// The field is rendered at `world_extent * zoom`, so the world occupies the middle `1 / zoom` of
    /// it. `None` when not exploring, which is the ordinary map view where the image *is* the world.
    pub explore: Option<Explore>,
}

/// The state of a field view: how far out it is pulled, and where the world sits in it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Explore {
    /// How many worlds across the rendered field is. `1.0` would show exactly the world.
    pub zoom: f32,
    /// The world's current extent in metres, for the readout during a gesture. The view formats it;
    /// it never decides it.
    pub world_extent_m: f64,
}

impl Explore {
    /// The world's rectangle inside `image`, the drawn extent of the whole field.
    ///
    /// Centred, because the world is centred on the field (#366): the rendered field reaches half of
    /// `world_extent * zoom` either side of the pan, and the world reaches half of `world_extent`, so
    /// the world is the middle `1 / zoom` of the image on both axes.
    pub fn world_rect(self, image: egui::Rect) -> egui::Rect {
        let frac = (1.0 / self.zoom.max(1.0)).clamp(0.0, 1.0);
        egui::Rect::from_center_size(image.center(), image.size() * frac)
    }
}

/// What one frame of the map view produced for its caller.
///
/// Two unrelated outputs travelled together rather than as separate returns because both are already
/// known by the time the frame is drawn, and a second call to work either of them out would repeat
/// the hit testing.
#[derive(Default)]
pub(crate) struct MapResult {
    /// A brush sample, when paint mode is on and the primary button is down over the map.
    pub sample: Option<PaintSample>,
    /// Wheel travel to apply to the field pull-back, in points, when exploring. Zero otherwise: in
    /// the ordinary map view the wheel has already been spent on the image zoom.
    pub field_scroll: f32,
    /// How far the pattern was dragged this frame, as a fraction of the field view on each axis, to
    /// add to the explored node's pan. Applied every frame of the drag: the view is centred on the
    /// pan, so this is self-consistent and the pattern tracks the hand.
    ///
    /// A fraction rather than a distance, so the view never needs to know how wide the world is;
    /// turning it into metres is the caller's job, being the only place that knows.
    pub explore_pan: egui::Vec2,
    /// A finished resize, as a multiplier on the world extent, handed back once on release. `None` on
    /// every other frame, including every frame of the drag: writing it live would rescale the render
    /// under the handle.
    pub explore_resize: Option<f32>,
}

/// A paint sample from the 2D map while paint mode is active: a normalized `[0, 1]` position in the
/// field, and whether it begins a new stroke (the primary button was pressed this frame) or extends
/// the current one (held and dragging).
pub(crate) struct PaintSample {
    /// Normalized x in `[0, 1]`.
    pub x: f32,
    /// Normalized y in `[0, 1]`.
    pub y: f32,
    /// True on the frame the stroke began; false while dragging it.
    pub begin: bool,
}

/// The active brush, for the on-surface cursor drawn while painting: two rings (the brush radius and
/// its `radius * hardness` full-strength core) plus a small raise/lower mark. `Some` exactly when a
/// paint node is the target, so `is_some()` is "paint mode on".
#[derive(Clone, Copy)]
pub(crate) struct BrushCursor {
    /// Brush radius as a fraction of the region width (the stroke model's unit).
    pub radius: f32,
    /// Edge hardness in `[0, 1]`: the inner ring sits at `radius * hardness`.
    pub hardness: f32,
    /// True in Raise mode (mark `+`), false in Lower (`−`).
    pub raise: bool,
}

/// The dark-halo + light-core stroke pair that keeps the brush cursor legible over any terrain, light
/// or dark. Drawn dark-under-light so the ring reads on any background without relying on colour.
pub(crate) fn cursor_strokes() -> (egui::Stroke, egui::Stroke) {
    (
        egui::Stroke::new(2.6, egui::Color32::from_black_alpha(150)),
        egui::Stroke::new(1.2, egui::Color32::from_white_alpha(235)),
    )
}

/// Draws the small raise (`+`) / lower (`−`) mark just outside the top-right of a cursor of screen
/// radius `screen_r` centred at `center`. Shape-based (not colour), so it reads under red/green
/// colour vision.
pub(crate) fn draw_mode_badge(
    painter: &egui::Painter,
    center: egui::Pos2,
    screen_r: f32,
    raise: bool,
) {
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    let pos = center + egui::vec2(diag, -diag) * (screen_r + 7.0);
    let mark = if raise { "+" } else { "−" };
    let font = egui::FontId::proportional(15.0);
    painter.text(
        pos + egui::vec2(0.6, 0.6),
        egui::Align2::CENTER_CENTER,
        mark,
        font.clone(),
        egui::Color32::from_black_alpha(170),
    );
    painter.text(
        pos,
        egui::Align2::CENTER_CENTER,
        mark,
        font,
        egui::Color32::from_white_alpha(240),
    );
}

/// Which projection the main viewport draws.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
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
    /// A world-box gesture in flight, or `None`. View state: it exists only between press and
    /// release, and what it produces is handed back once, on release.
    box_drag: Option<BoxDrag>,
}

/// Which part of the world box a gesture grabbed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grab {
    /// The field itself: drags the pattern under a fixed viewfinder, which writes the node's pan.
    Pan,
    /// A corner of the world box: resizes the world, which writes the world extent.
    Resize,
}

/// A drag on the field view in flight.
///
/// The two gestures settle differently, and the difference is forced rather than chosen.
///
/// **A pan applies live.** The world box is fixed at the centre of the view and the pattern is
/// dragged under it, the way a map is dragged under a viewfinder. Writing the pan each frame is
/// self-consistent: the view is centred on the pan, so moving the pan by exactly what the pointer
/// travelled moves the content by exactly that much, and the pattern tracks the hand.
///
/// An earlier version moved the box instead and committed on release, which read as a snap: the box
/// returned to the centre and hauled the pattern along with it. The box cannot be moved off centre,
/// because the view is centred on the same pan the box is, so there was nowhere for it to go.
///
/// **A resize is held until release.** The field is rendered at `world_extent * zoom`, so writing the
/// extent mid-drag would rescale the render under a box whose screen size had not changed, and the
/// handle would run from the pointer. That is the shape that cost several rounds on the magnitude
/// ruler, where recomputing the overlay position every frame made ticking move the ruler, which moved
/// the column under a stationary cursor. Holding it is also what makes the readout necessary rather
/// than decorative: until release, the pending extent exists nowhere else.
struct BoxDrag {
    grab: Grab,
    /// Pointer position now.
    ///
    /// Where the gesture began is not kept, because neither gesture needs it: a resize measures the
    /// pointer against the box's settled centre, and a pan works from the frame's own delta, having
    /// already applied everything before it.
    current: egui::Pos2,
}

impl Default for View2d {
    fn default() -> Self {
        Self {
            gpu: None,
            mode: ShadeMode::Height,
            light: DEFAULT_LIGHT,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            box_drag: None,
        }
    }
}

/// Corner handle half-size in points, and the slack around a corner that counts as grabbing it.
///
/// The grab radius is larger than the drawn handle, so a corner is catchable without precision aim
/// while the mark itself stays small enough not to hide the terrain under it.
const HANDLE_R: f32 = 4.0;
const HANDLE_GRAB_R: f32 = 10.0;
/// The smallest the world box may be dragged to, in points. Below this the box is a dot and its
/// corners are indistinguishable, so the gesture has nothing left to aim at.
const MIN_BOX_PX: f32 = 12.0;

/// What a drag beginning at `pos` would do, given the world box at `world`.
///
/// A corner resizes; anywhere else pans, inside the box as well as outside it. Corners win where they
/// overlap, since the finer gesture should be the one that is hard to miss, and they are given more
/// slack than they are drawn with so a corner is catchable without precise aim.
///
/// The inside of the box is not special, which is the point of a fixed viewfinder: the box marks where
/// the world is and the pattern moves under it, so a drag through the middle has nothing to mean that
/// a drag at the edge does not.
fn grab_at(world: egui::Rect, pos: egui::Pos2) -> Grab {
    let corners = [
        world.left_top(),
        world.right_top(),
        world.left_bottom(),
        world.right_bottom(),
    ];
    if corners.iter().any(|c| (pos - *c).length() <= HANDLE_GRAB_R) {
        Grab::Resize
    } else {
        Grab::Pan
    }
}

/// The box a gesture would leave behind, given the settled box and the drag so far.
///
/// A resize is anchored at the **centre** and stays square, for two reasons that agree. World extent
/// is one number, so a box with independent sides would promise something the data cannot hold. And
/// since #366 the world is centred on the field, so growing the extent grows the terrain evenly about
/// its centre: a box growing from a corner would contradict what the terrain does.
///
/// The square half-size follows the axis the pointer moved furthest on, so a diagonal drag does the
/// obvious thing rather than averaging into something between the two.
fn dragged_box(settled: egui::Rect, drag: &BoxDrag, image: egui::Rect) -> egui::Rect {
    if drag.grab == Grab::Pan {
        // A pan is applied as it happens, so the box is already where it always is: the centre.
        return settled;
    }
    let centre = settled.center();
    let reach = (drag.current - centre).abs();
    let half = reach.x.max(reach.y).clamp(
        MIN_BOX_PX * 0.5,
        (image.width().min(image.height()) * 0.5).max(MIN_BOX_PX * 0.5),
    );
    egui::Rect::from_center_size(centre, egui::Vec2::splat(half * 2.0))
}

/// A length in metres, written the way it reads at the scale it is: kilometres once it is one, metres
/// below that. A 2.5 km world should not read as `2500`, and a 400 m one should not read as `0.4 km`.
fn format_extent(metres: f64) -> String {
    if metres >= 1000.0 {
        format!("{:.2} km", metres / 1000.0)
    } else {
        format!("{metres:.0} m")
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
        brush: Option<BrushCursor>,
    ) -> MapResult {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let paint_active = brush.is_some();

        // Double-click resets the view, except while painting (where it would be a stray dab).
        if response.double_clicked() && !paint_active {
            self.reset_view();
        }
        // Pan with the middle button always, or the primary button when not painting; painting takes
        // the primary drag on the map.
        let pan_primary = !paint_active && response.dragged_by(egui::PointerButton::Primary);
        if pan_primary || response.dragged_by(egui::PointerButton::Middle) {
            self.pan += response.drag_delta();
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        // Scroll means one thing at a time. In the ordinary map view it magnifies or shrinks the
        // image, which is what it has always done. While exploring it is reported back instead, for
        // the caller to pull the field further out or bring it in, and the image is held at fit: two
        // nested zooms in one view, both driven by the wheel, would be indistinguishable in the hand.
        let mut field_scroll = 0.0;
        if scroll != 0.0
            && response.hovered()
            && let Some(cursor) = response.hover_pos()
        {
            if display.explore.is_some() {
                field_scroll = scroll;
            } else {
                self.zoom_about(cursor, rect.center(), scroll);
            }
        }
        if display.explore.is_some() {
            // Held at fit, so the field fills the pane and the world box is the only thing that
            // changes size. A leftover pan or zoom from the map view would otherwise persist here.
            self.reset_view();
        }

        // The field's pixel size and the image transform, computed before shading so the paint
        // mapping and the image draw share one rect.
        let size = field
            .filter(|f| f.width() > 0 && f.height() > 0)
            .map(|f| egui::vec2(f.width() as f32, f.height() as f32));
        let image_rect = size.map(|s| {
            let fit = fit_scale(s, rect.size());
            egui::Rect::from_center_size(rect.center() + self.pan, s * (fit * self.zoom))
        });

        // The world box: settled, then adjusted by any gesture in flight. Both are wanted below, the
        // settled one to hit-test against and the pending one to draw and to measure.
        let settled_box = display
            .explore
            .zip(image_rect)
            .map(|(explore, ir)| explore.world_rect(ir));

        let mut explore_pan = egui::Vec2::ZERO;
        let mut explore_resize = None;
        if let (Some(settled), Some(ir)) = (settled_box, image_rect) {
            let hover = response.hover_pos();
            if self.box_drag.is_none()
                && let Some(pos) = hover
                && response.drag_started_by(egui::PointerButton::Primary)
            {
                self.box_drag = Some(BoxDrag {
                    grab: grab_at(settled, pos),
                    current: pos,
                });
            }
            if let Some(drag) = &mut self.box_drag {
                if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                    drag.current = pos;
                }
                let grab = drag.grab;
                // Escape abandons the gesture. Checked before the release, so a cancelled drag cannot
                // also commit on the same frame. A pan has nothing to abandon, having applied as it
                // went; the pattern is simply left where the hand put it.
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.box_drag = None;
                } else if grab == Grab::Pan {
                    // Live, and self-consistent: the view is centred on the pan, so moving the pan by
                    // what the pointer travelled moves the content by exactly that much and the
                    // pattern tracks the hand. Reported as a fraction of the field, negated because
                    // dragging the pattern one way looks further the other way, as dragging a map does.
                    explore_pan = -response.drag_delta() / ir.size();
                    if !ui.input(|i| i.pointer.primary_down()) {
                        self.box_drag = None;
                    }
                } else if !ui.input(|i| i.pointer.primary_down()) {
                    let pending = dragged_box(settled, drag, ir);
                    self.box_drag = None;
                    explore_resize = (settled.width() > 0.0)
                        .then(|| pending.width() / settled.width())
                        .filter(|s| (s - 1.0).abs() > f32::EPSILON);
                }
            }
            // The cursor, so a corner reads as something to pull before it is pulled, and the field as
            // something to drag.
            let showing = self
                .box_drag
                .as_ref()
                .map(|d| d.grab)
                .or_else(|| hover.map(|pos| grab_at(settled, pos)));
            match showing {
                Some(Grab::Resize) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe),
                Some(Grab::Pan) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grab),
                None => {}
            }
        }

        // A paint sample: the primary button held over the map, mapped to normalized coordinates.
        let sample = if paint_active
            && let Some(ir) = image_rect
            && response.is_pointer_button_down_on()
            && ui.input(|i| i.pointer.primary_down())
            && let Some(pos) = response.interact_pointer_pos()
        {
            Some(PaintSample {
                x: ((pos.x - ir.min.x) / ir.width()).clamp(0.0, 1.0),
                y: ((pos.y - ir.min.y) / ir.height()).clamp(0.0, 1.0),
                begin: ui.input(|i| i.pointer.primary_pressed()),
            })
        } else {
            None
        };

        // Shade the field on the GPU (a no-op re-shade when nothing but pan/zoom changed), then draw
        // it at the image transform, clipped to the pane; a black fill stands in with no field or GPU.
        let params = ShadeParams {
            output: display.output,
            mode: self.mode,
            scale: display.scale,
            light: self.light,
            sea_level: display.sea_level,
            show_water: display.show_water,
        };
        let shaded = match (render_state, field) {
            (Some(rs), Some(field)) if field.width() > 0 && field.height() > 0 => Some(
                self.gpu
                    .get_or_insert_with(|| Gpu2d::new(rs))
                    .shade(rs, field, params),
            ),
            _ => None,
        };
        let painter = ui.painter_at(rect);
        match (shaded, image_rect) {
            (Some(id), Some(ir)) => painter.image(
                id,
                ir,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            ),
            _ => painter.rect_filled(rect, 0.0, egui::Color32::BLACK),
        };

        // The brush cursor: rings sized to the brush (and its hardness core) plus the raise/lower
        // mark, with the OS pointer hidden so only the ring shows where the stroke lands. The 2D map is
        // flat, so the rings are plain circles scaled by the field's on-screen width.
        if let Some(brush) = brush
            && let Some(ir) = image_rect
            && let Some(pos) = ui.ctx().pointer_latest_pos()
            && rect.contains(pos)
            // Only when the map is the top layer here, so a dialog over it keeps its own pointer.
            && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            let r = brush.radius * ir.width();
            let (dark, light) = cursor_strokes();
            painter.circle_stroke(pos, r, dark);
            painter.circle_stroke(pos, r, light);
            if brush.hardness > 0.02 {
                painter.circle_stroke(pos, r * brush.hardness, dark);
                painter.circle_stroke(pos, r * brush.hardness, light);
            }
            draw_mode_badge(&painter, pos, r, brush.raise);
        }

        // The world's outline on the field, at its pending position and size while a gesture is in
        // flight. Drawn with the brush cursor's dark-under-light stroke pair for the reason documented
        // there: it reads over any terrain without relying on colour, which a box over both bright
        // snow and dark water needs.
        if let (Some(explore), Some(ir), Some(settled)) = (display.explore, image_rect, settled_box)
        {
            let (dark, light) = cursor_strokes();
            let world = match &self.box_drag {
                Some(drag) => dragged_box(settled, drag, ir),
                None => settled,
            };
            painter.rect_stroke(world, 0.0, dark, egui::StrokeKind::Middle);
            painter.rect_stroke(world, 0.0, light, egui::StrokeKind::Middle);

            // Corner handles, so the box reads as an object with something to pull rather than an
            // annotation. Filled, since an outline on an outline is hard to see at this size.
            for corner in [
                world.left_top(),
                world.right_top(),
                world.left_bottom(),
                world.right_bottom(),
            ] {
                let mark = egui::Rect::from_center_size(corner, egui::Vec2::splat(HANDLE_R * 2.0));
                painter.rect_filled(mark, 0.0, egui::Color32::from_black_alpha(150));
                painter.rect_filled(mark.shrink(1.0), 0.0, egui::Color32::from_white_alpha(235));
            }

            // The pending extent, in the units it reads at. Only while dragging: this is the one place
            // the pending value exists, since nothing is written until release, and showing it the
            // rest of the time would duplicate the World panel to no purpose.
            if let Some(drag) = &self.box_drag {
                let pending = dragged_box(settled, drag, ir);
                let scale = if settled.width() > 0.0 {
                    f64::from(pending.width() / settled.width())
                } else {
                    1.0
                };
                let text = format_extent(explore.world_extent_m * scale);
                let at = egui::pos2(world.center().x, world.top() - 6.0);
                // Same dark-under-light reasoning as the outline, as a halo behind the glyphs.
                for (offset, colour) in [
                    (egui::vec2(1.0, 1.0), egui::Color32::from_black_alpha(180)),
                    (egui::Vec2::ZERO, egui::Color32::from_white_alpha(240)),
                ] {
                    painter.text(
                        at + offset,
                        egui::Align2::CENTER_BOTTOM,
                        &text,
                        egui::FontId::monospace(12.0),
                        colour,
                    );
                }
            }
        }

        MapResult {
            sample,
            field_scroll,
            explore_pan,
            explore_resize,
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

    /// A field view at `zoom`. The world extent only feeds the readout, so it is arbitrary here.
    fn at_zoom(zoom: f32) -> Explore {
        Explore {
            zoom,
            world_extent_m: 1024.0,
        }
    }

    #[test]
    fn the_world_box_is_the_middle_fraction_of_the_field() {
        let image = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 400.0));

        // Four worlds across: the world is a quarter of the view on each axis, centred, because the
        // world is centred on the field (#366). Not anchored at a corner, which is what it would be
        // if the field still started at the world's origin.
        let quarter = at_zoom(4.0).world_rect(image);
        assert_eq!(quarter.size(), egui::vec2(100.0, 100.0));
        assert_eq!(quarter.center(), image.center());

        // One world across is the world itself, which is the ordinary map view.
        let all = at_zoom(1.0).world_rect(image);
        assert_eq!(all, image);

        // A zoom below one would ask for a box larger than the field, which cannot be shown: the
        // world cannot exceed the view it is drawn inside.
        let clamped = at_zoom(0.25).world_rect(image);
        assert_eq!(clamped, image);
    }

    #[test]
    fn the_world_box_shrinks_as_the_field_pulls_back() {
        let image = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(256.0, 256.0));
        let mut last = f32::INFINITY;
        for zoom in [1.0, 2.0, 4.0, 8.0, 32.0] {
            let w = at_zoom(zoom).world_rect(image).width();
            assert!(
                w < last || zoom == 1.0,
                "zoom {zoom} did not shrink the box"
            );
            assert!(w > 0.0, "zoom {zoom} shrank the box to nothing");
            last = w;
        }
    }

    fn drag(grab: Grab, to: egui::Pos2) -> BoxDrag {
        BoxDrag { grab, current: to }
    }

    #[test]
    fn a_corner_resizes_about_the_centre_and_stays_square() {
        let image = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0));
        let settled = at_zoom(4.0).world_rect(image);
        assert_eq!(settled.size(), egui::vec2(100.0, 100.0));

        // Pull the bottom-right corner out to 80 points from the centre on x, 60 on y.
        let centre = settled.center();
        let pulled = dragged_box(
            settled,
            &drag(Grab::Resize, centre + egui::vec2(80.0, 60.0)),
            image,
        );
        // Square, from the axis that moved furthest: a diagonal drag does the obvious thing rather
        // than averaging into something between the two.
        assert_eq!(pulled.size(), egui::vec2(160.0, 160.0));
        // And the centre held, because that is what changing the world extent does to the terrain
        // (#366). A corner-anchored resize would have moved it.
        assert_eq!(pulled.center(), centre);
    }

    #[test]
    fn a_resize_cannot_pass_the_field_or_collapse_to_a_dot() {
        let image = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0));
        let settled = at_zoom(4.0).world_rect(image);
        let centre = settled.center();

        // Far outside the view: held at the field, since a world larger than the field could not be
        // seen and the pull-back would have to fight the gesture to show it.
        let huge = dragged_box(
            settled,
            &drag(Grab::Resize, centre + egui::vec2(9_000.0, 9_000.0)),
            image,
        );
        assert_eq!(huge.width(), 400.0);

        // Dragged onto the centre: held at the floor, or the box would be a dot with no corners left
        // to aim at and the gesture could not be walked back.
        let tiny = dragged_box(settled, &drag(Grab::Resize, centre), image);
        assert_eq!(tiny.width(), MIN_BOX_PX);
    }

    #[test]
    fn a_corner_resizes_and_everywhere_else_pans() {
        let image = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0));
        let world = at_zoom(4.0).world_rect(image);

        // Corners win where they overlap the field, so the finer gesture is the one hard to miss.
        assert_eq!(grab_at(world, world.left_top()), Grab::Resize);
        assert_eq!(grab_at(world, world.right_bottom()), Grab::Resize);
        // Just inside a corner, still within its slack, which is wider than the mark is drawn.
        assert_eq!(
            grab_at(world, world.left_top() + egui::vec2(3.0, 3.0)),
            Grab::Resize
        );

        // Everywhere else pans, and the inside of the box is not special: the box marks where the
        // world is and the pattern moves under it, so a drag through the middle has nothing to mean
        // that a drag outside does not.
        assert_eq!(grab_at(world, world.center()), Grab::Pan);
        assert_eq!(grab_at(world, image.left_top()), Grab::Pan);
    }

    #[test]
    fn a_pan_leaves_the_box_where_it_was() {
        // The box cannot be moved off centre, because the view is centred on the same pan the box is.
        // A pan moves the pattern under it instead, so the box a pan leaves behind is the settled one.
        let image = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0));
        let settled = at_zoom(4.0).world_rect(image);
        let panned = dragged_box(settled, &drag(Grab::Pan, egui::pos2(320.0, 90.0)), image);
        assert_eq!(panned, settled);
    }

    #[test]
    fn an_extent_reads_in_the_units_it_is() {
        // A 2.5 km world should not read as 2500, and a 400 m one should not read as 0.4 km.
        assert_eq!(format_extent(2500.0), "2.50 km");
        assert_eq!(format_extent(1000.0), "1.00 km");
        assert_eq!(format_extent(999.0), "999 m");
        assert_eq!(format_extent(400.0), "400 m");
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
