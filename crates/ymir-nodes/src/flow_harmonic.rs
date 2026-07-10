//! Flow selector (harmonic): the isotropic-flat-resolution variant of [`crate::flow_select`].
//!
//! Identical to the Flow selector in every respect — a `[0, 1]` drainage-channel selection on the
//! `height` layer, computed on demand from the input — except how it gives filled flats a drainage
//! direction. Where Flow uses a geodesic-Euclidean distance transform (fast, but leaving faint
//! octagonal creases at roughly 22.5 degrees), this uses a smooth harmonic "flow potential"
//! ([`crate::hydrology::resolve_flats_harmonic`]), which carries no grid- or diagonal-direction
//! bias at all, at the cost of an iterative solve. It exists alongside Flow so the two can be
//! wired side by side and compared; the params are the same so a graph can swap one for the other.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::hydrology::{
    Grid, drainage_area_mfd, fill_depressions, log_normalize_span, resolve_flats_harmonic,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.flow_harmonic";

/// Default band over the normalized `[0, 1]` flow: selects the upper range (channels) out of the
/// box, with the top open so the largest rivers are included.
const DEFAULT_MIN: f64 = 0.3;
const DEFAULT_MAX: f64 = 1.0;
const DEFAULT_FALLOFF: f64 = 0.15;
/// Default flow concentration (the MFD slope exponent), matching the Flow and Stream nodes.
const DEFAULT_CONCENTRATION: f64 = 1.5;
/// Default maximum depression fill, in working height units, matching the Flow and Stream nodes.
const DEFAULT_FILL: f64 = 0.05;

/// Harmonic flow selector: one input, one output. Writes the selection to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct FlowHarmonic;

impl Operator for FlowHarmonic {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MIN),
                ),
                ParamSpec::new(
                    "max",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MAX),
                ),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FALLOFF),
                ),
                ParamSpec::new(
                    "concentration",
                    ParamKind::Float { min: 1.0, max: 6.0 },
                    ParamValue::Float(DEFAULT_CONCENTRATION),
                ),
                ParamSpec::new(
                    "fill",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FILL),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let min = params.get_f64("min", DEFAULT_MIN) as f32;
        let max = params.get_f64("max", DEFAULT_MAX) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0) as f32;
        let concentration = params.get_f64("concentration", DEFAULT_CONCENTRATION) as f32;
        let max_fill = params.get_f64("fill", DEFAULT_FILL) as f32;

        // Identical to the Flow selector except that the filled flats are resolved with the
        // isotropic harmonic flow potential rather than a distance transform, so the drainage
        // carries no residual grid- or diagonal-direction bias.
        let grid = Grid { width, height };
        let bed = h.as_slice().to_vec();
        let cell_area = {
            let m = ctx.meters_per_cell() as f32;
            (m * m).max(1e-12)
        };
        let filled = resolve_flats_harmonic(&fill_depressions(&bed, &grid, max_fill), &grid);
        let area = drainage_area_mfd(&filled, &grid, concentration, cell_area);
        // Stretch across the actual range so ridges read 0 and the largest channels read 1.
        let flow = log_normalize_span(&area);

        // Band-select the normalized flow, softening over `falloff` at each edge (the same
        // trapezoid as the Slope and Flow selectors).
        let selection = Layer::from_fn(width, height, |x, y| {
            let f = flow[y * width + x];
            let lower = smoothstep(min - falloff, min, f);
            let upper = 1.0 - smoothstep(max, max + falloff, f);
            (lower * upper).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(FlowHarmonic) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_select::FlowSelect;
    use ymir_core::Region;

    /// A ramp high at the top (y = 0), low at the bottom, so flow accumulates downhill.
    fn ramp(size: usize) -> Field {
        let layer = Layer::from_fn(size, size, |_, y| 1.0 - y as f32 / (size - 1) as f32);
        Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    fn ctx(field: &Field) -> EvalContext {
        EvalContext::new(field.width(), field.height(), field.region(), 0).with_world_extent(256.0)
    }

    fn select(input: &Field) -> Field {
        FlowHarmonic
            .eval(Inputs::required_only(&[input]), &Params::new(), &ctx(input))
            .unwrap()
            .remove(0)
    }

    #[test]
    fn high_flow_channels_are_selected_and_ridges_are_not() {
        let out = select(&ramp(32));
        let at = |x, y| out.layer(layers::HEIGHT).unwrap().get(x, y).unwrap();
        // The column's drainage collects near the bottom (high flow, selected); the ridge top has
        // no upstream and is rejected.
        assert!(at(16, 30) > 0.5, "high-flow channel should select");
        assert!(
            at(16, 0) < 0.1,
            "the no-upstream ridge top should not select"
        );
    }

    #[test]
    fn stays_in_unit_range() {
        let out = select(&ramp(32));
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = select(&ramp(32));
        assert!(out.layer(layers::MASK).is_none());
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(32);
        input.set_layer("debris", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = FlowHarmonic
            .eval(
                Inputs::required_only(&[&input]),
                &Params::new(),
                &ctx(&input),
            )
            .unwrap()
            .remove(0);
        assert_eq!(out.layer("debris").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(32);
        assert_eq!(select(&input).content_hash(), select(&input).content_hash());
    }

    #[test]
    fn differs_from_the_distance_flow_selector() {
        // The whole point of the node: on terrain with flats to resolve, the harmonic potential
        // yields a different drainage selection than the distance-transform Flow selector. A basin
        // (plateau + deep pit + one outlet) forces flats that the two resolvers treat differently.
        let size = 24;
        let mut z = vec![0.6_f32; size * size];
        for y in 8..16 {
            for x in 8..16 {
                z[y * size + x] = 0.0;
            }
        }
        z[(size - 1) * size + size / 2] = 0.0;
        let field = Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::from_vec(size, size, z)));

        let harmonic = FlowHarmonic
            .eval(
                Inputs::required_only(&[&field]),
                &Params::new(),
                &ctx(&field),
            )
            .unwrap()
            .remove(0);
        let distance = FlowSelect
            .eval(
                Inputs::required_only(&[&field]),
                &Params::new(),
                &ctx(&field),
            )
            .unwrap()
            .remove(0);
        assert_ne!(
            harmonic.layer(layers::HEIGHT).unwrap().content_hash(),
            distance.layer(layers::HEIGHT).unwrap().content_hash(),
            "the harmonic selector should resolve flats differently from the distance one"
        );
    }

    #[test]
    fn spec_is_a_selector_modifier() {
        assert_eq!(FlowHarmonic.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(FlowHarmonic.spec().type_id, TYPE_ID);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made =
            ymir_core::registry::make(TYPE_ID).expect("harmonic flow selector is registered");
        let input = ramp(16);
        let via = made
            .eval(
                Inputs::required_only(&[&input]),
                &Params::new(),
                &ctx(&input),
            )
            .unwrap();
        let direct = select(&input);
        assert_eq!(
            via[0].layer(layers::HEIGHT).unwrap().content_hash(),
            direct.layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }
}
