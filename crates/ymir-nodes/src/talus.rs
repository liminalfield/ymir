//! Talus (thermal) relaxation: the shared hillslope substrate.
//!
//! Material on any slope steeper than the angle of repose slides to its lower neighbours; gentler
//! ground is left untouched. So this softens over-steep faces — scree, and the freshly incised
//! channel walls the Stream node leaves — into straight talus slopes, *without* smoothing the
//! terrain's finer detail the way a linear diffusion (a lowpass over everything) would. The
//! Thermal node relaxes a surface with it, and the Stream node interleaves it so its channels
//! gain a cross-section instead of staying one-cell slots.
//!
//! One Jacobi pass is two parallel gather phases: each cell reads the previous full heights and
//! writes only itself, so the result is byte-identical regardless of `rayon`'s thread count.

use rayon::prelude::*;

use crate::hydrology::NEIGHBORS;

/// The fixed inputs of a relaxation pass: grid size, the resolution-aware repose threshold (how
/// far a cell may stand above a neighbour, over the neighbour's distance, before it sheds), and
/// the shed strength (the fraction of the steepest excess moved per pass).
#[derive(Clone, Copy)]
pub(crate) struct Pass {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) talus_per_cell: f32,
    pub(crate) strength: f32,
}

/// One Jacobi relaxation pass, as two parallel gather phases writing per-cell movement into
/// `delta` (which the caller then adds to the heights). `moved` and `total_excess` are scratch,
/// reused across passes by the caller.
///
/// Mass-conserving by construction: what a cell sheds is exactly the sum its lower neighbours
/// receive. Phase one computes, for every cell, how much it sheds (`moved`) and the total downhill
/// excess (`total_excess`) that splits the shed among its lower neighbours. Phase two has every
/// cell gather its own movement: minus what it sheds, plus its share of what each higher neighbour
/// sheds. Both phases read shared `heights` and write only their own cell, so the result is
/// byte-identical regardless of thread count. Out-of-bounds neighbours are skipped, so material is
/// held at the boundary.
pub(crate) fn relax_pass(
    heights: &[f32],
    moved: &mut [f32],
    total_excess: &mut [f32],
    delta: &mut [f32],
    pass: &Pass,
) {
    // One row per chunk (`max(1)` keeps the chunk size non-zero for a zero-width field).
    let row = pass.width.max(1);

    // Phase one: each cell's shed amount and its downhill excess sum.
    moved
        .par_chunks_mut(row)
        .zip(total_excess.par_chunks_mut(row))
        .enumerate()
        .for_each(|(y, (moved_row, excess_row))| {
            for (x, (m, te)) in moved_row.iter_mut().zip(excess_row.iter_mut()).enumerate() {
                (*m, *te) = shed_at(heights, x, y, pass);
            }
        });

    // Phase two: each cell gathers its incoming and outgoing movement.
    delta
        .par_chunks_mut(row)
        .enumerate()
        .for_each(|(y, delta_row)| {
            for (x, cell) in delta_row.iter_mut().enumerate() {
                *cell = gather_at(heights, moved, total_excess, x, y, pass);
            }
        });
}

/// Phase one at one cell: `(moved, total_excess)`. `moved` is the material the cell sheds this
/// pass, a stable fraction of its steepest downhill excess; `total_excess` is the sum of excess
/// over its lower neighbours, by which `moved` is split among them. Both are zero when no
/// neighbour is steeper than repose.
fn shed_at(heights: &[f32], x: usize, y: usize, pass: &Pass) -> (f32, f32) {
    let here = heights[y * pass.width + x];
    let mut total_excess = 0.0_f32;
    let mut max_excess = 0.0_f32;
    for (dx, dy, dist) in NEIGHBORS {
        let nx = x as i32 + dx;
        let ny = y as i32 + dy;
        if nx < 0 || ny < 0 || nx >= pass.width as i32 || ny >= pass.height as i32 {
            continue; // boundary holds material in-domain
        }
        let diff = here - heights[ny as usize * pass.width + nx as usize];
        // Lower neighbours steeper than repose only; the threshold scales with distance so
        // diagonals are not favoured.
        let threshold = pass.talus_per_cell * dist;
        if diff <= threshold {
            continue;
        }
        let excess = diff - threshold;
        total_excess += excess;
        max_excess = max_excess.max(excess);
    }
    (pass.strength * max_excess * 0.5, total_excess)
}

/// Phase two at one cell: its net movement this pass. Minus what it sheds (`-moved[c]`), plus,
/// from each higher neighbour `s` steeper than repose, that neighbour's share
/// `moved[s] * excess(s -> c) / total_excess[s]`. Read from the receiving side, so the per-cell
/// sum does not depend on cell order.
fn gather_at(
    heights: &[f32],
    moved: &[f32],
    total_excess: &[f32],
    x: usize,
    y: usize,
    pass: &Pass,
) -> f32 {
    let idx = y * pass.width + x;
    let here = heights[idx];
    let mut net = -moved[idx];
    for (dx, dy, dist) in NEIGHBORS {
        let nx = x as i32 + dx;
        let ny = y as i32 + dy;
        if nx < 0 || ny < 0 || nx >= pass.width as i32 || ny >= pass.height as i32 {
            continue;
        }
        let nidx = ny as usize * pass.width + nx as usize;
        // The excess this higher neighbour measured downhill to here (the same value it used when
        // shedding), so the share received matches what the neighbour sent.
        let threshold = pass.talus_per_cell * dist;
        let excess = heights[nidx] - here - threshold;
        if excess > 0.0 && total_excess[nidx] > 0.0 {
            net += moved[nidx] * (excess / total_excess[nidx]);
        }
    }
    net
}
