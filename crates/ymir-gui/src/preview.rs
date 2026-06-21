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
use ymir_core::{CancelToken, Error, EvalCache, EvalRequest, Field, Graph, NodeId};

use crate::shade::{DEFAULT_LIGHT, ShadeMode, field_to_image};

/// Worker-side persistent cache capacity, in cached node results.
const WORKER_CACHE_CAP: usize = 64;
/// Size (px) of the relief light dial. Also the reserved height of the shading
/// controls row, so toggling Height/Relief never shifts the preview (#40).
pub(crate) const LIGHT_DIAL_SIZE: f32 = 40.0;
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
    /// How to shade the preview (height vs relief), toggled in the pane (#40).
    mode: ShadeMode,
    /// Relief light direction (unit vector), steered by dragging over the relief
    /// image (#40).
    light: [f32; 3],
    /// The most recent evaluated field, kept so a mode toggle can re-render without
    /// re-evaluating the graph.
    last_field: Option<Field>,
    texture: Option<egui::TextureHandle>,
    /// The (field hash, mode, light bits) the current texture was built from; the
    /// texture is rebuilt when any changes.
    texture_key: Option<(u64, ShadeMode, [u32; 3])>,
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
            mode: ShadeMode::Height,
            light: DEFAULT_LIGHT,
            last_field: None,
            texture: None,
            texture_key: None,
        }
    }

    /// The current shading mode, for the pane's toggle.
    pub(crate) fn mode(&self) -> ShadeMode {
        self.mode
    }

    /// Sets the shading mode; the texture is rebuilt on the next `poll` if it changed.
    pub(crate) fn set_mode(&mut self, mode: ShadeMode) {
        self.mode = mode;
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
                Ok(outcome) => self.apply(outcome),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.eval_error = Some("preview worker stopped".to_string());
                    break;
                }
            }
        }
        // Build/refresh the texture for the latest field and shading mode (a no-op
        // when neither changed), so a mode toggle re-renders without re-evaluating.
        self.refresh_texture(ctx);
        // Keep ticking while a result is in flight or a debounced submit is due, so
        // the async update and the trailing submit both happen promptly even when
        // the UI would otherwise be idle.
        if self.generation > self.shown || self.pending_key.is_some() {
            ctx.request_repaint();
        }
    }

    fn apply(&mut self, outcome: Outcome) {
        if outcome.generation() <= self.shown {
            return; // superseded
        }
        self.shown = outcome.generation();
        match outcome {
            Outcome::Ready { field, .. } => {
                self.eval_error = None;
                // Keep the field; `refresh_texture` re-uploads only when the field or
                // the shading mode changed.
                self.last_field = Some(field);
            }
            Outcome::Failed { message, .. } => {
                self.eval_error = Some(message);
                self.last_field = None;
                self.texture = None;
                self.texture_key = None;
            }
        }
    }

    /// Rebuilds the preview texture from the last field when the field or the shading
    /// mode has changed since the texture was uploaded. Cheap to call every frame.
    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let Some(field) = self.last_field.as_ref() else {
            return;
        };
        let key = (
            field.content_hash().to_u64(),
            self.mode,
            self.light.map(f32::to_bits),
        );
        if self.texture_key == Some(key) {
            return;
        }
        let image = field_to_image(field, self.mode, self.light);
        self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        self.texture_key = Some(key);
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

    /// A short label for the current status, for the pane's status-dot tooltip.
    pub(crate) fn status_label(&self) -> &'static str {
        match self.status() {
            Status::UpToDate => "Up to date",
            Status::Processing => "Evaluating…",
            Status::Error => "Error",
        }
    }

    /// Renders the preview body: the error message when failed, else the most recent
    /// image (kept visible while a refresh is in flight). In relief mode the image is
    /// draggable to steer the light (#40). The status and controls are drawn by the
    /// pane around this.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        if self.status() == Status::Error {
            if let Some(err) = self.structural_error.as_ref().or(self.eval_error.as_ref()) {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            return;
        }
        // Copy the texture's id/size so the immutable borrow of `self.texture` ends
        // before the drag below mutates the light.
        let Some(image) = self.texture.as_ref().map(|t| (t.id(), t.size_vec2())) else {
            return;
        };
        let sized = egui::load::SizedTexture::new(image.0, image.1);
        // Drag steers the light only in relief mode; height mode is non-interactive.
        let sense = if self.mode == ShadeMode::Relief {
            egui::Sense::drag()
        } else {
            egui::Sense::hover()
        };
        let resp = ui.add(
            egui::Image::new(sized)
                .max_width(ui.available_width())
                .maintain_aspect_ratio(true)
                .sense(sense),
        );
        if self.mode == ShadeMode::Relief
            && resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            self.set_light_from_drag(pos, resp.rect);
        }
    }

    /// Steers the relief light from a drag at `pos` over `rect` (the image or the
    /// indicator); the exact centre is ignored, keeping the current light.
    fn set_light_from_drag(&mut self, pos: egui::Pos2, rect: egui::Rect) {
        if let Some(light) = light_from_drag(pos, rect) {
            self.light = light;
        }
    }

    /// A small disk that shows the relief light direction — a dot whose angle is the
    /// azimuth and whose radius is the altitude — and lets you set it by dragging
    /// (sharing the image's mapping). Only meaningful in relief mode (#40).
    pub(crate) fn light_indicator(&mut self, ui: &mut egui::Ui) {
        let size = LIGHT_DIAL_SIZE;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::drag());
        let center = rect.center();
        let radius = size * 0.5 - 3.0;
        let painter = ui.painter_at(rect);
        let disk = ui.visuals().weak_text_color();
        let sun = egui::Color32::from_rgb(0xf2, 0xc4, 0x4d);
        painter.circle_stroke(center, radius, egui::Stroke::new(1.0, disk));
        // The light's horizontal projection (lx, ly) maps straight onto the disk.
        let dot = center + egui::vec2(self.light[0], self.light[1]) * radius;
        painter.line_segment([center, dot], egui::Stroke::new(1.5, sun));
        painter.circle_filled(dot, 3.5, sun);

        let resp = resp.on_hover_text("Drag to set the relief light");
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            self.set_light_from_drag(pos, rect);
        }
    }
}

/// The relief light direction for a drag at `pos` over `rect`: the cursor's angle
/// from the centre sets the azimuth, and its distance the altitude (centre =
/// high/soft, edge = low/grazing), clamped so the light is never fully overhead nor
/// fully grazing. `None` for the exact centre (no direction). Pure and unit-tested.
fn light_from_drag(pos: egui::Pos2, rect: egui::Rect) -> Option<[f32; 3]> {
    let half = rect.size() * 0.5;
    if half.x <= 0.0 || half.y <= 0.0 {
        return None;
    }
    let (u, v) = (
        (pos.x - rect.center().x) / half.x,
        (pos.y - rect.center().y) / half.y,
    );
    let dist = (u * u + v * v).sqrt();
    if dist < 1e-4 {
        return None;
    }
    let horizontal = dist.clamp(0.2, 0.95);
    let lz = (1.0 - horizontal * horizontal).max(0.0).sqrt();
    Some([u / dist * horizontal, v / dist * horizontal, lz])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_from_drag_maps_cursor_to_a_unit_light() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));

        // Dragging to the right edge → light points right (+x), level (y ≈ 0).
        let right = light_from_drag(egui::pos2(100.0, 50.0), rect).expect("direction");
        assert!(right[0] > 0.0 && right[1].abs() < 1e-3);

        // Upper-left → light points up-left (-x, -y).
        let up_left = light_from_drag(egui::pos2(0.0, 0.0), rect).expect("direction");
        assert!(up_left[0] < 0.0 && up_left[1] < 0.0);

        // Always a unit vector.
        let n = up_left[0] * up_left[0] + up_left[1] * up_left[1] + up_left[2] * up_left[2];
        assert!((n.sqrt() - 1.0).abs() < 1e-4);

        // The exact centre has no direction.
        assert!(light_from_drag(egui::pos2(50.0, 50.0), rect).is_none());
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
