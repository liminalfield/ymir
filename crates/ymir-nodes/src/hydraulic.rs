//! Hydraulic erosion: water carving the terrain (virtual-pipes shallow-water model).
//!
//! Rain falls, flows downhill through "virtual pipes", picks up and drops sediment, and
//! evaporates. Fast-moving water over steep ground can carry more sediment than it holds, so
//! it dissolves the bed and carves valleys; where it slows or pools its capacity falls and the
//! load drops out, building fans and filling hollows. This is the process that turns an fBm
//! base into landscape: drainage networks, ridgelines, alluvial flats.
//!
//! Three outputs, each a clean standalone field: `heightfield` (the eroded terrain), `water`
//! (the water depth on its height layer), and `sediment` (the suspended-sediment load). Each
//! byproduct lives on its own port and is tapped as needed — wired onward, or viewed by
//! selecting the output in the preview.
//!
//! It is a grid (Eulerian) simulation after Mei et al. (2007). Each iteration: rain adds
//! water; pipes between neighbours move it by water-surface gradient; a velocity field falls
//! out of the flux; the velocity and local tilt set a sediment *capacity* that erodes the bed
//! when it is under-saturated and deposits when over; the suspended sediment is advected along
//! the velocity; and a fraction evaporates. Every sub-step reads the previous full state and
//! writes each cell from its own neighbour reads (Jacobi), so the result is independent of
//! cell iteration order and deterministic, the same discipline the thermal node follows. The
//! water itself is mass-conserving apart from rain and evaporation. The simulation runs in
//! grid units for now (pipe length one cell); expressing the physics in world units is a
//! later, resolution-aware step, as is tuning the default rates.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.hydraulic_erosion";

/// Integration timestep. Small enough that the explicit updates stay stable given the outflow
/// is capped to the available water and the per-step bed change is clamped.
const DT: f32 = 0.1;
/// Gravity: accelerates flow down a water-surface gradient.
const GRAVITY: f32 = 9.81;
/// Virtual pipe cross-section, a flow-rate scale.
const PIPE_AREA: f32 = 1.0;
/// Pipe length: the cell spacing, in grid units for now (one cell).
const PIPE_LENGTH: f32 = 1.0;
/// Cell footprint area, converting a flux (volume/time) to a height change.
const CELL_AREA: f32 = PIPE_LENGTH * PIPE_LENGTH;

/// Minimum local tilt fed to the capacity model, so water still carries (a little) over flat
/// ground rather than dropping its whole load the instant the slope vanishes.
const MIN_TILT: f32 = 0.05;
/// Velocity magnitude is clamped to this before driving capacity and advection, so a thin,
/// fast film cannot blow the capacity up or jump the advection more than a cell or so.
const MAX_SPEED: f32 = 2.0;
/// Safety cap on how much a single cell's bed may erode or deposit in one step. Bounds the
/// explicit integrator so extreme parameters cannot make the terrain explode; applied to the
/// exchanged amount so erosion and deposition stay mass-conserving between bed and sediment.
const MAX_BED_DELTA: f32 = 0.05;
/// Below this water depth a cell has no meaningful velocity (avoids dividing by ~0).
const MIN_DEPTH: f32 = 1e-4;

/// Default rain added to each cell per iteration (before the timestep).
const DEFAULT_RAIN: f64 = 0.01;
/// Default fraction of water evaporated per iteration (before the timestep).
const DEFAULT_EVAPORATION: f64 = 0.02;
/// Default sediment capacity coefficient (how much load fast, steep water can carry).
const DEFAULT_CAPACITY: f64 = 0.05;
/// Default erosion (dissolving) rate: fraction of the capacity deficit cut from the bed.
const DEFAULT_EROSION: f64 = 0.1;
/// Default deposition rate: fraction of the over-capacity load dropped back to the bed.
const DEFAULT_DEPOSITION: f64 = 0.05;
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
            inputs: vec![PortSpec::new("in"), PortSpec::optional("mask")],
            // The eroded terrain on the primary port, plus a tap for each byproduct as a clean
            // standalone field.
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
                    "capacity",
                    ParamKind::Float { min: 0.0, max: 4.0 },
                    ParamValue::Float(DEFAULT_CAPACITY),
                ),
                ParamSpec::new(
                    "erosion",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_EROSION),
                ),
                ParamSpec::new(
                    "deposition",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_DEPOSITION),
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

        let rates = Rates {
            rain: params.get_f64("rain", DEFAULT_RAIN) as f32,
            evaporation: params.get_f64("evaporation", DEFAULT_EVAPORATION) as f32,
            capacity: params.get_f64("capacity", DEFAULT_CAPACITY) as f32,
            erosion: params.get_f64("erosion", DEFAULT_EROSION) as f32,
            deposition: params.get_f64("deposition", DEFAULT_DEPOSITION) as f32,
        };
        let iterations = params
            .get_i64("iterations", DEFAULT_ITERATIONS)
            .clamp(0, 100_000) as usize;

        // The original terrain, kept for the mask composite at the end.
        let source = input.layer_or(layers::HEIGHT, 0.0);
        // Optional mask (soft-layer contract, like Thermal and Stream): an explicit mask input
        // wins (its height is the selection), else the input's own mask layer, else erode
        // everywhere. Never gates the connection.
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        let sim = Sim { width, height };
        let mut state = SimState::new(source.as_slice());

        for _ in 0..iterations {
            // The simulation is the slow node; poll cancellation each iteration so a
            // superseded preview aborts instead of running to completion.
            if ctx.is_cancelled() {
                return Err(Error::Cancelled);
            }
            for w in &mut state.water {
                *w += rates.rain * DT;
            }
            update_flux(&mut state, &sim);
            update_water_and_velocity(&mut state, &sim);
            erode_and_deposit(&mut state, &sim, &rates);
            advect_sediment(&mut state, &sim);
            // Evaporation: the water shrinks, and the share of its suspended load that the
            // departed water can no longer carry settles onto the bed. Without this the
            // eroded material stays orphaned in suspension and is lost rather than deposited,
            // so erosion only removes terrain instead of redistributing it.
            let keep = 1.0 - rates.evaporation * DT;
            let settle = 1.0 - keep;
            for ((water, load), bed) in state
                .water
                .iter_mut()
                .zip(state.sediment.iter_mut())
                .zip(state.terrain.iter_mut())
            {
                let dropped = *load * settle;
                *bed += dropped;
                *load -= dropped;
                *water *= keep;
            }
        }

        let region = input.region();
        let layer = |values: Vec<f32>| {
            Arc::new(Layer::from_fn(width, height, |x, y| values[y * width + x]))
        };

        // Output 0, `heightfield`: the eroded terrain composited over the original through the
        // mask (confine), passing the input's other layers through. A fully masked-out cell
        // keeps its original height, masked-in takes the eroded height, partials blend; the
        // water and sediment taps below report the full simulation. Outputs 1 and 2 are the
        // water depth and suspended sediment as clean standalone fields.
        let mut heightfield = input.clone();
        heightfield.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(width, height, |x, y| {
                let idx = y * width + x;
                let original = source.get(x, y).unwrap_or(0.0);
                let m = mask.get(x, y).unwrap_or(1.0);
                original + (state.terrain[idx] - original) * m
            })),
        );
        let water_field =
            Field::new(width, height, region).with_layer(layers::HEIGHT, layer(state.water));
        let sediment_field =
            Field::new(width, height, region).with_layer(layers::HEIGHT, layer(state.sediment));

        Ok(vec![heightfield, water_field, sediment_field])
    }
}

/// The per-evaluation rates, read once from the params.
#[derive(Clone, Copy)]
struct Rates {
    rain: f32,
    evaporation: f32,
    capacity: f32,
    erosion: f32,
    deposition: f32,
}

/// The grid dimensions, bundled so the per-cell helpers share one source of truth.
#[derive(Clone, Copy)]
struct Sim {
    width: usize,
    height: usize,
}

/// The mutable simulation state. The bed (`terrain`) is now cut and filled by erosion; `water`
/// and `sediment` are the depth and suspended load; the four `flux_*` pipes carry water between
/// neighbours and persist across iterations (their stored flow is the model's momentum);
/// `vel_x`/`vel_y` is the per-cell velocity derived from the flux each step.
struct SimState {
    terrain: Vec<f32>,
    water: Vec<f32>,
    sediment: Vec<f32>,
    /// Outflow to the left, right, top (`y - 1`) and bottom (`y + 1`) neighbour.
    flux_l: Vec<f32>,
    flux_r: Vec<f32>,
    flux_t: Vec<f32>,
    flux_b: Vec<f32>,
    vel_x: Vec<f32>,
    vel_y: Vec<f32>,
}

impl SimState {
    fn new(terrain: &[f32]) -> Self {
        let n = terrain.len();
        Self {
            terrain: terrain.to_vec(),
            water: vec![0.0; n],
            sediment: vec![0.0; n],
            flux_l: vec![0.0; n],
            flux_r: vec![0.0; n],
            flux_t: vec![0.0; n],
            flux_b: vec![0.0; n],
            vel_x: vec![0.0; n],
            vel_y: vec![0.0; n],
        }
    }
}

/// Updates every cell's outflow pipes from the current water surface (bed + water). Each pipe
/// accelerates with the surface-height drop toward its neighbour (never negative: pipes only
/// push water out), then all four are scaled together if they would drain more than the cell's
/// water this step, which keeps the depth non-negative and the sim stable. Reads only the bed
/// and water (never other pipes), writing only its own cell, so it is order-independent.
fn update_flux(state: &mut SimState, sim: &Sim) {
    let (w, h) = (sim.width, sim.height);
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let surface = state.terrain[c] + state.water[c];
            let outflow = |nx: i32, ny: i32, prev: f32| -> f32 {
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    return 0.0; // boundary: no outflow leaves the domain
                }
                let nc = ny as usize * w + nx as usize;
                let drop = surface - (state.terrain[nc] + state.water[nc]);
                (prev + DT * PIPE_AREA * GRAVITY * drop / PIPE_LENGTH).max(0.0)
            };
            let (xi, yi) = (x as i32, y as i32);
            let mut fl = outflow(xi - 1, yi, state.flux_l[c]);
            let mut fr = outflow(xi + 1, yi, state.flux_r[c]);
            let mut ft = outflow(xi, yi - 1, state.flux_t[c]);
            let mut fb = outflow(xi, yi + 1, state.flux_b[c]);

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

/// Updates every cell's water depth from the net pipe flow and derives its velocity. Depth
/// changes by inflow (each neighbour's pipe pointing back) minus outflow; velocity is the
/// horizontal/vertical water passing through divided by the mean depth over the step (so a
/// shallow cell does not read as arbitrarily fast — guarded below `MIN_DEPTH`). Reads only the
/// final pipes and the cell's own water, so it updates in place and stays order-independent.
fn update_water_and_velocity(state: &mut SimState, sim: &Sim) {
    let (w, h) = (sim.width, sim.height);
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let (fl, fr, ft, fb) = (
                state.flux_l[c],
                state.flux_r[c],
                state.flux_t[c],
                state.flux_b[c],
            );
            let outflow = fl + fr + ft + fb;
            let mut inflow = 0.0;
            // Neighbour pipes pointing at this cell.
            let (l, r, t, b) = (x > 0, x + 1 < w, y > 0, y + 1 < h);
            if l {
                inflow += state.flux_r[c - 1];
            }
            if r {
                inflow += state.flux_l[c + 1];
            }
            if t {
                inflow += state.flux_b[c - w];
            }
            if b {
                inflow += state.flux_t[c + w];
            }

            let depth_before = state.water[c];
            let depth_after = (depth_before + DT * (inflow - outflow) / CELL_AREA).max(0.0);
            let mean_depth = 0.5 * (depth_before + depth_after);

            // Net water passing through the cell in each axis (Mei et al. §3.3).
            let in_l = if l { state.flux_r[c - 1] } else { 0.0 };
            let in_r = if r { state.flux_l[c + 1] } else { 0.0 };
            let in_t = if t { state.flux_b[c - w] } else { 0.0 };
            let in_b = if b { state.flux_t[c + w] } else { 0.0 };
            let dw_x = 0.5 * (in_l - fl + fr - in_r);
            let dw_y = 0.5 * (in_t - ft + fb - in_b);

            let (vx, vy) = if mean_depth > MIN_DEPTH {
                (
                    dw_x / (PIPE_LENGTH * mean_depth),
                    dw_y / (PIPE_LENGTH * mean_depth),
                )
            } else {
                (0.0, 0.0)
            };

            state.water[c] = depth_after;
            state.vel_x[c] = vx;
            state.vel_y[c] = vy;
        }
    }
}

/// Erodes or deposits at every cell from the sediment-capacity model. Capacity rises with the
/// local tilt and the water speed; where the suspended load is under capacity the bed
/// dissolves into suspension, where it is over the surplus drops back to the bed. The exchange
/// is computed from the start-of-step bed (read-only) into a scratch delta, then applied, so it
/// is order-independent, and the amount is clamped so a single step cannot blow the bed up,
/// with bed and sediment moving by the same amount so the pair is conserved.
fn erode_and_deposit(state: &mut SimState, sim: &Sim, rates: &Rates) {
    let (w, h) = (sim.width, sim.height);
    // Scratch: signed amount moved from bed into suspension at each cell (negative = deposit).
    let mut exchange = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let speed = state.vel_x[c].hypot(state.vel_y[c]).min(MAX_SPEED);
            let tilt = local_tilt(&state.terrain, x, y, sim).max(MIN_TILT);
            let capacity = rates.capacity * tilt * speed;

            let load = state.sediment[c];
            let amount = if capacity > load {
                // Under capacity: dissolve bed into suspension.
                (rates.erosion * (capacity - load)).min(MAX_BED_DELTA)
            } else {
                // Over capacity: drop load back to the bed (negative exchange). Never deposit
                // more than is actually suspended.
                -(rates.deposition * (load - capacity))
                    .min(load)
                    .min(MAX_BED_DELTA)
            };
            exchange[c] = amount;
        }
    }
    for ((bed, load), &moved) in state
        .terrain
        .iter_mut()
        .zip(state.sediment.iter_mut())
        .zip(&exchange)
    {
        *bed -= moved;
        *load += moved;
    }
}

/// The sine of the local tilt angle from the bed gradient (central differences in grid units,
/// one-sided at the boundary). `sin(atan(slope)) = slope / sqrt(1 + slope^2)`, so it is bounded
/// in `[0, 1)` and well behaved on steep ground.
fn local_tilt(terrain: &[f32], x: usize, y: usize, sim: &Sim) -> f32 {
    let (w, h) = (sim.width, sim.height);
    let at = |cx: usize, cy: usize| terrain[cy * w + cx];
    let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w - 1));
    let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h - 1));
    // Divide by the actual span (1 or 2 cells at the boundary) so edges are not exaggerated.
    let gx = (at(xp, y) - at(xm, y)) / (xp - xm).max(1) as f32;
    let gy = (at(x, yp) - at(x, ym)) / (yp - ym).max(1) as f32;
    let slope = gx.hypot(gy);
    slope / (1.0 + slope * slope).sqrt()
}

/// Advects the suspended sediment along the velocity field: each cell pulls the load from where
/// it was a step ago (`pos - velocity * dt`), bilinearly sampled. The backtrace reads the
/// previous sediment (a snapshot) and writes a fresh buffer, so it is order-independent.
fn advect_sediment(state: &mut SimState, sim: &Sim) {
    let (w, h) = (sim.width, sim.height);
    let mut moved = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let src_x = x as f32 - state.vel_x[c] * DT;
            let src_y = y as f32 - state.vel_y[c] * DT;
            moved[c] = sample_bilinear(&state.sediment, sim, src_x, src_y);
        }
    }
    state.sediment = moved;
}

/// Bilinearly samples `grid` at the (clamped) continuous position `(x, y)` in cell units.
fn sample_bilinear(grid: &[f32], sim: &Sim, x: f32, y: f32) -> f32 {
    let (w, h) = (sim.width, sim.height);
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let g = |cx: usize, cy: usize| grid[cy * w + cx];
    let top = g(x0, y0) * (1.0 - tx) + g(x1, y0) * tx;
    let bottom = g(x0, y1) * (1.0 - tx) + g(x1, y1) * tx;
    top * (1.0 - ty) + bottom * ty
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

    /// A slope from high (left) to low (right), so water runs across it and erodes.
    fn ramp_field() -> Field {
        let layer = Layer::from_fn(32, 32, |x, _| 1.0 - x as f32 / 31.0);
        Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    /// A bowl: high at the edges, low in the middle, so water drains inward.
    fn bowl_field() -> Field {
        let layer = Layer::from_fn(32, 32, |x, y| {
            let (cx, cy) = (15.5_f32, 15.5_f32);
            let r = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            (r / 22.0).min(1.0)
        });
        Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    /// Default-ish params with an explicit iteration count.
    fn params(iterations: i64) -> Params {
        Params::new()
            .with("rain", ParamValue::Float(0.02))
            .with("iterations", ParamValue::Int(iterations))
    }

    fn outputs(input: &Field, params: &Params) -> Vec<Field> {
        HydraulicErosion
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
    }

    #[test]
    fn is_deterministic() {
        let input = bowl_field();
        let p = params(20);
        let run = || {
            outputs(&input, &p)
                .remove(0)
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_zero_mask_protects_the_terrain() {
        // Soft-layer mask convention (as Thermal and Stream): a zero mask confines erosion to
        // nowhere, so the heightfield output equals the input terrain exactly.
        let input = ramp_field();
        let mask = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.0)));
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let required = [&input];
        let optional = [Some(&mask)];
        let out = HydraulicErosion
            .eval(Inputs::new(&required, &optional), &params(20), &ctx())
            .unwrap();
        assert_eq!(
            before,
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "a zero mask must protect the terrain from erosion"
        );
    }

    #[test]
    fn heightfield_output_is_clean_terrain() {
        // Output 0 is the eroded terrain only; byproducts live on their own ports.
        let input = bowl_field();
        let out = outputs(&input, &params(20)).remove(0);
        let names: Vec<&str> = out.layers().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            [layers::HEIGHT],
            "the heightfield output must carry only the height layer"
        );
    }

    #[test]
    fn erodes_the_terrain() {
        // Rain over a slope cuts the bed: the eroded terrain differs from the input.
        let input = ramp_field();
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let after = outputs(&input, &params(60))
            .remove(0)
            .layer(layers::HEIGHT)
            .unwrap()
            .content_hash();
        assert_ne!(before, after, "erosion must change the terrain");
    }

    #[test]
    fn produces_suspended_sediment() {
        // Running water dissolves the bed, so some sediment is in suspension at the end.
        let input = ramp_field();
        let sediment = outputs(&input, &params(60)).remove(2);
        let total: f32 = sediment
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .sum();
        assert!(
            total > 0.0,
            "erosion should leave suspended sediment: {total}"
        );
    }

    #[test]
    fn stays_finite_under_heavy_rain() {
        // A safety check that the explicit integrator does not blow up: no NaN/inf in the bed
        // after many iterations of strong rain on a steep ramp.
        let input = ramp_field();
        let p = Params::new()
            .with("rain", ParamValue::Float(0.2))
            .with("capacity", ParamValue::Float(4.0))
            .with("erosion", ParamValue::Float(1.0))
            .with("iterations", ParamValue::Int(120));
        let out = outputs(&input, &p).remove(0);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|v| v.is_finite()),
            "the bed must stay finite"
        );
    }

    #[test]
    fn water_pools_in_the_low_ground() {
        // The water sim still routes water downhill; the bowl's centre ends up wettest.
        let input = bowl_field();
        let water = outputs(&input, &params(60)).remove(1);
        let depth = water.layer(layers::HEIGHT).unwrap();
        let centre = depth.get(16, 16).unwrap();
        let corner = depth.get(1, 1).unwrap();
        assert!(
            centre > corner,
            "water should pool in the low centre: centre {centre}, corner {corner}"
        );
    }

    #[test]
    fn zero_iterations_passes_the_terrain_through() {
        let input = bowl_field();
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let after = outputs(&input, &params(0))
            .remove(0)
            .layer(layers::HEIGHT)
            .unwrap()
            .content_hash();
        assert_eq!(
            before, after,
            "no iterations should leave the terrain untouched"
        );
    }

    #[test]
    fn spec_has_the_three_outputs() {
        let spec = HydraulicErosion.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "water", "sediment"]);
    }

    #[test]
    fn cancelled_simulation_aborts() {
        let cancel = ymir_core::CancelToken::new();
        cancel.cancel();
        let ctx = EvalContext::new(32, 32, Region::UNIT, 0).with_cancel(cancel);
        let input = bowl_field();
        let err = HydraulicErosion
            .eval(Inputs::required_only(&[&input]), &params(50), &ctx)
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = bowl_field();
        let p = params(10);
        let made = ymir_core::registry::make(TYPE_ID).expect("hydraulic operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &p, &ctx())
            .unwrap();
        let direct = outputs(&input, &p);
        assert_eq!(
            via_registry[0]
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash(),
            direct[0].layer(layers::HEIGHT).unwrap().content_hash(),
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
