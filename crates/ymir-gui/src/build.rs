//! The full-resolution Build (#7): evaluates the selected output endpoints at the
//! build resolution on a worker thread, so a slow build (high-res erosion) never
//! freezes the UI. One-shot per click — unlike the debounced, latest-wins preview.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread;

use eframe::egui;
use ymir_core::{EvalCache, EvalRequest, Graph};

/// Worker cache capacity for one build, so a shared upstream feeding several outputs
/// is evaluated once.
const BUILD_CACHE_CAP: usize = 64;

/// The worker's reply: how many outputs were written, or the first failure.
enum Outcome {
    Done(usize),
    Failed(String),
}

/// The build's coarse state, for the status shown beside the Build button.
enum Status {
    Idle,
    Building,
    Done(usize),
    Failed(String),
}

/// Drives one off-thread build at a time. The UI calls [`start`](Self::start) on a
/// Build click, [`poll`](Self::poll) each frame to collect the result, and
/// [`show`](Self::show) to render the status.
pub(crate) struct BuildRunner {
    /// The in-flight build's result channel, if any.
    rx: Option<Receiver<Outcome>>,
    status: Status,
}

impl BuildRunner {
    pub(crate) fn new() -> Self {
        Self {
            rx: None,
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
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.status = Status::Building;
        thread::spawn(move || {
            let outcome = run(&graph, &targets, &request);
            // shortcut-ok: the receiver only drops if the app has exited; nothing to recover.
            let _ = tx.send(outcome);
        });
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

    /// Draws the build status (a spinner while building, then a result or error).
    pub(crate) fn show(&self, ui: &mut egui::Ui) {
        match &self.status {
            Status::Idle => {}
            Status::Building => {
                ui.spinner();
                ui.weak("Building…");
            }
            Status::Done(n) => {
                let plural = if *n == 1 { "" } else { "s" };
                ui.weak(format!("Built {n} output{plural}"));
            }
            Status::Failed(message) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        }
    }
}

/// Evaluates each target endpoint with a shared cache, returning how many succeeded
/// or the first failure. Every failure is a value, never a panic.
fn run(graph: &Graph, targets: &[u64], request: &EvalRequest) -> Outcome {
    let mut cache = EvalCache::new(BUILD_CACHE_CAP);
    let mut built = 0;
    for &stable_id in targets {
        let Some(id) = graph.node_id_of(stable_id) else {
            continue; // removed between click and build; skip it
        };
        match graph.evaluate(id, request, &mut cache) {
            Ok(_) => built += 1,
            Err(err) => return Outcome::Failed(err.to_string()),
        }
    }
    Outcome::Done(built)
}
