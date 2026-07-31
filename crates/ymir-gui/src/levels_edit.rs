//! The Levels editor widget (#369): the input window, the midtone bend and the output window
//! drawn as one picture.
//!
//! Levels is a relationship between five numbers, not five independent values, and rendered as
//! five sliders that relationship has to be held in the reader's head. Here the input
//! distribution runs along the bottom, the transfer curve crosses the plot, and each window
//! bound is a line on the axis it belongs to, so the window, the bend and the placement are one
//! thing to look at.
//!
//! The curve is drawn by calling the same [`LevelsTransfer`] the node applies, so what is drawn
//! is what happens. Both axes are in field values rather than a fixed `[0, 1]`, because the
//! fields Levels most needs to window (a `Distance` output in metres) do not live in `[0, 1]`.

use eframe::egui;
use ymir_core::LevelsTransfer;

use crate::preview::Histogram;
use crate::theme;

/// Widget height in points.
const HEIGHT: f32 = 150.0;
/// Maximum widget width in points (the inspector panel is narrow).
const MAX_WIDTH: f32 = 260.0;
/// How much of an axis's span to leave as breathing room at each end, so a bound sitting at the
/// extreme of the data still has a visible line rather than merging with the border.
const PAD_FRACTION: f32 = 0.08;
/// Half-width of a handle marker's base, in points.
const MARKER: f32 = 4.0;

/// An axis's value range, low to high.
type Range = (f32, f32);

/// Pads a raw value range so nothing is drawn flush against the border, widening a degenerate
/// range to a unit interval rather than leaving a zero span for the mapping to divide by.
fn padded(lo: f32, hi: f32) -> Range {
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let span = hi - lo;
    if span > 0.0 {
        let pad = span * PAD_FRACTION;
        (lo - pad, hi + pad)
    } else {
        (lo - 0.5, hi + 0.5)
    }
}

/// The horizontal axis: every input value the picture has to account for, which is the incoming
/// distribution plus the window bounds themselves.
///
/// The bounds are folded in so a window set outside the data (or with no data at all) still
/// shows its handles, rather than leaving them off-screen with nothing to grab.
fn input_range(transfer: &LevelsTransfer, histogram: Option<&Histogram>) -> Range {
    let mut lo = transfer.in_low.min(transfer.in_high);
    let mut hi = transfer.in_low.max(transfer.in_high);
    if let Some(h) = histogram.filter(|h| !h.bins.is_empty()) {
        lo = lo.min(h.min);
        hi = hi.max(h.max);
    }
    padded(lo, hi)
}

/// The vertical axis: the output window. The transfer is flat at `out_low` and `out_high`
/// outside the input window, so this is exactly the range the curve occupies.
fn output_range(transfer: &LevelsTransfer) -> Range {
    padded(transfer.out_low, transfer.out_high)
}

/// Maps a value on `range` to a fraction of the way along it, `0` at the low end.
fn fraction(value: f32, range: Range) -> f32 {
    let span = range.1 - range.0;
    if span.abs() > f32::EPSILON {
        (value - range.0) / span
    } else {
        0.5
    }
}

/// Screen x for an input value.
fn x_of(value: f32, range: Range, rect: egui::Rect) -> f32 {
    rect.left() + fraction(value, range) * rect.width()
}

/// Screen y for an output value, with y pointing up.
fn y_of(value: f32, range: Range, rect: egui::Rect) -> f32 {
    rect.bottom() - fraction(value, range) * rect.height()
}

/// Draws the Levels picture for `transfer`, with `histogram` (the node's input distribution)
/// behind it. Read-only: the numbers are edited by the rows beneath it.
pub(crate) fn levels_picture(
    ui: &mut egui::Ui,
    transfer: LevelsTransfer,
    histogram: Option<&Histogram>,
) {
    let size = egui::vec2(ui.available_width().min(MAX_WIDTH), HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let x_range = input_range(&transfer, histogram);
    let y_range = output_range(&transfer);

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

    // The input distribution along the bottom, placed by the values its bins cover. This is the
    // thing being windowed, so it is the one part that must be honest about where the data sits.
    if let Some(hist) = histogram.filter(|h| !h.bins.is_empty()) {
        let bar_color = visuals.weak_text_color().gamma_multiply(0.5);
        let width = hist.bin_width();
        for (i, &h) in hist.bins.iter().enumerate() {
            if h <= 0.0 {
                continue;
            }
            let lo = hist.value_at_bin(i);
            let hi = if width > 0.0 { lo + width } else { lo };
            let x0 = x_of(lo, x_range, rect);
            // A degenerate bin still needs a visible width, so it is drawn a point wide.
            let x1 = x_of(hi, x_range, rect).max(x0 + 1.0);
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, rect.bottom() - h * rect.height()),
                egui::pos2(x1, rect.bottom()),
            );
            painter.rect_filled(bar, 0, bar_color);
        }
    }

    // The transfer curve, sampled across the width by calling what the node calls, so the
    // drawing cannot disagree with the result.
    let steps = 96;
    let curve_stroke = egui::Stroke::new(1.5, visuals.text_color());
    let mut prev: Option<egui::Pos2> = None;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let value = x_range.0 + t * (x_range.1 - x_range.0);
        let point = egui::pos2(
            rect.left() + t * rect.width(),
            y_of(transfer.apply(value), y_range, rect),
        );
        if let Some(p) = prev {
            painter.line_segment([p, point], curve_stroke);
        }
        prev = Some(point);
    }

    // The window bounds. Each is a line on the axis it acts along, with a marker on that axis's
    // edge: input bounds are vertical (they cut the input domain), output bounds horizontal
    // (they place the result). Drawn in the accent so they read as the controls rather than as
    // more grid, and distinguished by position rather than by hue.
    let accent = theme::ACCENT_PRIMARY;
    let bound_stroke = egui::Stroke::new(1.0, accent);
    for value in [transfer.in_low, transfer.in_high] {
        let x = x_of(value, x_range, rect);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            bound_stroke,
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x - MARKER, rect.bottom()),
                egui::pos2(x + MARKER, rect.bottom()),
                egui::pos2(x, rect.bottom() - MARKER * 1.6),
            ],
            accent,
            egui::Stroke::NONE,
        ));
    }
    for value in [transfer.out_low, transfer.out_high] {
        let y = y_of(value, y_range, rect);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            bound_stroke,
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(rect.left(), y - MARKER),
                egui::pos2(rect.left(), y + MARKER),
                egui::pos2(rect.left() + MARKER * 1.6, y),
            ],
            accent,
            egui::Stroke::NONE,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0))
    }

    #[test]
    fn a_degenerate_range_widens_instead_of_collapsing() {
        let (lo, hi) = padded(0.25, 0.25);
        assert!(hi > lo, "expected a widened range, got {lo}..{hi}");
    }

    #[test]
    fn padding_keeps_the_bounds_off_the_edges() {
        let (lo, hi) = padded(0.0, 1.0);
        assert!(lo < 0.0 && hi > 1.0);
    }

    #[test]
    fn the_input_axis_covers_the_window_even_with_no_data() {
        let transfer = LevelsTransfer {
            in_low: 100.0,
            in_high: 400.0,
            ..LevelsTransfer::NEUTRAL
        };
        let (lo, hi) = input_range(&transfer, None);
        assert!(
            lo < 100.0 && hi > 400.0,
            "the handles must be reachable, got {lo}..{hi}"
        );
    }

    #[test]
    fn the_input_axis_covers_data_lying_outside_the_window() {
        // A window set narrowly inside a wide distribution: the axis still shows the whole
        // distribution, so it is visible that the window is discarding most of it.
        let transfer = LevelsTransfer {
            in_low: 10.0,
            in_high: 20.0,
            ..LevelsTransfer::NEUTRAL
        };
        let hist = Histogram {
            bins: vec![1.0, 1.0],
            min: 0.0,
            max: 400.0,
        };
        let (lo, hi) = input_range(&transfer, Some(&hist));
        assert!(lo < 0.0 && hi > 400.0, "got {lo}..{hi}");
    }

    #[test]
    fn a_value_maps_left_to_right_and_bottom_to_top() {
        let r = rect();
        let range = (0.0, 10.0);
        assert!(x_of(0.0, range, r) < x_of(10.0, range, r));
        // y is up, so a larger output value is a smaller screen y.
        assert!(y_of(10.0, range, r) < y_of(0.0, range, r));
    }

    #[test]
    fn a_zero_width_axis_maps_to_the_middle_rather_than_dividing_by_zero() {
        let r = rect();
        let f = fraction(5.0, (5.0, 5.0));
        assert_eq!(f, 0.5);
        assert!(x_of(5.0, (5.0, 5.0), r).is_finite());
    }
}
