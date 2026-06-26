//! Hydraulic erosion: water flowing over the terrain (virtual-pipes shallow-water model).
//!
//! This first step simulates only the water. Rain falls, flows downhill to lower neighbours
//! through "virtual pipes", and evaporates, leaving a `water` depth that shows where water
//! pools and runs. The terrain (`height`) is passed through unchanged; erosion and deposition
//! couple to it in a later step. Useful on its own as a wetness/flow map.
//!
//! The node has three outputs, each a clean standalone field: `heightfield` (the terrain,
//! unchanged this step), `water` (the water depth on its height layer), and `sediment`
//! (stubbed empty until the erosion step fills it). Each byproduct lives on its own port and
//! is tapped as needed — wired onward, or viewed by selecting the output in the preview.
//!
//! It is a grid (Eulerian) simulation after Mei et al. (2007): each cell exchanges water
//! with its four neighbours through pipes whose flow is driven by the difference in water
//! surface height. Every sub-step reads the previous full state and writes each cell from its
//! own neighbour reads (Jacobi), so the result is independent of cell iteration order and
//! deterministic, the same discipline the thermal node follows. Water leaving a cell is
//! exactly the water its neighbour receives, and no flux crosses the domain boundary, so the
//! water is mass-conserving apart from the rain that adds it and the evaporation that removes
//! it. The simulation runs in grid units for now (pipe length one cell); expressing the
//! physics in world units is a later, resolution-aware step.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.hydraulic_erosion";

/// Integration timestep. Small enough that the explicit flux update stays stable given the
/// outflow is also capped to the available water each step.
const DT: f32 = 0.1;
/// Gravity: accelerates flow down a water-surface gradient.
const GRAVITY: f32 = 9.81;
/// Virtual pipe cross-section, a flow-rate scale.
const PIPE_AREA: f32 = 1.0;
/// Pipe length: the cell spacing, in grid units for now (one cell).
const PIPE_LENGTH: f32 = 1.0;
/// Cell footprint area, converting a flux (volume/time) to a height change.
const CELL_AREA: f32 = PIPE_LENGTH * PIPE_LENGTH;

/// Default rain added to each cell per iteration (before the timestep).
const DEFAULT_RAIN: f64 = 0.01;
/// Default fraction of water evaporated per iteration (before the timestep).
const DEFAULT_EVAPORATION: f64 = 0.02;
/// Default simulation iterations.
const DEFAULT_ITERATIONS: i64 = 60;

/// Hydraulic erosion modifier: one input, three outputs (heightfield, water, sediment).
#[derive(Clone)]
pub struct HydraulicErosion;

impl Operator for HydraulicErosion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![PortSpec::new("in")],
            // The terrain on the primary port, plus a tap for each byproduct as a clean
            // standalone field. Sediment is stubbed until the erosion step produces it; flow
            // waits for a step that defines its scalar form (it is a vector underneath).
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("water"),
                PortSpec::new("sediment"),
            ],
            params: vec![
                ParamSpec::new(
                    "rain",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_RAIN),
                ),
                ParamSpec::new(
                    "evaporation",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_EVAPORATION),
                ),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 0, max: 1000 },
                    ParamValue::Int(DEFAULT_ITERATIONS),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();

        let rain = params.get_f64("rain", DEFAULT_RAIN) as f32;
        let evaporation = params.get_f64("evaporation", DEFAULT_EVAPORATION) as f32;
        let iterations = params
            .get_i64("iterations", DEFAULT_ITERATIONS)
            .clamp(0, 100_000) as usize;

        // The terrain is fixed in this step: water flows over it but does not yet cut it.
        let terrain = input.layer_or(layers::HEIGHT, 0.0);
        let bed = terrain.as_slice().to_vec();

        let sim = Sim { width, height };
        let mut state = WaterState::new(bed.len());

        for _ in 0..iterations {
            // The simulation is the slow node; poll cancellation each iteration so a
            // superseded preview aborts instead of running to completion.
            if ctx.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Rain adds water uniformly; flow redistributes it; evaporation removes a
            // fraction. Each is a full pass over the grid, in this order.
            for w in &mut state.water {
                *w += rain * DT;
            }
            update_flux(&bed, &mut state, &sim);
            update_water(&mut state, &sim);
            let keep = 1.0 - evaporation * DT;
            for w in &mut state.water {
                *w *= keep;
            }
        }

        let water = Arc::new(Layer::from_fn(width, height, |x, y| {
            state.water[y * width + x]
        }));
        let region = input.region();

        // Output 0, `heightfield`: the terrain unchanged this step (in == out). Clean — the
        // byproducts live on their own ports, not bundled here.
        let heightfield = input.clone();

        // Output 1, `water`: the water depth as a standalone field (on the height layer), so
        // it can be wired, shaped, viewed, or exported directly.
        let water_field = Field::new(width, height, region).with_layer(layers::HEIGHT, water);

        // Output 2, `sediment`: stubbed until the erosion step produces it. An empty field so
        // the port exists and the node's output shape is settled now.
        let sediment_field = Field::new(width, height, region)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(width, height, 0.0)));

        Ok(vec![heightfield, water_field, sediment_field])
    }
}

/// The grid dimensions, bundled so the per-cell helpers share one source of truth.
#[derive(Clone, Copy)]
struct Sim {
    width: usize,
    height: usize,
}

/// The mutable simulation state: water depth and the four outflow pipes per cell. The pipes
/// persist across iterations (their stored flow is the model's momentum).
struct WaterState {
    water: Vec<f32>,
    /// Outflow to the left, right, top (`y - 1`) and bottom (`y + 1`) neighbour.
    flux_l: Vec<f32>,
    flux_r: Vec<f32>,
    flux_t: Vec<f32>,
    flux_b: Vec<f32>,
}

impl WaterState {
    fn new(n: usize) -> Self {
        Self {
            water: vec![0.0; n],
            flux_l: vec![0.0; n],
            flux_r: vec![0.0; n],
            flux_t: vec![0.0; n],
            flux_b: vec![0.0; n],
        }
    }
}

/// Updates every cell's outflow pipes from the current water surface. Each pipe accelerates
/// with the surface-height drop toward its neighbour (never going negative: pipes only push
/// water out), then all four are scaled down together if they would drain more than the
/// cell's water this step, which keeps the depth non-negative and the sim stable. Reads only
/// the bed and water (never other pipes), and writes only its own cell, so it is
/// order-independent.
fn update_flux(bed: &[f32], state: &mut WaterState, sim: &Sim) {
    let (w, h) = (sim.width, sim.height);
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let surface = bed[c] + state.water[c];
            let outflow = |nx: i32, ny: i32, prev: f32| -> f32 {
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    return 0.0; // boundary: no outflow leaves the domain
                }
                let nc = ny as usize * w + nx as usize;
                let drop = surface - (bed[nc] + state.water[nc]);
                (prev + DT * PIPE_AREA * GRAVITY * drop / PIPE_LENGTH).max(0.0)
            };
            let (xi, yi) = (x as i32, y as i32);
            let mut fl = outflow(xi - 1, yi, state.flux_l[c]);
            let mut fr = outflow(xi + 1, yi, state.flux_r[c]);
            let mut ft = outflow(xi, yi - 1, state.flux_t[c]);
            let mut fb = outflow(xi, yi + 1, state.flux_b[c]);

            // Cap total outflow at the water actually present (as a volume over this step).
            let total = fl + fr + ft + fb;
            if total > 0.0 {
                let available = state.water[c] * CELL_AREA;
                let scale = (available / (total * DT)).min(1.0);
                fl *= scale;
                fr *= scale;
                ft *= scale;
                fb *= scale;
            }
            state.flux_l[c] = fl;
            state.flux_r[c] = fr;
            state.flux_t[c] = ft;
            state.flux_b[c] = fb;
        }
    }
}

/// Updates every cell's water depth from the net pipe flow: what flows in from the four
/// neighbours minus what flows out. Reads only the (already final) pipes, never another
/// cell's water, so it updates depth in place and stays order-independent. A cell's inflow
/// from a neighbour is that neighbour's outflow pipe pointing back at it.
fn update_water(state: &mut WaterState, sim: &Sim) {
    let (w, h) = (sim.width, sim.height);
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let outflow = state.flux_l[c] + state.flux_r[c] + state.flux_t[c] + state.flux_b[c];
            let mut inflow = 0.0;
            if x > 0 {
                inflow += state.flux_r[c - 1]; // left neighbour flowing right, into here
            }
            if x + 1 < w {
                inflow += state.flux_l[c + 1]; // right neighbour flowing left
            }
            if y > 0 {
                inflow += state.flux_b[c - w]; // top neighbour flowing down
            }
            if y + 1 < h {
                inflow += state.flux_t[c + w]; // bottom neighbour flowing up
            }
            // Guard against a tiny negative from floating-point rounding.
            state.water[c] = (state.water[c] + DT * (inflow - outflow) / CELL_AREA).max(0.0);
        }
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(HydraulicErosion) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{EvalContext, Region};

    fn ctx() -> EvalContext {
        EvalContext::new(32, 32, Region::UNIT, 0)
    }

    /// A bowl: high at the edges, low in the middle, so water should drain inward.
    fn bowl_field() -> Field {
        let layer = Layer::from_fn(32, 32, |x, y| {
            let (cx, cy) = (15.5_f32, 15.5_f32);
            let r = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            (r / 22.0).min(1.0)
        });
        Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    /// Builds Params from `(name, value)` float pairs, with an explicit iteration count.
    fn params(rain: f64, evaporation: f64, iterations: i64) -> Params {
        Params::new()
            .with("rain", ParamValue::Float(rain))
            .with("evaporation", ParamValue::Float(evaporation))
            .with("iterations", ParamValue::Int(iterations))
    }

    /// Output 0, the `heightfield` (terrain).
    fn run(input: &Field, params: &Params) -> Field {
        HydraulicErosion
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
            .remove(0)
    }

    /// Output 1, the `water` depth (on its height layer).
    fn water(input: &Field, params: &Params) -> Field {
        HydraulicErosion
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
            .remove(1)
    }

    #[test]
    fn is_deterministic() {
        let input = bowl_field();
        let p = params(0.02, 0.01, 20);
        assert_eq!(
            water(&input, &p)
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash(),
            water(&input, &p)
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash(),
        );
    }

    #[test]
    fn terrain_passes_through_unchanged() {
        // This step simulates water only; the height layer must be untouched.
        let input = bowl_field();
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let after = run(&input, &params(0.02, 0.02, 20))
            .layer(layers::HEIGHT)
            .unwrap()
            .content_hash();
        assert_eq!(before, after, "step 1 must not change the terrain");
    }

    #[test]
    fn water_is_conserved_without_evaporation() {
        // With no evaporation, the only water is what the rain adds: rain * dt per cell per
        // iteration. Flow moves it around but neither creates nor destroys it, and none
        // leaves the domain, so the total matches the rain put in.
        let input = bowl_field();
        let (rain, iters) = (0.02_f64, 15_i64);
        let out = water(&input, &params(rain, 0.0, iters));
        let total: f64 = out
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum();
        let expected = rain * f64::from(DT) * iters as f64 * (32.0 * 32.0);
        assert!(
            (total - expected).abs() < 1e-2,
            "water not conserved: {total} vs {expected}"
        );
    }

    #[test]
    fn water_pools_in_the_low_ground() {
        // Uniform rain over a bowl: water drains toward the centre, so the middle ends up
        // deeper than a corner.
        let input = bowl_field();
        let out = water(&input, &params(0.02, 0.01, 60));
        let depth = out.layer(layers::HEIGHT).unwrap();
        let centre = depth.get(16, 16).unwrap();
        let corner = depth.get(1, 1).unwrap();
        assert!(
            centre > corner,
            "water should pool in the low centre: centre {centre}, corner {corner}"
        );
    }

    #[test]
    fn zero_iterations_leaves_no_water() {
        let input = bowl_field();
        let out = water(&input, &params(0.02, 0.02, 0));
        let total: f32 = out.layer(layers::HEIGHT).unwrap().as_slice().iter().sum();
        assert_eq!(total, 0.0, "no iterations should produce no water");
    }

    #[test]
    fn spec_has_the_three_outputs() {
        let spec = HydraulicErosion.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "water", "sediment"]);
    }

    #[test]
    fn heightfield_output_is_clean_terrain() {
        // Output 0 is the terrain only; byproducts live on their own ports, not bundled here.
        let input = bowl_field();
        let out = run(&input, &params(0.02, 0.01, 30));
        assert!(
            out.layer(layers::WATER).is_none(),
            "the heightfield output must not carry a water layer"
        );
    }

    #[test]
    fn sediment_output_is_empty_in_this_step() {
        // Sediment is stubbed until the erosion step; its field is all zero for now.
        let input = bowl_field();
        let outs = HydraulicErosion
            .eval(
                Inputs::required_only(&[&input]),
                &params(0.02, 0.01, 30),
                &ctx(),
            )
            .unwrap();
        let total: f32 = outs[2]
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .sum();
        assert_eq!(total, 0.0, "sediment is stubbed empty until erosion");
    }

    #[test]
    fn cancelled_simulation_aborts() {
        let cancel = ymir_core::CancelToken::new();
        cancel.cancel();
        let ctx = EvalContext::new(32, 32, Region::UNIT, 0).with_cancel(cancel);
        let input = bowl_field();
        let err = HydraulicErosion
            .eval(
                Inputs::required_only(&[&input]),
                &params(0.02, 0.02, 50),
                &ctx,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = bowl_field();
        let p = params(0.02, 0.01, 10);
        let made = ymir_core::registry::make(TYPE_ID).expect("hydraulic operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &p, &ctx())
            .unwrap();
        let direct = water(&input, &p);
        assert_eq!(
            via_registry[1]
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash(),
            direct.layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }

    #[test]
    fn spec_is_a_modifier() {
        assert_eq!(
            HydraulicErosion.spec().kind(),
            ymir_core::NodeKind::Modifier
        );
        assert_eq!(HydraulicErosion.spec().type_id, TYPE_ID);
    }
}
