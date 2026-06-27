//! The full-resolution Build (#7): evaluates the selected output endpoints at the
//! build resolution on a worker thread, so a slow build (high-res erosion) never
//! freezes the UI. One-shot per click — unlike the debounced, latest-wins preview.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread;

use eframe::egui;
use ymir_core::{CancelToken, Error, EvalCache, EvalRequest, FieldStore, Graph};

/// Memory budget for the build cache's hot tier: holds a typical build in RAM, while larger
/// builds spill to the disk tier. (Will become a user setting.)
const BUILD_MEMORY_BUDGET: usize = 1 << 30; // 1 GiB
/// Disk budget for the build cache's warm tier under the user cache dir, so an unchanged
/// rebuild reuses results and survives restarts. (Will become a user setting.)
const BUILD_DISK_BUDGET: u64 = 4 << 30; // 4 GiB

/// The worker's reply: how many outputs were written, the first failure, or that it
/// aborted on a cancellation request.
enum Outcome {
    Done(usize),
    Cancelled,
    Failed(String),
}

/// The build's coarse state, for the status shown beside the Build button.
enum Status {
    Idle,
    Building,
    Done(usize),
    Cancelled,
    Failed(String),
}

/// Drives one off-thread build at a time. The UI calls [`start`](Self::start) on a
/// Build click, [`poll`](Self::poll) each frame to collect the result, and
/// [`show`](Self::show) to render the status.
pub(crate) struct BuildRunner {
    /// The in-flight build's result channel, if any.
    rx: Option<Receiver<Outcome>>,
    /// The current build's cancellation flag. The worker's request holds a clone; the
    /// Cancel button sets it, and the erosion (and the evaluator between nodes) polls it.
    cancel: CancelToken,
    status: Status,
}

impl BuildRunner {
    pub(crate) fn new() -> Self {
        Self {
            rx: None,
            cancel: CancelToken::new(),
            status: Status::Idle,
        }
    }

    /// Whether a build is currently running (so the button can disable itself).
    pub(crate) fn is_building(&self) -> bool {
        matches!(self.status, Status::Building)
    }

    /// Starts a build: evaluates each `target` (a node `stable_id`) at `request` on a
    /// worker thread against a snapshot `graph`. Each export endpoint writes its file
    /// as the side effect of being evaluated.
    pub(crate) fn start(&mut self, graph: Graph, targets: Vec<u64>, request: EvalRequest) {
        // A fresh token per build; the worker's request carries a clone so the erosion's
        // per-pass polling can abort it.
        let cancel = CancelToken::new();
        self.cancel = cancel.clone();
        let request = request.with_cancel(cancel);
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.status = Status::Building;
        thread::spawn(move || {
            let mut cache = build_cache();
            let outcome = run(&graph, &targets, &request, &mut cache);
            // shortcut-ok: the receiver only drops if the app has exited; nothing to recover.
            let _ = tx.send(outcome);
        });
    }

    /// Requests cancellation of the in-flight build. The worker aborts within one erosion
    /// pass (or one node), and [`poll`](Self::poll) then settles the status to cancelled.
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Records a build that could not even be started (e.g. nothing selected, or too
    /// many outputs), so it surfaces in the status instead of silently doing nothing.
    pub(crate) fn report(&mut self, message: String) {
        self.status = Status::Failed(message);
    }

    /// Collects the worker's result if ready, and keeps the UI repainting while a
    /// build is in flight so the status updates promptly when it finishes.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Outcome::Done(n)) => {
                self.status = Status::Done(n);
                self.rx = None;
            }
            Ok(Outcome::Cancelled) => {
                self.status = Status::Cancelled;
                self.rx = None;
            }
            Ok(Outcome::Failed(message)) => {
                self.status = Status::Failed(message);
                self.rx = None;
            }
            Err(TryRecvError::Empty) => ctx.request_repaint(),
            Err(TryRecvError::Disconnected) => {
                self.status = Status::Failed("build worker stopped".to_string());
                self.rx = None;
            }
        }
    }

    /// Draws the build status: a Cancel button and spinner while building, then a result,
    /// a cancelled note, or an error.
    pub(crate) fn show(&self, ui: &mut egui::Ui) {
        match &self.status {
            Status::Idle => {}
            Status::Building => {
                // Once cancellation is requested the button is replaced by a winding-down
                // note, since the worker only stops at its next pass boundary.
                let cancelling = self.cancel.is_cancelled();
                if cancelling {
                    ui.weak("Cancelling…");
                } else if ui.button("Cancel").clicked() {
                    self.cancel();
                }
                ui.spinner();
                ui.weak("Building…");
            }
            Status::Done(n) => {
                let plural = if *n == 1 { "" } else { "s" };
                ui.weak(format!("Built {n} output{plural}"));
            }
            Status::Cancelled => {
                ui.weak("Cancelled");
            }
            Status::Failed(message) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        }
    }
}

/// Builds the cache for one build: a byte-bounded memory tier over the on-disk warm tier in
/// the user cache directory, so an unchanged rebuild reuses results (and they survive
/// restarts). If the cache directory is unavailable, the build still runs memory-only.
fn build_cache() -> EvalCache {
    let cache = EvalCache::with_memory_budget(BUILD_MEMORY_BUDGET);
    match open_store() {
        Some(store) => cache.with_disk(store),
        None => cache,
    }
}

/// Opens the build cache's disk store under the user cache directory, or `None` if that
/// directory is unavailable. Shared by the build (which writes through it) and the viewport
/// (which reads build-quality fields back from it).
pub(crate) fn open_store() -> Option<FieldStore> {
    FieldStore::default_dir().and_then(|dir| FieldStore::open(dir, BUILD_DISK_BUDGET))
}

/// Evaluates each target endpoint with the given cache, returning how many succeeded or the
/// first failure. Every failure is a value, never a panic. The cache is a parameter so a test
/// can inject one over a temp directory.
fn run(graph: &Graph, targets: &[u64], request: &EvalRequest, cache: &mut EvalCache) -> Outcome {
    let mut built = 0;
    for &stable_id in targets {
        let Some(id) = graph.node_id_of(stable_id) else {
            continue; // removed between click and build; skip it
        };
        match graph.evaluate(id, request, cache) {
            Ok(_) => built += 1,
            // A cancelled build is a calm, expected outcome, not an error to alarm with.
            Err(Error::Cancelled) => return Outcome::Cancelled,
            Err(err) => return Outcome::Failed(err.to_string()),
        }
    }
    Outcome::Done(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ymir_core::{
        EvalContext, Field, Inputs, Layer, NodeSpec, Operator, Params, PortSpec, Region, layers,
    };

    /// A generator that counts its evaluations, so a test can prove the build reused a cached
    /// result instead of recomputing.
    #[derive(Clone)]
    struct CountingGen {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingGen {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.counting_gen",
                category: "test",
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, ctx: &EvalContext) -> ymir_core::Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region).with_layer(
                    layers::HEIGHT,
                    Arc::new(Layer::filled(ctx.width, ctx.height, 0.5)),
                ),
            ])
        }
    }

    #[test]
    fn an_unchanged_rebuild_reuses_the_disk_cache_across_builds() {
        let dir = std::env::temp_dir().join(format!("ymir-build-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // shortcut-ok: pre-clean any stale dir

        let calls = Arc::new(AtomicUsize::new(0));
        let mut graph = Graph::new();
        let node = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::clone(&calls),
            }),
            Params::new(),
        );
        let stable = graph.stable_id(node).expect("node has a stable id");
        let request = EvalRequest::new(8, 8, Region::UNIT, 0);

        // First build: a fresh memory tier over the disk store; the generator computes once and
        // is written through to disk.
        let mut first = EvalCache::with_memory_budget(1 << 20)
            .with_disk(FieldStore::open(dir.clone(), 1 << 20).expect("disk store opens"));
        run(&graph, &[stable], &request, &mut first);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Second build: a *fresh* memory tier over the same disk directory (the real per-build
        // pattern). It must hit disk, not recompute.
        let mut second = EvalCache::with_memory_budget(1 << 20)
            .with_disk(FieldStore::open(dir.clone(), 1 << 20).expect("disk store opens"));
        run(&graph, &[stable], &request, &mut second);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "an unchanged rebuild must reuse the disk cache, not recompute"
        );

        let _ = std::fs::remove_dir_all(&dir); // shortcut-ok: best-effort test cleanup
    }
}
