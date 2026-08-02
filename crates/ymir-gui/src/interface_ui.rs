//! Authoring a subgraph's parameter interface (#373, Phase 2).
//!
//! A subgraph could declare parameters, but only from code. This is the surface that lets someone
//! declare them: the list of what an authored node exposes, edited on the container itself.
//!
//! The list is part of the *definition*, not of this instance. Editing it changes what every
//! instance of that subgraph exposes, which is the point: an authored node has one schema, the
//! way a native one does. Values stay per-instance and are edited by the ordinary parameter rows
//! above.

use eframe::egui;
use ymir_core::{Curve, InterfaceKind, InterfaceParam, ParamValue, Unit};

/// One entry in the kind menu: how it is written, and the declaration choosing it produces.
///
/// A constructor rather than a value because a kind carries its bounds, so choosing "Number"
/// means a fresh float range rather than a shared one.
type KindChoice = (&'static str, fn() -> InterfaceKind);

/// The kinds offered when declaring a parameter.
///
/// Not every [`InterfaceKind`]: this is the menu, so it is ordered by how often a terrain
/// parameter wants each rather than by the enum's declaration.
const KINDS: [KindChoice; 5] = [
    ("Number", || InterfaceKind::Float {
        min: 0.0,
        max: 1000.0,
    }),
    ("Whole number", || InterfaceKind::Int { min: 0, max: 64 }),
    ("Switch", || InterfaceKind::Bool),
    ("Curve", || InterfaceKind::Curve),
    ("Colour", || InterfaceKind::Color),
];

/// The label a kind is written as, for the dropdown's closed state.
fn kind_label(kind: &InterfaceKind) -> &'static str {
    match kind {
        InterfaceKind::Float { .. } => "Number",
        InterfaceKind::Int { .. } => "Whole number",
        InterfaceKind::Bool => "Switch",
        InterfaceKind::Text => "Text",
        InterfaceKind::Curve => "Curve",
        InterfaceKind::Color => "Colour",
    }
}

/// A default that matches `kind`, for when a declaration changes type.
///
/// A default whose variant disagrees with its kind is a value the node can never read, so the
/// change carries one rather than leaving the pair inconsistent.
fn default_for(kind: &InterfaceKind) -> ParamValue {
    match kind {
        InterfaceKind::Float { min, .. } => ParamValue::Float(min.max(0.0)),
        InterfaceKind::Int { min, .. } => ParamValue::Int((*min).max(0)),
        InterfaceKind::Bool => ParamValue::Bool(false),
        InterfaceKind::Text => ParamValue::Text(String::new()),
        InterfaceKind::Curve => ParamValue::Curve(Curve::identity()),
        InterfaceKind::Color => ParamValue::Color([1.0, 1.0, 1.0]),
    }
}

/// A name that is not already declared, for a freshly added parameter.
fn fresh_name(existing: &[InterfaceParam]) -> String {
    let taken = |candidate: &str| existing.iter().any(|p| p.name == candidate);
    if !taken("value") {
        return "value".to_owned();
    }
    (2..)
        .map(|n| format!("value_{n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| "value".to_owned())
}

/// Whether `name` can be referenced from an expression inside.
///
/// The name is an identifier, not prose: it is what an inner expression writes. A name the
/// expression compiler cannot tokenize would declare a parameter nothing inside could ever read,
/// so it is rejected at the point of typing rather than discovered later as an unknown name.
pub(crate) fn is_referenceable(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Why a declaration cannot be used, or `None` when it is fine.
pub(crate) fn declaration_problem(interface: &[InterfaceParam], index: usize) -> Option<String> {
    let param = interface.get(index)?;
    if param.name.is_empty() {
        return Some("needs a name".to_owned());
    }
    if !is_referenceable(&param.name) {
        return Some("letters, digits and _ only, not starting with a digit".to_owned());
    }
    let duplicate = interface
        .iter()
        .enumerate()
        .any(|(i, other)| i != index && other.name == param.name);
    duplicate.then(|| "another parameter already has this name".to_owned())
}

/// Draws the interface editor for an authored node, returning the edited list when it changed.
///
/// `interface` is what the node currently declares. Nothing is written back unless something
/// changed, so a frame that only draws costs no graph revision.
pub(crate) fn interface_editor(
    ui: &mut egui::Ui,
    interface: &[InterfaceParam],
) -> Option<Vec<InterfaceParam>> {
    let mut edited: Vec<InterfaceParam> = interface.to_vec();
    let mut changed = false;
    let mut remove: Option<usize> = None;

    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        crate::param_ui::plain_label(ui, "Interface");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("+")
                .on_hover_text("Declare a parameter on this node")
                .clicked()
            {
                let kind = InterfaceKind::Float {
                    min: 0.0,
                    max: 1000.0,
                };
                let default = default_for(&kind);
                edited.push(InterfaceParam::new(fresh_name(&edited), kind, default));
                changed = true;
            }
        });
    });

    if edited.is_empty() {
        ui.small("Nothing declared. A parameter added here appears on every instance of this node, and anything inside can read it by name.");
        return changed.then_some(edited);
    }

    for index in 0..edited.len() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut name = edited[index].name.clone();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut name)
                        .desired_width(96.0)
                        .font(egui::TextStyle::Monospace),
                )
                .changed()
            {
                edited[index].name = name;
                changed = true;
            }
            egui::ComboBox::from_id_salt(("interface-kind", index))
                .selected_text(kind_label(&edited[index].kind))
                .width(110.0)
                .show_ui(ui, |ui| {
                    for (label, make) in KINDS {
                        let candidate = make();
                        let selected = kind_label(&edited[index].kind) == label;
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            edited[index].kind = candidate;
                            edited[index].default = default_for(&edited[index].kind);
                            changed = true;
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("−")
                    .on_hover_text("Remove this parameter")
                    .clicked()
                {
                    remove = Some(index);
                }
                // Metres or nothing: the only unit a declared length wants today, and the one
                // that makes the inspector edit it as a quantity rather than a slider.
                let mut metres = edited[index].unit == Some(Unit::Meters);
                if matches!(edited[index].kind, InterfaceKind::Float { .. })
                    && ui
                        .checkbox(&mut metres, "m")
                        .on_hover_text("Declare this a length in metres")
                        .changed()
                {
                    edited[index].unit = metres.then_some(Unit::Meters);
                    changed = true;
                }
            });
        });
        if let Some(problem) = declaration_problem(&edited, index) {
            ui.small(egui::RichText::new(problem).color(crate::theme::WARNING));
        }
    }

    if let Some(index) = remove {
        edited.remove(index);
        changed = true;
    }
    changed.then_some(edited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(name: &str) -> InterfaceParam {
        InterfaceParam::new(
            name,
            InterfaceKind::Float { min: 0.0, max: 1.0 },
            ParamValue::Float(0.0),
        )
    }

    #[test]
    fn a_name_an_expression_could_not_write_is_rejected() {
        // The name is an identifier, not prose: it is what an inner expression types. A name the
        // compiler cannot tokenize would declare a parameter nothing inside could ever read.
        assert!(is_referenceable("beach_width"));
        assert!(is_referenceable("_x2"));
        assert!(!is_referenceable(""));
        assert!(!is_referenceable("beach width"));
        assert!(!is_referenceable("2wide"));
        assert!(!is_referenceable("beach-width"));
    }

    #[test]
    fn a_duplicate_name_is_reported_against_both() {
        let list = [declared("width"), declared("width")];
        assert!(declaration_problem(&list, 0).is_some());
        assert!(declaration_problem(&list, 1).is_some());
    }

    #[test]
    fn a_distinct_well_formed_name_has_no_problem() {
        let list = [declared("width"), declared("amplitude")];
        assert_eq!(declaration_problem(&list, 0), None);
        assert_eq!(declaration_problem(&list, 1), None);
    }

    #[test]
    fn a_fresh_name_avoids_the_ones_already_taken() {
        assert_eq!(fresh_name(&[]), "value");
        assert_eq!(fresh_name(&[declared("value")]), "value_2");
        assert_eq!(
            fresh_name(&[declared("value"), declared("value_2")]),
            "value_3"
        );
    }

    #[test]
    fn changing_a_kind_carries_a_default_that_matches_it() {
        // A default whose variant disagrees with its kind is a value the node can never read.
        for (_, make) in KINDS {
            let kind = make();
            let default = default_for(&kind);
            let spec = InterfaceParam::new("p", kind, default).to_spec();
            // `ParamSpec::new` debug-asserts the pair agrees; reaching here means it does.
            assert_eq!(spec.name, "p");
        }
    }
}
