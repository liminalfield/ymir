//! Per-evaluation context handed to operators.

use crate::cancel::CancelToken;
use crate::region::Region;

/// The context an operator receives for one evaluation.
///
/// It carries the requested resolution, the region being evaluated, the
/// already-derived seed the operator should use, the world extent (the
/// meters-to-cells bridge for world-unit parameters), and a cancellation signal.
/// It deliberately does **not** carry the target endpoint: which node the
/// evaluation was requested for is the evaluator's concern, not an operator's.
#[derive(Clone, Debug)]
pub struct EvalContext {
    /// Requested grid width in cells.
    pub width: usize,
    /// Requested grid height in cells.
    pub height: usize,
    /// The world-space region being evaluated.
    pub region: Region,
    /// The seed the operator should use, already derived from the global seed and
    /// the node's stable identity by the evaluator.
    pub seed: u64,
    /// Physical size of the full `UNIT` region along x, in world units (meters).
    /// Private so operators go through [`meters_per_cell`](Self::meters_per_cell)
    /// and [`world_to_cells`](Self::world_to_cells), which fold in resolution and
    /// region correctly.
    world_extent: f64,
    cancel: CancelToken,
}

impl EvalContext {
    /// Creates an evaluation context with no cancellation attached.
    #[must_use]
    pub fn new(width: usize, height: usize, region: Region, seed: u64) -> Self {
        Self {
            width,
            height,
            region,
            seed,
            world_extent: 1.0,
            cancel: CancelToken::new(),
        }
    }

    /// Attaches a cancellation token (used by the evaluator to thread the
    /// request's token into each node's context).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Sets the world's physical size along x, in world units (meters) across the
    /// full `UNIT` region. Defaults to `1.0` (a unit-sized world). Cells are kept
    /// square, so the y extent follows from the grid aspect.
    #[must_use]
    pub fn with_world_extent(mut self, world_extent: f64) -> Self {
        self.world_extent = world_extent;
        self
    }

    /// World units (meters) spanned by one cell at this resolution and extent.
    ///
    /// Region-aware (`region.width()` is the normalized span being evaluated), so a
    /// tile covers the same ground per cell as the matching untiled build, and
    /// isotropic, since cells are square. This is the meters-to-cells bridge that
    /// makes world-unit parameters resolution-independent.
    #[must_use]
    pub fn meters_per_cell(&self) -> f64 {
        self.region.width() * self.world_extent / self.width as f64
    }

    /// Converts a length in world units (meters) to a count of cells at this
    /// resolution and extent. Fractional; a caller rounds as it needs. Assumes a
    /// positive extent and a non-empty grid.
    #[must_use]
    pub fn world_to_cells(&self, meters: f64) -> f64 {
        meters / self.meters_per_cell()
    }

    /// Whether evaluation has been asked to cancel. Long-running operators (e.g.
    /// erosion) should poll this inside their loops and return
    /// [`Error::Cancelled`](crate::Error::Cancelled) early when it is `true`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_extent_defaults_to_a_unit_world() {
        let ctx = EvalContext::new(256, 256, Region::UNIT, 0);
        // A unit world across 256 cells: one cell spans 1/256.
        assert!((ctx.meters_per_cell() - 1.0 / 256.0).abs() < 1e-12);
    }

    #[test]
    fn meters_per_cell_uses_extent_and_resolution() {
        // A 2 km world across 4096 cells: about 0.488 m/cell.
        let ctx = EvalContext::new(4096, 4096, Region::UNIT, 0).with_world_extent(2000.0);
        assert!((ctx.meters_per_cell() - 2000.0 / 4096.0).abs() < 1e-9);
    }

    #[test]
    fn world_to_cells_is_resolution_independent() {
        // The same physical radius maps to a cell count that scales with
        // resolution, so it measures the same world distance at any resolution.
        let lo = EvalContext::new(1024, 1024, Region::UNIT, 0).with_world_extent(2000.0);
        let hi = EvalContext::new(4096, 4096, Region::UNIT, 0).with_world_extent(2000.0);
        let cells_lo = lo.world_to_cells(50.0);
        let cells_hi = hi.world_to_cells(50.0);
        // Four times the resolution covers the same 50 m in four times the cells.
        assert!((cells_hi / cells_lo - 4.0).abs() < 1e-9);
        // The round-trip recovers the physical length at both resolutions.
        assert!((cells_lo * lo.meters_per_cell() - 50.0).abs() < 1e-9);
        assert!((cells_hi * hi.meters_per_cell() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn meters_per_cell_is_region_aware_so_a_tile_matches_untiled() {
        // A quarter-region tile at resolution W covers the same ground per cell as
        // the untiled world at resolution 2W: region.width() scales the extent, so
        // a tiled build matches an untiled one at equal cell density.
        let tile = EvalContext::new(512, 512, Region::new(0.0, 0.0, 0.5, 0.5), 0)
            .with_world_extent(2000.0);
        let untiled = EvalContext::new(1024, 1024, Region::UNIT, 0).with_world_extent(2000.0);
        assert!((tile.meters_per_cell() - untiled.meters_per_cell()).abs() < 1e-12);
    }
}
