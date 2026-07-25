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
use ymir_core::{
    CancelToken, Error, EvalCache, EvalRequest, Field, Graph, NodeId, OUTPUT_TYPE_ID, layers,
};

use crate::shade::{
    DEFAULT_LIGHT, HeightScale, ShadeMode, WaterStyle, apply_water, field_to_image,
};

/// Worker-side persistent cache capacity, in cached node results.
const WORKER_CACHE_CAP: usize = 64;
/// Number of bins in the input histogram drawn behind the curve/levels editors (#15).
const HISTOGRAM_BINS: usize = 64;
/// Minimum interval between preview submissions. A fast parameter drag throttles to
/// this cadence instead of queuing a job every frame; the final, settled value is
/// always submitted once the interval elapses (the trailing value wins).
const DEBOUNCE_SECS: f64 = 0.08;
/// Status-indicator colours, from the Ymir Dark palette (#104): up-to-date is `success`,
/// processing is `warning`. The error state reuses the theme's error colour at render time.
const STATUS_OK: egui::Color32 = crate::theme::SUCCESS;
const STATUS_BUSY: egui::Color32 = crate::theme::WARNING;

/// A unit of preview work: a graph snapshot to evaluate for one target node.
struct Job {
    graph: Graph,
    /// Persistent id of the node to preview, resolved against the snapshot on the
    /// worker (the snapshot's runtime `NodeId`s are its own).
    target: u64,
    request: EvalRequest,
    generation: u64,
    /// When previewing inside a subgraph (#106), the live fields to bind to its Input
    /// markers so the preview shows real data instead of the zero stand-in. `None` at the
    /// top level.
    binding: Option<crate::SubgraphInputs>,
}

/// The worker's reply for one job.
enum Outcome {
    Ready {
        generation: u64,
        /// The previewed node's `stable_id`, so the histogram can be matched to the node
        /// the inspector is editing.
        target: u64,
        /// Every output of the previewed node, so the pane can switch which one it shows
        /// without re-evaluating (the engine computes all outputs together).
        fields: Vec<Field>,
        /// Normalized bin heights of the node's *input* distribution (the field feeding
        /// it), for the editor histogram. `None` for a generator (no input) or a failed
        /// input eval.
        histogram: Option<Vec<f32>>,
    },
    Failed {
        generation: u64,
        message: String,
    },
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

/// The identity a preview texture was built from: field hash, output index, shading mode and
/// scale, relief light bits, sea-level bits, and whether water is shown. The texture is rebuilt
/// when any of these change.
type TextureKey = (u64, usize, ShadeMode, HeightScale, [u32; 3], u32, bool);

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
    /// How Height shading maps values (auto-range vs fixed [0, 1]), toggled in the pane
    /// (#83). Ignored in relief mode.
    scale: HeightScale,
    /// Relief light direction (unit vector), steered by dragging over the relief
    /// image (#40).
    light: [f32; 3],
    /// Sea level (normalized height) for the water overlay, mirrored from the World settings
    /// each frame. Presentation only: it drives the overlay, never the evaluation (#96).
    sea_level: f32,
    /// Whether to draw the water overlay, mirrored from the World settings' Show water toggle.
    show_water: bool,
    /// Which output port the preview displays, by index. A multi-output node (e.g. hydraulic
    /// erosion's heightfield/water/sediment) is viewed one output at a time; switching is
    /// instant since all outputs are kept. Clamped to the available outputs.
    display_output: usize,
    /// The most recent evaluated node's outputs, kept so switching the shown output or the
    /// shading mode re-renders without re-evaluating the graph. Empty when none.
    last_outputs: Vec<Field>,
    /// The input histogram of the most recent result, and the node it is for. Surfaced
    /// to the curve/levels editors via [`input_histogram`](Self::input_histogram).
    last_histogram: Option<Vec<f32>>,
    histogram_target: Option<u64>,
    texture: Option<egui::TextureHandle>,
    /// The (field hash, output index, mode, scale, light bits, sea-level bits, show-water) the
    /// current texture was built from; the texture is rebuilt when any changes. Sea level enters
    /// the key only while water is shown, so moving the slider with water off costs no rebuild.
    texture_key: Option<TextureKey>,
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
            scale: HeightScale::Auto,
            light: DEFAULT_LIGHT,
            sea_level: 0.0,
            show_water: false,
            display_output: 0,
            last_outputs: Vec::new(),
            last_histogram: None,
            histogram_target: None,
            texture: None,
            texture_key: None,
        }
    }

    /// The currently shown output field, if any, for the 3D viewport to mesh from. Shares the
    /// preview's already-evaluated output rather than evaluating again, and follows the output
    /// picker so the viewport meshes whatever the 2D preview shows.
    pub(crate) fn field(&self) -> Option<&Field> {
        self.shown_field()
    }

    /// The output the picker currently selects, clamped to what the node produced.
    fn shown_field(&self) -> Option<&Field> {
        if self.last_outputs.is_empty() {
            return None;
        }
        let index = self.display_output.min(self.last_outputs.len() - 1);
        self.last_outputs.get(index)
    }

    /// Blanks the preview: cancels any in-flight job, drops the shown outputs, texture, and errors,
    /// and forgets the last submission so a re-selected node evaluates afresh. Used when there is no
    /// preview target (the user dismissed the preview by clicking empty canvas). Idempotent, so it is
    /// cheap to call every frame while nothing is previewed.
    pub(crate) fn clear(&mut self) {
        if self.last_outputs.is_empty()
            && self.texture.is_none()
            && self.submitted_key.is_none()
            && self.pending_key.is_none()
            && self.structural_error.is_none()
            && self.eval_error.is_none()
        {
            return;
        }
        self.current_cancel.cancel();
        self.submitted_key = None;
        self.pending_key = None;
        self.structural_error = None;
        self.eval_error = None;
        self.last_outputs = Vec::new();
        self.last_histogram = None;
        self.histogram_target = None;
        self.texture = None;
        self.texture_key = None;
    }

    /// The current shading mode, for the pane's toggle.
    pub(crate) fn mode(&self) -> ShadeMode {
        self.mode
    }

    /// Sets the shading mode; the texture is rebuilt on the next `poll` if it changed.
    pub(crate) fn set_mode(&mut self, mode: ShadeMode) {
        self.mode = mode;
    }

    /// The current Height display scale, for the pane's toggle.
    pub(crate) fn scale(&self) -> HeightScale {
        self.scale
    }

    /// Sets the Height display scale; the texture is rebuilt on the next `poll` if it
    /// changed.
    pub(crate) fn set_scale(&mut self, scale: HeightScale) {
        self.scale = scale;
    }

    /// Mirrors the World settings' sea level and Show water toggle into the preview, so the water
    /// overlay follows the same controls as the 3D plane. The texture recomposites on the next
    /// `poll` if either changed (no graph re-evaluation).
    pub(crate) fn set_water(&mut self, sea_level: f32, show_water: bool) {
        self.sea_level = sea_level;
        self.show_water = show_water;
    }

    /// The output index the preview is set to display.
    pub(crate) fn display_output(&self) -> usize {
        self.display_output
    }

    /// Sets the output index to display; the texture is rebuilt on the next `poll` if it
    /// changed.
    pub(crate) fn set_display_output(&mut self, index: usize) {
        self.display_output = index;
    }

    /// The input distribution (normalized bin heights over `[0, 1]`) of `node`, for the
    /// histogram behind its curve/levels editor (#15). `None` unless the most recent
    /// preview result is for that node, so the histogram always matches the editor.
    pub(crate) fn input_histogram(&self, node: u64) -> Option<&[f32]> {
        if self.histogram_target == Some(node) {
            self.last_histogram.as_deref()
        } else {
            None
        }
    }

    /// Submits a fresh evaluation if the previewed output would differ from the last
    /// one submitted. A structural error (disconnected input, cycle) is detected
    /// here, synchronously and cheaply, and shown instead of submitting work.
    pub(crate) fn sync(
        &mut self,
        graph: &Graph,
        target: NodeId,
        request: EvalRequest,
        now: f64,
        binding: Option<&crate::SubgraphInputs>,
    ) {
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
            self.submit(graph, target, request, binding);
        }
    }

    fn submit(
        &mut self,
        graph: &Graph,
        target: NodeId,
        request: EvalRequest,
        binding: Option<&crate::SubgraphInputs>,
    ) {
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
            binding: binding.cloned(),
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
            Outcome::Ready {
                fields,
                target,
                histogram,
                ..
            } => {
                self.eval_error = None;
                // Keep all outputs; `refresh_texture` re-uploads only when the shown output
                // or the shading mode changed.
                self.last_outputs = fields;
                self.last_histogram = histogram;
                self.histogram_target = Some(target);
            }
            Outcome::Failed { message, .. } => {
                self.eval_error = Some(message);
                self.last_outputs = Vec::new();
                self.last_histogram = None;
                self.histogram_target = None;
                self.texture = None;
                self.texture_key = None;
            }
        }
    }

    /// Rebuilds the preview texture from the last field when the field or the shading
    /// mode has changed since the texture was uploaded. Cheap to call every frame.
    fn refresh_texture(&mut self, ctx: &egui::Context) {
        let index = self
            .display_output
            .min(self.last_outputs.len().saturating_sub(1));
        let Some(field) = self.last_outputs.get(index) else {
            return;
        };
        // Each output is a standalone field; show its height layer. Sea level enters the key only
        // while water is shown, so toggling water off frees the slider from rebuilding.
        let water_bits = if self.show_water {
            self.sea_level.to_bits()
        } else {
            0
        };
        let key = (
            field.content_hash().to_u64(),
            index,
            self.mode,
            self.scale,
            self.light.map(f32::to_bits),
            water_bits,
            self.show_water,
        );
        if self.texture_key == Some(key) {
            return;
        }
        let mut image = field_to_image(field, layers::HEIGHT, self.mode, self.scale, self.light);
        if self.show_water {
            apply_water(
                &mut image,
                field,
                layers::HEIGHT,
                self.sea_level,
                &WaterStyle::default(),
            );
        }
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
                // Bound by height too, so the square preview fits the fixed-height pane
                // instead of overflowing it at narrow widths.
                .max_height(ui.available_height())
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

    /// Steers the relief light from a drag at `pos` over `rect` (the previewed image); the exact
    /// centre is ignored, keeping the current light.
    fn set_light_from_drag(&mut self, pos: egui::Pos2, rect: egui::Rect) {
        if let Some(light) = crate::sun::light_from_drag(pos, rect) {
            self.light = light;
        }
    }

    /// The relief light's azimuth and altitude in degrees, for the dial readout.
    pub(crate) fn light_angles(&self) -> (f32, f32) {
        crate::sun::light_angles(self.light)
    }

    /// The relief sun dial: shows the light direction and steers it on drag. Only meaningful in
    /// relief mode (#40). A thin wrapper over the shared [`crate::sun::dial`] widget.
    pub(crate) fn light_indicator(&mut self, ui: &mut egui::Ui) {
        crate::sun::dial(ui, &mut self.light);
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
    // Inside a subgraph (#106), bind the live input fields so the preview shows real data
    // rather than the Input markers' zero stand-in.
    let bound = job
        .binding
        .as_ref()
        .map(|b| b.bound_fields(&job.graph, &job.request));
    // The (source, port) pairs to display: a normal node's own outputs (so a multi-output
    // node previews all of them), or for an Output marker the single field feeding it (the
    // subgraph's result), since the marker itself is an endpoint with no output.
    let is_output_marker = job
        .graph
        .spec(target)
        .is_some_and(|spec| spec.type_id == OUTPUT_TYPE_ID);
    let outs: Vec<(NodeId, usize)> = if is_output_marker {
        job.graph.input_source(target, 0).into_iter().collect()
    } else {
        let output_count = job.graph.spec(target).map_or(0, |spec| spec.outputs.len());
        (0..output_count).map(|i| (target, i)).collect()
    };
    let result = match &bound {
        Some(bound) => job.graph.evaluate_bound(bound, &outs, &job.request, cache),
        // An Output marker reads the field feeding it, so it goes through evaluate_bound
        // (with no bindings) even at the top level.
        None if is_output_marker => job.graph.evaluate_bound(&[], &outs, &job.request, cache),
        // A normal node evaluates directly, which flushes the worker cache for cross-job reuse.
        None => job
            .graph
            .evaluate(target, &job.request, cache)
            .map(|a| a.to_vec()),
    };
    match result {
        Ok(outputs) => Some(if outputs.is_empty() {
            Outcome::Failed {
                generation,
                message: "node has no output to preview".to_string(),
            }
        } else {
            let histogram = match &bound {
                Some(bound) => {
                    bound_input_histogram(&job.graph, target, &job.request, cache, bound)
                }
                // The node's input source is upstream of the target, so it is already in
                // the cache from the eval above; this re-eval is a cheap hit.
                None => input_histogram(&job.graph, target, &job.request, cache),
            };
            Outcome::Ready {
                generation,
                target: job.target,
                fields: outputs,
                histogram,
            }
        }),
        Err(Error::Cancelled) => None,
        Err(err) => Some(Outcome::Failed {
            generation,
            message: err.to_string(),
        }),
    }
}

/// The distribution of `target`'s first input field, as normalized histogram bins, or
/// `None` when the node has no wired input (a generator) or the input cannot be
/// evaluated. Best-effort: a display aid, never the cause of a preview failure.
fn input_histogram(
    graph: &Graph,
    target: NodeId,
    request: &EvalRequest,
    cache: &mut EvalCache,
) -> Option<Vec<f32>> {
    let (source, port) = graph.input_source(target, 0)?;
    let fields = graph.evaluate(source, request, cache).ok()?;
    let field = fields.get(port)?;
    Some(field_histogram(field, HISTOGRAM_BINS))
}

/// Like [`input_histogram`], but inside a subgraph (#106): the input source is evaluated
/// with the live fields bound to the Input markers, so the histogram matches the real input
/// the curve/levels editor is shaping rather than a flat zero.
fn bound_input_histogram(
    graph: &Graph,
    target: NodeId,
    request: &EvalRequest,
    cache: &mut EvalCache,
    bound: &[(NodeId, Field)],
) -> Option<Vec<f32>> {
    let (source, port) = graph.input_source(target, 0)?;
    let fields = graph
        .evaluate_bound(bound, &[(source, port)], request, cache)
        .ok()?;
    Some(field_histogram(fields.first()?, HISTOGRAM_BINS))
}

/// Bins a field's `height` layer into `bins` buckets over `[0, 1]`, returning each
/// bin's count normalized to the tallest bin (so heights are in `[0, 1]` for drawing).
/// Values outside `[0, 1]` clamp into the end bins, so out-of-range data shows as a
/// spike against the edge. Order-independent, so it is deterministic.
fn field_histogram(field: &Field, bins: usize) -> Vec<f32> {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let mut counts = vec![0u32; bins];
    for &value in layer.as_slice() {
        let t = value.clamp(0.0, 1.0);
        let idx = ((t * bins as f32) as usize).min(bins - 1);
        counts[idx] += 1;
    }
    let max = counts.iter().copied().max().unwrap_or(0).max(1);
    counts.iter().map(|&c| c as f32 / max as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_histogram_bins_clamps_and_normalizes() {
        use std::sync::Arc;
        use ymir_core::{Layer, Region, layers};

        // Values 0.0, 0.5, 2.0 (clamps to 1.0), -1.0 (clamps to 0.0) across a 4-bin range.
        let field = Field::new(2, 2, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(2, 2, |x, y| {
                [0.0, 0.5, 2.0, -1.0][y * 2 + x]
            })),
        );
        // bin 0: 0.0 and the clamped -1.0 (count 2); bin 2: 0.5; bin 3: the clamped 2.0.
        // Normalized to the tallest bin (count 2): [1.0, 0.0, 0.5, 0.5].
        assert_eq!(field_histogram(&field, 4), vec![1.0, 0.0, 0.5, 0.5]);
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
            binding: None,
        };

        // A superseded (pre-cancelled) job evaluates to nothing, so the worker
        // reports no stale result.
        let mut cache = EvalCache::new(4);
        assert!(evaluate_job(&job, &mut cache).is_none());
    }

    #[test]
    fn output_marker_previews_the_field_feeding_it() {
        use ymir_core::{Params, Region, registry};

        // An Output marker is an endpoint, but previewing it shows the field wired into it
        // (the subgraph's result), not a "no output" failure.
        let mut graph = Graph::new();
        let source = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let marker = graph.add_op(
            registry::make("subgraph.output").expect("output marker"),
            Params::new(),
        );
        graph
            .connect(source, 0, marker, 0)
            .expect("source -> marker");
        let target = graph.stable_id(marker).expect("handle");
        let job = Job {
            graph,
            target,
            request: EvalRequest::new(16, 16, Region::UNIT, 0),
            generation: 1,
            binding: None,
        };

        let mut cache = EvalCache::new(4);
        match evaluate_job(&job, &mut cache).expect("not cancelled") {
            Outcome::Ready { fields, .. } => {
                assert_eq!(fields.len(), 1, "previews the field feeding the marker");
            }
            Outcome::Failed { message, .. } => panic!("expected a preview, got: {message}"),
        }
    }
}
