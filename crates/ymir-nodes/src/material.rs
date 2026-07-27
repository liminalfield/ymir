//! Material: name a region of the terrain, so it can be previewed in colour and exported as a
//! weight map (#267).
//!
//! One node per material. It writes a single `[0, 1]` weight layer, `material.<name>`, holding
//! how much of that material each cell carries, and passes every other layer through untouched,
//! including the weight layers of the materials before it.
//!
//! **Weights are independent.** This node does not reduce the weight of any material already on
//! the field. `material.rock` is exactly what the rock selection said, whatever `material.snow`
//! says in the same cell, so overlapping coverage and per-cell sums above one are expected rather
//! than errors. That matches how the maps are consumed: a game engine normalizes weight-blended
//! landscape layers at render and takes the stacking order from its own material, not from the
//! maps. Ymir's layer order exists only so the viewport can predict that result, so it is view
//! state and never reaches this node. See `design/texturing.md`.
//!
//! A material with nothing wired to its mask covers everything, which is how a base material is
//! expressed: the all-ones map that guarantees no cell is left unclaimed.
//!
//! A pure per-cell write: resolution- and world-independent, so it is `NO_WORLD` and
//! byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.material";

/// The material name used when the parameter is empty. A layer has to be named something, and
/// `material.` with nothing after it would be a name no one could refer to.
const DEFAULT_NAME: &str = "material";

/// The default colour of a new Material node: a mid neutral.
///
/// Deliberately not a hue. Assigning distinct default colours is worth doing (a set of materials
/// should never come up as a red-versus-green pair, which is unreadable under the most common
/// colour vision deficiency), but doing it well means knowing what other materials are already in
/// the graph, which an operator cannot see. That belongs to the editor when it creates the node,
/// and is tracked as open decision 4 in the design note.
const DEFAULT_COLOR: [f64; 3] = [0.5, 0.5, 0.5];

/// Writes one material's weight layer. One required input (the field), one optional (the
/// selection), one output.
#[derive(Clone)]
pub struct Material;

/// The material name to use, from the parameter: trimmed, and falling back when it is empty.
///
/// Trimmed because a trailing space is invisible in the inspector but would make
/// `material.rock ` a different layer from `material.rock`, which is the kind of difference that
/// costs an afternoon.
fn material_name(params: &Params) -> &str {
    let name = params.get_str("name", DEFAULT_NAME).trim();
    if name.is_empty() { DEFAULT_NAME } else { name }
}

impl Operator for Material {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "material",
            // The selection is declared after the field, so it is the optional one.
            inputs: vec![PortSpec::new("in"), PortSpec::optional("mask")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "name",
                    ParamKind::Text,
                    ParamValue::Text(DEFAULT_NAME.into()),
                ),
                ParamSpec::new("color", ParamKind::Color, ParamValue::Color(DEFAULT_COLOR)),
            ],
            // The layer this node emits depends on the name parameter, so it cannot be declared
            // as a fixed string here. The prefix is the honest thing to advertise.
            emitted_layers: vec![layers::MATERIAL_PREFIX],
            mask_aware: true,
        }
    }

    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());

        // Where this material goes. An explicit selection wins and rides on its height layer,
        // which is the convention every mask-aware node here follows (a Slope or Height selector
        // writes its [0, 1] there). With nothing wired, the field's own carried mask applies, and
        // with no mask either the material covers everything, which is the base material.
        let selection = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        // Clamped because a weight outside [0, 1] is not meaningful: a selector is free to hand
        // over values that ran out of range upstream, and an engine reading a negative weight
        // would do something arbitrary with it. Per-cell and pure, so byte-identical at any
        // thread count.
        let weight = Layer::from_par_fn(width, height, |x, y| {
            selection.get(x, y).unwrap_or(1.0).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::material(material_name(params)), Arc::new(weight));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Material) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{NodeKind, Region};

    fn ctx(width: usize, height: usize) -> EvalContext {
        EvalContext::new(width, height, Region::UNIT, 0)
    }

    /// A field whose height ramps across x, so a test can tell cells apart.
    fn terrain(width: usize, height: usize) -> Field {
        Field::new(width, height, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(width, height, |x, _| {
                x as f32 / width as f32
            })),
        )
    }

    /// A selection field carrying `value` on its height layer, which is where a selector puts it.
    fn selection(width: usize, height: usize, value: f32) -> Field {
        Field::new(width, height, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::filled(width, height, value)),
        )
    }

    fn params(name: &str) -> Params {
        Params::new().with("name", ParamValue::Text(name.into()))
    }

    /// Runs the operator over `field`, with `mask` wired to the optional selection port or not.
    fn eval(field: &Field, mask: Option<&Field>, params: &Params) -> Field {
        Material
            .eval(
                Inputs::new(&[field], &[mask]),
                params,
                &ctx(field.width(), field.height()),
            )
            .expect("eval")
            .into_iter()
            .next()
            .expect("one output")
    }

    #[test]
    fn an_unmasked_material_covers_everything() {
        // The base material: the all-ones map that guarantees no cell is left unclaimed.
        let out = eval(&terrain(8, 8), None, &params("rock"));
        let weight = out.layer(&layers::material("rock")).expect("weight layer");
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(weight.get(x, y), Some(1.0), "cell ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_selection_becomes_the_weight() {
        let out = eval(
            &terrain(8, 8),
            Some(&selection(8, 8, 0.25)),
            &params("grass"),
        );
        let weight = out.layer(&layers::material("grass")).expect("weight layer");
        assert_eq!(weight.get(3, 4), Some(0.25));
    }

    #[test]
    fn weights_are_independent_of_the_materials_already_present() {
        // The decision the design turns on: this node never reduces another material's weight.
        // Overlapping coverage and a per-cell sum above one are expected, because the engine
        // normalizes and takes stacking order from its own material rather than from the maps.
        let rock = eval(&terrain(8, 8), None, &params("rock"));
        let both = eval(&rock, Some(&selection(8, 8, 1.0)), &params("snow"));

        assert_eq!(
            both.layer(&layers::material("rock"))
                .expect("rock survives")
                .get(2, 2),
            Some(1.0),
            "snow at full weight must not take anything from rock"
        );
        assert_eq!(
            both.layer(&layers::material("snow"))
                .expect("snow written")
                .get(2, 2),
            Some(1.0)
        );
    }

    #[test]
    fn a_weight_outside_the_range_is_clamped() {
        // A selector is free to hand over values that ran out of range upstream, and an engine
        // reading a negative weight would do something arbitrary with it.
        let over = eval(&terrain(4, 4), Some(&selection(4, 4, 3.5)), &params("a"));
        assert_eq!(
            over.layer(&layers::material("a")).expect("a").get(0, 0),
            Some(1.0)
        );

        let under = eval(&terrain(4, 4), Some(&selection(4, 4, -2.0)), &params("b"));
        assert_eq!(
            under.layer(&layers::material("b")).expect("b").get(0, 0),
            Some(0.0)
        );
    }

    #[test]
    fn every_other_layer_passes_through_untouched() {
        // The pass-through invariant: a Material node inserted anywhere leaves the terrain alone.
        let input = terrain(8, 8);
        let out = eval(&input, None, &params("rock"));
        assert_eq!(
            out.layer(layers::HEIGHT).map(Arc::as_ptr),
            input.layer(layers::HEIGHT).map(Arc::as_ptr),
            "the height layer should be the same allocation, not a copy"
        );
    }

    #[test]
    fn a_carried_mask_scopes_a_material_with_nothing_wired() {
        // Follows the mask contract every other node here honours: with no explicit selection,
        // the field's own mask applies. Worth knowing when relying on an unmasked node as the
        // all-ones base, since a mask riding the field will scope it.
        let masked = terrain(4, 4).with_layer(layers::MASK, Arc::new(Layer::filled(4, 4, 0.5)));
        let out = eval(&masked, None, &params("rock"));
        assert_eq!(
            out.layer(&layers::material("rock"))
                .expect("rock")
                .get(0, 0),
            Some(0.5)
        );
    }

    #[test]
    fn a_blank_or_padded_name_still_yields_a_referable_layer() {
        // A trailing space is invisible in the inspector but would make `material.rock ` a
        // different layer from `material.rock`.
        let padded = eval(&terrain(4, 4), None, &params("  rock  "));
        assert!(padded.layer(&layers::material("rock")).is_some());

        let blank = eval(&terrain(4, 4), None, &params("   "));
        assert!(
            blank.layer(&layers::material(DEFAULT_NAME)).is_some(),
            "an empty name falls back rather than producing a layer nothing can refer to"
        );
    }

    #[test]
    fn the_node_is_a_modifier_with_one_optional_input() {
        let spec = Material.spec();
        assert_eq!(spec.kind(), NodeKind::Modifier);
        assert_eq!(spec.inputs.len(), 2);
        assert!(
            !spec.inputs[0].optional && spec.inputs[1].optional,
            "the selection is optional, so an unmasked base material is a valid graph"
        );
    }

    #[test]
    fn it_is_registered() {
        let made = ymir_core::registry::make(TYPE_ID).expect("material is registered");
        assert_eq!(made.spec().type_id, TYPE_ID);
    }
}
