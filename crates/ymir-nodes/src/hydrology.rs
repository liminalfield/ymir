//! Shared drainage primitives: depression filling, flat resolution, flow routing, and
//! accumulation.
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

/// Priority-flood depression filling (Barnes-Lehman-Soille) with a depth cap: shallow pits fill
/// completely so flow routes through them, but a pit that would need more than `max_fill` to fill
/// keeps its floor below the spill, so it stays a depression — a lake, a local base level —
/// instead of being filled flat and fanned across. Deterministic: cells are processed in
/// (elevation, insertion-order) priority. The filled terrain only routes flow.
///
/// Filled basins come out exactly flat, with no downhill direction of their own; run the result
/// through [`resolve_flats`] before routing so those flats drain across their true geometry
/// instead of stalling.
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
            let spill = node.elev.min(bed[nc] + max_fill);
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

/// "Not yet part of any labelled flat" marker for [`resolve_flats`].
const NO_LABEL: u32 = u32::MAX;

/// Fast-sweeping passes for the eikonal flat solver (each pass runs the four alternating sweep
/// directions). A cap, not a target: one pass is exact on a convex flat, while non-convex flats
/// whose geodesics bend around walls need a few. Small and fixed, so the solve stays O(n) per
/// flat and deterministic.
const EIKONAL_PASSES: u32 = 4;

/// The Godunov upwind eikonal update from the two axis minima at unit spacing, or infinity when
/// neither axis has a settled neighbour. Solves `(T-m1)^2 + (T-m2)^2 = 1`, falling back to the
/// one-sided `m1 + 1` when the second axis cannot contribute.
fn eikonal_godunov(umin: f32, vmin: f32) -> f32 {
    let (m1, m2) = (umin.min(vmin), umin.max(vmin));
    if m1.is_infinite() {
        f32::INFINITY
    } else if m2.is_infinite() || m2 - m1 >= 1.0 {
        m1 + 1.0
    } else {
        let d = m2 - m1;
        (m1 + m2 + (2.0 - d * d).sqrt()) / 2.0
    }
}

/// The smallest settled distance among a cell's same-flat, in-domain neighbours, returned as
/// `(horizontal, vertical, diagonal)` minima.
fn eikonal_neighbour_mins(
    t: &[f32],
    domain: &[bool],
    label: &[u32],
    grid: &Grid,
    x: usize,
    y: usize,
    lab: u32,
) -> (f32, f32, f32) {
    let (w, h) = (grid.width, grid.height);
    let c = y * w + x;
    let sample = |nc: usize, acc: &mut f32| {
        if domain[nc] && label[nc] == lab {
            *acc = acc.min(t[nc]);
        }
    };
    let mut umin = f32::INFINITY;
    if x > 0 {
        sample(c - 1, &mut umin);
    }
    if x + 1 < w {
        sample(c + 1, &mut umin);
    }
    let mut vmin = f32::INFINITY;
    if y > 0 {
        sample(c - w, &mut vmin);
    }
    if y + 1 < h {
        sample(c + w, &mut vmin);
    }
    let mut dmin = f32::INFINITY;
    if x > 0 && y > 0 {
        sample(c - w - 1, &mut dmin);
    }
    if x + 1 < w && y > 0 {
        sample(c - w + 1, &mut dmin);
    }
    if x > 0 && y + 1 < h {
        sample(c + w - 1, &mut dmin);
    }
    if x + 1 < w && y + 1 < h {
        sample(c + w + 1, &mut dmin);
    }
    (umin, vmin, dmin)
}

/// Geodesic distance from the `source` cells, restricted to `domain` cells of the same flat, by
/// solving the eikonal equation `|grad T| = 1` with a Godunov upwind update and Zhao fast
/// sweeping. Unlike a chamfer or breadth-first distance (whose lattice metric leaves creases at
/// the half-diagonals), this approximates the continuous distance, so its gradient is isotropic.
/// `label` isolates flats and walls: propagation never crosses into another label or out of the
/// domain, giving a wall-respecting distance. Cells outside the domain, and flats no source
/// reaches, stay infinite. Deterministic: a fixed sweep order, with no ties or priority queue.
fn eikonal_flats(source: &[bool], domain: &[bool], label: &[u32], grid: &Grid) -> Vec<f32> {
    let (w, h) = (grid.width, grid.height);
    let n = w * h;
    let mut t = vec![f32::INFINITY; n];
    for c in 0..n {
        if domain[c] && source[c] {
            t[c] = 0.0;
        }
    }

    // The four alternating sweep orderings (x forward/back crossed with y forward/back).
    let xs_fwd: Vec<usize> = (0..w).collect();
    let xs_rev: Vec<usize> = (0..w).rev().collect();
    let ys_fwd: Vec<usize> = (0..h).collect();
    let ys_rev: Vec<usize> = (0..h).rev().collect();
    let orders = [
        (&xs_fwd, &ys_fwd),
        (&xs_fwd, &ys_rev),
        (&xs_rev, &ys_fwd),
        (&xs_rev, &ys_rev),
    ];

    // Phase 1: isotropic Godunov fast sweeping over the 4-connected stencil. Bit-exact, so a
    // symmetric flat resolves symmetrically.
    for _ in 0..EIKONAL_PASSES {
        let mut changed = false;
        for (xs, ys) in orders {
            for &y in ys.iter() {
                for &x in xs.iter() {
                    let c = y * w + x;
                    if !domain[c] || source[c] {
                        continue;
                    }
                    let (umin, vmin, _) =
                        eikonal_neighbour_mins(&t, domain, label, grid, x, y, label[c]);
                    let cand = eikonal_godunov(umin, vmin);
                    if cand < t[c] {
                        t[c] = cand;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Phase 2: bridge diagonal-only pinches. A flat is labelled with 8-connectivity, but the
    // Godunov stencil is 4-connected, so a region joined to its outlet only through a diagonal
    // pinch stays infinite after Phase 1 and would stall as a false sink. Resolve only the cells
    // still infinite, adding a diagonal step where no orthogonal neighbour exists — so the
    // isotropic Phase-1 values are never touched, and this whole phase is skipped when the flats
    // have no pinches.
    let mut pinch: Vec<bool> = t.iter().map(|v| v.is_infinite()).collect();
    for c in 0..n {
        pinch[c] = pinch[c] && domain[c] && !source[c];
    }
    if pinch.iter().any(|&p| p) {
        for _ in 0..EIKONAL_PASSES {
            let mut changed = false;
            for (xs, ys) in orders {
                for &y in ys.iter() {
                    for &x in xs.iter() {
                        let c = y * w + x;
                        if !pinch[c] {
                            continue;
                        }
                        let (umin, vmin, dmin) =
                            eikonal_neighbour_mins(&t, domain, label, grid, x, y, label[c]);
                        let mut cand = eikonal_godunov(umin, vmin);
                        if dmin.is_finite() {
                            cand = cand.min(dmin + core::f32::consts::SQRT_2);
                        }
                        if cand < t[c] {
                            t[c] = cand;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }
    t
}

/// Resolve drainage directions across the flats left by [`fill_depressions`], so filled basins
/// drain across their real geometry instead of along grid-aligned spokes.
///
/// A filled basin comes out perfectly flat, which leaves its cells with no downhill direction.
/// Simply tilting the fill in flood order (the naive fix) biases every flat toward the eight grid
/// directions, so drainage collapses into straight axis and diagonal lines. Instead this
/// superimposes two gradients over each flat — one growing away from the surrounding higher
/// terrain, and one, weighted twice as strongly, growing toward the flat's lower outlets —
/// following Barnes, Lehman & Soille (2014). The result is a convergent micro-relief that
/// steepest-descent and multiple-flow routing follow into natural dendritic drainage.
///
/// The distances are continuous geodesic distances, obtained by solving the eikonal equation
/// `|grad T| = 1` with fast sweeping, not a lattice metric. A chamfer or breadth-first distance
/// has its worst error at the half-diagonals (about 22.5 degrees), so its gradient kinks there and
/// multiple-flow routing re-collapses onto grid-aligned creases; the eikonal gradient has unit
/// magnitude in every direction, so it channelises toward the nearest outlet with no preferred
/// direction.
///
/// The added relief is a few ULPs per cell, scaled to the local magnitude and far below the
/// working height range, so it steers routing without visibly altering terrain. Genuine sinks — a
/// capped lake floor with no outlet — carry no outlet edge, receive no gradient, and stay base
/// levels. Deterministic: a fixed-order sweep, with no priority queue, tie-break, or hash
/// iteration.
pub(crate) fn resolve_flats(filled: &[f32], grid: &Grid) -> Vec<f32> {
    let (w, h) = (grid.width, grid.height);
    let n = w * h;
    let mut resolved = filled.to_vec();

    // Per-cell classification against the eight neighbours:
    //   flat    - shares its elevation with a neighbour (part of an equal-elevation region);
    //   defined - already drains (a strictly lower neighbour, or the domain edge, which drains
    //             off-grid); the flat cells that are *not* defined are the interior to resolve;
    //   higher  - borders strictly higher terrain.
    let mut flat = vec![false; n];
    let mut defined = vec![false; n];
    let mut higher = vec![false; n];
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            let e = filled[c];
            let mut eq = false;
            let mut low = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut hi = false;
            for &(dx, dy, _) in &NEIGHBORS {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let ne = filled[ny as usize * w + nx as usize];
                if ne == e {
                    eq = true;
                } else if ne < e {
                    low = true;
                } else {
                    hi = true;
                }
            }
            flat[c] = eq;
            defined[c] = low;
            higher[c] = hi;
        }
    }

    // Label each flat (a connected equal-elevation region) with a deterministic row-major flood
    // fill, so the two gradients can be inverted per flat by its own maximum distance.
    let mut label = vec![NO_LABEL; n];
    let mut num_labels: u32 = 0;
    let mut work: Vec<usize> = Vec::new();
    for start in 0..n {
        if !flat[start] || label[start] != NO_LABEL {
            continue;
        }
        let e = filled[start];
        let lab = num_labels;
        num_labels += 1;
        label[start] = lab;
        work.push(start);
        while let Some(c) = work.pop() {
            let (cx, cy) = (c % w, c / w);
            for &(dx, dy, _) in &NEIGHBORS {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let nc = ny as usize * w + nx as usize;
                if flat[nc] && label[nc] == NO_LABEL && filled[nc] == e {
                    label[nc] = lab;
                    work.push(nc);
                }
            }
        }
    }
    if num_labels == 0 {
        return resolved; // no flats to resolve
    }

    // Two isotropic geodesic distances over the flats, by eikonal fast sweeping:
    //   away   - from the higher-terrain boundary, over the undefined flat interior;
    //   toward - from the outlets, over the whole flat.
    // Same composition as Barnes-Lehman-Soille, but the distances are continuous (isotropic)
    // rather than a lattice metric, so no grid- or diagonal-direction creases survive.
    let mut away_source = vec![false; n];
    let mut away_domain = vec![false; n];
    let mut toward_source = vec![false; n];
    let mut toward_domain = vec![false; n];
    for c in 0..n {
        if !flat[c] {
            continue;
        }
        toward_domain[c] = true;
        if defined[c] {
            toward_source[c] = true;
        } else {
            away_domain[c] = true;
            away_source[c] = higher[c];
        }
    }
    let away = eikonal_flats(&away_source, &away_domain, &label, grid);
    let toward = eikonal_flats(&toward_source, &toward_domain, &label, grid);

    // Each flat's greatest away distance, used to invert that gradient (high near the walls).
    let mut flat_heights = vec![0.0_f32; num_labels as usize];
    for c in 0..n {
        if away[c].is_finite() {
            let peak = &mut flat_heights[label[c] as usize];
            *peak = peak.max(away[c]);
        }
    }

    // Superimpose the two gradients — away from the walls (inverted), plus twice the distance
    // toward the outlets — so drainage descends toward an outlet from every cell. Apply as a few
    // ULPs of relief, scaled to each cell's magnitude, well below the working height range so it
    // steers routing without visibly altering terrain. A flat with no outlet (its toward distance
    // stays infinite) is a genuine sink and is left untouched.
    for c in 0..n {
        if label[c] == NO_LABEL || !toward[c].is_finite() {
            continue;
        }
        let walls = if away[c].is_finite() {
            flat_heights[label[c] as usize] - away[c]
        } else {
            0.0
        };
        let relief = walls + 2.0 * toward[c];
        if relief != 0.0 {
            let e = resolved[c];
            let step = f32::EPSILON * e.abs().max(1.0);
            resolved[c] = e + relief * step;
        }
    }
    resolved
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

    /// A square flat basin: 1.0 walls around a 0.5 interior, with a single lower outlet at the
    /// bottom-centre, on the `x = 3` axis. The archetypal case the flood-order tilt used to
    /// streak into grid-aligned lines.
    fn flat_basin() -> (Vec<f32>, Grid) {
        let (w, h) = (7, 7);
        let mut z = vec![0.5_f32; w * h];
        for y in 0..h {
            for x in 0..w {
                if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    z[y * w + x] = 1.0;
                }
            }
        }
        z[6 * w + 3] = 0.0; // outlet in the bottom border, on the x = 3 axis
        (
            z,
            Grid {
                width: w,
                height: h,
            },
        )
    }

    #[test]
    fn resolve_flats_is_deterministic() {
        let (z, grid) = flat_basin();
        assert_eq!(resolve_flats(&z, &grid), resolve_flats(&z, &grid));
    }

    #[test]
    fn resolve_flats_drains_every_flat_cell_toward_the_outlet() {
        let (z, grid) = flat_basin();
        let (w, h) = (grid.width, grid.height);
        let r = resolve_flats(&z, &grid);
        // Every interior flat cell gains a strictly lower resolved neighbour, so steepest descent
        // never stalls on the flat.
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let here = r[y * w + x];
                let mut drains = false;
                for &(dx, dy, _) in &NEIGHBORS {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if r[ny as usize * w + nx as usize] < here {
                        drains = true;
                        break;
                    }
                }
                assert!(drains, "flat cell ({x},{y}) has no downhill neighbour");
            }
        }
        // The micro-relief descends toward the outlet: the far side of the flat sits above the
        // cell next to the outlet.
        let top = r[w + 3]; // (3, 1)
        let near_outlet = r[5 * w + 3]; // (3, 5)
        assert!(
            top > near_outlet,
            "flat should descend toward the outlet: top {top}, near-outlet {near_outlet}"
        );
    }

    #[test]
    fn resolve_flats_is_symmetric_about_the_outlet_axis() {
        let (z, grid) = flat_basin();
        let (w, h) = (grid.width, grid.height);
        let r = resolve_flats(&z, &grid);
        // Mirror pairs across x = 3 are bit-identical: the resolution follows the flat's geometry,
        // not a grid direction or the flood traversal order (which the old tilt leaked).
        for y in 1..h - 1 {
            for x in 1..3 {
                let left = r[y * w + x];
                let right = r[y * w + (6 - x)];
                assert_eq!(
                    left,
                    right,
                    "asymmetry at row {y}, columns {x} and {}",
                    6 - x
                );
            }
        }
    }

    #[test]
    fn eikonal_distance_is_isotropic() {
        // A disk-shaped flat with a single centre source. The eikonal distance should track the
        // true Euclidean radius within a few percent at every angle. A chamfer distance is exact
        // on the axes and diagonals but over-estimates by ~8% at the half-diagonal (~22.5 deg),
        // a sharp crease; the eikonal error is small there and never a peak above the diagonal,
        // which is what stops multiple-flow routing from re-collapsing onto grid-aligned streaks.
        let (w, h) = (81usize, 81usize);
        let (cx, cy) = (40i32, 40i32);
        let radius = 36.0f32;
        let grid = Grid {
            width: w,
            height: h,
        };
        let n = w * h;
        let mut domain = vec![false; n];
        let mut label = vec![NO_LABEL; n];
        for y in 0..h {
            for x in 0..w {
                let r = (((x as i32 - cx).pow(2) + (y as i32 - cy).pow(2)) as f32).sqrt();
                if r <= radius {
                    domain[y * w + x] = true;
                    label[y * w + x] = 0;
                }
            }
        }
        let mut source = vec![false; n];
        source[cy as usize * w + cx as usize] = true;
        let t = eikonal_flats(&source, &domain, &label, &grid);
        let rel_err = |dx: i32, dy: i32| {
            let c = (cy + dy) as usize * w + (cx + dx) as usize;
            let euclid = (((dx * dx + dy * dy) as f32).sqrt()).max(1e-6);
            (t[c] - euclid).abs() / euclid
        };
        let axis = rel_err(30, 0);
        let diag = rel_err(21, 21);
        let half = rel_err(28, 12); // ~23 deg, the chamfer error maximum

        assert!(axis < 0.02, "axis error {axis}");
        assert!(diag < 0.05, "diagonal error {diag}");
        // The half-diagonal error is small (a chamfer would be ~0.08 here) and, unlike a chamfer
        // crease, is not a sharp peak above the diagonal.
        assert!(half < 0.04, "half-diagonal error {half}");
        assert!(
            half <= diag + 0.01,
            "half-diagonal should not spike above the diagonal (crease): half {half}, diag {diag}"
        );
    }

    #[test]
    fn resolve_flats_bridges_diagonal_pinches() {
        // Two flat blocks joined only through a single diagonal-pinch cell, with the outlet in the
        // first block. The Godunov stencil is 4-connected, so without the pinch-bridging pass the
        // second block (reachable only across the diagonal) would stay infinite and stall as false
        // sinks. Every flat cell must drain.
        let (w, h) = (9usize, 9usize);
        let idx = |x: usize, y: usize| y * w + x;
        let mut z = vec![1.0_f32; w * h]; // walls everywhere
        for y in 1..=3 {
            for x in 1..=3 {
                z[idx(x, y)] = 0.5; // block A
            }
        }
        z[idx(4, 4)] = 0.5; // the diagonal bridge; its orthogonal neighbours are all walls
        for y in 5..=7 {
            for x in 5..=7 {
                z[idx(x, y)] = 0.5; // block B, reachable from A only across the pinch
            }
        }
        z[idx(2, 4)] = 0.0; // an outlet just below block A
        let grid = Grid {
            width: w,
            height: h,
        };
        let r = resolve_flats(&z, &grid);
        for y in 0..h {
            for x in 0..w {
                if (z[idx(x, y)] - 0.5).abs() > 1e-6 {
                    continue; // only the flat cells
                }
                let here = r[idx(x, y)];
                let mut drains = false;
                for &(dx, dy, _) in &NEIGHBORS {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if r[ny as usize * w + nx as usize] < here {
                        drains = true;
                        break;
                    }
                }
                assert!(
                    drains,
                    "flat cell ({x},{y}) beyond a diagonal pinch does not drain"
                );
            }
        }
    }
}
