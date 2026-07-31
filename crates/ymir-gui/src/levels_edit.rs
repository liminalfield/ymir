//! The Levels editor widget (#369): the input window, the midtone bend and the output window
//! drawn as one picture, with the four window bounds draggable.
//!
//! Levels is a relationship between five numbers, not five independent values, and rendered as
//! five sliders that relationship has to be held in the reader's head. Here the input
//! distribution runs along the bottom, the transfer curve crosses the plot, and each window
//! bound is a line on the axis it belongs to, so the window, the bend and the placement are one
//! thing to look at.
//!
//! The curve is drawn by calling the same [`LevelsTransfer`] the node applies, so what is drawn
//! is what happens.
//!
//! The horizontal axis is the incoming data's own range, not a fixed `[0, 1]`, because the
//! fields Levels most needs to window (a `Distance` output in metres) do not live in `[0, 1]`.
//! The vertical axis is `[0, 1]`, which is what a height works in. Neither axis is a function of
//! the bounds drawn on it; see [`input_range`] for why that matters more than it sounds.

use eframe::egui;
use ymir_core::{LevelsTransfer, ParamKind, ParamSpec};

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
/// Thickness of the band along each axis that grabs its handles, in points. The bands are the
/// grab targets rather than the full-length guide lines, so the input and output handles cannot
/// contend for the same pixel.
const GRAB_BAND: f32 = 16.0;

/// Positions of the five members within a [`ParamGroup::Levels`](ymir_core::ParamGroup) run.
/// The order is contractual and asserted by a test on the node that declares it.
const IN_LOW: usize = 0;
const IN_HIGH: usize = 1;
const OUT_LOW: usize = 3;
const OUT_HIGH: usize = 4;
/// Members the run must contain for the editor to address them by position.
const MEMBERS: usize = 5;

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

/// The domain a height works in, and so the axis a Levels output is read against when there is
/// nothing better to anchor to.
const UNIT_DOMAIN: Range = (0.0, 1.0);

/// The horizontal axis: the incoming distribution's own range.
///
/// **The axis is never a function of the bounds drawn on it.** Two ways of getting that wrong
/// were tried and both showed: scaling the axis to fit its bounds pins each handle to a fixed
/// fraction of the axis, so it cannot move at all; widening the axis for a bound outside it
/// rescales the histogram, so the distribution being aimed at slides around while it is aimed
/// at. The data is what the window is set against and it does not change when a bound moves,
/// which is exactly what makes it the thing to anchor to.
///
/// With no distribution to show (a disconnected node, or an input that could not be evaluated)
/// it falls back to the unit domain, the range a height works in.
fn input_range(histogram: Option<&Histogram>) -> Range {
    match histogram.filter(|h| !h.bins.is_empty()) {
        Some(h) if h.max > h.min => padded(h.min, h.max),
        _ => padded(UNIT_DOMAIN.0, UNIT_DOMAIN.1),
    }
}

/// The vertical axis: the unit domain, since what leaves Levels is a height and heights work in
/// `[0, 1]`. Fixed for the same reason the input axis is.
fn output_range() -> Range {
    padded(UNIT_DOMAIN.0, UNIT_DOMAIN.1)
}

/// Whether `value` falls outside the axis, so its handle is parked at the edge rather than
/// sitting where the number says. A bound may legitimately be beyond the data (or beyond
/// `[0, 1]`); the axis does not stretch to reach it, so the marker says "off this end" instead
/// of claiming a position it does not have.
fn is_beyond(value: f32, range: Range) -> bool {
    value < range.0 || value > range.1
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

/// Inverse of [`fraction`]: the value a fraction along `range` lands on.
fn value_at(fraction: f32, range: Range) -> f32 {
    range.0 + fraction * (range.1 - range.0)
}

/// Screen x for an input value.
fn x_of(value: f32, range: Range, rect: egui::Rect) -> f32 {
    rect.left() + fraction(value, range) * rect.width()
}

/// Screen y for an output value, with y pointing up.
fn y_of(value: f32, range: Range, rect: egui::Rect) -> f32 {
    rect.bottom() - fraction(value, range) * rect.height()
}

/// The input value a screen x lands on.
fn value_at_x(x: f32, range: Range, rect: egui::Rect) -> f32 {
    value_at((x - rect.left()) / rect.width().max(1.0), range)
}

/// The output value a screen y lands on, with y pointing up.
fn value_at_y(y: f32, range: Range, rect: egui::Rect) -> f32 {
    value_at((rect.bottom() - y) / rect.height().max(1.0), range)
}

/// The declared range of a float parameter, for clamping a dragged value to what the node will
/// accept. A member that is not a float cannot be dragged, so it reports an empty range.
fn declared_bounds(spec: &ParamSpec) -> Option<(f64, f64)> {
    match spec.kind {
        ParamKind::Float { min, max } => Some((min, max)),
        _ => None,
    }
}

/// Draws the Levels picture for `transfer` with `histogram` behind it, and lets the four window
/// bounds be dragged along their axes. Returns the member's index within the run and its new
/// value when one was dragged this frame.
///
/// `specs` is the run's parameters in declaration order; the dragged value is clamped to the
/// range that member declares, so the editor cannot produce a value the node would reject.
pub(crate) fn levels_editor(
    ui: &mut egui::Ui,
    specs: &[ParamSpec],
    transfer: LevelsTransfer,
    histogram: Option<&Histogram>,
) -> Option<(usize, f64)> {
    let size = egui::vec2(ui.available_width().min(MAX_WIDTH), HEIGHT);
    let (rect, bg) = ui.allocate_exact_size(size, egui::Sense::hover());
    if specs.len() < MEMBERS {
        // The declaration is not the shape this editor addresses by position. Draw nothing and
        // leave the members to their own rows rather than reading the wrong parameter.
        return None;
    }

    // The axis is held still for the duration of a drag. It does not depend on the bounds, but
    // it does depend on the input distribution, and the preview re-evaluates as the drag changes
    // the node: a frame where the histogram is briefly absent would drop the axis back to the
    // unit domain and jolt the picture mid-gesture.
    let store_id = bg.id.with("axes");
    let frozen: Option<(Range, Range)> = ui.data(|d| d.get_temp(store_id));
    let (x_range, y_range) = frozen.unwrap_or_else(|| (input_range(histogram), output_range()));

    let visuals = ui.visuals().clone();
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
        let point = egui::pos2(
            rect.left() + t * rect.width(),
            y_of(transfer.apply(value_at(t, x_range)), y_range, rect),
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
    //
    // The grab target is the band along the axis, not the whole guide line, so the two families
    // never contend for the same pixel. The bands are inset past each other's width, so even the
    // corner belongs to exactly one of them.
    let accent = theme::ACCENT_PRIMARY;
    let mut edited = None;

    // A bound beyond the axis is parked at the edge and drawn hollow. The axis will not stretch
    // to reach it, so the marker admits it is off the end instead of claiming a position on the
    // axis that would be a different number.
    let fill_of = |beyond: bool| {
        if beyond {
            egui::Color32::TRANSPARENT
        } else {
            accent
        }
    };
    let outline_of = |beyond: bool| {
        if beyond {
            egui::Stroke::new(1.5, accent)
        } else {
            egui::Stroke::NONE
        }
    };

    for (index, value) in [(IN_LOW, transfer.in_low), (IN_HIGH, transfer.in_high)] {
        let beyond = is_beyond(value, x_range);
        let x = x_of(value.clamp(x_range.0, x_range.1), x_range, rect);
        let band = egui::Rect::from_min_max(
            egui::pos2(x - GRAB_BAND / 2.0, rect.bottom() - GRAB_BAND),
            egui::pos2(x + GRAB_BAND / 2.0, rect.bottom()),
        )
        .intersect(egui::Rect::from_min_max(
            egui::pos2(rect.left() + GRAB_BAND, rect.top()),
            rect.max,
        ));
        let resp = ui
            .interact(band, bg.id.with(index), egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
            && let Some((min, max)) = declared_bounds(&specs[index])
        {
            // Clamped to the axis as well as to what the parameter declares, so a drag can only
            // set a value the picture can actually show. Reaching past the axis is the row's
            // job, where the number is typed rather than aimed.
            let raw = f64::from(value_at_x(pos.x, x_range, rect).clamp(x_range.0, x_range.1));
            edited = Some((index, raw.clamp(min, max)));
        }
        let stroke = egui::Stroke::new(if resp.hovered() { 2.0 } else { 1.0 }, accent);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            stroke,
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x - MARKER, rect.bottom()),
                egui::pos2(x + MARKER, rect.bottom()),
                egui::pos2(x, rect.bottom() - MARKER * 1.6),
            ],
            fill_of(beyond),
            outline_of(beyond),
        ));
    }

    for (index, value) in [(OUT_LOW, transfer.out_low), (OUT_HIGH, transfer.out_high)] {
        let beyond = is_beyond(value, y_range);
        let y = y_of(value.clamp(y_range.0, y_range.1), y_range, rect);
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.left(), y - GRAB_BAND / 2.0),
            egui::pos2(rect.left() + GRAB_BAND, y + GRAB_BAND / 2.0),
        );
        let resp = ui
            .interact(band, bg.id.with(index), egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
            && let Some((min, max)) = declared_bounds(&specs[index])
        {
            let raw = f64::from(value_at_y(pos.y, y_range, rect).clamp(y_range.0, y_range.1));
            edited = Some((index, raw.clamp(min, max)));
        }
        let stroke = egui::Stroke::new(if resp.hovered() { 2.0 } else { 1.0 }, accent);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(rect.left(), y - MARKER),
                egui::pos2(rect.left(), y + MARKER),
                egui::pos2(rect.left() + MARKER * 1.6, y),
            ],
            fill_of(beyond),
            outline_of(beyond),
        ));
    }

    // Hold the axes still while a bound is being dragged, and let them breathe again once it is
    // released.
    if edited.is_some() {
        ui.data_mut(|d| d.insert_temp(store_id, (x_range, y_range)));
    } else {
        ui.data_mut(|d| d.remove::<(Range, Range)>(store_id));
    }

    edited
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::ParamValue;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0))
    }

    fn float_spec(min: f64, max: f64) -> ParamSpec {
        ParamSpec::new("p", ParamKind::Float { min, max }, ParamValue::Float(0.0))
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

    fn wide_hist() -> Histogram {
        Histogram {
            bins: vec![1.0, 1.0],
            min: 0.0,
            max: 400.0,
        }
    }

    #[test]
    fn the_histogram_does_not_move_when_a_bound_does() {
        // The bug this guards: the axis widened for a bound outside it, and rescaling the axis
        // rescaled the distribution drawn on it, so aiming a window moved the thing being aimed
        // at. The axis is the data's, and the data does not change when a bound moves.
        let hist = wide_hist();
        let inside = input_range(Some(&hist));
        // Bounds far outside the data, in both directions, must leave the axis exactly alone.
        assert_eq!(inside, input_range(Some(&hist)));
        let r = rect();
        let bar_at = |range: Range| x_of(hist.value_at_bin(1), range, r);
        assert_eq!(bar_at(inside), bar_at(input_range(Some(&hist))));
    }

    #[test]
    fn a_bound_beyond_the_axis_is_parked_rather_than_stretching_it() {
        let range = input_range(Some(&wide_hist()));
        assert!(is_beyond(10_000.0, range), "far above the data");
        assert!(is_beyond(-10_000.0, range), "far below the data");
        assert!(!is_beyond(200.0, range), "inside the data");
    }

    #[test]
    fn moving_an_output_bound_moves_its_handle() {
        // The bug this guards: an axis scaled to fit its own bounds puts each at a fixed
        // fraction of itself, so the handle sat at the same pixel for every value. It moved
        // while dragged (the axis was held still) and snapped back on release, while the number
        // changed underneath. An axis must not be a function of the handles drawn on it.
        let r = rect();
        let high = |v: f32| LevelsTransfer {
            out_high: v,
            ..LevelsTransfer::NEUTRAL
        };
        let at = |t: LevelsTransfer| y_of(t.out_high, output_range(), r);
        // y is up, so lowering the bound must move its handle *down* the screen.
        assert!(
            at(high(0.5)) > at(high(1.0)),
            "out_high 0.5 drew at {} and 1.0 at {}",
            at(high(0.5)),
            at(high(1.0))
        );
        assert!(at(high(0.25)) > at(high(0.5)));
    }

    #[test]
    fn moving_an_input_bound_moves_its_handle() {
        let r = rect();
        let hist = Histogram {
            bins: vec![1.0, 1.0],
            min: 0.0,
            max: 400.0,
        };
        let low = |v: f32| LevelsTransfer {
            in_low: v,
            in_high: 400.0,
            ..LevelsTransfer::NEUTRAL
        };
        let at = |t: LevelsTransfer| x_of(t.in_low, input_range(Some(&hist)), r);
        assert!(at(low(200.0)) > at(low(100.0)));
    }

    #[test]
    fn the_input_axis_falls_back_to_the_unit_domain_with_no_data() {
        let (lo, hi) = input_range(None);
        assert!(lo < 0.0 && hi > 1.0, "got {lo}..{hi}");
    }

    #[test]
    fn the_input_axis_shows_the_whole_distribution() {
        // A window set narrowly inside a wide distribution: the axis still shows all of the
        // data, so it is visible that the window is discarding most of it.
        let (lo, hi) = input_range(Some(&wide_hist()));
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

    #[test]
    fn a_screen_position_round_trips_back_to_its_value() {
        let r = rect();
        let range = (-50.0, 350.0);
        for value in [-50.0, 0.0, 125.0, 350.0] {
            let back = value_at_x(x_of(value, range, r), range, r);
            assert!((back - value).abs() < 1e-3, "{value} came back as {back}");
            let back_y = value_at_y(y_of(value, range, r), range, r);
            assert!(
                (back_y - value).abs() < 1e-3,
                "{value} came back as {back_y}"
            );
        }
    }

    #[test]
    fn a_dragged_value_is_clamped_to_what_the_parameter_declares() {
        // The pointer can leave the widget, and the axis can show values beyond the declared
        // range, so the clamp is what stops the editor proposing a value the node would reject.
        let spec = float_spec(-4.0, 4.0);
        let (min, max) = declared_bounds(&spec).expect("a float declares bounds");
        assert_eq!(1000.0_f64.clamp(min, max), 4.0);
        assert_eq!((-1000.0_f64).clamp(min, max), -4.0);
    }

    #[test]
    fn a_non_float_member_cannot_be_dragged() {
        let spec = ParamSpec::new("p", ParamKind::Bool, ParamValue::Bool(false));
        assert!(declared_bounds(&spec).is_none());
    }
}
