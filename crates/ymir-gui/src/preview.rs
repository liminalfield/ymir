//! Background preview evaluation (GUI step 6b).
//!
//! The selected node's output is evaluated on a worker thread, so a slow node
//! (erosion) never stalls the UI. The UI submits a cheap graph *snapshot* whenever
//! the previewed output's signature ([`Graph::output_key`]) changes, and polls for
//! results. Supersession is latest-wins: the worker drains its queue and only
//! evaluates the newest state, and the UI shows only the newest result. The worker
//! owns a persistent cache, so an unchanged upstream is reused across requests.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::{self, JoinHandle};

use eframe::egui;
use ymir_core::{CancelToken, Error, EvalCache, EvalRequest, Field, Graph, NodeId, layers};

/// Worker-side persistent cache capacity, in cached node results.
const WORKER_CACHE_CAP: usize = 64;
/// Minimum interval between preview submissions. A fast parameter drag throttles to
/// this cadence instead of queuing a job every frame; the final, settled value is
/// always submitted once the interval elapses (the trailing value wins).
const DEBOUNCE_SECS: f64 = 0.08;
/// Status-indicator colours. Red reuses the theme's error colour at render time.
const STATUS_OK: egui::Color32 = egui::Color32::from_rgb(0x53, 0xb0, 0x5a);
const STATUS_BUSY: egui::Color32 = egui::Color32::from_rgb(0xe0, 0xa8, 0x2e);

/// A unit of preview work: a graph snapshot to evaluate for one target node.
struct Job {
    graph: Graph,
    /// Persistent id of the node to preview, resolved against the snapshot on the
    /// worker (the snapshot's runtime `NodeId`s are its own).
    target: u64,
    request: EvalRequest,
    generation: u64,
}

/// The worker's reply for one job.
enum Outcome {
    Ready { generation: u64, field: Field },
    Failed { generation: u64, message: String },
}

impl Outcome {
    fn generation(&self) -> u64 {
        match self {
            Outcome::Ready { generation, .. } | Outcome::Failed { generation, .. } => *generation,
        }
    }
}

/// The preview's coarse state, surfaced as a stoplight indicator. "Processing" is
/// observable only because evaluation runs off the UI thread; a synchronous eval
/// would freeze the frame and never show it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Settled and current.
    UpToDate,
    /// A newer evaluation is in flight.
    Processing,
    /// The previewed node failed, structurally or during evaluation.
    Error,
}

/// Drives background preview evaluation. The UI calls [`sync`](Self::sync) (submit
/// if changed), [`poll`](Self::poll) (collect results), then [`show`](Self::show)
/// (render), every frame.
pub(crate) struct PreviewEngine {
    job_tx: Sender<Job>,
    result_rx: Receiver<Outcome>,
    /// Kept so the worker is owned by the engine and stops when it is dropped (the
    /// job sender closes, the worker's `recv` returns, the loop ends).
    _worker: JoinHandle<()>,
    /// The most recent job number submitted.
    generation: u64,
    /// The job number currently shown; results at or below it are stale.
    shown: u64,
    /// The last output signature submitted, so unchanged work is not resubmitted.
    submitted_key: Option<u64>,
    /// A changed signature waiting for the debounce interval before submission;
    /// always the latest, so the trailing value wins.
    pending_key: Option<u64>,
    /// Time of the last submission (seconds), for debounce throttling.
    last_submit_time: f64,
    /// Cancellation for the in-flight job, cancelled when a newer job supersedes it
    /// so a slow erosion preview aborts instead of running to completion.
    current_cancel: CancelToken,
    /// A structural error from the synchronous key check (e.g. a disconnected
    /// input), recomputed each frame; takes priority over a stale image.
    structural_error: Option<String>,
    /// The last evaluation error reported by the worker.
    eval_error: Option<String>,
    texture: Option<egui::TextureHandle>,
    texture_hash: Option<u64>,
}

impl PreviewEngine {
    pub(crate) fn new() -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<Outcome>();
        let worker = thread::spawn(move || worker_loop(&job_rx, &result_tx));
        Self {
            job_tx,
            result_rx,
            _worker: worker,
            generation: 0,
            shown: 0,
            submitted_key: None,
            pending_key: None,
            last_submit_time: 0.0,
            current_cancel: CancelToken::new(),
            structural_error: None,
            eval_error: None,
            texture: None,
            texture_hash: None,
        }
    }

    /// Submits a fresh evaluation if the previewed output would differ from the last
    /// one submitted. A structural error (disconnected input, cycle) is detected
    /// here, synchronously and cheaply, and shown instead of submitting work.
    pub(crate) fn sync(&mut self, graph: &Graph, target: NodeId, request: EvalRequest, now: f64) {
        match graph.output_key(target, &request) {
            Ok(key) => {
                self.structural_error = None;
                if self.submitted_key != Some(key) {
                    // Remember the latest changed signature; it is submitted below
                    // once the debounce interval elapses, so the trailing value wins.
                    self.pending_key = Some(key);
                }
            }
            Err(err) => {
                self.structural_error = Some(err.to_string());
                // Force a resubmit once the graph is valid again.
                self.submitted_key = None;
                self.pending_key = None;
            }
        }

        if let Some(key) = self.pending_key
            && now - self.last_submit_time >= DEBOUNCE_SECS
        {
            self.submitted_key = Some(key);
            self.pending_key = None;
            self.last_submit_time = now;
            self.submit(graph, target, request);
        }
    }

    fn submit(&mut self, graph: &Graph, target: NodeId, request: EvalRequest) {
        let Some(target) = graph.stable_id(target) else {
            return;
        };
        // Abort whatever the worker is currently evaluating: it is now superseded.
        self.current_cancel.cancel();
        let cancel = CancelToken::new();
        self.current_cancel = cancel.clone();
        self.generation += 1;
        let job = Job {
            graph: graph.clone(),
            target,
            request: request.with_cancel(cancel),
            generation: self.generation,
        };
        if self.job_tx.send(job).is_err() {
            self.eval_error = Some("preview worker stopped".to_string());
        }
    }

    /// Collects worker results, keeping only the newest, and requests a repaint
    /// while a result is still in flight so the async update shows promptly even
    /// when the UI would otherwise be idle.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        loop {
            match self.result_rx.try_recv() {
                Ok(outcome) => self.apply(outcome, ctx),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.eval_error = Some("preview worker stopped".to_string());
                    break;
                }
            }
        }
        // Keep ticking while a result is in flight or a debounced submit is due, so
        // the async update and the trailing submit both happen promptly even when
        // the UI would otherwise be idle.
        if self.generation > self.shown || self.pending_key.is_some() {
            ctx.request_repaint();
        }
    }

    fn apply(&mut self, outcome: Outcome, ctx: &egui::Context) {
        if outcome.generation() <= self.shown {
            return; // superseded
        }
        self.shown = outcome.generation();
        match outcome {
            Outcome::Ready { field, .. } => {
                self.eval_error = None;
                // Re-upload only when the previewed field actually changed.
                let hash = field.content_hash().to_u64();
                if self.texture_hash != Some(hash) {
                    self.texture = Some(ctx.load_texture(
                        "preview",
                        field_to_image(&field),
                        egui::TextureOptions::LINEAR,
                    ));
                    self.texture_hash = Some(hash);
                }
            }
            Outcome::Failed { message, .. } => {
                self.eval_error = Some(message);
                self.texture = None;
                self.texture_hash = None;
            }
        }
    }

    /// The coarse status, for the stoplight. A structural error is current
    /// (recomputed each frame); an in-flight evaluation supersedes any stale
    /// evaluation error.
    fn status(&self) -> Status {
        if self.structural_error.is_some() {
            Status::Error
        } else if self.pending_key.is_some() || self.generation > self.shown {
            Status::Processing
        } else if self.eval_error.is_some() {
            Status::Error
        } else {
            Status::UpToDate
        }
    }

    /// The current status as a single indicator colour, for a node-header badge.
    /// Red uses the theme's error colour.
    pub(crate) fn status_color(&self, visuals: &egui::Visuals) -> egui::Color32 {
        match self.status() {
            Status::UpToDate => STATUS_OK,
            Status::Processing => STATUS_BUSY,
            Status::Error => visuals.error_fg_color,
        }
    }

    /// Draws the stoplight: a coloured dot plus a short label.
    fn status_chip(ui: &mut egui::Ui, status: Status) {
        let (color, label) = match status {
            Status::UpToDate => (STATUS_OK, "Up to date"),
            Status::Processing => (STATUS_BUSY, "Evaluating…"),
            Status::Error => (ui.visuals().error_fg_color, "Error"),
        };
        ui.horizontal(|ui| {
            let diameter = ui.text_style_height(&egui::TextStyle::Body) * 0.6;
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), diameter * 0.5, color);
            ui.label(label);
        });
    }

    /// Renders the current preview: a status stoplight, then either the error
    /// message (when failed) or the most recent image. A processing state keeps the
    /// last image visible while the refresh is in flight.
    pub(crate) fn show(&self, ui: &mut egui::Ui) {
        let status = self.status();
        Self::status_chip(ui, status);

        if status == Status::Error {
            if let Some(err) = self.structural_error.as_ref().or(self.eval_error.as_ref()) {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            return;
        }
        if let Some(texture) = &self.texture {
            let width = ui.available_width();
            let sized = egui::load::SizedTexture::new(texture.id(), texture.size_vec2());
            ui.add(
                egui::Image::new(sized)
                    .max_width(width)
                    .maintain_aspect_ratio(true),
            );
        }
    }
}

/// The worker: evaluates submitted jobs with a persistent cache, skipping
/// superseded ones. Exits when the job channel closes (the engine is dropped).
fn worker_loop(job_rx: &Receiver<Job>, result_tx: &Sender<Outcome>) {
    let mut cache = EvalCache::new(WORKER_CACHE_CAP);
    while let Ok(mut job) = job_rx.recv() {
        // Latest-wins: drain to the newest queued job and skip the rest entirely.
        while let Ok(newer) = job_rx.try_recv() {
            job = newer;
        }
        // A cancelled job was superseded: evaluate_job returns None and nothing is
        // reported, avoiding a flash of a stale or "cancelled" result.
        if let Some(outcome) = evaluate_job(&job, &mut cache)
            && result_tx.send(outcome).is_err()
        {
            break; // the UI is gone
        }
    }
}

/// Evaluates one job's target to a single preview field, mapping every failure to a
/// message rather than panicking. Returns `None` if the job was cancelled (a newer
/// job superseded it), so it is not reported.
fn evaluate_job(job: &Job, cache: &mut EvalCache) -> Option<Outcome> {
    let generation = job.generation;
    let Some(target) = job.graph.node_id_of(job.target) else {
        return Some(Outcome::Failed {
            generation,
            message: "node was removed".to_string(),
        });
    };
    match job.graph.evaluate(target, &job.request, cache) {
        Ok(outputs) => Some(match outputs.first() {
            Some(field) => Outcome::Ready {
                generation,
                field: field.clone(),
            },
            None => Outcome::Failed {
                generation,
                message: "node has no output to preview".to_string(),
            },
        }),
        Err(Error::Cancelled) => None,
        Err(err) => Some(Outcome::Failed {
            generation,
            message: err.to_string(),
        }),
    }
}

// ---- field -> image ---------------------------------------------------------

/// Maps a normalized height value to an 8-bit grayscale level, matching the PNG
/// export's mapping (clamp to `[0, 1]`, scale to `0..=255`, round).
fn gray8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Builds a grayscale image from a field's `height` layer for the 2D preview.
fn field_to_image(field: &Field) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let mut rgba = Vec::with_capacity(layer.len() * 4);
    for &value in layer.as_slice() {
        let g = gray8(value);
        rgba.extend_from_slice(&[g, g, g, 255]);
    }
    egui::ColorImage::from_rgba_unmultiplied([layer.width(), layer.height()], &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray8_maps_and_clamps() {
        assert_eq!(gray8(0.0), 0);
        assert_eq!(gray8(1.0), 255);
        assert_eq!(gray8(-0.5), 0);
        assert_eq!(gray8(1.5), 255);
        assert_eq!(gray8(0.5), 128);
    }

    #[test]
    fn a_cancelled_job_reports_nothing() {
        use ymir_core::{Params, Region, registry};

        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let target = graph.stable_id(id).expect("stable id");

        let cancel = CancelToken::new();
        cancel.cancel();
        let job = Job {
            graph,
            target,
            request: EvalRequest::new(32, 32, Region::UNIT, 0).with_cancel(cancel),
            generation: 1,
        };

        // A superseded (pre-cancelled) job evaluates to nothing, so the worker
        // reports no stale result.
        let mut cache = EvalCache::new(4);
        assert!(evaluate_job(&job, &mut cache).is_none());
    }
}
