//! Flow (Water Sim) selector: selects drainage channels from a shallow-water simulation.
//!
//! The counterpart to the grid [`crate::flow_select::FlowSelect`] node. Where that accumulates
//! drainage by a discrete flow-direction rule (and so is prone to fill fans, breach scars, and
//! step-ladder aliasing on raw noise), this floods the terrain with rain and runs a shallow-water
//! simulation (Mei et al. 2007 virtual pipes, in [`crate::water_sim`]). Water pools in pits and
//! spills on its own, and the flux field is continuous, so the emergent channels carry none of
//! those artifacts. The two nodes sit side by side for A/B comparison.
//!
//! Output matches the other selectors: a `[0, 1]` selection on the **`height`** layer, high where
//! water pools over the run (channels, valley floors, basins), softened to zero outside the chosen
//! band, so the rest of the toolset shapes and applies it. Like them it derives the selection from
//! scratch and is not mask-aware.
//!
//! The simulation is resolution-dependent physics (like erosion): a finer build resolves more of
//! the drainage network, so the result genuinely changes with resolution, and `iterations` wants
//! to grow with it so water still routes across the domain. A low-resolution preview therefore
//! approximates the full build rather than reproducing it.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::hydrology::{Grid, log_normalize_span};
use crate::water_sim::{SimParams, simulate_flow};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.flow_water";

/// Default band over the normalized `[0, 1]` flow: selects the upper range (channels), top open
/// so the largest rivers are included. Matches the grid Flow selector.
const DEFAULT_MIN: f64 = 0.3;
const DEFAULT_MAX: f64 = 1.0;
const DEFAULT_FALLOFF: f64 = 0.15;
/// Default rainfall per step: the depth of water added to every cell each iteration.
const DEFAULT_RAIN: f64 = 0.01;
/// Default evaporation per step: the fraction of each cell's water removed each iteration. Drains
/// pooled water over time so lakes do not grow without bound and the channels stay legible.
const DEFAULT_EVAPORATION: f64 = 0.02;
/// Default number of simulation steps. Enough for water to route across a preview-sized domain;
/// raise it at build resolution so flow still crosses the larger grid.
const DEFAULT_ITERATIONS: i64 = 200;

/// Flow (Water Sim) selector: one input, one output. Writes the selection to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct FlowWater;

impl Operator for FlowWater {
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
                    "rain",
                    ParamKind::Float {
                        min: 0.0001,
                        max: 0.1,
                    },
                    ParamValue::Float(DEFAULT_RAIN),
                ),
                ParamSpec::new(
                    "evaporation",
                    ParamKind::Float { min: 0.0, max: 0.2 },
                    ParamValue::Float(DEFAULT_EVAPORATION),
                ),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 1, max: 5000 },
                    ParamValue::Int(DEFAULT_ITERATIONS),
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
        let sim = SimParams {
            rain: params.get_f64("rain", DEFAULT_RAIN) as f32,
            evaporation: params
                .get_f64("evaporation", DEFAULT_EVAPORATION)
                .clamp(0.0, 1.0) as f32,
            iterations: params.get_i64("iterations", DEFAULT_ITERATIONS).max(0) as usize,
        };

        // Run the shallow-water simulation over the input terrain, accumulating water depth, then
        // log-stretch across the actual range so the driest ground reads 0 and the wettest reads 1
        // (making the band meaningful). Cancellation is surfaced as an error, matching erosion.
        let grid = Grid { width, height };
        let bed = h.as_slice().to_vec();
        let acc =
            simulate_flow(&bed, &grid, &sim, || ctx.is_cancelled()).ok_or(Error::Cancelled)?;
        let flow = log_normalize_span(&acc);

        // Band-select the normalized flow, softening over `falloff` at each edge (the same
        // trapezoid as the other selectors).
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
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(FlowWater) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    /// A ramp high at the top (y = 0), low at the bottom, so water accumulates downhill.
    fn ramp(size: usize) -> Field {
        let layer = Layer::from_fn(size, size, |_, y| 1.0 - y as f32 / (size - 1) as f32);
        Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    /// A flat plateau with a deep central pit, so water pools in the pit and the well-drained
    /// corners stay dry.
    fn basin(size: usize) -> Field {
        let mut z = vec![0.5_f32; size * size];
        for y in size * 3 / 8..size * 5 / 8 {
            for x in size * 3 / 8..size * 5 / 8 {
                z[y * size + x] = 0.0;
            }
        }
        Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::from_vec(size, size, z)))
    }

    fn select(input: &Field, params: Params) -> Field {
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 0)
            .with_world_extent(256.0);
        FlowWater
            .eval(Inputs::required_only(&[input]), &params, &ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn the_wettest_low_ground_is_selected_over_the_dry_corners() {
        // On a basin, water pools in the pit and the corners drain off the open edges. With the
        // band raised to isolate the wettest ground, the pit selects while a dry corner does not,
        // proving the simulated depth drives the selection.
        let size = 32;
        let out = select(
            &basin(size),
            Params::new().with("min", ParamValue::Float(0.9)),
        );
        let mid = size / 2;
        assert!(at(&out, mid, mid) > 0.5, "the pooled pit should select");
        assert!(
            at(&out, 1, 1) < 0.2,
            "a well-drained dry corner should not select"
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = select(&ramp(32), Params::new());
        assert!(out.layer(layers::MASK).is_none());
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(32);
        input.set_layer("debris", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = select(&input, Params::new());
        assert_eq!(out.layer("debris").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = select(&ramp(32), Params::new());
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(24);
        assert_eq!(
            select(&input, Params::new()).content_hash(),
            select(&input, Params::new()).content_hash()
        );
    }

    #[test]
    fn cancellation_is_reported() {
        let input = ramp(16);
        let cancel = ymir_core::CancelToken::new();
        cancel.cancel();
        let ctx = EvalContext::new(16, 16, Region::UNIT, 0)
            .with_world_extent(256.0)
            .with_cancel(cancel);
        let err = FlowWater
            .eval(Inputs::required_only(&[&input]), &Params::new(), &ctx)
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn spec_is_a_selector_modifier() {
        assert_eq!(FlowWater.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(FlowWater.spec().type_id, TYPE_ID);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = ymir_core::registry::make(TYPE_ID).expect("flow water selector is registered");
        let input = ramp(16);
        let params = Params::new();
        let ctx = EvalContext::new(16, 16, Region::UNIT, 0).with_world_extent(256.0);
        let via = made
            .eval(Inputs::required_only(&[&input]), &params, &ctx)
            .unwrap();
        let direct = FlowWater
            .eval(Inputs::required_only(&[&input]), &params, &ctx)
            .unwrap();
        assert_eq!(
            via[0].layer(layers::HEIGHT).unwrap().content_hash(),
            direct[0].layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }
}
