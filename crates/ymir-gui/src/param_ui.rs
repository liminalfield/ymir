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
    }
}

/// Height of a parameter row's reset-icon / value band.
const DENSE_ROW_H: f32 = 22.0;
/// Fixed width of a parameter row's value box.
const VALUE_W: f32 = 54.0;

/// A small faint revert glyph, shown when a value is off its default; clicking it resets that one
/// parameter. Returns its response.
fn reset_icon(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, DENSE_ROW_H), egui::Sense::click());
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

/// A custom horizontal slider filling the available width: a 4px deep track, an accent fill up to
/// the handle, and a white handle with a ring. Drag or click anywhere on it to set. Marks its
/// response changed only when the value actually moves.
fn slider(ui: &mut egui::Ui, value: &mut f64, min: f64, max: f64, log: bool) -> egui::Response {
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

/// A parameter row's label: muted, in a friendly Title-Case form. Shared with the frame inspector
/// so its rows read in the same grammar as the node parameters.
pub(crate) fn param_label(ui: &mut egui::Ui, name: &str) {
    ui.label(
        egui::RichText::new(prettify_param_name(name))
            .family(egui::FontFamily::Monospace)
            .size(12.0)
            .color(crate::theme::TEXT_SECONDARY),
    );
}

/// Turns a snake_case parameter id into a friendly display label: underscores become spaces and
/// each word is capitalised (`erode_inland_basins` -> `Erode Inland Basins`), matching the
/// Title-Case of node names. A pure presentation transform; the underlying param id is unchanged,
/// so lookups, hashing, and save/load still use the raw name.
fn prettify_param_name(name: &str) -> String {
    name.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A 34x18 pill toggle: an accent track with the knob right when on, a raised track with the knob
/// left when off. Returns its response (click to flip).
fn toggle(ui: &mut egui::Ui, on: bool) -> egui::Response {
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

/// An integer stepper: a deep field with a minus button, the value in the centre, and a plus button.
/// Steps by one within `[min, max]`. Returns whether the value changed.
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
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        value.to_string(),
        egui::FontId::new(13.0, egui::FontFamily::Monospace),
        crate::theme::TEXT_PRIMARY,
    );
    changed
}

/// Renders the editor for one parameter and returns the new value if the user
/// changed it this frame, or `None` otherwise. The widget choice is [`widget_for`];
/// this is the thin egui-touching layer over that pure mapping. A value whose
/// variant disagrees with its kind (or an unknown kind) falls through to a
/// read-only display, so a mismatch is shown, never edited wrongly.
pub(crate) fn edit(
    ui: &mut egui::Ui,
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
                param_label(ui, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let value = ui
                        .add_sized(
                            egui::vec2(VALUE_W, ui.spacing().interact_size.y),
                            egui::DragValue::new(&mut x)
                                .range(min..=max)
                                .speed(speed)
                                .fixed_decimals(3),
                        )
                        .on_hover_text("Drag to scrub \u{b7} click to type");
                    if value.changed() {
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
            // world-unit range is too coarse to slide. Degrees wrap rather than clamp, so
            // dragging below 0 rolls to 359.9 (a small counter-clockwise turn); metric quantities
            // clamp to their range.
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Float(d) => *d,
                _ => x,
            };
            let degrees = matches!(unit, Unit::Degrees);
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut drag = egui::DragValue::new(&mut x).suffix(unit_suffix(unit));
                    drag = if degrees {
                        drag.speed(0.5).fixed_decimals(1)
                    } else {
                        drag.speed(1.0).range(min..=max)
                    };
                    let value = ui
                        .add_sized(
                            egui::vec2(VALUE_W + 16.0, ui.spacing().interact_size.y),
                            drag,
                        )
                        .on_hover_text("Drag to scrub \u{b7} click to type");
                    if value.changed() {
                        let stored = if degrees { x.rem_euclid(360.0) } else { x };
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
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if stepper(ui, &mut x, min, max) {
                        result = Some(ParamValue::Int(x));
                    }
                });
            });
            result
        }
        (Widget::Checkbox, ParamValue::Bool(v)) => {
            let mut x = *v;
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, name);
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
                param_label(ui, name);
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
                param_label(ui, name);
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
                param_label(ui, name);
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
    fn param_names_display_as_friendly_title_case() {
        assert_eq!(prettify_param_name("width"), "Width");
        assert_eq!(
            prettify_param_name("erode_inland_basins"),
            "Erode Inland Basins"
        );
        assert_eq!(prettify_param_name("world_extent"), "World Extent");
        // Stray underscores do not leave double spaces or empty words.
        assert_eq!(prettify_param_name("a__b_"), "A B");
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
}
