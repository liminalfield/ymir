//! Per-evaluation context handed to operators.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::cancel::CancelToken;
use crate::compute::ComputeContext;
use crate::progress::{Progress, ProgressSink};
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
    /// Where to report this node's own progress, already bound to its identity by the evaluator
    /// so an operator never needs to know its `stable_id` (#284). `None` when nothing is
    /// watching, which is the normal case and costs one `Option` check per report.
    ///
    /// The last percent reported rides alongside so a loop that calls this every iteration emits
    /// at most a hundred events: the caller reports as often as is convenient, and this decides
    /// what is worth saying.
    progress: Option<(Arc<dyn ProgressSink>, u64, Arc<AtomicU8>)>,
    /// Test-only recorder of which world fields an evaluation actually reads, an OR of the
    /// [`ACCESS_WORLD_EXTENT`](Self::ACCESS_WORLD_EXTENT) / `_HEIGHT` / `_SEA_LEVEL` bits. `None`
    /// in production, so every accessor does a single null check and nothing more; the cache-key
    /// dependency guard (a test over every node) attaches one to verify a node's declared
    /// [`ContextDeps`](crate::ContextDeps) cover every field its `eval` touches. Not part of the
    /// context's identity, so it is ignored by hashing and cloning-for-identity concerns.
    access_log: Option<Arc<AtomicU8>>,
    /// Optional handle to a compute device, threaded through by the evaluator when the
    /// request carries one. A GPU-capable operator downcasts it (see [`ComputeContext`])
    /// and uses the GPU path; when it is `None`, the operator falls back to CPU. Held as
    /// an `Arc` so cloning a context (which the evaluator does per node) is a pointer bump,
    /// and so it stays a GPU-type-free capability marker in core.
    compute: Option<Arc<dyn ComputeContext>>,
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
            access_log: None,
            compute: None,
            progress: None,
        }
    }

    /// The access-log bit for a read of the world horizontal extent (directly or through
    /// [`meters_per_cell`](Self::meters_per_cell) / [`world_to_cells`](Self::world_to_cells)).
    pub const ACCESS_WORLD_EXTENT: u8 = 1;
    /// The access-log bit for a read of the world vertical extent (directly or through
    /// [`real_slope_scale`](Self::real_slope_scale)).
    pub const ACCESS_WORLD_HEIGHT: u8 = 2;
    /// The access-log bit for a read of the world sea level.
    pub const ACCESS_SEA_LEVEL: u8 = 4;

    /// Attaches a recorder that accumulates which world fields this context's accessors read,
    /// for the dependency guard. Production evaluation never sets one, so the accessors stay a
    /// null check away from their prior behavior.
    #[must_use]
    pub fn with_access_log(mut self, log: Arc<AtomicU8>) -> Self {
        self.access_log = Some(log);
        self
    }

    /// Records a world-field read into the access log when one is attached (a no-op otherwise).
    /// `Relaxed` is sufficient: the guard reads the log only after the evaluation it observes has
    /// fully joined, and the bits only ever accumulate by OR.
    fn record(&self, bits: u8) {
        if let Some(log) = &self.access_log {
            log.fetch_or(bits, Ordering::Relaxed);
        }
    }

    /// Attaches a cancellation token (used by the evaluator to thread the
    /// request's token into each node's context).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Attaches a compute-device handle, so a GPU-capable operator can run on the
    /// GPU. The evaluator calls this to thread the request's device into each
    /// node's context; an operator reads it back through [`compute`](Self::compute).
    #[must_use]
    pub fn with_compute(mut self, compute: Arc<dyn ComputeContext>) -> Self {
        self.compute = Some(compute);
        self
    }

    /// The compute-device handle for this evaluation, or `None` on a CPU-only run.
    ///
    /// A GPU-capable operator downcasts it to the concrete device type from the GPU
    /// crate and takes the GPU path when present, falling back to CPU when absent
    /// (the soft-capability contract, mirroring the soft-layer contract):
    ///
    /// ```ignore
    /// match ctx.compute().and_then(|c| c.as_any().downcast_ref::<GpuContext>()) {
    ///     Some(gpu) => /* GPU path */,
    ///     None => /* CPU fallback */,
    /// }
    /// ```
    #[must_use]
    pub fn compute(&self) -> Option<&dyn ComputeContext> {
        self.compute.as_deref()
    }

    /// The compute handle itself, for the one caller that has to pass it on rather than use it: a
    /// subgraph container, which builds a request for its inner graph and must carry the device
    /// across that boundary or every node inside silently takes its CPU path.
    pub(crate) fn compute_handle(&self) -> Option<Arc<dyn ComputeContext>> {
        self.compute.clone()
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
        // Reads world_height directly and world_extent through meters_per_cell, so it records
        // both: a slope-aware node depends on the vertical and the horizontal extent alike.
        self.record(Self::ACCESS_WORLD_HEIGHT);
        self.world_height / self.meters_per_cell()
    }

    /// The world's vertical span (meters) that a normalized height of `1.0` represents.
    ///
    /// Export reads this to write absolute-meters heightmaps (`height × world_height`).
    /// Slope-aware operators want [`real_slope_scale`](Self::real_slope_scale) instead,
    /// which folds in the horizontal cell size to give a true rise-over-run.
    #[must_use]
    pub fn world_height(&self) -> f64 {
        self.record(Self::ACCESS_WORLD_HEIGHT);
        self.world_height
    }

    /// The sea/base level as a normalized height (see [`with_sea_level`](Self::with_sea_level)).
    /// A world global; the coastal shaper and stream-power base level read it, and the viewport
    /// draws water at it.
    #[must_use]
    pub fn sea_level(&self) -> f64 {
        self.record(Self::ACCESS_SEA_LEVEL);
        self.sea_level
    }

    /// The world's physical size along x, in world units (meters) across the full `UNIT`
    /// region. A subgraph container reads it to thread the same extent into its inner
    /// evaluation; ordinary operators want [`meters_per_cell`](Self::meters_per_cell) or
    /// [`world_to_cells`](Self::world_to_cells), which fold in resolution and region.
    #[must_use]
    pub fn world_extent(&self) -> f64 {
        self.record(Self::ACCESS_WORLD_EXTENT);
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
        // Folds in world_extent (resolution and region are always keyed, so they are not
        // recorded), so any node sizing a param in world units records a world_extent read.
        self.record(Self::ACCESS_WORLD_EXTENT);
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

    /// Attaches a progress sink bound to `node`. Called by the evaluator, which knows the node's
    /// identity; operators only ever call [`report_progress`](Self::report_progress).
    #[must_use]
    pub fn with_progress(mut self, sink: Arc<dyn ProgressSink>, node: u64) -> Self {
        self.progress = Some((sink, node, Arc::new(AtomicU8::new(u8::MAX))));
        self
    }

    /// Reports how far through its work this node is, as a fraction in `0.0..=1.0`.
    ///
    /// A long operator should call this beside its cancellation poll: the same loop, the same
    /// cadence, and no need to rate-limit at the call site, since an event is emitted only when
    /// the whole percent changes. A value outside the range is clamped rather than rejected: a
    /// misreported fraction should not fail an evaluation that is otherwise fine.
    ///
    /// Reporting nothing is a valid choice, and the honest one for a node that cannot say how far
    /// along it is. The pane shows elapsed time for those, never an invented bar.
    pub fn report_progress(&self, fraction: f32) {
        let Some((sink, node, last)) = &self.progress else {
            return;
        };
        // Truncated, not rounded: rounding reaches 100 while the last half-percent of the work is
        // still running, and a bar that sits full while a node keeps going is worse than one that
        // arrives late. Only an explicit 1.0 reads as 100. A NaN saturates to 0 through the cast.
        let percent = (fraction.clamp(0.0, 1.0) * 100.0) as u8;
        if last.swap(percent, Ordering::Relaxed) != percent {
            sink.report(Progress::Fraction {
                node: *node,
                percent,
            });
        }
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

    fn logged() -> (EvalContext, Arc<AtomicU8>) {
        let log = Arc::new(AtomicU8::new(0));
        let ctx = EvalContext::new(64, 64, Region::UNIT, 0)
            .with_world_extent(1000.0)
            .with_world_height(256.0)
            .with_sea_level(0.3)
            .with_access_log(Arc::clone(&log));
        (ctx, log)
    }

    #[test]
    fn access_log_records_each_world_field_read() {
        // Each accessor records exactly the fields it reads, so the dependency guard sees the
        // true read-set. Resolution and region are always keyed, so they are never recorded.
        // Each accessor's return value is used (it is `#[must_use]`), which is also what triggers
        // the recording being asserted.
        let (ctx, log) = logged();
        assert_eq!(ctx.sea_level(), 0.3);
        assert_eq!(log.load(Ordering::Relaxed), EvalContext::ACCESS_SEA_LEVEL);

        let (ctx, log) = logged();
        assert!(ctx.meters_per_cell() > 0.0);
        assert_eq!(
            log.load(Ordering::Relaxed),
            EvalContext::ACCESS_WORLD_EXTENT
        );

        let (ctx, log) = logged();
        assert_eq!(ctx.world_height(), 256.0);
        assert_eq!(
            log.load(Ordering::Relaxed),
            EvalContext::ACCESS_WORLD_HEIGHT
        );

        // The two indirect readers must record their full dependence, or a node could exclude a
        // field it reaches through them and silently serve a stale field.
        let (ctx, log) = logged();
        assert!(ctx.world_to_cells(50.0) > 0.0);
        assert_eq!(
            log.load(Ordering::Relaxed),
            EvalContext::ACCESS_WORLD_EXTENT
        );

        let (ctx, log) = logged();
        assert!(ctx.real_slope_scale() > 0.0);
        assert_eq!(
            log.load(Ordering::Relaxed),
            EvalContext::ACCESS_WORLD_HEIGHT | EvalContext::ACCESS_WORLD_EXTENT,
            "a slope-aware read depends on both the vertical and the horizontal extent"
        );
    }

    #[test]
    fn accessors_are_a_no_op_without_a_log() {
        // Production evaluation attaches no log, so the accessors just return their values.
        let ctx = EvalContext::new(64, 64, Region::UNIT, 0).with_sea_level(0.3);
        assert_eq!(ctx.sea_level(), 0.3);
        assert!(ctx.real_slope_scale().is_finite());
    }

    /// Collects reported progress, so a test can assert on what a loop actually emitted.
    #[derive(Debug, Default)]
    struct Recorder(std::sync::Mutex<Vec<Progress>>);

    impl ProgressSink for Recorder {
        fn report(&self, progress: Progress) {
            if let Ok(mut events) = self.0.lock() {
                events.push(progress);
            }
        }
    }

    #[test]
    fn progress_is_reported_only_when_the_whole_percent_moves() {
        // #284: a loop calls this every iteration, so the context decides what is worth saying.
        // Ten thousand passes must not become ten thousand events.
        let recorder = Arc::new(Recorder::default());
        let ctx = EvalContext::new(8, 8, Region::UNIT, 0)
            .with_progress(Arc::clone(&recorder) as Arc<dyn ProgressSink>, 7);
        for i in 0..10_000 {
            ctx.report_progress(i as f32 / 10_000.0);
        }
        let events = recorder.0.lock().expect("not poisoned").clone();
        assert_eq!(events.len(), 100, "one per whole percent, 0 through 99");
        assert_eq!(
            events[0],
            Progress::Fraction {
                node: 7,
                percent: 0
            }
        );
        assert_eq!(
            events[99],
            Progress::Fraction {
                node: 7,
                percent: 99
            }
        );
    }

    #[test]
    fn a_misreported_fraction_is_clamped_rather_than_fatal() {
        // A wrong fraction should never fail an evaluation that is otherwise fine.
        let recorder = Arc::new(Recorder::default());
        let ctx = EvalContext::new(8, 8, Region::UNIT, 0)
            .with_progress(Arc::clone(&recorder) as Arc<dyn ProgressSink>, 1);
        ctx.report_progress(-5.0);
        ctx.report_progress(4.2);
        ctx.report_progress(f32::NAN);
        let events = recorder.0.lock().expect("not poisoned").clone();
        assert_eq!(
            events,
            vec![
                Progress::Fraction {
                    node: 1,
                    percent: 0
                },
                Progress::Fraction {
                    node: 1,
                    percent: 100
                },
                Progress::Fraction {
                    node: 1,
                    percent: 0
                },
            ]
        );
    }

    #[test]
    fn reporting_without_a_sink_is_a_no_op() {
        // The normal case: nothing is watching, and an operator's reporting costs one check.
        let ctx = EvalContext::new(8, 8, Region::UNIT, 0);
        ctx.report_progress(0.5);
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
