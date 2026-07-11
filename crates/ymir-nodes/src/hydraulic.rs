//! Hydraulic erosion by droplet simulation (Beyer's method, as implemented by Lague).
//!
//! Rain is simulated as many droplets. Each is dropped at a random spot and runs downhill: it
//! samples the terrain and its gradient, blends a little momentum into its direction (`inertia`),
//! steps one cell, and carries sediment up to a capacity set by its speed, water, and the slope it
//! just descended. Where it moves faster and steeper than it can carry, it dissolves the bed over a
//! small brush radius; where it slows, pools, or climbs, it drops the surplus back. Run enough
//! droplets and the whole surface is worked into rills, gullies, and softened, sediment-filled
//! hollows. The deposition is half of why it reads as weathered rather than chopped.
//!
//! The erosion brush (a weighted disc, not a single cell) is what keeps channels from collapsing
//! into one-pixel slots. Droplets run in parallel across the cores, all reading and writing one
//! shared heightmap through relaxed atomics: writes can race and occasionally lose an update, which
//! is fine here (determinism is not a goal for erosion, and the visual result is unchanged) and is
//! what makes a build-resolution run tractable. Spawn positions derive from the global seed and the
//! droplet index, so they do not depend on thread scheduling and a reload reproduces the same rain.
//!
//! Outputs: `heightfield` (the eroded terrain, composited over the original through the mask), and
//! taps for `wear` and `deposition` (tracked from the actual erode/deposit events, so they are
//! exact, not a height difference) and `flow` (droplet visitation density, a where-water-runs map
//! read off the terrain the droplets are carving).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use rayon::prelude::*;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::erosion;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.hydraulic_erosion";

/// Gravity converting a height drop into speed. A feel constant, not physical SI.
const GRAVITY: f32 = 4.0;
/// A droplet's starting speed and water volume.
const INIT_SPEED: f32 = 1.0;
const INIT_WATER: f32 = 1.0;
/// Slope floor fed to the capacity model, so a droplet on near-flat ground still carries a little
/// sediment rather than dropping its whole load the instant the slope vanishes.
const MIN_SLOPE: f32 = 0.01;
/// Steps a droplet lives before it evaporates, expressed at the reference resolution and scaled
/// with it (a droplet crosses the same world distance at any resolution).
const LIFETIME_REFERENCE_RES: f64 = 256.0;
const BASE_LIFETIME: f64 = 30.0;
/// Odd constant (golden-ratio-derived) for mixing the droplet index into a spawn seed.
const SEED_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// Default droplet count as a multiple of the cell count: how thoroughly the surface is worked.
const DEFAULT_DENSITY: f64 = 3.0;
/// Default momentum blend: near 0 the droplet follows the gradient exactly and deepens valleys;
/// higher lets it carry across flats and meander.
const DEFAULT_INERTIA: f64 = 0.05;
/// Default sediment capacity coefficient (how much load a fast, steep, full droplet carries).
const DEFAULT_CAPACITY: f64 = 4.0;
/// Default erosion rate: fraction of the capacity deficit dissolved from the bed each step.
const DEFAULT_EROSION: f64 = 0.3;
/// Default deposition rate: fraction of the over-capacity load dropped back each step.
const DEFAULT_DEPOSITION: f64 = 0.3;
/// Default evaporation: fraction of a droplet's water lost each step.
const DEFAULT_EVAPORATION: f64 = 0.02;
/// Default erosion brush radius in cells: the disc erosion is spread over, which is what prevents
/// one-cell ravines. Radius 1 gives thin unnatural slots.
const DEFAULT_RADIUS: i64 = 3;

/// Hydraulic erosion modifier: one input (plus optional mask/hardness), and the erosion taps.
#[derive(Clone)]
pub struct HydraulicErosion;

impl Operator for HydraulicErosion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![PortSpec::new("in"), PortSpec::optional("mask")],
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("wear"),
                PortSpec::new("deposition"),
                PortSpec::new("flow"),
            ],
            params: vec![
                ParamSpec::new(
                    "density",
                    ParamKind::Float {
                        min: 0.0,
                        max: 16.0,
                    },
                    ParamValue::Float(DEFAULT_DENSITY),
                ),
                ParamSpec::new(
                    "inertia",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_INERTIA),
                ),
                ParamSpec::new(
                    "capacity",
                    ParamKind::Float {
                        min: 0.1,
                        max: 16.0,
                    },
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
                    "evaporation",
                    ParamKind::Float { min: 0.0, max: 0.2 },
                    ParamValue::Float(DEFAULT_EVAPORATION),
                ),
                ParamSpec::new(
                    "radius",
                    ParamKind::Int { min: 1, max: 8 },
                    ParamValue::Int(DEFAULT_RADIUS),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let n = width * height;

        let rates = Rates {
            inertia: params.get_f64("inertia", DEFAULT_INERTIA) as f32,
            capacity: params.get_f64("capacity", DEFAULT_CAPACITY) as f32,
            erosion: params.get_f64("erosion", DEFAULT_EROSION) as f32,
            deposition: params.get_f64("deposition", DEFAULT_DEPOSITION) as f32,
            evaporation: params.get_f64("evaporation", DEFAULT_EVAPORATION) as f32,
        };
        let density = params.get_f64("density", DEFAULT_DENSITY).max(0.0);
        let droplets = (density * n as f64).round() as usize;
        let radius = params.get_i64("radius", DEFAULT_RADIUS).clamp(1, 64) as i32;
        // Lifetime scales with resolution so a droplet travels the same world distance whatever the
        // grid, keeping the preview representative of the build.
        let lifetime =
            ((BASE_LIFETIME * width as f64 / LIFETIME_REFERENCE_RES).round() as usize).max(1);

        // A droplet needs a 2x2 stencil, so a 1x1 (or thinner) grid has nowhere to run: pass
        // through untouched rather than special-casing the sampler.
        if width < 2 || height < 2 || droplets == 0 {
            let zeros = || erosion::byproduct_field(vec![0.0; n], width, height, input.region());
            return Ok(vec![input.clone(), zeros(), zeros(), zeros()]);
        }

        let source = input.layer_or(layers::HEIGHT, 0.0);
        // The mask is a per-cell hardness/erodibility factor (Beyer's erosion factor): erosion and
        // deposition at a cell scale by it, so a 0 cell is protected and partials modulate. An
        // explicit mask input wins, else the input's own mask layer, else erode everywhere.
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        let brush = Brush::new(radius, width, height);
        let grid = Grid { width, height };
        // Atomic working buffers shared across the droplet threads.
        let map: Vec<AtomicU32> = source
            .as_slice()
            .iter()
            .map(|&h| AtomicU32::new(h.to_bits()))
            .collect();
        let wear: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
        let deposition: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
        let flow: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
        let bufs = Buffers {
            map: &map,
            wear: &wear,
            deposition: &deposition,
            flow: &flow,
            mask: mask.as_slice(),
        };
        let seed_base = ctx.seed ^ 0x44_59_44_52_4f_50_00_01;

        (0..droplets).into_par_iter().for_each(|i| {
            // A cancelled preview: make the remaining droplets cheap no-ops (checking the flag is a
            // relaxed atomic load), then the error is raised after the loop.
            if ctx.is_cancelled() {
                return;
            }
            // Spawn from the index, not a shared RNG, so the rain is the same set regardless of how
            // rayon schedules the droplets.
            let mut rng =
                SplitMix64::new(seed_base.wrapping_add((i as u64).wrapping_mul(SEED_GAMMA)));
            let start_x = rng.next_f32() * (width - 1) as f32;
            let start_y = rng.next_f32() * (height - 1) as f32;
            simulate_droplet(&bufs, &grid, &brush, &rates, lifetime, start_x, start_y);
        });
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let region = input.region();
        let mut heightfield = input.clone();
        heightfield.set_layer(layers::HEIGHT, Arc::new(from_atomics(&map, width, height)));
        Ok(vec![
            heightfield,
            erosion::byproduct_field(read_atomics(&wear), width, height, region),
            erosion::byproduct_field(read_atomics(&deposition), width, height, region),
            erosion::byproduct_field(normalize_flow(&read_atomics(&flow)), width, height, region),
        ])
    }
}

/// The per-evaluation droplet rates read once from the params.
#[derive(Clone, Copy)]
struct Rates {
    inertia: f32,
    capacity: f32,
    erosion: f32,
    deposition: f32,
    evaporation: f32,
}

/// Grid dimensions, bundled so the helpers share one source of truth.
#[derive(Clone, Copy)]
struct Grid {
    width: usize,
    height: usize,
}

/// The shared buffers a droplet writes: the heightmap it carves, the wear/deposition it tracks, and
/// the visitation flow (all atomic so droplets race safely), plus the read-only mask that modulates
/// its erosion and deposition.
#[derive(Clone, Copy)]
struct Buffers<'a> {
    map: &'a [AtomicU32],
    wear: &'a [AtomicU32],
    deposition: &'a [AtomicU32],
    flow: &'a [AtomicU32],
    mask: &'a [f32],
}

/// Reads a cell's `f32` from its atomic slot.
#[inline]
fn load(cell: &AtomicU32) -> f32 {
    f32::from_bits(cell.load(Relaxed))
}

/// Adds `delta` to a cell's `f32` with a relaxed load-then-store. Not a true atomic
/// read-modify-write: two threads can race and lose an update, which is accepted here (a slightly
/// lighter erode/deposit at a contended cell, invisible in the result).
#[inline]
fn add(cell: &AtomicU32, delta: f32) {
    cell.store((load(cell) + delta).to_bits(), Relaxed);
}

/// Simulates one droplet from `(start_x, start_y)`, carving the shared heightmap and accumulating
/// the wear/deposition it does and the cells it visits.
fn simulate_droplet(
    bufs: &Buffers,
    grid: &Grid,
    brush: &Brush,
    rates: &Rates,
    lifetime: usize,
    start_x: f32,
    start_y: f32,
) {
    let w = grid.width;
    let (mut px, mut py) = (start_x, start_y);
    let (mut dx, mut dy) = (0.0_f32, 0.0_f32);
    let mut speed = INIT_SPEED;
    let mut water = INIT_WATER;
    let mut sediment = 0.0_f32;

    for _ in 0..lifetime {
        // The node is the top-left of the enclosing 2x2 cell, clamped to the last full stencil so
        // the deposit weights and the sampler stay in bounds at the far edge.
        let node_x = (px as usize).min(grid.width - 2);
        let node_y = (py as usize).min(grid.height - 2);
        let cell = node_y * w + node_x;
        let offset_x = px - node_x as f32;
        let offset_y = py - node_y as f32;
        add(&bufs.flow[cell], 1.0);

        let (height, gx, gy) = height_and_gradient(bufs.map, grid, px, py);

        // Blend momentum with the downhill gradient, then take a unit step (independent of speed,
        // so the droplet cannot skip a cell).
        dx = dx * rates.inertia - gx * (1.0 - rates.inertia);
        dy = dy * rates.inertia - gy * (1.0 - rates.inertia);
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1e-8 {
            dx /= len;
            dy /= len;
        }
        px += dx;
        py += dy;

        // Stopped, or ran off the map (past the last full 2x2 stencil): the droplet ends.
        if (dx == 0.0 && dy == 0.0)
            || px < 0.0
            || px > (grid.width - 1) as f32
            || py < 0.0
            || py > (grid.height - 1) as f32
        {
            break;
        }

        let new_height = height_and_gradient(bufs.map, grid, px, py).0;
        let delta_height = new_height - height;

        // How much this droplet can carry now: more when fast, wet, and dropping steeply.
        let capacity = (-delta_height).max(MIN_SLOPE) * speed * water * rates.capacity;

        if sediment > capacity || delta_height > 0.0 {
            // Over capacity, or climbing: drop the surplus. When climbing, fill up to the rise so
            // it does not overfill. Deposition goes to the four cells of the current node by
            // bilinear weight, so it can settle a single pit precisely.
            let deposit = if delta_height > 0.0 {
                delta_height.min(sediment)
            } else {
                (sediment - capacity) * rates.deposition
            };
            sediment -= deposit_bilinear(bufs, grid, cell, offset_x, offset_y, deposit);
        } else {
            // Under capacity: dissolve the bed over the brush, never more than the drop just made
            // (so a droplet does not dig a hole behind itself). What is removed enters suspension.
            let erode = ((capacity - sediment) * rates.erosion).min(-delta_height);
            sediment += brush.erode(bufs.map, bufs.wear, bufs.mask, node_x, node_y, erode);
        }

        // Gravity accelerates a descent (delta_height < 0); evaporation thins the water.
        speed = (speed * speed + delta_height * GRAVITY).max(0.0).sqrt();
        water *= 1.0 - rates.evaporation;
    }
}

/// Bilinearly samples the height at `(x, y)` and the terrain gradient there, from the enclosing
/// 2x2 cell. `(x, y)` must lie in `[0, w-1] x [0, h-1]` so the stencil is in bounds.
fn height_and_gradient(map: &[AtomicU32], grid: &Grid, x: f32, y: f32) -> (f32, f32, f32) {
    let w = grid.width;
    let x0 = (x as usize).min(grid.width - 2);
    let y0 = (y as usize).min(grid.height - 2);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let nw = load(&map[y0 * w + x0]);
    let ne = load(&map[y0 * w + x0 + 1]);
    let sw = load(&map[(y0 + 1) * w + x0]);
    let se = load(&map[(y0 + 1) * w + x0 + 1]);
    let gradient_x = (ne - nw) * (1.0 - fy) + (se - sw) * fy;
    let gradient_y = (sw - nw) * (1.0 - fx) + (se - ne) * fx;
    let height =
        nw * (1.0 - fx) * (1.0 - fy) + ne * fx * (1.0 - fy) + sw * (1.0 - fx) * fy + se * fx * fy;
    (height, gradient_x, gradient_y)
}

/// Deposits `amount` into the four cells around `cell` by bilinear weight, each scaled by the mask
/// (so a protected cell keeps its height). Returns the total actually deposited, which is what
/// leaves suspension. `cell` is an interior node (the droplet is within the domain), so the four
/// stencil cells are in bounds.
fn deposit_bilinear(
    bufs: &Buffers,
    grid: &Grid,
    cell: usize,
    offset_x: f32,
    offset_y: f32,
    amount: f32,
) -> f32 {
    let w = grid.width;
    let weights = [
        (cell, (1.0 - offset_x) * (1.0 - offset_y)),
        (cell + 1, offset_x * (1.0 - offset_y)),
        (cell + w, (1.0 - offset_x) * offset_y),
        (cell + w + 1, offset_x * offset_y),
    ];
    let mut total = 0.0;
    for (c, weight) in weights {
        let placed = amount * weight * bufs.mask[c];
        add(&bufs.map[c], placed);
        add(&bufs.deposition[c], placed);
        total += placed;
    }
    total
}

/// A precomputed erosion brush: the in-bounds cell offsets within the radius and their normalized
/// falloff weights (heavier at the centre), so eroding a disc is a fixed table lookup per droplet.
struct Brush {
    /// Offsets from a node's cell index, with their weights. Recomputed per resolution.
    cells: Vec<(isize, f32)>,
    radius: i32,
    grid: Grid,
}

impl Brush {
    fn new(radius: i32, width: usize, height: usize) -> Self {
        let r = radius as f32;
        let mut raw: Vec<(i32, i32, f32)> = Vec::new();
        let mut sum = 0.0;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist <= r {
                    let weight = 1.0 - dist / r;
                    raw.push((dx, dy, weight));
                    sum += weight;
                }
            }
        }
        let cells = raw
            .into_iter()
            .map(|(dx, dy, weight)| (dy as isize * width as isize + dx as isize, weight / sum))
            .collect();
        Self {
            cells,
            radius,
            grid: Grid { width, height },
        }
    }

    /// Erodes `amount` from the bed around node `(node_x, node_y)`, spread over the brush and scaled
    /// by the mask, tracking the wear. Returns the total actually removed, which enters suspension.
    /// Brush cells that fall off the domain edge are skipped.
    fn erode(
        &self,
        map: &[AtomicU32],
        wear: &[AtomicU32],
        mask: &[f32],
        node_x: usize,
        node_y: usize,
        amount: f32,
    ) -> f32 {
        let center = (node_y * self.grid.width + node_x) as isize;
        let r = self.radius as isize;
        let (w, h) = (self.grid.width as isize, self.grid.height as isize);
        let (cx, cy) = (node_x as isize, node_y as isize);
        let mut total = 0.0;
        for &(offset, weight) in &self.cells {
            // Guard the edge: only erode a brush cell whose real (x, y) is in bounds, so a brush
            // near the border does not wrap onto the far side.
            let idx = center + offset;
            if idx < 0 || idx >= w * h {
                continue;
            }
            let (gx, gy) = (idx % w, idx / w);
            if (gx - cx).abs() > r || (gy - cy).abs() > r {
                continue;
            }
            let c = idx as usize;
            let removed = amount * weight * mask[c];
            add(&map[c], -removed);
            add(&wear[c], removed);
            total += removed;
        }
        total
    }
}

/// Copies an atomic buffer out to a plain `f32` vector once the droplets are done.
fn read_atomics(cells: &[AtomicU32]) -> Vec<f32> {
    cells.iter().map(load).collect()
}

/// Wraps an atomic heightmap into a [`Layer`].
fn from_atomics(cells: &[AtomicU32], width: usize, height: usize) -> Layer {
    Layer::from_vec(width, height, read_atomics(cells))
}

/// Normalizes the droplet visitation into `[0, 1]`, log-stretched (it spans orders of magnitude)
/// so tributaries stay visible alongside the trunks.
fn normalize_flow(flow: &[f32]) -> Vec<f32> {
    let max = flow.iter().copied().fold(0.0_f32, f32::max);
    let denom = (1.0 + max).ln().max(1e-6);
    flow.iter()
        .map(|&f| ((1.0 + f.max(0.0)).ln() / denom).clamp(0.0, 1.0))
        .collect()
}

/// SplitMix64: a tiny, fast PRNG for droplet spawn positions, seeded per droplet so the rain does
/// not depend on thread scheduling.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(SEED_GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float in `[0, 1)` from the top 24 bits.
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(HydraulicErosion) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(n: usize) -> EvalContext {
        EvalContext::new(n, n, Region::UNIT, 7)
    }

    fn noise(n: usize, seed: u64) -> Field {
        use crate::noise::{FbmParams, fbm_field};
        fbm_field(n, n, Region::UNIT, FbmParams::default(), seed)
    }

    fn run(input: &Field, params: Params, n: usize) -> Vec<Field> {
        HydraulicErosion
            .eval(Inputs::required_only(&[input]), &params, &ctx(n))
            .unwrap()
    }

    fn sum(field: &Field) -> f64 {
        field
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum()
    }

    #[test]
    fn spec_outputs_are_the_erosion_taps() {
        let spec = HydraulicErosion.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "wear", "deposition", "flow"]);
    }

    #[test]
    fn it_carves_and_deposits() {
        // Over a run the terrain changes, and both the wear and deposition taps carry material:
        // the model erodes AND deposits (deposition is the whole point).
        let input = noise(64, 3);
        let out = run(&input, Params::default(), 64);
        assert_ne!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            input.layer(layers::HEIGHT).unwrap().content_hash(),
            "erosion should modify the terrain"
        );
        assert!(sum(&out[1]) > 0.0, "some material should be worn away");
        assert!(sum(&out[2]) > 0.0, "some material should be deposited");
    }

    #[test]
    fn mask_protects_its_region() {
        // A zero mask erodes and deposits nothing (every write is scaled to zero), so the terrain
        // is untouched despite the run — regardless of how the droplets race.
        let input = noise(48, 2);
        let zero = Field::new(48, 48, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(48, 48, 0.0)));
        let out = HydraulicErosion
            .eval(
                Inputs::new(&[&input], &[Some(&zero)]),
                &Params::default(),
                &ctx(48),
            )
            .unwrap();
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            input.layer(layers::HEIGHT).unwrap().content_hash(),
            "a fully masked-out field is unchanged"
        );
    }

    #[test]
    fn zero_density_passes_terrain_through() {
        let input = noise(48, 1);
        let params = Params::new().with("density", ParamValue::Float(0.0));
        let out = run(&input, params, 48);
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            input.layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }

    #[test]
    fn tiny_grid_passes_through() {
        let input = Field::new(1, 1, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(1, 1, 0.5)));
        let out = HydraulicErosion
            .eval(
                Inputs::required_only(&[&input]),
                &Params::default(),
                &ctx(1),
            )
            .unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().get(0, 0).unwrap(),
            0.5
        );
    }

    #[test]
    fn cancellation_is_reported() {
        let input = noise(64, 4);
        let cancel = ymir_core::CancelToken::new();
        cancel.cancel();
        let c = EvalContext::new(64, 64, Region::UNIT, 7).with_cancel(cancel);
        let params = Params::new().with("density", ParamValue::Float(16.0));
        let err = HydraulicErosion
            .eval(Inputs::required_only(&[&input]), &params, &c)
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn registry_constructs_it() {
        assert!(ymir_core::registry::make(TYPE_ID).is_some());
    }
}
