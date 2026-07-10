//! Shallow-water simulation (Mei, Decaudin & Hu 2007, the "virtual pipes" model): the shared
//! substrate for physically-simulated water flow, free of the grid-drainage artifacts (fill
//! fans, breach scars, MFD step-ladders) that dog accumulation-based routing on raw noise.
//!
//! Water is a depth field over the terrain. Between each pair of axial neighbours runs a
//! virtual pipe whose outflow flux is driven by the difference in water-surface height. Each
//! step: rain adds depth, the pipe fluxes update from the surface gradient (and scale down so a
//! cell never drains more water than it holds), the depth integrates the net flux, evaporation
//! removes a little, and the depth is accumulated into a flow map. Over a run under steady rain
//! water pools where the catchment is large (channels, valley floors, basins) and stays shallow
//! on ridges, so the accumulated depth lights up the drainage network. Channels emerge from the
//! physics rather than from a discrete flow-direction rule, so the map carries none of the grid
//! artifacts.
//!
//! The update is a Jacobi scheme: every pass reads the previous buffers and writes fresh ones,
//! so each cell depends only on its neighbours' old values. That makes it order-independent,
//! hence deterministic and bit-exact regardless of `rayon`'s thread count (per CLAUDE.md, byte
//! exactness kept where it is free). The domain edge is transmissive: water drains off only where
//! the terrain slopes down and out (a real outlet), so a closed basin still conserves its water.

use rayon::prelude::*;

use crate::hydrology::Grid;

/// Gravity, scaling the surface-gradient force on each pipe. A fixed constant, not a knob.
const GRAVITY: f32 = 9.81;
/// Virtual pipe cross-section, folded into the flux force. Fixed at 1; the tunable rates
/// (rain, evaporation) shape the result instead.
const PIPE_AREA: f32 = 1.0;
/// Pipe length / cell spacing, in cell units. The simulation runs on the normalized grid (like
/// the other drainage primitives), so it is 1: slopes are height-per-cell, matching how the
/// Flow selector reads the terrain.
const PIPE_LENGTH: f32 = 1.0;
/// Integration time step. Small enough that the flux stays stable; the per-cell outflow scaling
/// keeps it from over-draining regardless, so this is not a delicate CFL knob.
const DT: f32 = 0.02;

/// The tunable simulation inputs. The physical constants (gravity, pipe geometry, time step)
/// are fixed; these are what a user drives.
#[derive(Clone, Copy)]
pub(crate) struct SimParams {
    /// Depth of water added to every cell each step (steady rainfall).
    pub(crate) rain: f32,
    /// Fraction of a cell's water removed each step (evaporation), in `[0, 1)`.
    pub(crate) evaporation: f32,
    /// Number of simulation steps. Water travels a bounded distance per step, so more steps let
    /// it route across a larger domain; the count wants to scale with resolution.
    pub(crate) iterations: usize,
}

/// A running shallow-water simulation over a fixed terrain bed. Owns the water depth, the pipe
/// fluxes, and the accumulated flow map, plus scratch buffers reused across steps so a long run
/// does not reallocate.
///
/// The domain edge is transmissive: the terrain is extrapolated across the border, so water
/// drains off only where the ground slopes down and out (a real outlet, like a river leaving the
/// tile), and does not pour off a high edge or pile up against it. On a flat edge nothing drains,
/// so a closed basin conserves its water apart from rain and evaporation.
pub(crate) struct Sim<'bed> {
    bed: &'bed [f32],
    width: usize,
    height: usize,
    /// Water depth per cell.
    depth: Vec<f32>,
    /// Outflow flux per cell to its four axial neighbours, ordered `[left, right, top, bottom]`.
    flux: Vec<[f32; 4]>,
    /// Accumulated water depth per cell over the run: the flow map (deep = channel/basin).
    acc: Vec<f32>,
    // Scratch, swapped in each step.
    depth_next: Vec<f32>,
    flux_next: Vec<[f32; 4]>,
    /// Post-rain depth for the current step, read by both the flux and depth passes.
    rained: Vec<f32>,
}

/// Flux slot indices, matching the `[left, right, top, bottom]` order of [`Sim::flux`].
const L: usize = 0;
const R: usize = 1;
const T: usize = 2;
const B: usize = 3;

impl<'bed> Sim<'bed> {
    /// Starts a dry simulation over `bed` (dimensions from `grid`): no water, no flux, an empty
    /// flow map. `bed.len()` must be `grid.width * grid.height`.
    pub(crate) fn new(bed: &'bed [f32], grid: &Grid) -> Self {
        let n = grid.width * grid.height;
        Self {
            bed,
            width: grid.width,
            height: grid.height,
            depth: vec![0.0; n],
            flux: vec![[0.0; 4]; n],
            acc: vec![0.0; n],
            depth_next: vec![0.0; n],
            flux_next: vec![[0.0; 4]; n],
            rained: vec![0.0; n],
        }
    }

    /// Advances the simulation one step under the given rain and evaporation, accumulating this
    /// step's water depth into the flow map.
    pub(crate) fn step(&mut self, rain: f32, evaporation: f32) {
        let (w, h) = (self.width, self.height);

        // Rain: add depth everywhere, into the per-step buffer both later passes read.
        self.rained
            .par_iter_mut()
            .zip(self.depth.par_iter())
            .for_each(|(out, &d)| *out = d + rain);

        // Flux pass: each pipe's outflow grows with the water-surface drop to its neighbour, then
        // all four are scaled so the cell cannot drain more water than it holds this step. A
        // gather (reads old flux and rained depth, writes this cell only), so it is deterministic.
        let (bed, rained) = (self.bed, &self.rained);
        let flux = &self.flux;
        self.flux_next
            .par_iter_mut()
            .enumerate()
            .for_each(|(c, out)| {
                let (x, y) = (c % w, c / w);
                let surface = bed[c] + rained[c];
                let mut f = [0.0_f32; 4];
                let mut pipe = |slot: usize, neighbour_surface: f32| {
                    let dh = surface - neighbour_surface;
                    f[slot] =
                        (flux[c][slot] + DT * PIPE_AREA * GRAVITY * dh / PIPE_LENGTH).max(0.0);
                };
                // The water-surface height a pipe drains toward: the real neighbour in the
                // interior. Off the edge it is transmissive, the bed extrapolated across the
                // border from the opposite in-domain neighbour (`2*bed[c] - bed[back]`), so a pipe
                // drains out only where the terrain slopes down and out (a real outlet), not off a
                // high edge (which would brighten outward-sloping crops). With no opposite
                // neighbour (a 1-wide domain) the edge is flat, so nothing drains.
                let surface_of = |fwd: Option<usize>, back: Option<usize>| match fwd {
                    Some(n) => bed[n] + rained[n],
                    None => back.map_or(bed[c], |b| 2.0 * bed[c] - bed[b]) + rained[c],
                };
                let left = (x > 0).then(|| c - 1);
                let right = (x + 1 < w).then(|| c + 1);
                let up = (y > 0).then(|| c - w);
                let down = (y + 1 < h).then(|| c + w);
                pipe(L, surface_of(left, right));
                pipe(R, surface_of(right, left));
                pipe(T, surface_of(up, down));
                pipe(B, surface_of(down, up));
                // Scale so total outflow over dt does not exceed the water present: this is what
                // conserves water and keeps depth non-negative.
                let sum = f[L] + f[R] + f[T] + f[B];
                if sum > 0.0 {
                    let k = (rained[c] * PIPE_LENGTH * PIPE_LENGTH / (sum * DT)).min(1.0);
                    for v in &mut f {
                        *v *= k;
                    }
                }
                *out = f;
            });

        // Depth pass: integrate net flux into new depth, evaporate, and accumulate the depth into
        // the flow map. Depth (not instantaneous flux) is the accumulation signal here: a virtual
        // pipe's flux tracks the water-surface gradient, not how much water is present, so on a
        // gentle channel water backs up (deepens) rather than the flux rising. Water therefore
        // pools where the catchment is large (channels, valley floors, basins) and stays shallow
        // on ridges, so the time-integrated depth reads as a continuous, artifact-free flow map.
        // Another gather over the freshly computed fluxes.
        let flux_next = &self.flux_next;
        self.depth_next
            .par_iter_mut()
            .zip(self.acc.par_iter_mut())
            .enumerate()
            .for_each(|(c, (depth_out, acc_out))| {
                let (x, y) = (c % w, c / w);
                let fc = flux_next[c];
                // Flux arriving from each neighbour is that neighbour's outflow toward this cell.
                let from_left = if x > 0 { flux_next[c - 1][R] } else { 0.0 };
                let from_right = if x + 1 < w { flux_next[c + 1][L] } else { 0.0 };
                let from_top = if y > 0 { flux_next[c - w][B] } else { 0.0 };
                let from_bottom = if y + 1 < h { flux_next[c + w][T] } else { 0.0 };
                let inflow = from_left + from_right + from_top + from_bottom;
                let outflow = fc[L] + fc[R] + fc[T] + fc[B];
                let d2 = rained[c] + DT * (inflow - outflow) / (PIPE_LENGTH * PIPE_LENGTH);

                let depth = (d2 * (1.0 - evaporation)).max(0.0);
                *acc_out += depth;
                *depth_out = depth;
            });

        std::mem::swap(&mut self.depth, &mut self.depth_next);
        std::mem::swap(&mut self.flux, &mut self.flux_next);
    }

    /// Total water currently in the domain, for conservation checks.
    #[cfg(test)]
    fn total_water(&self) -> f64 {
        self.depth.iter().map(|&d| f64::from(d)).sum()
    }

    /// Consumes the simulation, returning the accumulated flow map (one value per cell).
    pub(crate) fn into_accumulation(self) -> Vec<f32> {
        self.acc
    }
}

/// Runs the shallow-water simulation over `bed` for `params.iterations` steps under steady rain,
/// returning the accumulated flow map. Polls `should_cancel` each step, returning `None` if it
/// asks to stop (so a caller can surface cancellation).
pub(crate) fn simulate_flow(
    bed: &[f32],
    grid: &Grid,
    params: &SimParams,
    should_cancel: impl Fn() -> bool,
) -> Option<Vec<f32>> {
    let mut sim = Sim::new(bed, grid);
    for _ in 0..params.iterations {
        if should_cancel() {
            return None;
        }
        sim.step(params.rain, params.evaporation);
    }
    Some(sim.into_accumulation())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp high at the top (y = 0), low at the bottom, so water runs downhill.
    fn ramp(grid: &Grid) -> Vec<f32> {
        (0..grid.width * grid.height)
            .map(|i| 1.0 - (i / grid.width) as f32 / (grid.height - 1) as f32)
            .collect()
    }

    /// A flat plateau with a deep square pit in the middle, so water pools in the pit.
    fn basin(grid: &Grid) -> Vec<f32> {
        let (w, h) = (grid.width, grid.height);
        let mut bed = vec![0.5_f32; w * h];
        for y in h * 3 / 8..h * 5 / 8 {
            for x in w * 3 / 8..w * 5 / 8 {
                bed[y * w + x] = 0.0;
            }
        }
        bed
    }

    #[test]
    fn is_deterministic_across_runs() {
        let grid = Grid {
            width: 24,
            height: 24,
        };
        let bed = ramp(&grid);
        let params = SimParams {
            rain: 0.01,
            evaporation: 0.05,
            iterations: 60,
        };
        let a = simulate_flow(&bed, &grid, &params, || false).unwrap();
        let b = simulate_flow(&bed, &grid, &params, || false).unwrap();
        // Bit-exact: the gather update is order-independent, so thread count cannot perturb it.
        assert_eq!(a, b);
    }

    #[test]
    fn a_flat_basin_conserves_water_without_rain_or_evaporation() {
        // Seed a slug of water on a flat bed with no rain/evap. The flat edge is a non-outlet
        // (transmissive extrapolation gives it no slope), so the total must hold as the water
        // redistributes: every pipe's outflow is another cell's inflow and the scaling keeps
        // depth non-negative.
        let grid = Grid {
            width: 20,
            height: 20,
        };
        let bed = vec![0.5_f32; grid.width * grid.height];
        let mut sim = Sim::new(&bed, &grid);
        for y in 8..12 {
            for x in 8..12 {
                sim.depth[y * grid.width + x] = 1.0;
            }
        }
        let before = sim.total_water();
        for _ in 0..80 {
            sim.step(0.0, 0.0);
        }
        let after = sim.total_water();
        assert!(
            (after - before).abs() / before < 1e-4,
            "water not conserved: {before} -> {after}",
        );
    }

    #[test]
    fn open_boundary_drains_water_off_a_slope() {
        // A seeded slug on a ramp with open edges drains off the low (bottom) border, where the
        // terrain slopes down and out, unlike the closed case above which conserves it. A flat bed
        // would not drain (the transmissive edge only lets water out where the terrain descends),
        // so the slope is what makes the outlet.
        let grid = Grid {
            width: 20,
            height: 20,
        };
        let bed = ramp(&grid);
        let mut sim = Sim::new(&bed, &grid);
        for y in 8..12 {
            for x in 8..12 {
                sim.depth[y * grid.width + x] = 1.0;
            }
        }
        let before = sim.total_water();
        for _ in 0..300 {
            sim.step(0.0, 0.0);
        }
        assert!(
            sim.total_water() < before * 0.5,
            "the open low edge should drain the water off the slope",
        );
    }

    #[test]
    fn water_pools_in_a_basin() {
        // Rain on a plateau with a deep central pit: water collects in the pit far more than on
        // the surrounding flat, so the accumulated depth reads the low ground as the drainage
        // target. This is the core property, that depth concentrates where water goes.
        let grid = Grid {
            width: 32,
            height: 32,
        };
        let bed = basin(&grid);
        let params = SimParams {
            rain: 0.01,
            evaporation: 0.02,
            iterations: 300,
        };
        let acc = simulate_flow(&bed, &grid, &params, || false).unwrap();
        let (w, mid) = (grid.width, grid.width / 2);
        let pit = acc[mid * w + mid];
        let flat = acc[2 * w + 2];
        assert!(
            pit > flat * 1.5,
            "the pit ({pit}) should pool far more water than the flat ({flat})",
        );
    }

    #[test]
    fn cancellation_stops_the_run() {
        let grid = Grid {
            width: 8,
            height: 8,
        };
        let bed = ramp(&grid);
        let params = SimParams {
            rain: 0.01,
            evaporation: 0.02,
            iterations: 1000,
        };
        assert!(simulate_flow(&bed, &grid, &params, || true).is_none());
    }

    #[test]
    fn tiny_grid_does_not_panic() {
        let grid = Grid {
            width: 1,
            height: 1,
        };
        let bed = vec![0.5_f32];
        let params = SimParams {
            rain: 0.01,
            evaporation: 0.02,
            iterations: 10,
        };
        let acc = simulate_flow(&bed, &grid, &params, || false).unwrap();
        assert_eq!(acc.len(), 1);
    }
}
