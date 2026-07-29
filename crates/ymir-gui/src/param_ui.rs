//! The parameter inspector (GUI step 5, issue #6): maps a node's `ParamSpec`
//! schema to editor widgets with no per-node code.
//!
//! The schema-to-widget mapping ([`widget_for`]) and value resolution
//! ([`current_value`]) are pure and unit-tested; only [`edit`] touches egui. Edits
//! are written back to the canonical graph by the caller via `Graph::set_params`.

use eframe::egui;
use ymir_core::{ParamKind, ParamSpec, ParamValue, Params, Scale, Unit};

/// The editor widget a parameter kind maps to. Derived purely from the schema, so
/// the mapping is unit-testable without egui.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Widget {
    /// A slider over `[min, max]` for a bounded, unit-less float (a ratio). `logarithmic`
    /// distributes the track by ratio rather than increment (for a frequency or a scale).
    Slider {
        min: f64,
        max: f64,
        logarithmic: bool,
    },
    /// A value field over `[min, max]` for a float carrying a unit (an open physical
    /// quantity, e.g. a world-unit length), shown with the unit as a suffix. A slider
    /// over a wide world-unit range is too coarse and unlabelled; this is precise and
    /// type-able instead.
    Quantity { min: f64, max: f64, unit: Unit },
    /// A drag value over `[min, max]` for an integer.
    IntDrag { min: i64, max: i64 },
    /// A checkbox for a boolean.
    Checkbox,
    /// A single-line text field.
    Text,
    /// A filesystem-path text field with a Browse button (a native file picker).
    Path,
    /// A dropdown over a fixed set of option ids.
    Dropdown { options: &'static [&'static str] },
    /// A visual transfer-curve editor.
    CurveEditor,
    /// A colour swatch that opens a picker.
    ColorPicker,
    /// A kind this build cannot edit yet. `ParamKind` is `#[non_exhaustive]`, so a
    /// future kind degrades to a read-only display rather than risk corrupting a
    /// value it does not understand.
    ReadOnly,
}

/// Maps a parameter schema to its editor widget. Takes the whole spec, since a
/// float's widget depends on whether it carries a unit (an open quantity edits as a
/// value field, a bare ratio as a slider).
pub(crate) fn widget_for(spec: &ParamSpec) -> Widget {
    match &spec.kind {
        ParamKind::Float { min, max } => match spec.unit {
            Some(unit) => Widget::Quantity {
                min: *min,
                max: *max,
                unit,
            },
            None => Widget::Slider {
                min: *min,
                max: *max,
                logarithmic: spec.scale == Scale::Logarithmic,
            },
        },
        ParamKind::Int { min, max } => Widget::IntDrag {
            min: *min,
            max: *max,
        },
        ParamKind::Bool => Widget::Checkbox,
        ParamKind::Text => Widget::Text,
        ParamKind::Path => Widget::Path,
        ParamKind::Enum { options } => Widget::Dropdown { options },
        ParamKind::Curve => Widget::CurveEditor,
        ParamKind::Color => Widget::ColorPicker,
        // ParamKind is #[non_exhaustive]; an unknown future kind degrades, never
        // panics. This is graceful degradation, not a swallowed case.
        _ => Widget::ReadOnly,
    }
}

/// The effective value of a parameter for a node: the value the node has set, or
/// the schema default when it has not set one.
pub(crate) fn current_value(params: &Params, spec: &ParamSpec) -> ParamValue {
    params
        .get(&spec.name)
        .cloned()
        .unwrap_or_else(|| spec.default.clone())
}

/// The display suffix for a unit, including a leading space (egui draws it abutting
/// the number). Prose lives here in the GUI, never in the schema.
fn unit_suffix(unit: Unit) -> &'static str {
    match unit {
        Unit::Meters => " m",
        Unit::Degrees => "°",
    }
}

/// A short human display of a value, for the read-only fallback.
pub(crate) fn value_text(value: &ParamValue) -> String {
    match value {
        ParamValue::Float(v) => format!("{v}"),
        ParamValue::Int(v) => format!("{v}"),
        ParamValue::Bool(v) => format!("{v}"),
        ParamValue::Text(v) => v.clone(),
        ParamValue::Curve(c) => format!("curve ({} points)", c.points().len()),
        ParamValue::Strokes(s) => format!("painted ({} strokes)", s.len()),
        // Hex, because that is how a colour is written down everywhere else a user will meet
        // this one: a picker, a manifest, an engine's material settings.
        ParamValue::Color(rgb) => {
            let [r, g, b] = srgb_bytes(*rgb);
            format!("#{r:02X}{g:02X}{b:02X}")
        }
    }
}

/// A stored colour as the 0-255 channels egui works in.
///
/// The stored value is sRGB in `[0, 1]` (see [`ParamValue::Color`]), so this is a scale and a
/// round, not a colour-space conversion. Rounding rather than truncating keeps a value that
/// round-trips through the picker from drifting down a step each time.
fn srgb_bytes(rgb: [f64; 3]) -> [u8; 3] {
    rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The inverse of [`srgb_bytes`].
fn srgb_from_bytes(bytes: [u8; 3]) -> [f64; 3] {
    bytes.map(|c| f64::from(c) / 255.0)
}

/// Fixed width of a parameter row's value box.
const VALUE_W: f32 = 54.0;

/// A small faint revert glyph, shown when a value is off its default; clicking it resets that one
/// parameter. Returns its response.
fn reset_icon(ui: &mut egui::Ui) -> egui::Response {
    // Allocate the same height as the value field beside it (its `add_sized` uses this same
    // interact size), so the glyph appearing when a value goes off-default cannot grow the row.
    let h = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, h), egui::Sense::click());
    let color = if resp.hovered() {
        crate::theme::TEXT_SECONDARY
    } else {
        crate::theme::TEXT_TERTIARY
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
        egui::FontId::proportional(12.0),
        color,
    );
    resp.on_hover_text("Reset to default")
}

/// Clamps or wraps a scrubbed value to its bounds: `wrap` (e.g. 360 for degrees) rolls the value
/// around that period; otherwise `clamp` bounds it to `[lo, hi]`; with neither it passes through.
pub(crate) fn finalize_value(v: f64, wrap: Option<f64>, clamp: Option<(f64, f64)>) -> f64 {
    if let Some(period) = wrap {
        v.rem_euclid(period)
    } else if let Some((lo, hi)) = clamp {
        v.clamp(lo, hi)
    } else {
        v
    }
}

/// Whether a degree-valued quantity should wrap around the circle rather than clamp to its range.
/// Only a *direction* wraps: an azimuth or rotation declares the full turn (`0..360`), so scrubbing
/// past either end rolls over. A *bounded* angle (a slope grade, a beach face, a spread) declares a
/// sub-circle range, and a value past its max is not a smaller angle the other way round but simply
/// impossible (a slope cannot exceed 90 degrees), so it clamps to the author's declared max like a
/// metric quantity. Keyed on the declared span, so the schema's range is what decides.
fn angle_wraps(min: f64, max: f64) -> bool {
    max - min >= 360.0
}

/// Display precision (decimal places) for a metric quantity: always hundredths of a metre.
///
/// Fixed rather than derived. It used to come from the parameter's declared maximum, which meant
/// every length in the app was edited in whole metres, because all of them declare a 100 km ceiling
/// and none had ever narrowed it (#352). A typed 2.5 rounded to 3, and on a small world a blur
/// radius could only be 0, 1 or 2.
///
/// Deliberately not derived from the cell size or the world extent either. Precision that shifts
/// when the build resolution changes is invisible coupling: the same project would present
/// differently between a preview and a final build, with nothing on screen to explain why.
///
/// `max_decimals` trims trailing zeros, so a whole value still reads `5 m` and only a fractional one
/// shows its decimals. The scrub speed is a separate question, answered by [`metric_scrub_speed`].
const METRIC_DECIMALS: usize = 2;

/// Scrub step for a metric quantity, from the value being dragged.
///
/// Proportional to where the value already is, not to its declared range. Dragging near 8 m moves in
/// small steps; dragging near 5 km moves in large ones. So one rule suits a blur radius of a couple
/// of metres and a coastal reach of kilometres, and the parameter's declared ceiling never enters
/// into it.
///
/// It was briefly range-proportional, which was worse than what it replaced. Every metric parameter
/// in `ymir-nodes` declares a 100 km ceiling, so a thousandth of the range came to 100 m per step,
/// and reaching 8 m by dragging was harder than reaching 100 km. A declared maximum is an outer
/// bound, not a statement about the values anyone works at, so it is the wrong thing to scale by.
///
/// Kept separate from the display precision, which stays fixed at [`METRIC_DECIMALS`]. Deriving
/// those two from one number is what forced whole-metre display before #352.
fn metric_scrub_speed(value: f64) -> f64 {
    // A fiftieth of the current value, so roughly fifty pixels of drag doubles it. Floored so a
    // value sitting at zero is not frozen there.
    (value.abs() / 50.0).max(0.05)
}

/// Layers infinite (wrapping) scrub onto a numeric value box. While `resp` is dragged, the value is
/// driven by *raw* mouse motion (immune to the screen edge, unlike egui's position-based DragValue),
/// and the cursor is locked in place and hidden so the pointer never runs off the edge and returns
/// where it started on release. `finalize` clamps or wraps the result. Returns whether the value
/// moved this frame.
///
/// The caller builds the DragValue with `speed(0.0)` so its own edge-limited drag is inert and only
/// this scrub moves the value; DragValue still supplies display, click-to-type, and formatting.
///
/// `CursorGrab::Locked` is what pointer-lock supports on Wayland, Windows, and macOS; it pins the
/// pointer natively while still delivering relative motion (winit rejects a bare cursor *warp* on
/// Wayland — "cursor position can be set only for locked cursor"). On X11 Locked is unsupported and
/// winit logs it once per drag; the scrub still runs from raw XInput2 motion, only the pointer is
/// not pinned.
pub(crate) fn scrub_drag(
    ui: &egui::Ui,
    resp: &egui::Response,
    value: &mut f64,
    speed: f64,
    finalize: impl Fn(f64) -> f64,
) -> bool {
    if resp.drag_started() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::Locked,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }
    let mut changed = false;
    if resp.dragged() {
        // Raw device motion ignores the screen edge; fall back to the position delta only if the
        // integration does not supply it (then the scrub is bounded, as it was before).
        let dx = match ui.input(|i| i.pointer.motion()) {
            Some(m) => f64::from(m.x),
            None => f64::from(resp.drag_delta().x),
        };
        if dx != 0.0 {
            *value = finalize(*value + dx * speed);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::None,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    }
    changed
}

/// A custom horizontal slider filling the available width: a 4px deep track, an accent fill up to
/// the handle, and a white handle with a ring. Drag or click anywhere on it to set. Marks its
/// response changed only when the value actually moves.
pub(crate) fn slider(
    ui: &mut egui::Ui,
    value: &mut f64,
    min: f64,
    max: f64,
    log: bool,
) -> egui::Response {
    let w = ui.available_width().max(24.0);
    let (rect, mut resp) =
        ui.allocate_exact_size(egui::vec2(w, 14.0), egui::Sense::click_and_drag());
    let r = 5.5_f32;
    let usable = (w - 2.0 * r).max(1.0);
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(w, 4.0));
    let t = to_t(*value, min, max, log).clamp(0.0, 1.0) as f32;
    let hx = rect.left() + r + t * usable;
    let cy = rect.center().y;
    let painter = ui.painter();
    painter.rect_filled(track, 2.0, crate::theme::BG_ABYSS);
    painter.rect_filled(
        egui::Rect::from_min_max(track.left_top(), egui::pos2(hx, track.bottom())),
        2.0,
        crate::theme::ACCENT_PRIMARY,
    );
    painter.circle_filled(egui::pos2(hx, cy), r, crate::theme::TEXT_PRIMARY);
    painter.circle_stroke(
        egui::pos2(hx, cy),
        r,
        egui::Stroke::new(2.0, crate::theme::BG_SURFACE),
    );
    let before = *value;
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let nt = (f64::from(pos.x - rect.left() - r) / f64::from(usable)).clamp(0.0, 1.0);
        *value = from_t(nt, min, max, log);
    }
    if *value != before {
        resp.mark_changed();
    }
    resp
}

/// Normalizes a value to `0..1` across `[min, max]`, log-scaled when `log` and the range is positive.
fn to_t(x: f64, min: f64, max: f64, log: bool) -> f64 {
    if log && min > 0.0 && max > 0.0 {
        (x.ln() - min.ln()) / (max.ln() - min.ln())
    } else if (max - min).abs() < f64::EPSILON {
        0.0
    } else {
        (x - min) / (max - min)
    }
}

/// The inverse of [`to_t`]: a `0..1` position back to a value in `[min, max]`.
fn from_t(t: f64, min: f64, max: f64, log: bool) -> f64 {
    if log && min > 0.0 && max > 0.0 {
        (min.ln() + t * (max.ln() - min.ln())).exp()
    } else {
        min + t * (max - min)
    }
}

/// A node parameter's row label: the resolved display name in muted mono, with the parameter's
/// one-line description shown as a hover tooltip when the catalog has one. Label and description
/// come from [`ymir_nodes::resolve_param`], so the inspector, the tooltip, and the generated
/// reference all read the same strings.
pub(crate) fn param_label(ui: &mut egui::Ui, type_id: &str, name: &str) {
    let resolved = ymir_nodes::resolve_param(type_id, name);
    let resp = render_label(ui, &resolved.label);
    if let Some(desc) = resolved.description {
        resp.on_hover_text(desc);
    }
}

/// Renders an already-display label (the frame and colour-picker rows) in the same muted mono
/// style as a parameter label, without catalog resolution or a tooltip.
pub(crate) fn plain_label(ui: &mut egui::Ui, text: &str) {
    render_label(ui, text);
}

/// Draws a row label in the shared muted-mono style and returns its response, so a caller can
/// attach a hover tooltip.
fn render_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        egui::RichText::new(text)
            .family(egui::FontFamily::Monospace)
            .size(12.0)
            .color(crate::theme::TEXT_SECONDARY),
    )
}

/// A 34x18 pill toggle: an accent track with the knob right when on, a raised track with the knob
/// left when off. Returns its response (click to flip).
pub(crate) fn toggle(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 18.0), egui::Sense::click());
    let track = if on {
        crate::theme::ACCENT_PRIMARY
    } else {
        crate::theme::BG_HOVER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 9.0, track);
    let knob_x = if on {
        rect.right() - 9.0
    } else {
        rect.left() + 9.0
    };
    painter.circle_filled(
        egui::pos2(knob_x, rect.center().y),
        6.5,
        crate::theme::TEXT_PRIMARY,
    );
    resp.on_hover_text(if on { "On" } else { "Off" })
}

/// How far an integer scrub travels per point of pointer motion (#246). Deliberately flat rather
/// than derived from the parameter's range: a range-proportional speed, as the sliders use, would
/// be millions per point on fbm's seed (`0..=i32::MAX`) and a whole step per point on octaves
/// (`1..=12`). Two points per step suits both, and typing covers the long jumps.
const SCRUB_UNITS_PER_POINT: f64 = 0.5;

/// Where an integer scrub settles: rounded to a whole step, then held inside the bounds the
/// schema declared (#246). Both halves matter. Without the round a drag would leave a fraction
/// that the box shows and the graph stores as a truncated value; without the clamp a scrub could
/// carry a seed below zero or octaves past their maximum, which the widget must not allow since
/// the range is the node author's declared intent. Pure, so it is unit-tested.
fn settle_int(v: f64, min: i64, max: i64) -> i64 {
    v.round().clamp(min as f64, max as f64) as i64
}

/// Splits a scrubbed position into the whole step it settles on and the sub-step remainder to carry
/// into the next frame (#246). The position is bounded first, so the remainder is always within half
/// a step: pushing on against a limit cannot bank an ever-growing carry that a drag back the other
/// way would have to unwind before the value moved. Pure, so it is unit-tested.
fn split_scrub(v: f64, min: i64, max: i64) -> (i64, f64) {
    let bounded = v.clamp(min as f64, max as f64);
    let settled = settle_int(bounded, min, max);
    (settled, bounded - settled as f64)
}

/// An integer stepper: a deep field with a minus button, the value in the centre, and a plus button.
/// The buttons step by one within `[min, max]`; the centre is the same value box the float params
/// use, so an integer can be scrubbed with an infinite cursor-locked drag or clicked and typed
/// (#246). A wide range (fbm's seed runs to `i32::MAX`) is unreachable one click at a time, and
/// scrubbing at any range-proportional speed would be either useless there or twitchy on a range
/// like octaves' 1..12, so the scrub is a flat unit per pixel and typing covers the long jumps.
/// Returns whether the value changed.
fn stepper(ui: &mut egui::Ui, value: &mut i64, min: i64, max: i64) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(104.0, 24.0), egui::Sense::hover());
    let btn_w = 26.0;
    let minus = egui::Rect::from_min_size(rect.left_top(), egui::vec2(btn_w, rect.height()));
    let plus = egui::Rect::from_min_size(
        egui::pos2(rect.right() - btn_w, rect.top()),
        egui::vec2(btn_w, rect.height()),
    );
    // Seed the button ids from this allocation's own (auto-unique) id, not `ui.id()`, which several
    // stepper rows share: otherwise every stepper's minus/plus collide (egui's red id-clash boxes).
    let minus_resp = ui.interact(minus, resp.id.with("minus"), egui::Sense::click());
    let plus_resp = ui.interact(plus, resp.id.with("plus"), egui::Sense::click());
    let mut changed = false;
    if minus_resp.clicked() && *value > min {
        *value -= 1;
        changed = true;
    }
    if plus_resp.clicked() && *value < max {
        *value += 1;
        changed = true;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, crate::theme::BG_ABYSS);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, crate::theme::LINE),
        egui::StrokeKind::Inside,
    );
    let glyph = |r: &egui::Response, active: bool| {
        if !active {
            crate::theme::TEXT_TERTIARY
        } else if r.hovered() {
            crate::theme::TEXT_PRIMARY
        } else {
            crate::theme::TEXT_SECONDARY
        }
    };
    painter.text(
        minus.center(),
        egui::Align2::CENTER_CENTER,
        "\u{2212}",
        egui::FontId::proportional(15.0),
        glyph(&minus_resp, *value > min),
    );
    painter.text(
        plus.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(15.0),
        glyph(&plus_resp, *value < max),
    );
    // The centre of the field is a value box rather than painted text: a DragValue supplies
    // click-to-type and formatting, and its own edge-limited drag is made inert (speed 0) so the
    // wrapping-free infinite scrub below is the only thing that moves it, exactly as the float
    // params do. It is drawn without a frame so the field painted above stays the visible control.
    let centre = egui::Rect::from_min_max(
        egui::pos2(minus.right(), rect.top()),
        egui::pos2(plus.left(), rect.bottom()),
    );
    // Seeded from this allocation's own (auto-unique) id for the same reason the buttons are: the
    // stepper rows share a parent ui, so an auto-generated id would clash between rows and two
    // steppers would share one text-edit state.
    let value_resp = ui
        .push_id(resp.id, |ui| {
            let v = ui.visuals_mut();
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.weak_bg_fill = egui::Color32::TRANSPARENT;
                w.bg_stroke = egui::Stroke::NONE;
                w.fg_stroke.color = crate::theme::TEXT_PRIMARY;
            }
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(13.0, egui::FontFamily::Monospace),
            );
            ui.put(
                centre,
                egui::DragValue::new(value)
                    .speed(0.0)
                    .range(min as f64..=max as f64),
            )
            .on_hover_text("Drag to scrub \u{b7} click to type")
        })
        .inner;
    changed |= value_resp.changed();
    // The scrub runs on a float mirror (that is what `scrub_drag` drives) and lands back on the
    // integer, keeping the sub-step remainder across frames in this widget's own temp state. That
    // carry is what makes a slow drag work: rounding every frame and re-seeding from the stored
    // integer would throw away a third of a pixel of motion each time, so the value would sit
    // still until the pointer moved fast enough and then jump.
    let carry_id = value_resp.id.with("scrub-carry");
    let mut carry: f64 = if value_resp.drag_started() {
        0.0
    } else {
        ui.data(|d| d.get_temp(carry_id)).unwrap_or(0.0)
    };
    let mut scrubbed_value = *value as f64 + carry;
    if scrub_drag(
        ui,
        &value_resp,
        &mut scrubbed_value,
        SCRUB_UNITS_PER_POINT,
        |v| v.clamp(min as f64, max as f64),
    ) {
        let (settled, remainder) = split_scrub(scrubbed_value, min, max);
        carry = remainder;
        // Only a real move counts: reporting a change on every dragged frame would rewrite the
        // parameter (and re-key the preview) while the value stood still.
        if settled != *value {
            *value = settled;
            changed = true;
        }
    }
    ui.data_mut(|d| d.insert_temp(carry_id, carry));
    changed
}

/// Renders the editor for one parameter and returns the new value if the user
/// changed it this frame, or `None` otherwise. The widget choice is [`widget_for`];
/// this is the thin egui-touching layer over that pure mapping. A value whose
/// variant disagrees with its kind (or an unknown kind) falls through to a
/// read-only display, so a mismatch is shown, never edited wrongly.
pub(crate) fn edit(
    ui: &mut egui::Ui,
    type_id: &str,
    spec: &ParamSpec,
    current: &ParamValue,
    histogram: Option<&[f32]>,
    popout: &mut bool,
) -> Option<ParamValue> {
    let name = spec.name.as_str();
    match (widget_for(spec), current) {
        (
            Widget::Slider {
                min,
                max,
                logarithmic,
            },
            ParamValue::Float(v),
        ) => {
            // A two-line row: the mono label and, right-aligned, a reset icon (only when off default)
            // plus the scrub/type value; then a full-width slider beneath. The single-line label ->
            // control -> value was too tight for the panel width.
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Float(d) => *d,
                _ => x,
            };
            let speed = (max - min) * 0.002;
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let value = ui
                        .add_sized(
                            egui::vec2(VALUE_W, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut x)
                                .range(min..=max)
                                .speed(0.0)
                                .fixed_decimals(3),
                        )
                        .on_hover_text("Drag to scrub \u{b7} click to type");
                    let scrubbed = scrub_drag(ui, &value, &mut x, speed, |v| {
                        finalize_value(v, None, Some((min, max)))
                    });
                    if value.changed() || scrubbed {
                        result = Some(ParamValue::Float(x));
                    }
                    if (x - default).abs() > f64::EPSILON && reset_icon(ui).clicked() {
                        x = default;
                        result = Some(ParamValue::Float(default));
                    }
                });
            });
            if slider(ui, &mut x, min, max, logarithmic).changed() {
                result = Some(ParamValue::Float(x));
            }
            result
        }
        (Widget::Quantity { min, max, unit }, ParamValue::Float(v)) => {
            // Same row grammar as the other params (mono label left, control right), but a
            // type/scrub value field with the unit as a suffix and no slider beneath: a wide
            // world-unit range is too coarse to slide. A direction (a full-circle angle) wraps, so
            // dragging below 0 rolls to 359.9 (a small counter-clockwise turn); a bounded angle (a
            // slope grade) and every metric quantity clamp to their declared range instead, so an
            // impossible value (a slope past 90) can never be scrubbed or typed in.
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Float(d) => *d,
                _ => x,
            };
            let degrees = matches!(unit, Unit::Degrees);
            let wraps = degrees && angle_wraps(min, max);
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Decimals are set explicitly (DragValue derives them from `speed`, which is 0
                    // here, so otherwise it shows full float precision). Degrees always show a fixed
                    // tenth; a metric length shows a precision that suits its declared range (whole
                    // metres for a wide reach, up to a tenth for a small length like a berm crest).
                    // Display precision and scrub speed are answered separately: a length shows
                    // hundredths whatever its range, and scrubs by a step suited to that range.
                    let speed = if degrees { 0.5 } else { metric_scrub_speed(x) };
                    // DragValue supplies display, click-to-type, and formatting; its own drag is
                    // made inert (speed 0) so the infinite scrub below is the only thing that moves
                    // the value.
                    let mut drag = egui::DragValue::new(&mut x)
                        .suffix(unit_suffix(unit))
                        .speed(0.0);
                    drag = if degrees {
                        drag.fixed_decimals(1)
                    } else {
                        drag.max_decimals(METRIC_DECIMALS)
                    };
                    // Everything but a wrapping direction clamps click-to-type to the declared
                    // range; a wrapping angle has no meaningful bound to type against (it rolls).
                    if !wraps {
                        drag = drag.range(min..=max);
                    }
                    let value = ui
                        .add_sized(
                            egui::vec2(VALUE_W + 16.0, ui.spacing().interact_size.y),
                            drag,
                        )
                        .on_hover_text("Drag to scrub \u{b7} click to type");
                    let scrubbed = scrub_drag(ui, &value, &mut x, speed, |v| {
                        if wraps {
                            finalize_value(v, Some(360.0), None)
                        } else {
                            finalize_value(v, None, Some((min, max)))
                        }
                    });
                    if value.changed() || scrubbed {
                        let stored = if wraps { x.rem_euclid(360.0) } else { x };
                        result = Some(ParamValue::Float(stored));
                    }
                    if (x - default).abs() > f64::EPSILON && reset_icon(ui).clicked() {
                        x = default;
                        result = Some(ParamValue::Float(default));
                    }
                });
            });
            result
        }
        (Widget::IntDrag { min, max }, ParamValue::Int(v)) => {
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Int(d) => *d,
                _ => x,
            };
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if stepper(ui, &mut x, min, max) {
                        result = Some(ParamValue::Int(x));
                    }
                    // The same revert affordance the float rows carry, so an integer is not the
                    // one parameter kind you cannot put back (#246).
                    if x != default && reset_icon(ui).clicked() {
                        result = Some(ParamValue::Int(default));
                    }
                });
            });
            result
        }
        (Widget::Checkbox, ParamValue::Bool(v)) => {
            let mut x = *v;
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if toggle(ui, x).clicked() {
                        x = !x;
                        result = Some(ParamValue::Bool(x));
                    }
                });
            });
            result
        }
        (Widget::Text, ParamValue::Text(v)) => {
            let mut x = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut x)
                            .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                            .text_color(crate::theme::TEXT_PRIMARY)
                            .background_color(crate::theme::BG_ABYSS)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    result = Some(ParamValue::Text(x.clone()));
                }
            });
            result
        }
        (Widget::Path, ParamValue::Text(v)) => {
            // A path text field plus a Browse button opening the native file picker. The
            // text stays editable (paste or type a path); Browse fills it in.
            let mut x = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                if ui.button("Browse\u{2026}").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Image", &["png"])
                        .pick_file()
                {
                    x = path.display().to_string();
                    result = Some(ParamValue::Text(x.clone()));
                }
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut x)
                            .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                            .text_color(crate::theme::TEXT_PRIMARY)
                            .background_color(crate::theme::BG_ABYSS)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    result = Some(ParamValue::Text(x.clone()));
                }
            });
            result
        }
        (Widget::Dropdown { options }, ParamValue::Text(v)) => {
            let mut selected = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = ui.button(format!(
                        "{}   {}",
                        selected,
                        egui_phosphor::regular::CARET_DOWN
                    ));
                    egui::Popup::menu(&button).show(|ui| {
                        ui.set_min_width(button.rect.width());
                        for option in options {
                            if ui
                                .selectable_label(selected.as_str() == *option, *option)
                                .clicked()
                            {
                                selected = (*option).to_string();
                                result = Some(ParamValue::Text(selected.clone()));
                                ui.close();
                            }
                        }
                    });
                });
            });
            result
        }
        (Widget::ColorPicker, ParamValue::Color(rgb)) => {
            let mut bytes = srgb_bytes(*rgb);
            let mut changed = None;
            ui.horizontal(|ui| {
                ui.label(name);
                // egui's own picker, so the interaction is the one a user already knows. It
                // works in 8-bit sRGB, which is what the stored value scales to exactly.
                if ui.color_edit_button_srgb(&mut bytes).changed() {
                    changed = Some(ParamValue::Color(srgb_from_bytes(bytes)));
                }
                ui.weak(value_text(current));
            });
            changed
        }
        (Widget::CurveEditor, ParamValue::Curve(curve)) => {
            ui.label(name);
            let result = crate::curve_edit::curve_editor(ui, curve, histogram);
            *popout = result.popout_clicked;
            result.changed.map(ParamValue::Curve)
        }
        _ => {
            ui.horizontal(|ui| {
                ui.label(name);
                ui.weak(value_text(current));
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: ParamKind, default: ParamValue) -> ParamSpec {
        ParamSpec::new("p", kind, default)
    }

    #[test]
    fn settle_int_rounds_then_clamps_to_the_declared_range() {
        // #246: a scrub lands on whole steps and can never leave the schema's range.
        assert_eq!(settle_int(3.4, 0, 12), 3);
        assert_eq!(settle_int(3.6, 0, 12), 4);
        // fbm's seed is declared non-negative: scrubbing down stops at zero rather than going
        // negative, and a long drag up stops at the declared maximum.
        assert_eq!(settle_int(-8.0, 0, i64::from(i32::MAX)), 0);
        assert_eq!(
            settle_int(1e12, 0, i64::from(i32::MAX)),
            i64::from(i32::MAX)
        );
        // A signed range keeps both ends.
        assert_eq!(settle_int(-10_500.0, -10_000, 10_000), -10_000);
        assert_eq!(settle_int(-2.5, -10_000, 10_000), -3);
    }

    #[test]
    fn split_scrub_carries_the_sub_step_remainder_and_keeps_it_bounded() {
        // #246: the remainder is what makes a slow drag work. Rounding each frame and dropping it
        // would ignore motion below half a step, so the value would stall and then jump.
        let (settled, carry) = split_scrub(3.3, 0, 12);
        assert_eq!(settled, 3);
        assert!((carry - 0.3).abs() < 1e-9);

        // Three frames of a third of a step each cross one whole step, carrying the remainder.
        let mut value = 3_i64;
        let mut carry = 0.0;
        for _ in 0..3 {
            let (next, remainder) = split_scrub(value as f64 + carry + 0.34, 0, 12);
            value = next;
            carry = remainder;
        }
        assert_eq!(value, 4);

        // Pushing well past a limit banks no runaway carry, so a drag back the other way moves the
        // value at once instead of unwinding a debt first.
        let (settled, carry) = split_scrub(500.0, 0, 12);
        assert_eq!(settled, 12);
        assert!(
            carry.abs() <= 0.5,
            "carry stayed within half a step: {carry}"
        );
        let (settled, _) = split_scrub(settled as f64 + carry - 0.6, 0, 12);
        assert_eq!(settled, 11, "reversing moves immediately");
    }

    #[test]
    fn drawing_the_stepper_leaves_the_value_alone() {
        // Drawing must not rewrite the value: a param edit is what marks a project modified, so a
        // widget that nudged its own value on the first frame would report a phantom edit.
        let mut value = 65_323_i64;
        egui::__run_test_ui(|ui| {
            assert!(!stepper(ui, &mut value, 0, i64::from(i32::MAX)));
        });
        assert_eq!(value, 65_323);
    }

    #[test]
    fn finalize_value_wraps_or_clamps() {
        // Degrees wrap around the period: below 0 rolls up, at/above the period rolls down.
        assert_eq!(finalize_value(-0.1, Some(360.0), None), 359.9);
        assert!((finalize_value(370.0, Some(360.0), None) - 10.0).abs() < 1e-9);
        assert_eq!(finalize_value(45.0, Some(360.0), None), 45.0);
        // Metric clamps to its range.
        assert_eq!(finalize_value(-5.0, None, Some((0.0, 100.0))), 0.0);
        assert_eq!(finalize_value(150.0, None, Some((0.0, 100.0))), 100.0);
        assert_eq!(finalize_value(30.0, None, Some((0.0, 100.0))), 30.0);
        // With neither, the value passes through.
        assert_eq!(finalize_value(12.5, None, None), 12.5);
    }

    #[test]
    fn reset_glyph_matches_the_value_field_height() {
        // The reset glyph shares a horizontal row with the value field, so if it allocated a taller
        // box the row would grow the moment a value went off its default (#142). Both must use the
        // same interact height.
        let mut glyph_h = 0.0;
        let mut field_h = 0.0;
        egui::__run_test_ui(|ui| {
            field_h = ui.spacing().interact_size.y;
            glyph_h = reset_icon(ui).rect.height();
        });
        assert!(field_h > 0.0, "interact height should be non-zero");
        assert_eq!(
            glyph_h, field_h,
            "the reset glyph must be the same height as the value field, or the row grows when it appears"
        );
    }

    #[test]
    fn a_logarithmic_float_maps_to_a_log_slider() {
        let linear = spec(
            ParamKind::Float {
                min: 1.0,
                max: 64.0,
            },
            ParamValue::Float(2.0),
        );
        assert_eq!(
            widget_for(&linear),
            Widget::Slider {
                min: 1.0,
                max: 64.0,
                logarithmic: false,
            }
        );
        let log = spec(
            ParamKind::Float {
                min: 1.0,
                max: 64.0,
            },
            ParamValue::Float(2.0),
        )
        .logarithmic();
        assert_eq!(
            widget_for(&log),
            Widget::Slider {
                min: 1.0,
                max: 64.0,
                logarithmic: true,
            }
        );
    }

    #[test]
    fn each_kind_maps_to_its_widget() {
        assert_eq!(
            widget_for(&spec(
                ParamKind::Float { min: 0.0, max: 1.0 },
                ParamValue::Float(0.0)
            )),
            Widget::Slider {
                min: 0.0,
                max: 1.0,
                logarithmic: false,
            }
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Int { min: 1, max: 12 },
                ParamValue::Int(1)
            )),
            Widget::IntDrag { min: 1, max: 12 }
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Bool, ParamValue::Bool(false))),
            Widget::Checkbox
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Text, ParamValue::Text(String::new()))),
            Widget::Text
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Path, ParamValue::Text(String::new()))),
            Widget::Path
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Enum {
                    options: &["add", "mix"]
                },
                ParamValue::Text("add".into())
            )),
            Widget::Dropdown {
                options: &["add", "mix"]
            }
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Curve,
                ParamValue::Curve(ymir_core::Curve::identity())
            )),
            Widget::CurveEditor
        );
    }

    #[test]
    fn a_unit_bearing_float_is_a_quantity_not_a_slider() {
        // A world-unit length edits as a quantity (value field + unit), where a bare
        // ratio over the same kind would be a slider.
        let length = spec(
            ParamKind::Float {
                min: 0.0,
                max: 100.0,
            },
            ParamValue::Float(8.0),
        )
        .with_unit(Unit::Meters);
        assert_eq!(
            widget_for(&length),
            Widget::Quantity {
                min: 0.0,
                max: 100.0,
                unit: Unit::Meters
            }
        );
    }

    #[test]
    fn only_a_full_circle_angle_wraps() {
        // A direction declares the whole turn and rolls over when scrubbed past either end.
        assert!(angle_wraps(0.0, 360.0));
        // A bounded angle (a slope grade, a beach face, a spread) declares a sub-circle range, so a
        // value past its max is impossible, not a smaller angle the other way, and it clamps.
        assert!(!angle_wraps(0.0, 80.0)); // coastal beach/bluff grade
        assert!(!angle_wraps(0.0, 90.0)); // thermal talus / slope threshold
        assert!(!angle_wraps(1.0, 180.0)); // an angular spread
    }

    #[test]
    fn a_length_shows_hundredths_whatever_its_range() {
        // The precision used to come from the declared max, and every length in the app declares a
        // 100 km ceiling, so all of them edited in whole metres and a typed 2.5 rounded to 3 (#352).
        // It is now fixed, so the range cannot take the decimals away.
        assert_eq!(METRIC_DECIMALS, 2);
    }

    #[test]
    fn a_typed_fraction_of_a_metre_survives() {
        // The point of #352: a blur radius of 2.5 m has to stay 2.5 m. Nothing on the commit path
        // may round it to the scrub step or to a whole metre.
        let blur_range = Some((0.0, 100_000.0));
        assert!((finalize_value(2.5, None, blur_range) - 2.5).abs() < f64::EPSILON);
        assert!((finalize_value(0.25, None, blur_range) - 0.25).abs() < f64::EPSILON);
        // Still clamped to the declared range, which is the one thing that may change a value.
        assert!((finalize_value(-3.0, None, blur_range) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scrub_speed_follows_the_value_not_the_declared_range() {
        // The bug this replaces: the step was a thousandth of the declared range, and every metric
        // parameter declares a 100 km ceiling, so everything scrubbed at 100 m a step and a depth
        // of 8 m was unreachable by dragging.
        //
        // Scaling by the value instead means a parameter sitting at 8 moves in small steps whatever
        // its declared maximum, and one sitting at 5000 moves in large ones.
        let small = metric_scrub_speed(8.0);
        let large = metric_scrub_speed(5000.0);
        assert!(
            small < 1.0,
            "8 m should scrub finer than a metre, got {small}"
        );
        assert!(
            large > 10.0,
            "5 km should scrub in tens of metres, got {large}"
        );
        assert!(large > small * 100.0, "the step must scale with the value");
        // Zero is floored rather than frozen: a value starting at zero must still be draggable.
        let zero = metric_scrub_speed(0.0);
        assert!(
            zero > 0.0 && zero <= 0.1,
            "zero should still move, got {zero}"
        );
        // Negative values scrub at the same rate as their positive twin.
        assert!((metric_scrub_speed(-500.0) - metric_scrub_speed(500.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn current_value_prefers_set_value_then_falls_back_to_default() {
        let spec = ParamSpec::new(
            "frequency",
            ParamKind::Float { min: 0.0, max: 8.0 },
            ParamValue::Float(2.0),
        );
        // Absent: the schema default.
        assert_eq!(current_value(&Params::new(), &spec), ParamValue::Float(2.0));
        // Present: the node's set value wins.
        let params = Params::new().with("frequency", ParamValue::Float(3.5));
        assert_eq!(current_value(&params, &spec), ParamValue::Float(3.5));
    }

    #[test]
    fn value_text_renders_each_variant() {
        assert_eq!(value_text(&ParamValue::Int(7)), "7");
        assert_eq!(value_text(&ParamValue::Bool(true)), "true");
        assert_eq!(value_text(&ParamValue::Text("ridge".into())), "ridge");
    }

    #[test]
    fn an_edit_writes_through_to_the_graph() {
        // Mirrors what params_pane does on a changed value, minus the egui widget:
        // resolve the current value, write the edit back with set_params, then
        // verify the change landed in the canonical graph.
        use ymir_core::{Graph, registry};

        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let spec = graph.spec(id).expect("spec");
        let pspec = spec
            .params
            .iter()
            .find(|p| matches!(p.kind, ParamKind::Float { .. }))
            .expect("fbm has a float parameter");

        // Before any edit, the effective value is the schema default.
        assert_eq!(
            current_value(&graph.params(id).cloned().unwrap_or_default(), pspec),
            pspec.default
        );

        // Apply an edit the way the pane does, then write it back.
        let mut params = graph.params(id).cloned().unwrap_or_default();
        params.insert(pspec.name.clone(), ParamValue::Float(0.123));
        graph.set_params(id, params).expect("set_params");

        // The graph now holds the edited value.
        assert_eq!(
            current_value(&graph.params(id).cloned().unwrap_or_default(), pspec),
            ParamValue::Float(0.123)
        );
    }

    #[test]
    fn a_color_spec_gets_a_picker() {
        assert_eq!(
            widget_for(&spec(ParamKind::Color, ParamValue::Color([0.0, 0.0, 0.0]))),
            Widget::ColorPicker,
            "the widget comes from the spec, so a Material node needs no per-node UI code"
        );
    }

    #[test]
    fn a_color_survives_a_trip_through_the_pickers_byte_channels() {
        // egui edits in 8-bit sRGB while the value is stored as floats, so every edit is a
        // round trip through bytes. A value that came from the picker must come back unchanged,
        // or a colour would drift a step every time the inspector redrew it.
        for bytes in [[0, 0, 0], [255, 255, 255], [1, 128, 254], [77, 33, 200]] {
            assert_eq!(srgb_bytes(srgb_from_bytes(bytes)), bytes);
        }
    }

    #[test]
    fn a_color_reads_as_hex() {
        // Hex is how a colour is written everywhere else a user will meet this one: a picker,
        // an exported manifest, an engine's material settings.
        assert_eq!(value_text(&ParamValue::Color([1.0, 0.0, 0.0])), "#FF0000");
        assert_eq!(value_text(&ParamValue::Color([0.0, 0.0, 0.0])), "#000000");
        assert_eq!(
            value_text(&ParamValue::Color([2.0, -1.0, 0.5])),
            "#FF0080",
            "an out-of-range channel clamps rather than wrapping to a nonsense colour"
        );
    }
}
