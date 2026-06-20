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

/// How the preview shades the height layer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadeMode {
    /// Raw height mapped to grayscale (clamped to `[0, 1]`), matching the export.
    Height,
    /// Relief: each cell shaded by its surface normal under a fixed light, so height
    /// *changes* (slopes, carved valleys) are legible even when subtle (#40).
    Relief,
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
    /// last image visible while the refresh is in flight. In relief mode the image is
    /// draggable to steer the light (#40).
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let status = self.status();
        Self::status_chip(ui, status);

        if status == Status::Error {
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
        let size = 48.0;
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

// ---- field -> image ---------------------------------------------------------

/// Maps a normalized height value to an 8-bit grayscale level, matching the PNG
/// export's mapping (clamp to `[0, 1]`, scale to `0..=255`, round).
fn gray8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Default relief light: from the upper-left, partway up (a conventional NW
/// hillshade). `+x` is right, `+y` is down (image space). Pre-normalized. Steerable by
/// dragging over the relief image (#40).
const DEFAULT_LIGHT: [f32; 3] = [-0.5014, -0.6017, 0.6217];
/// Vertical exaggeration for relief, so subtle height changes (erosion) are legible.
const RELIEF_EXAGGERATION: f32 = 2.0;
/// Ambient term so slopes facing away from the light are dim, not pure black.
const RELIEF_AMBIENT: f32 = 0.25;

/// Lambert shade in `[0, 1]` for a cell whose height gradient (per unit region) is
/// `(gx, gy)`, lit from `light` (a unit vector). Flat ground reads a mid-tone; slopes
/// facing the light brighten, those facing away darken. Pure: the normal/lambert math,
/// kept separate from rendering so it is unit-testable.
fn relief_shade(gx: f32, gy: f32, light: [f32; 3]) -> f32 {
    // Surface normal of the height field is (-gx, -gy, 1), normalized.
    let inv_len = 1.0 / (gx * gx + gy * gy + 1.0).sqrt();
    let n = [-gx * inv_len, -gy * inv_len, inv_len];
    let lambert = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
    RELIEF_AMBIENT + (1.0 - RELIEF_AMBIENT) * lambert
}

/// Builds the preview image from a field's `height` layer, in the chosen mode.
fn field_to_image(field: &Field, mode: ShadeMode, light: [f32; 3]) -> egui::ColorImage {
    match mode {
        ShadeMode::Height => height_image(field),
        ShadeMode::Relief => relief_image(field, light),
    }
}

/// Raw height mapped straight to grayscale, matching the PNG export.
fn height_image(field: &Field) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let mut rgba = Vec::with_capacity(layer.len() * 4);
    for &value in layer.as_slice() {
        let g = gray8(value);
        rgba.extend_from_slice(&[g, g, g, 255]);
    }
    egui::ColorImage::from_rgba_unmultiplied([layer.width(), layer.height()], &rgba)
}

/// Relief (hillshade) image: each cell shaded by its surface normal. The gradient is
/// per unit region (central difference scaled by the cell count), so the shading
/// reads the same at any preview resolution.
fn relief_image(field: &Field, light: [f32; 3]) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let (w, h) = (layer.width(), layer.height());
    let at = |x: usize, y: usize| layer.get(x, y).unwrap_or(0.0);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w.saturating_sub(1)));
            let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h.saturating_sub(1)));
            // d(height)/d(unit region) ≈ Δheight / (Δcells / cell_count), exaggerated.
            let gx =
                (at(xp, y) - at(xm, y)) * RELIEF_EXAGGERATION * w as f32 / (xp - xm).max(1) as f32;
            let gy =
                (at(x, yp) - at(x, ym)) * RELIEF_EXAGGERATION * h as f32 / (yp - ym).max(1) as f32;
            let s = gray8(relief_shade(gx, gy, light));
            rgba.extend_from_slice(&[s, s, s, 255]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
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
    fn relief_shade_is_lit_bounded_and_directional() {
        // Flat ground reads a mid-tone (not black, not white).
        let flat = relief_shade(0.0, 0.0, DEFAULT_LIGHT);
        assert!(
            flat > 0.1 && flat < 0.9,
            "flat shade {flat} should be mid-tone"
        );

        // A slope facing the light (upper-left) is brighter than one facing away.
        let toward = relief_shade(0.6, 0.0, DEFAULT_LIGHT);
        let away = relief_shade(-0.6, 0.0, DEFAULT_LIGHT);
        assert!(
            toward > away,
            "{toward} (toward light) should exceed {away} (away)"
        );

        // Stays in range even for a near-vertical slope.
        let steep = relief_shade(50.0, -50.0, DEFAULT_LIGHT);
        assert!((0.0..=1.0).contains(&steep), "shade {steep} out of range");
    }

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
