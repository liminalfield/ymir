//! Shared drainage primitives: depression filling, flow routing, and accumulation.
//!
//! The reusable substrate beneath any drainage-based node, stream-power erosion today and a
//! Rivers node, hydrology conditioning, or further erosion models later. It lives here, beside
//! the operators that use it, rather than inside one node, so the next drainage node does not
//! reimplement pit-free routing. Everything is serial and deterministic (a priority queue and
//! sorted/stack orderings with stable tie-breaks), which is what keeps the nodes reproducible.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Eight-neighbour offsets with their distances (diagonals are `sqrt(2)` away), so a slope is
/// the height drop over true distance and steepest descent is not biased toward the axes.
pub(crate) const NEIGHBORS: [(i32, i32, f32); 8] = [
    (-1, 0, 1.0),
    (1, 0, 1.0),
    (0, -1, 1.0),
    (0, 1, 1.0),
    (-1, -1, core::f32::consts::SQRT_2),
    (1, -1, core::f32::consts::SQRT_2),
    (-1, 1, core::f32::consts::SQRT_2),
    (1, 1, core::f32::consts::SQRT_2),
];

/// Epsilon tilt applied during depression filling, so flow routes across filled flats instead
/// of stalling. Tiny relative to the working height range, so it never visibly raises terrain.
const FILL_EPSILON: f32 = 1e-5;

/// The grid dimensions, bundled so the primitives share one source of truth.
#[derive(Clone, Copy)]
pub(crate) struct Grid {
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// The flow graph: each cell's `to` receiver (the cell it drains into; itself for a base-level
/// sink) and the `dist` to that receiver in cell units (1 or sqrt(2)).
pub(crate) struct Receivers {
    pub(crate) to: Vec<usize>,
    pub(crate) dist: Vec<f32>,
}

/// A cell waiting in the priority-flood queue, ordered so the *lowest* filled elevation pops
/// first, with ties broken by insertion order so the flood is fully deterministic.
struct FloodNode {
    elev: f32,
    seq: u64,
    idx: usize,
}

impl Ord for FloodNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .elev
            .total_cmp(&self.elev)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for FloodNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for FloodNode {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for FloodNode {}

/// Priority-flood depression filling (Barnes-Lehman-Soille) with an epsilon tilt, then a depth
/// cap: shallow pits fill completely so flow routes through them, but a pit that would need more
/// than `max_fill` to fill keeps its floor below the spill, so it stays a depression — a lake,
/// a local base level — instead of being filled flat and fanned across. Deterministic: cells
/// are processed in (elevation, insertion-order) priority. The filled terrain only routes flow.
pub(crate) fn fill_depressions(bed: &[f32], grid: &Grid, max_fill: f32) -> Vec<f32> {
    let (w, h) = (grid.width, grid.height);
    let mut filled = bed.to_vec();
    let mut visited = vec![false; w * h];
    let mut heap = BinaryHeap::new();
    let mut seq = 0_u64;

    for y in 0..h {
        for x in 0..w {
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                let c = y * w + x;
                visited[c] = true;
                heap.push(FloodNode {
                    elev: filled[c],
                    seq,
                    idx: c,
                });
                seq += 1;
            }
        }
    }

    while let Some(node) = heap.pop() {
        let (x, y) = (node.idx % w, node.idx / w);
        for &(dx, dy, _) in &NEIGHBORS {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            let nc = ny as usize * w + nx as usize;
            if visited[nc] {
                continue;
            }
            visited[nc] = true;
            // The spill level reaching this cell; cap the fill so deep basins stay depressions.
            let spill = (node.elev + FILL_EPSILON).min(bed[nc] + max_fill);
            filled[nc] = bed[nc].max(spill);
            heap.push(FloodNode {
                elev: filled[nc],
                seq,
                idx: nc,
            });
            seq += 1;
        }
    }
    filled
}

/// Each cell's steepest-descent receiver on the (filled) terrain, with the distance to it.
/// A cell with no lower neighbour, and every boundary cell, is its own receiver — a base level
/// (the boundary is the domain outlet; an interior local minimum is a lake floor).
pub(crate) fn receivers(filled: &[f32], grid: &Grid) -> Receivers {
    let (w, h) = (grid.width, grid.height);
    let n = w * h;
    let mut to = vec![0_usize; n];
    let mut dist = vec![1.0_f32; n];
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            // Boundary cells drain off-grid: treat them as base level.
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                to[c] = c;
                continue;
            }
            let here = filled[c];
            let (mut best_slope, mut best, mut best_dist) = (0.0_f32, c, 1.0_f32);
            for &(dx, dy, d) in &NEIGHBORS {
                let nc = (y as i32 + dy) as usize * w + (x as i32 + dx) as usize;
                let slope = (here - filled[nc]) / d;
                if slope > best_slope {
                    best_slope = slope;
                    best = nc;
                    best_dist = d;
                }
            }
            to[c] = best;
            dist[c] = best_dist;
        }
    }
    Receivers { to, dist }
}

/// The Braun-Willett drainage stack: a topological order of the flow graph in which every cell
/// appears after the cell it drains into. Built by depth-first traversal up the donor tree from
/// each base-level sink, using a CSR donor layout (no per-cell allocation). Deterministic.
pub(crate) fn build_stack(receiver: &[usize]) -> Vec<usize> {
    let n = receiver.len();
    // Donor counts (a sink does not count as its own donor).
    let mut count = vec![0_u32; n];
    for (i, &r) in receiver.iter().enumerate() {
        if r != i {
            count[r] += 1;
        }
    }
    // Prefix-sum offsets, then scatter donors into the flat array.
    let mut offset = vec![0_u32; n + 1];
    for i in 0..n {
        offset[i + 1] = offset[i] + count[i];
    }
    let mut donors = vec![0_usize; offset[n] as usize];
    let mut cursor = offset[..n].to_vec();
    for (i, &r) in receiver.iter().enumerate() {
        if r != i {
            donors[cursor[r] as usize] = i;
            cursor[r] += 1;
        }
    }
    // DFS from each base-level sink; receivers are pushed before their donors.
    let mut stack = Vec::with_capacity(n);
    let mut work = Vec::new();
    for (i, &r) in receiver.iter().enumerate() {
        if r == i {
            work.push(i);
            while let Some(c) = work.pop() {
                stack.push(c);
                for k in offset[c]..offset[c + 1] {
                    work.push(donors[k as usize]);
                }
            }
        }
    }
    stack
}

/// Multiple-flow-direction drainage area, in the same units as `cell_area`. Each cell starts
/// with its own area; processed high-to-low, a cell hands its accumulated area to *every*
/// downhill neighbour in proportion to `(slope)^concentration`. Spreading the flow (rather than
/// dumping it all into one steepest neighbour, as D8 does) is what dissolves the grid bias —
/// the diagonal "rivers" and diamond facets of single-flow routing — into smooth dendritic
/// drainage. Deterministic: a stable sort by elevation, ties by index.
pub(crate) fn drainage_area_mfd(
    filled: &[f32],
    grid: &Grid,
    concentration: f32,
    cell_area: f32,
) -> Vec<f32> {
    let (w, h) = (grid.width, grid.height);
    let n = w * h;
    let mut area = vec![cell_area; n];
    // Process from the highest cell down, so a cell's area is complete before it is distributed.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| filled[b].total_cmp(&filled[a]));

    let mut weight = [0.0_f32; 8];
    for &c in &order {
        let (x, y) = (c % w, c / w);
        let here = filled[c];
        let mut total = 0.0_f32;
        for (k, &(dx, dy, d)) in NEIGHBORS.iter().enumerate() {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                weight[k] = 0.0;
                continue;
            }
            let drop = here - filled[ny as usize * w + nx as usize];
            weight[k] = if drop > 0.0 {
                let s = (drop / d).powf(concentration);
                total += s;
                s
            } else {
                0.0
            };
        }
        if total > 0.0 {
            let share = area[c] / total;
            for (k, &(dx, dy, _)) in NEIGHBORS.iter().enumerate() {
                if weight[k] > 0.0 {
                    let nc = (y as i32 + dy) as usize * w + (x as i32 + dx) as usize;
                    area[nc] += share * weight[k];
                }
            }
        }
    }
    area
}

/// Maps a raw accumulation (which spans orders of magnitude) to `[0, 1]` through
/// `log(1 + a) / log(1 + max)`, so tributaries stay visible alongside the trunks instead of
/// being swamped. Values at or above `max` clamp to 1. Used by Stream's flow output as a
/// relative weight (the floor is the accumulation `0`, not the field's minimum).
pub(crate) fn log_normalize(acc: &[f32], max: f32) -> Vec<f32> {
    let denom = (1.0 + max).ln().max(1e-6);
    acc.iter()
        .map(|&a| ((1.0 + a).ln() / denom).clamp(0.0, 1.0))
        .collect()
}

/// Log-stretches an accumulation across its own `[min, max]` to a full `[0, 1]`: the least-
/// drained cell reads `0`, the most-drained reads `1`. Unlike [`log_normalize`], the floor is
/// the field's actual minimum, so a uniform per-cell seed (e.g. physical cell area) cancels and
/// the result always spans the whole range, which is what a *selection* band needs.
pub(crate) fn log_normalize_span(acc: &[f32]) -> Vec<f32> {
    let min = acc.iter().copied().fold(f32::INFINITY, f32::min);
    let max = acc.iter().copied().fold(0.0_f32, f32::max);
    let lo = (1.0 + min.max(0.0)).ln();
    let span = ((1.0 + max).ln() - lo).max(1e-6);
    acc.iter()
        .map(|&a| (((1.0 + a).ln() - lo) / span).clamp(0.0, 1.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp high at the top (y = 0) and low at the bottom, so flow runs downhill.
    fn ramp(grid: &Grid) -> Vec<f32> {
        (0..grid.width * grid.height)
            .map(|i| 1.0 - (i / grid.width) as f32 / (grid.height - 1) as f32)
            .collect()
    }

    #[test]
    fn fill_depressions_fills_a_pit_to_its_spill_level() {
        // A high plateau with a centre pit that drains up a shallow trench to a boundary outlet.
        // With a generous fill cap it raises to about the spill (the trench), not to 0 and not to
        // the plateau, so flow can route out.
        let (w, h) = (5, 5);
        let mut bed = vec![1.0_f32; w * h];
        let pit = 2 * w + 2;
        bed[pit] = 0.0;
        bed[w + 2] = 0.2;
        bed[2] = 0.2;
        let filled = fill_depressions(
            &bed,
            &Grid {
                width: w,
                height: h,
            },
            1.0,
        );
        assert!(
            filled[pit] > 0.1 && filled[pit] < 0.3,
            "pit should fill to its spill level, got {}",
            filled[pit]
        );
        assert_eq!(filled[0], 1.0, "the plateau is untouched");
    }

    #[test]
    fn drainage_area_accumulates_downhill() {
        let grid = Grid {
            width: 8,
            height: 8,
        };
        let filled = fill_depressions(&ramp(&grid), &grid, 1.0);
        let area = drainage_area_mfd(&filled, &grid, 1.5, 1.0);
        let top = area[grid.width + 4]; // row y = 1
        let bottom = area[(grid.height - 2) * grid.width + 4]; // row y = 6
        assert!(
            bottom > top,
            "drainage should accumulate downhill: top {top}, bottom {bottom}"
        );
    }

    #[test]
    fn build_stack_visits_every_cell_once() {
        let grid = Grid {
            width: 6,
            height: 6,
        };
        let filled = fill_depressions(&ramp(&grid), &grid, 1.0);
        let stack = build_stack(&receivers(&filled, &grid).to);
        assert_eq!(stack.len(), grid.width * grid.height);
        let mut seen = stack.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            grid.width * grid.height,
            "the stack is a permutation of every cell"
        );
    }
}
