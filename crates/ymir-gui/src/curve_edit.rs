//! The curve editor widget (GUI step A2): a visual transfer-curve graph.
//!
//! Drag a control point to move it (the two endpoints are pinned to the domain
//! edges, x = 0 and x = 1, so only their height moves); click an empty spot to add
//! a point; right-click a point to remove it (keeping at least two). The graph maps
//! the unit square to the widget rect (y up), so what you draw is the transfer
//! function a shaping node applies. This is what makes levels/curve shaping
//! controllable, instead of opaque sliders.

use eframe::egui;
use ymir_core::Curve;

/// Widget height in points.
const HEIGHT: f32 = 160.0;
/// Maximum widget width in points (the inspector panel is narrow).
const MAX_WIDTH: f32 = 260.0;
/// Drawn radius of a control point.
const HANDLE_RADIUS: f32 = 4.0;
/// Pointer grab radius around a control point.
const GRAB_RADIUS: f32 = 8.0;

/// Maps a unit-square point `(0..1, 0..1)` to a screen position in `rect`, with `y`
/// pointing up (so `y = 1` is the top).
fn screen_from_unit(p: (f32, f32), rect: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        rect.left() + p.0 * rect.width(),
        rect.bottom() - p.1 * rect.height(),
    )
}

/// Inverse of [`screen_from_unit`]: maps a screen position to a unit-square point,
/// clamped to `[0, 1]`.
fn unit_from_screen(pos: egui::Pos2, rect: egui::Rect) -> (f32, f32) {
    (
        ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0),
    )
}

/// Renders the editable curve and returns the edited curve when it changed this
/// frame. `histogram` (normalized bin heights over the `[0, 1]` domain) is drawn faintly
/// behind the curve, so the transfer function can be shaped against where the input data
/// actually sits (#15).
pub(crate) fn curve_editor(
    ui: &mut egui::Ui,
    curve: &Curve,
    histogram: Option<&[f32]>,
) -> Option<Curve> {
    let mut points: Vec<(f32, f32)> = curve.points().to_vec();
    if points.len() < 2 {
        points = Curve::identity().points().to_vec();
    }

    let width = ui.available_width().min(MAX_WIDTH);
    let (rect, bg) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::click());

    let visuals = ui.visuals();
    let radius = egui::CornerRadius::same(2);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, radius, visuals.extreme_bg_color);
    painter.rect_stroke(
        rect,
        radius,
        visuals.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    // The input histogram, drawn faintly behind everything so the curve can be shaped
    // against where the data sits. Bins span the [0, 1] domain left to right; a spike at
    // the right edge means data above 1 (clamped in), i.e. normalize first.
    if let Some(bins) = histogram.filter(|h| !h.is_empty()) {
        let bar_color = visuals.weak_text_color().gamma_multiply(0.5);
        let bar_w = rect.width() / bins.len() as f32;
        for (i, &h) in bins.iter().enumerate() {
            if h <= 0.0 {
                continue;
            }
            let x0 = rect.left() + i as f32 * bar_w;
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, rect.bottom() - h * rect.height()),
                egui::pos2(x0 + bar_w, rect.bottom()),
            );
            painter.rect_filled(bar, 0, bar_color);
        }
    }
    // Quarter grid.
    let grid = visuals.widgets.noninteractive.bg_stroke;
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = rect.left() + t * rect.width();
        let y = rect.bottom() - t * rect.height();
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            grid,
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            grid,
        );
    }

    let mut changed = false;
    let mut to_delete = None;
    let n = points.len();

    // Per-point handles, allocated after the background so they sit on top and take
    // their own clicks/drags.
    for i in 0..n {
        let center = screen_from_unit(points[i], rect);
        let resp = ui.interact(
            egui::Rect::from_center_size(center, egui::Vec2::splat(GRAB_RADIUS * 2.0)),
            bg.id.with(i),
            egui::Sense::click_and_drag(),
        );
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let (nx, ny) = unit_from_screen(pos, rect);
            // The two endpoints are pinned to the domain edges (x = 0 and x = 1), so
            // only their height moves and the curve always spans the full width.
            // Interior points move in x too, clamped between their neighbours so the
            // order (and thus the handle indices) stays stable through a drag.
            let x = if i == 0 {
                0.0
            } else if i == n - 1 {
                1.0
            } else {
                nx.clamp(points[i - 1].0, points[i + 1].0)
            };
            points[i] = (x, ny);
            changed = true;
        }
        if resp.secondary_clicked() && n > 2 {
            to_delete = Some(i);
        }
    }

    if let Some(i) = to_delete {
        points.remove(i);
        changed = true;
    } else if bg.clicked()
        && let Some(pos) = bg.interact_pointer_pos()
    {
        points.push(unit_from_screen(pos, rect));
        changed = true;
    }

    points.sort_by(|a, b| a.0.total_cmp(&b.0));

    // The smooth (monotone cubic) curve, sampled across the width so the graph
    // matches exactly what the node applies. Endpoints are pinned to x = 0 and
    // x = 1, so it spans the full width.
    let curve_stroke = egui::Stroke::new(1.5, visuals.text_color());
    let drawn = Curve::new(points.clone());
    let sample = drawn.sampler();
    let steps = 64;
    let mut prev = screen_from_unit((0.0, sample(0.0)), rect);
    for s in 1..=steps {
        let x = s as f32 / steps as f32;
        let next = screen_from_unit((x, sample(x)), rect);
        painter.line_segment([prev, next], curve_stroke);
        prev = next;
    }

    // Handles on top of the curve.
    for &p in &points {
        let center = screen_from_unit(p, rect);
        painter.circle_filled(center, HANDLE_RADIUS, visuals.text_color());
        painter.circle_stroke(
            center,
            HANDLE_RADIUS,
            egui::Stroke::new(1.0, visuals.extreme_bg_color),
        );
    }

    changed.then(|| Curve::new(points))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_and_screen_round_trip_with_y_up() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 200.0));
        // Bottom-left of the rect is unit (0, 0); top-right is (1, 1).
        assert_eq!(unit_from_screen(egui::pos2(10.0, 220.0), rect), (0.0, 0.0));
        assert_eq!(unit_from_screen(egui::pos2(110.0, 20.0), rect), (1.0, 1.0));
        // Round trip a midpoint.
        let p = (0.25, 0.75);
        let back = unit_from_screen(screen_from_unit(p, rect), rect);
        assert!((back.0 - p.0).abs() < 1e-6 && (back.1 - p.1).abs() < 1e-6);
    }

    #[test]
    fn screen_maps_y_upward() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        // y = 1 maps to the top (smaller screen y) than y = 0.
        assert!(screen_from_unit((0.0, 1.0), rect).y < screen_from_unit((0.0, 0.0), rect).y);
    }
}
