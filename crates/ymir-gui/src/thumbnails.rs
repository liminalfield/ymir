//! Per-node heightmap thumbnails (#42/#72).
//!
//! A second background evaluator, alongside the preview engine, that renders a small
//! grayscale image of *every visible node's* output and uploads one texture per node
//! for the canvas to draw in the node body. It mirrors [`PreviewEngine`] but is
//! multi-target: one worker, recompute driven only by per-node `output_key` change (the
//! same signal behind the stale dots), throttled and latest-wins.
//!
//! # Resolution (#382)
//!
//! Nodes are evaluated at the *preview's* resolution and the resulting image is scaled
//! down, rather than the graph being evaluated small. Evaluating small is not a cheap
//! approximation of the node's output: erosion is resolution-dependent physics, so a
//! thumbnail computed at 96 cells is a picture of different terrain, and anything
//! downstream that thresholds it turns a small difference into a total one. A thumbnail
//! that shows something other than the node's output is worse than none, because it is
//! read as information.
//!
//! That is affordable only because the cache is shared with the preview
//! ([`LiveCache`](crate::live_cache::LiveCache)): the previewed node's chain is already
//! computed, so most thumbnails cost a cache hit and a downscale. A node off that chain
//! is genuinely evaluated, once, and then cached like any other.
//!
//! [`PreviewEngine`]: crate::preview::PreviewEngine

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use eframe::egui;
use ymir_core::{CancelToken, EvalRequest, Graph, OUTPUT_TYPE_ID};

use crate::canvas::Handle;
use crate::live_cache::LiveCache;
use crate::shade::{HeightScale, height_image, reduced};

/// Thumbnail *image* size, in pixels a side. The evaluation happens at the preview's
/// resolution (see the module docs); this is only how far the picture is scaled down
/// before it is uploaded, and the canvas draws it smaller still inside a node body.
pub(crate) const THUMB_RES: usize = 96;
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
    /// When evaluating inside a subgraph (#106), the live fields to bind to its Input markers
    /// so the interior shows real data instead of the zero stand-in. `None` at the top level.
    binding: Option<crate::SubgraphInputs>,
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
    /// Starts the engine and its worker, evaluating against the shared live cache (#382).
    pub(crate) fn new(cache: LiveCache) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<Shaded>();
        let worker = thread::spawn(move || worker_loop(&job_rx, &result_tx, &cache));
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
        binding: Option<&crate::SubgraphInputs>,
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
        self.submit(graph, &targets, request, binding);
        for t in &targets {
            if let Some(e) = self.entries.get_mut(&t.handle) {
                e.in_flight_key = Some(t.key);
            }
        }
        self.last_submit_time = now;
    }

    fn submit(
        &mut self,
        graph: &Graph,
        targets: &[Target],
        request: &EvalRequest,
        binding: Option<&crate::SubgraphInputs>,
    ) {
        // Abort whatever the worker is computing: it is now superseded.
        self.current_cancel.cancel();
        let cancel = CancelToken::new();
        self.current_cancel = cancel.clone();
        let job = Job {
            graph: graph.clone(),
            targets: targets.to_vec(),
            request: request.clone().with_cancel(cancel),
            binding: binding.cloned(),
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

/// The worker: evaluates submitted jobs against the shared live cache, draining to
/// the newest queued job so a backlog collapses to the current state. Exits when the
/// job channel closes (the engine is dropped).
fn worker_loop(job_rx: &Receiver<Job>, result_tx: &Sender<Shaded>, cache: &LiveCache) {
    while let Ok(mut job) = job_rx.recv() {
        while let Ok(newer) = job_rx.try_recv() {
            job = newer;
        }
        for shaded in evaluate_thumb_job(&job, cache) {
            if result_tx.send(shaded).is_err() {
                return; // the UI is gone
            }
        }
    }
}

/// Evaluates each target to a small grayscale image, sharing the cache so common
/// upstreams compute once. A node that fails (disconnected, cycle, cancelled, or an
/// operator error) simply yields no thumbnail this round.
///
/// The cache is locked per node rather than for the whole pass, so a preview job submitted
/// while this is running waits for one node rather than for the whole visible graph.
fn evaluate_thumb_job(job: &Job, cache: &LiveCache) -> Vec<Shaded> {
    let mut out = Vec::new();
    // Thumbnails always show the height layer, auto-ranged, so each node's shape is legible
    // at a glance regardless of its amplitude. The field is reduced to the thumbnail's pixel
    // size first: it was evaluated at the preview's resolution so that it is the node's real
    // output (#382), and what the canvas draws is a scaled-down picture of it.
    let push = |out: &mut Vec<Shaded>, t: &Target, field: &ymir_core::Field| {
        let small = reduced(field, ymir_core::layers::HEIGHT, THUMB_RES);
        out.push(Shaded {
            handle: t.handle,
            key: t.key,
            image: height_image(&small, ymir_core::layers::HEIGHT, HeightScale::Auto),
        });
    };
    // Inside a subgraph (#106), bind the live input fields to the Input markers, so the
    // interior is shaded against real data rather than the markers' zero stand-in.
    let bound = job
        .binding
        .as_ref()
        .map(|b| b.bound_fields(&job.graph, &job.request, &mut cache.lock()));
    for t in &job.targets {
        let Some(node_id) = job.graph.node_id_of(t.handle) else {
            continue;
        };
        // The field a node's thumbnail shows: its own output 0, except an Output marker
        // (an endpoint, #106) which shows the field feeding it — the subgraph's result at
        // that port. An unwired Output marker has nothing to show.
        let is_output_marker = job
            .graph
            .spec(node_id)
            .is_some_and(|spec| spec.type_id == OUTPUT_TYPE_ID);
        let source = if is_output_marker {
            match job.graph.input_source(node_id, 0) {
                Some(source) => source,
                None => continue,
            }
        } else {
            (node_id, 0)
        };
        let field = {
            let mut cache = cache.lock();
            match &bound {
                Some(bound) => job
                    .graph
                    .evaluate_bound(bound, &[source], &job.request, &mut cache)
                    .ok()
                    .and_then(|fields| fields.into_iter().next()),
                None => job
                    .graph
                    .evaluate(source.0, &job.request, &mut cache)
                    .ok()
                    .and_then(|outputs| outputs.get(source.1).cloned()),
            }
        };
        if let Some(field) = field {
            push(&mut out, t, &field);
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
    fn output_marker_thumbnail_shows_the_field_feeding_it() {
        // An Output marker is an endpoint, but its thumbnail shows the field wired into it
        // (the subgraph's result), not its own (absent) output.
        let mut graph = Graph::new();
        let source = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let marker = graph.add_op(
            registry::make("subgraph.output").expect("output marker"),
            Params::new(),
        );
        graph
            .connect(source, 0, marker, 0)
            .expect("source -> marker");
        let handle = graph.stable_id(marker).expect("handle");
        let job = Job {
            graph,
            targets: vec![Target { handle, key: 1 }],
            request: EvalRequest::new(16, 16, Region::UNIT, 0),
            binding: None,
        };
        let images = evaluate_thumb_job(&job, &LiveCache::new());
        assert_eq!(images.len(), 1, "the marker shows its incoming field");
        assert_eq!(images[0].handle, handle);
        assert_eq!(images[0].image.size, [16, 16]);
    }

    #[test]
    fn the_image_is_reduced_to_thumbnail_size_not_the_evaluation() {
        // The defect this fixes (#382): a thumbnail used to be evaluated at 96 cells, which for
        // anything downstream of erosion is a picture of different terrain rather than a smaller
        // picture of this one. The evaluation now runs at whatever resolution the request carries
        // (the preview's), and only the image is scaled down.
        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let handle = graph.stable_id(id).expect("stable id");
        let request = EvalRequest::new(256, 256, Region::UNIT, 0);
        let job = Job {
            graph,
            targets: vec![Target { handle, key: 1 }],
            request,
            binding: None,
        };
        let images = evaluate_thumb_job(&job, &LiveCache::new());
        assert_eq!(images.len(), 1);
        assert_eq!(
            images[0].image.size,
            [THUMB_RES, THUMB_RES],
            "the picture is thumbnail-sized however large the field it came from"
        );
    }

    #[test]
    fn a_thumbnail_of_a_previewed_node_costs_no_second_evaluation() {
        // Why the resolution above is affordable: the preview and the thumbnails share one cache,
        // so a node the preview already built is a hit here rather than a second run of it. With a
        // cache each, an eroded node was computed twice, concurrently, at full preview resolution.
        let cache = LiveCache::new();
        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let handle = graph.stable_id(id).expect("stable id");
        let request = EvalRequest::new(64, 64, Region::UNIT, 0);

        // Stand in for the preview worker: evaluate the node through the shared cache.
        graph
            .evaluate(id, &request, &mut cache.lock())
            .expect("preview evaluates");

        let job = Job {
            graph: graph.clone(),
            targets: vec![Target { handle, key: 1 }],
            request: request.clone(),
            binding: None,
        };
        assert_eq!(evaluate_thumb_job(&job, &cache).len(), 1);
        // Still current after the thumbnail pass: it read the entry rather than replacing it.
        let status = graph
            .cache_status(id, &request, &cache.lock())
            .expect("status");
        assert_eq!(
            status.get(&id),
            Some(&true),
            "the thumbnail must reuse the preview's cached result"
        );
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
            binding: None,
        };
        let images = evaluate_thumb_job(&job, &LiveCache::new());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].handle, handle);
        assert_eq!(images[0].key, 7);
        assert_eq!(images[0].image.size, [16, 16]);
    }
}
