//! The parameter inspector (GUI step 5, issue #6): maps a node's `ParamSpec`
//! schema to editor widgets with no per-node code.
//!
//! The schema-to-widget mapping ([`widget_for`]) and value resolution
//! ([`current_value`]) are pure and unit-tested; only [`edit`] touches egui. Edits
//! are written back to the canonical graph by the caller via `Graph::set_params`.

use eframe::egui;
use ymir_core::{ParamKind, ParamSpec, ParamValue, Params};

/// The editor widget a parameter kind maps to. Derived purely from the schema, so
/// the mapping is unit-testable without egui.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Widget {
    /// A slider over `[min, max]` for a float.
    Slider { min: f64, max: f64 },
    /// A drag value over `[min, max]` for an integer.
    IntDrag { min: i64, max: i64 },
    /// A checkbox for a boolean.
    Checkbox,
    /// A single-line text field.
    Text,
    /// A dropdown over a fixed set of option ids.
    Dropdown { options: &'static [&'static str] },
    /// A kind this build cannot edit yet. `ParamKind` is `#[non_exhaustive]`, so a
    /// future kind degrades to a read-only display rather than risk corrupting a
    /// value it does not understand.
    ReadOnly,
}

/// Maps a parameter kind to its editor widget.
pub(crate) fn widget_for(kind: &ParamKind) -> Widget {
    match kind {
        ParamKind::Float { min, max } => Widget::Slider {
            min: *min,
            max: *max,
        },
        ParamKind::Int { min, max } => Widget::IntDrag {
            min: *min,
            max: *max,
        },
        ParamKind::Bool => Widget::Checkbox,
        ParamKind::Text => Widget::Text,
        ParamKind::Enum { options } => Widget::Dropdown { options },
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
    match (widget_for(&spec.kind), current) {
        (Widget::Slider { min, max }, ParamValue::Float(v)) => {
            let mut x = *v;
            let resp = ui.add(egui::Slider::new(&mut x, min..=max).text(name));
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

    #[test]
    fn each_kind_maps_to_its_widget() {
        assert_eq!(
            widget_for(&ParamKind::Float { min: 0.0, max: 1.0 }),
            Widget::Slider { min: 0.0, max: 1.0 }
        );
        assert_eq!(
            widget_for(&ParamKind::Int { min: 1, max: 12 }),
            Widget::IntDrag { min: 1, max: 12 }
        );
        assert_eq!(widget_for(&ParamKind::Bool), Widget::Checkbox);
        assert_eq!(widget_for(&ParamKind::Text), Widget::Text);
        assert_eq!(
            widget_for(&ParamKind::Enum {
                options: &["add", "mix"]
            }),
            Widget::Dropdown {
                options: &["add", "mix"]
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
