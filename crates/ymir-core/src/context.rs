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
    /// Physical vertical span (meters) that a normalized height of `1.0` represents.
    /// Private so slope-aware operators go through [`real_slope_scale`](Self::real_slope_scale),
    /// which combines it with the horizontal cell size into a true rise-over-run scale.
    world_height: f64,
    /// The sea/base level as a normalized height: a world global several nodes agree on (the
    /// coastal shaper reshapes to it, stream-power grades rivers to it, the viewport draws water
    /// at it). Defaults to `0.0` (sea at the world base, i.e. no configured sea); the World-panel
    /// slider sets it. A world setting like [`world_height`](Self::world_height), never a node output.
    sea_level: f64,
    /// Subgraph nesting depth: 0 at the top level, raised by one each time a subgraph
    /// container evaluates its inner graph. A container checks it against the nesting limit
    /// so a pathologically deep stack reports rather than overflows.
    depth: u32,
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
            world_height: 1.0,
            sea_level: 0.0,
            depth: 0,
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

    /// Sets the world's vertical span (meters) that a normalized height of `1.0` represents.
    /// Defaults to `1.0`. Together with the horizontal cell size this gives slope-aware
    /// operators a true rise-over-run via [`real_slope_scale`](Self::real_slope_scale).
    #[must_use]
    pub fn with_world_height(mut self, world_height: f64) -> Self {
        self.world_height = world_height;
        self
    }

    /// Sets the sea/base level as a normalized height. Defaults to `0.0`. A world global that
    /// several nodes agree on (coastal reshaping, stream-power base level, the viewport water).
    #[must_use]
    pub fn with_sea_level(mut self, sea_level: f64) -> Self {
        self.sea_level = sea_level;
        self
    }

    /// The factor that turns a *per-cell* normalized height delta into a true slope
    /// (rise over run): `world_height / meters_per_cell`. A slope-aware operator multiplies its
    /// normalized `delta_height / cell_distance` by this to get a real tangent, so a talus angle
    /// or a slope selection means real degrees rather than normalized units, and scales
    /// correctly with the world's vertical and horizontal extents.
    #[must_use]
    pub fn real_slope_scale(&self) -> f64 {
        self.world_height / self.meters_per_cell()
    }

    /// The world's vertical span (meters) that a normalized height of `1.0` represents.
    ///
    /// Export reads this to write absolute-meters heightmaps (`height × world_height`).
    /// Slope-aware operators want [`real_slope_scale`](Self::real_slope_scale) instead,
    /// which folds in the horizontal cell size to give a true rise-over-run.
    #[must_use]
    pub fn world_height(&self) -> f64 {
        self.world_height
    }

    /// The sea/base level as a normalized height (see [`with_sea_level`](Self::with_sea_level)).
    /// A world global; the coastal shaper and stream-power base level read it, and the viewport
    /// draws water at it.
    #[must_use]
    pub fn sea_level(&self) -> f64 {
        self.sea_level
    }

    /// The world's physical size along x, in world units (meters) across the full `UNIT`
    /// region. A subgraph container reads it to thread the same extent into its inner
    /// evaluation; ordinary operators want [`meters_per_cell`](Self::meters_per_cell) or
    /// [`world_to_cells`](Self::world_to_cells), which fold in resolution and region.
    #[must_use]
    pub fn world_extent(&self) -> f64 {
        self.world_extent
    }

    /// The subgraph nesting depth of this evaluation: 0 at the top level. A subgraph
    /// container checks it against the nesting limit and sets it one deeper for its inner
    /// evaluation.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Sets the subgraph nesting depth. The evaluator threads the request's depth in; a
    /// subgraph container sets it one deeper before evaluating its inner graph.
    #[must_use]
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// A clone of the cancellation token, so a subgraph container can thread the same
    /// cancellation into its inner evaluation. Ordinary operators poll
    /// [`is_cancelled`](Self::is_cancelled) instead.
    #[must_use]
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
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
    fn real_slope_scale_combines_vertical_and_horizontal_extent() {
        // 1 km wide over 1024 cells, 256 m tall: a per-cell normalized delta scales by
        // world_height / meters_per_cell into a true rise-over-run.
        let ctx = EvalContext::new(1024, 1024, Region::UNIT, 0)
            .with_world_extent(1000.0)
            .with_world_height(256.0);
        let mpc = 1000.0 / 1024.0;
        assert!((ctx.real_slope_scale() - 256.0 / mpc).abs() < 1e-9);
    }

    #[test]
    fn world_height_defaults_to_a_unit_world() {
        // Unit vertical and horizontal extent over 256 cells: meters_per_cell is 1/256, so the
        // scale is its reciprocal.
        let ctx = EvalContext::new(256, 256, Region::UNIT, 0);
        assert!((ctx.real_slope_scale() - 256.0).abs() < 1e-9);
    }

    #[test]
    fn sea_level_defaults_to_zero_and_round_trips() {
        // No configured sea by default (sea at the world base); the setter carries it through.
        assert_eq!(EvalContext::new(4, 4, Region::UNIT, 0).sea_level(), 0.0);
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0).with_sea_level(0.35);
        assert!((ctx.sea_level() - 0.35).abs() < 1e-12);
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
