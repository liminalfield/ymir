//! Material: name and colour a selection, so it can be shown on the terrain (#267).
//!
//! One node per material. Its input is a **selection**, the `[0, 1]` field saying where the
//! material goes, and its parameters are the material's name and its preview colour. Its output is
//! that selection as a weight, so it can be tapped, previewed, or run onward into an export node
//! when you want that material's weight map on disk.
//!
//! It takes no terrain. An earlier arrangement threaded the heightfield through a chain of Material
//! nodes, which made the graph lie: the terrain came out exactly as it went in, so the wire looked
//! like a transformation while actually carrying an accumulating field. It also meant a different
//! terrain arriving mid-chain silently discarded every material before it. A material describes
//! where something is, not what the ground does, so it does not need the ground.
//!
//! Which materials are in play, in what order, and which are muted is a **MaterialSet**, and that
//! is a list in the editor rather than anything on the canvas. See `design/texturing.md`.
//!
//! A pure per-cell clamp: resolution- and world-independent, so it is `NO_WORLD` and
//! byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.material";

/// The default colour of a new Material node: a mid neutral.
///
/// Deliberately not a hue. Assigning distinct default colours is worth doing (a set of materials
/// should never come up as a red-versus-green pair, which is unreadable under the most common
/// colour vision deficiency), but doing it well means knowing what other materials are already in
/// the graph, which an operator cannot see. That belongs to the editor when it creates the node,
/// and is tracked as open decision 4 in the design note.
const DEFAULT_COLOR: [f64; 3] = [0.5, 0.5, 0.5];

/// Names and colours a selection. One input (the selection), one output (its weight).
#[derive(Clone)]
pub struct Material;

impl Operator for Material {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "material",
            inputs: vec![PortSpec::new("selection")],
            outputs: vec![PortSpec::new("out").selection()],
            // No name parameter. Every node already carries a display-name override, which is
            // editable in the inspector, serialized with the graph, and deliberately outside every
            // cache key because it is cosmetic. A material's name is exactly that, and a second
            // name field would leave the inspector showing two.
            params: vec![ParamSpec::new(
                "color",
                ParamKind::Color,
                ParamValue::Color(DEFAULT_COLOR),
            )],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());

        // A selector's [0, 1] rides on its height layer, which is the convention every node here
        // reading a selection follows.
        let selection = input.layer_or(layers::HEIGHT, 0.0);

        // Clamping is the node's whole job: it is what turns a selection into a weight. A selector
        // is free to hand over values that ran out of range upstream, and a weight outside [0, 1]
        // is not meaningful to anything that consumes one. Per-cell and pure, so byte-identical at
        // any thread count.
        let weight = Layer::from_par_fn(width, height, |x, y| {
            selection.get(x, y).unwrap_or(0.0).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(weight));
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

    /// A selection field: a selector writes its `[0, 1]` to the height layer, so that is where
    /// this reads one from.
    fn selection(values: &[f32]) -> Field {
        let n = values.len();
        Field::new(n, 1, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(n, 1, |x, _| values[x])),
        )
    }

    fn weight_of(field: &Field) -> Vec<f32> {
        let layer = field.layer(layers::HEIGHT).expect("height");
        (0..layer.width())
            .map(|x| layer.get(x, 0).unwrap_or(f32::NAN))
            .collect()
    }

    fn eval(input: &Field) -> Field {
        Material
            .eval(
                Inputs::required_only(&[input]),
                &Params::new(),
                &ctx(input.width(), input.height()),
            )
            .expect("eval")
            .into_iter()
            .next()
            .expect("one output")
    }

    #[test]
    fn a_selection_passes_through_as_the_weight() {
        let out = eval(&selection(&[0.0, 0.25, 1.0]));
        assert_eq!(weight_of(&out), vec![0.0, 0.25, 1.0]);
    }

    #[test]
    fn clamping_is_the_nodes_job() {
        // Turning a selection into a weight is the whole point of the node. A selector is free to
        // hand over values that ran out of range upstream, and a weight outside [0, 1] means
        // nothing to anything that consumes one.
        let out = eval(&selection(&[-2.0, 0.5, 4.0]));
        assert_eq!(weight_of(&out), vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn every_other_layer_passes_through_untouched() {
        let input =
            selection(&[0.5, 0.5]).with_layer(layers::FLOW, Arc::new(Layer::filled(2, 1, 0.7)));
        let out = eval(&input);
        assert_eq!(
            out.layer(layers::FLOW).map(Arc::as_ptr),
            input.layer(layers::FLOW).map(Arc::as_ptr),
            "an untouched layer should be the same allocation, not a copy"
        );
    }

    #[test]
    fn it_takes_a_selection_and_no_terrain() {
        // The shape the design turns on: a material says where something is, so it does not need
        // the ground. Threading terrain through made the graph claim a transformation that never
        // happened, and let a different terrain arriving discard the materials before it.
        let spec = Material.spec();
        assert_eq!(spec.kind(), NodeKind::Modifier);
        assert_eq!(spec.inputs.len(), 1, "one input, the selection");
        assert!(!spec.inputs[0].optional);
        assert_eq!(spec.outputs.len(), 1);
    }

    #[test]
    fn colour_is_its_only_parameter() {
        // The material's name is the node's own display name, which every node already has. A
        // `name` parameter beside it would show the inspector two name fields, and leave two
        // places to change one thing.
        let spec = Material.spec();
        let names: Vec<&str> = spec.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["color"]);
    }

    #[test]
    fn it_is_registered() {
        let made = ymir_core::registry::make(TYPE_ID).expect("material is registered");
        assert_eq!(made.spec().type_id, TYPE_ID);
    }
}
