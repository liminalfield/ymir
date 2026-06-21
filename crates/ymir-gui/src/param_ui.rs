//! The parameter inspector (GUI step 5, issue #6): maps a node's `ParamSpec`
//! schema to editor widgets with no per-node code.
//!
//! The schema-to-widget mapping ([`widget_for`]) and value resolution
//! ([`current_value`]) are pure and unit-tested; only [`edit`] touches egui. Edits
//! are written back to the canonical graph by the caller via `Graph::set_params`.

use eframe::egui;
use ymir_core::{ParamKind, ParamSpec, ParamValue, Params, Unit};

/// The editor widget a parameter kind maps to. Derived purely from the schema, so
/// the mapping is unit-testable without egui.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Widget {
    /// A slider over `[min, max]` for a bounded, unit-less float (a ratio).
    Slider { min: f64, max: f64 },
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
            },
        },
        ParamKind::Int { min, max } => Widget::IntDrag {
            min: *min,
            max: *max,
        },
        ParamKind::Bool => Widget::Checkbox,
        ParamKind::Text => Widget::Text,
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

/// Renders the editor for one parameter and returns the new value if the user
/// changed it this frame, or `None` otherwise. The widget choice is [`widget_for`];
/// this is the thin egui-touching layer over that pure mapping. A value whose
/// variant disagrees with its kind (or an unknown kind) falls through to a
/// read-only display, so a mismatch is shown, never edited wrongly.
pub(crate) fn edit(
    ui: &mut egui::Ui,
    spec: &ParamSpec,
    current: &ParamValue,
) -> Option<ParamValue> {
    let name = spec.name.as_str();
    match (widget_for(spec), current) {
        (Widget::Slider { min, max }, ParamValue::Float(v)) => {
            let mut x = *v;
            let resp = ui.add(egui::Slider::new(&mut x, min..=max).text(name));
            resp.changed().then_some(ParamValue::Float(x))
        }
        (Widget::Quantity { min, max, unit }, ParamValue::Float(v)) => {
            // An open physical quantity: a clamped, type-able value field with a 1-unit
            // drag step and the unit shown, not a coarse wide slider.
            let mut x = *v;
            let resp = ui
                .horizontal(|ui| {
                    let r = ui.add(
                        egui::DragValue::new(&mut x)
                            .range(min..=max)
                            .speed(1.0)
                            .suffix(unit_suffix(unit)),
                    );
                    ui.label(name);
                    r
                })
                .inner;
            resp.changed().then_some(ParamValue::Float(x))
        }
        (Widget::IntDrag { min, max }, ParamValue::Int(v)) => {
            let mut x = *v;
            let resp = ui
                .horizontal(|ui| {
                    let r = ui.add(egui::DragValue::new(&mut x).range(min..=max));
                    ui.label(name);
                    r
                })
                .inner;
            resp.changed().then_some(ParamValue::Int(x))
        }
        (Widget::Checkbox, ParamValue::Bool(v)) => {
            let mut x = *v;
            let resp = ui.checkbox(&mut x, name);
            resp.changed().then_some(ParamValue::Bool(x))
        }
        (Widget::Text, ParamValue::Text(v)) => {
            let mut x = v.clone();
            let resp = ui
                .horizontal(|ui| {
                    ui.label(name);
                    ui.add(egui::TextEdit::singleline(&mut x))
                })
                .inner;
            resp.changed().then_some(ParamValue::Text(x))
        }
        (Widget::Dropdown { options }, ParamValue::Text(v)) => {
            let mut selected = v.clone();
            let changed = ui
                .horizontal(|ui| {
                    ui.label(name);
                    egui::ComboBox::from_id_salt(name)
                        .selected_text(selected.clone())
                        .show_ui(ui, |ui| {
                            let mut changed = false;
                            for option in options {
                                changed |= ui
                                    .selectable_value(&mut selected, (*option).to_string(), *option)
                                    .changed();
                            }
                            changed
                        })
                        .inner
                        .unwrap_or(false)
                })
                .inner;
            changed.then_some(ParamValue::Text(selected))
        }
        (Widget::CurveEditor, ParamValue::Curve(curve)) => {
            ui.label(name);
            crate::curve_edit::curve_editor(ui, curve).map(ParamValue::Curve)
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
    fn each_kind_maps_to_its_widget() {
        assert_eq!(
            widget_for(&spec(
                ParamKind::Float { min: 0.0, max: 1.0 },
                ParamValue::Float(0.0)
            )),
            Widget::Slider { min: 0.0, max: 1.0 }
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
