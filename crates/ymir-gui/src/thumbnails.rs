//! Per-node heightmap thumbnails (#42/#72).
//!
//! A second background evaluator, alongside the preview engine, that renders a small
//! grayscale image of *every visible node's* output and uploads one texture per node
//! for the canvas to draw in the node body. It mirrors [`PreviewEngine`] but is
//! multi-target: one worker, a shared persistent cache so common upstreams compute
//! once, and recompute driven only by per-node `output_key` change (the same signal
//! behind the stale dots), throttled and latest-wins.
//!
//! [`PreviewEngine`]: crate::preview::PreviewEngine

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use eframe::egui;
use ymir_core::{CancelToken, EvalCache, EvalRequest, Graph};

use crate::canvas::Handle;
use crate::shade::{HeightScale, height_image};

/// Thumbnail evaluation resolution. Small enough that even erosion is cheap per node;
/// the canvas only ever draws these scaled down into a node body.
pub(crate) const THUMB_RES: usize = 96;
/// Worker cache capacity, in cached node results. Larger than the preview's so a
/// graph's worth of small thumbnail fields can stay resident.
const THUMB_CACHE_CAP: usize = 128;
/// Minimum interval between thumbnail submissions, so a fast parameter drag throttles
/// instead of resubmitting every frame.
const THUMB_DEBOUNCE_SECS: f64 = 0.08;
/// Texture uploads applied per frame. Uploads are the UI-thread cost, so a large batch
/// (e.g. the world seed changed, so every node's thumbnail did) is spread over frames.
const THUMB_MAX_UPLOADS_PER_FRAME: usize = 6;

/// A node to (re)evaluate for its thumbnail, tagged with the `output_key` it is being
/// computed for, so a result can be matched against the node's current desired key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    handle: Handle,
    key: u64,
}

/// A unit of thumbnail work: a graph snapshot and the set of nodes to evaluate.
struct Job {
    graph: Graph,
    targets: Vec<Target>,
    request: EvalRequest,
}

/// One shaded node result.
struct Shaded {
    handle: Handle,
    key: u64,
    image: egui::ColorImage,
}

/// The per-node thumbnail state held on the UI thread.
#[derive(Default)]
struct ThumbEntry {
    /// The node's current `output_key` (what its thumbnail *should* show).
    desired_key: Option<u64>,
    /// The key the uploaded texture was built from.
    texture_key: Option<u64>,
    /// The key currently being computed on the worker, if any.
    in_flight_key: Option<u64>,
    texture: Option<egui::TextureHandle>,
}

/// Drives background thumbnail evaluation. The UI calls [`sync`](Self::sync) with the
/// visible nodes each frame, [`poll`](Self::poll) to collect results, and
/// [`texture`](Self::texture) to fetch a node's thumbnail for drawing.
pub(crate) struct ThumbnailEngine {
    job_tx: Sender<Job>,
    result_rx: Receiver<Shaded>,
    _worker: JoinHandle<()>,
    entries: HashMap<Handle, ThumbEntry>,
    last_submit_time: f64,
    /// Cancellation for the in-flight job, fired when a newer job supersedes it so a
    /// slow batch aborts instead of finishing.
    current_cancel: CancelToken,
}

impl ThumbnailEngine {
    pub(crate) fn new() -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<Shaded>();
        let worker = thread::spawn(move || worker_loop(&job_rx, &result_tx));
        Self {
            job_tx,
            result_rx,
            _worker: worker,
            entries: HashMap::new(),
            last_submit_time: 0.0,
            current_cancel: CancelToken::new(),
        }
    }

    /// Updates the desired key of each visible node, drops entries no longer visible,
    /// and submits the dirty set (throttled) when there is work not already in flight.
    pub(crate) fn sync(
        &mut self,
        graph: &Graph,
        visible: &[Handle],
        request: &EvalRequest,
        now: f64,
    ) {
        // Forget nodes that are no longer visible (drops their textures).
        let present: HashSet<Handle> = visible.iter().copied().collect();
        self.entries.retain(|h, _| present.contains(h));

        // Refresh each visible node's desired key. A structural error (disconnected
        // input, cycle) or a removed node has no thumbnail, so drop its entry.
        for &handle in visible {
            match graph
                .node_id_of(handle)
                .and_then(|id| graph.output_key(id, request).ok())
            {
                Some(key) => self.entries.entry(handle).or_default().desired_key = Some(key),
                None => {
                    self.entries.remove(&handle);
                }
            }
        }

        // Throttle submissions, then submit the full dirty set when there is genuinely
        // new work (a node whose desired output is neither shown nor already in flight).
        if now - self.last_submit_time < THUMB_DEBOUNCE_SECS {
            return;
        }
        let targets = plan_submit(&self.entries);
        if targets.is_empty() {
            return;
        }
        self.submit(graph, &targets, request);
        for t in &targets {
            if let Some(e) = self.entries.get_mut(&t.handle) {
                e.in_flight_key = Some(t.key);
            }
        }
        self.last_submit_time = now;
    }

    fn submit(&mut self, graph: &Graph, targets: &[Target], request: &EvalRequest) {
        // Abort whatever the worker is computing: it is now superseded.
        self.current_cancel.cancel();
        let cancel = CancelToken::new();
        self.current_cancel = cancel.clone();
        let job = Job {
            graph: graph.clone(),
            targets: targets.to_vec(),
            request: request.clone().with_cancel(cancel),
        };
        let _ = self.job_tx.send(job); // shortcut-ok: worker only stops at app exit; nothing to recover
    }

    /// Collects worker results, uploading a texture for each whose key still matches
    /// the node's desired output. Texture uploads are the UI-thread cost, so cap them
    /// per frame and let the rest stream in over the next frames (a capped result keeps
    /// its in-flight marker, so the repaint below keeps draining it). Stale results are
    /// drained without counting against the cap.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let mut uploaded = 0;
        while uploaded < THUMB_MAX_UPLOADS_PER_FRAME {
            match self.result_rx.try_recv() {
                Ok(shaded) => {
                    if self.apply(shaded, ctx) {
                        uploaded += 1;
                    }
                }
                Err(_) => break,
            }
        }
        let waiting = self
            .entries
            .values()
            .any(|e| e.in_flight_key.is_some() && e.in_flight_key != e.texture_key);
        if waiting {
            ctx.request_repaint();
        }
    }

    /// Applies one result, returning whether it uploaded a texture (a stale,
    /// superseded result does not, so it does not count against the per-frame cap).
    fn apply(&mut self, shaded: Shaded, ctx: &egui::Context) -> bool {
        let Some(e) = self.entries.get_mut(&shaded.handle) else {
            return false;
        };
        // Apply only if this is still the node's desired output; otherwise a newer
        // change has superseded it and the result is stale.
        let uploaded = e.desired_key == Some(shaded.key);
        if uploaded {
            let name = format!("thumb-{}", shaded.handle);
            e.texture = Some(ctx.load_texture(name, shaded.image, egui::TextureOptions::LINEAR));
            e.texture_key = Some(shaded.key);
        }
        if e.in_flight_key == Some(shaded.key) {
            e.in_flight_key = None;
        }
        uploaded
    }

    /// The node's thumbnail texture, if one has been computed.
    pub(crate) fn texture(&self, handle: Handle) -> Option<&egui::TextureHandle> {
        self.entries.get(&handle).and_then(|e| e.texture.as_ref())
    }
}

/// The dirty set to submit: empty unless some node has *new* work (a desired output
/// neither shown nor already in flight). When there is, the full dirty set is
/// returned (every node whose texture is out of date), so a job superseded by a newer
/// submit never strands a node — the newer job carries it. Pure and unit-tested.
fn plan_submit(entries: &HashMap<Handle, ThumbEntry>) -> Vec<Target> {
    let has_new_work = entries.values().any(|e| {
        e.desired_key.is_some()
            && e.desired_key != e.texture_key
            && e.desired_key != e.in_flight_key
    });
    if !has_new_work {
        return Vec::new();
    }
    let mut dirty: Vec<Target> = entries
        .iter()
        .filter_map(|(&handle, e)| {
            let key = e.desired_key?;
            (Some(key) != e.texture_key).then_some(Target { handle, key })
        })
        .collect();
    // Deterministic order: stable submission and shared-cache locality.
    dirty.sort_by_key(|t| t.handle);
    dirty
}

/// The worker: evaluates submitted jobs with a persistent shared cache, draining to
/// the newest queued job so a backlog collapses to the current state. Exits when the
/// job channel closes (the engine is dropped).
fn worker_loop(job_rx: &Receiver<Job>, result_tx: &Sender<Shaded>) {
    let mut cache = EvalCache::new(THUMB_CACHE_CAP);
    while let Ok(mut job) = job_rx.recv() {
        while let Ok(newer) = job_rx.try_recv() {
            job = newer;
        }
        for shaded in evaluate_thumb_job(&job, &mut cache) {
            if result_tx.send(shaded).is_err() {
                return; // the UI is gone
            }
        }
    }
}

/// Evaluates each target to a small grayscale image, sharing the cache so common
/// upstreams compute once. A node that fails (disconnected, cycle, cancelled, or an
/// operator error) simply yields no thumbnail this round.
fn evaluate_thumb_job(job: &Job, cache: &mut EvalCache) -> Vec<Shaded> {
    let mut out = Vec::new();
    for t in &job.targets {
        let Some(node_id) = job.graph.node_id_of(t.handle) else {
            continue;
        };
        if let Ok(outputs) = job.graph.evaluate(node_id, &job.request, cache)
            && let Some(field) = outputs.first()
        {
            out.push(Shaded {
                handle: t.handle,
                key: t.key,
                // Thumbnails always show the height layer, auto-ranged, so each node's shape
                // is legible at a glance regardless of its amplitude.
                image: height_image(field, ymir_core::layers::HEIGHT, HeightScale::Auto),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Params, Region, registry};

    fn entry(desired: Option<u64>, texture: Option<u64>, in_flight: Option<u64>) -> ThumbEntry {
        ThumbEntry {
            desired_key: desired,
            texture_key: texture,
            in_flight_key: in_flight,
            texture: None,
        }
    }

    #[test]
    fn plan_submit_is_empty_with_no_new_work() {
        let mut entries = HashMap::new();
        // Up to date.
        entries.insert(1, entry(Some(10), Some(10), None));
        // Dirty, but already being computed for that key.
        entries.insert(2, entry(Some(20), None, Some(20)));
        assert!(plan_submit(&entries).is_empty());
    }

    #[test]
    fn plan_submit_returns_the_full_dirty_set_when_new_work_exists() {
        let mut entries = HashMap::new();
        entries.insert(1, entry(Some(10), Some(10), None)); // up to date -> excluded
        entries.insert(2, entry(Some(20), None, Some(20))); // in flight, still dirty -> included
        entries.insert(3, entry(Some(30), None, None)); // new work -> triggers a submit
        let handles: Vec<Handle> = plan_submit(&entries).iter().map(|t| t.handle).collect();
        // Node 1 is current; 2 and 3 are dirty (2 carried along for drain-safety).
        assert_eq!(handles, vec![2, 3]);
    }

    #[test]
    fn evaluate_thumb_job_produces_a_sized_image() {
        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let handle = graph.stable_id(id).expect("stable id");
        let job = Job {
            graph,
            targets: vec![Target { handle, key: 7 }],
            request: EvalRequest::new(16, 16, Region::UNIT, 0),
        };
        let mut cache = EvalCache::new(8);
        let images = evaluate_thumb_job(&job, &mut cache);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].handle, handle);
        assert_eq!(images[0].key, 7);
        assert_eq!(images[0].image.size, [16, 16]);
    }
}
