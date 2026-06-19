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
use ymir_core::{EvalCache, EvalRequest, Field, Graph, NodeId, layers};

/// Worker-side persistent cache capacity, in cached node results.
const WORKER_CACHE_CAP: usize = 64;

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
    last_key: Option<u64>,
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
            last_key: None,
            structural_error: None,
            eval_error: None,
            texture: None,
            texture_hash: None,
        }
    }

    /// Submits a fresh evaluation if the previewed output would differ from the last
    /// one submitted. A structural error (disconnected input, cycle) is detected
    /// here, synchronously and cheaply, and shown instead of submitting work.
    pub(crate) fn sync(&mut self, graph: &Graph, target: NodeId, request: EvalRequest) {
        match graph.output_key(target, &request) {
            Ok(key) => {
                self.structural_error = None;
                if self.last_key != Some(key) {
                    self.last_key = Some(key);
                    self.submit(graph, target, request);
                }
            }
            Err(err) => {
                self.structural_error = Some(err.to_string());
                // Force a resubmit once the graph is valid again.
                self.last_key = None;
            }
        }
    }

    fn submit(&mut self, graph: &Graph, target: NodeId, request: EvalRequest) {
        let Some(target) = graph.stable_id(target) else {
            return;
        };
        self.generation += 1;
        let job = Job {
            graph: graph.clone(),
            target,
            request,
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
        if self.generation > self.shown {
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

    /// Renders the current preview: a structural error first, then an evaluation
    /// error, then the image, then an "evaluating" hint while the first result is
    /// pending.
    pub(crate) fn show(&self, ui: &mut egui::Ui) {
        if let Some(err) = self.structural_error.as_ref().or(self.eval_error.as_ref()) {
            ui.colored_label(ui.visuals().error_fg_color, err);
            return;
        }
        match &self.texture {
            Some(texture) => {
                let width = ui.available_width();
                let sized = egui::load::SizedTexture::new(texture.id(), texture.size_vec2());
                ui.add(
                    egui::Image::new(sized)
                        .max_width(width)
                        .maintain_aspect_ratio(true),
                );
            }
            None => {
                ui.weak("Evaluating…");
            }
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
        let outcome = evaluate_job(&job, &mut cache);
        if result_tx.send(outcome).is_err() {
            break; // the UI is gone
        }
    }
}

/// Evaluates one job's target to a single preview field, mapping every failure to a
/// message rather than panicking.
fn evaluate_job(job: &Job, cache: &mut EvalCache) -> Outcome {
    let generation = job.generation;
    let Some(target) = job.graph.node_id_of(job.target) else {
        return Outcome::Failed {
            generation,
            message: "node was removed".to_string(),
        };
    };
    match job.graph.evaluate(target, &job.request, cache) {
        Ok(outputs) => match outputs.first() {
            Some(field) => Outcome::Ready {
                generation,
                field: field.clone(),
            },
            None => Outcome::Failed {
                generation,
                message: "node has no output to preview".to_string(),
            },
        },
        Err(err) => Outcome::Failed {
            generation,
            message: err.to_string(),
        },
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
}
