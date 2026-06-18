//! The pull-based, memoized evaluator.
//!
//! Evaluation pulls from a requested target node, recursively evaluating its
//! upstream inputs, and memoizes results. Two cache tiers cooperate:
//!
//! - A per-pull working set holds every node the current evaluation touches and
//!   is never evicted mid-pull, so a small persistent cache can never drop a
//!   result the active path still needs (no thrashing on deep graphs).
//! - A persistent [`EvalCache`], bounded by an LRU policy, carries results across
//!   evaluations so only nodes downstream of a change recompute.
//!
//! A node's cache key is composed from its upstream nodes' keys, not by
//! re-hashing their full-resolution output fields: determinism makes an upstream
//! key a faithful proxy for its output, which keeps the key cheap. True
//! output-byte hashing stays reserved for golden tests.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::context::EvalContext;
use crate::error::{Error, Result};
use crate::field::Field;
use crate::graph::{Graph, NodeId};
use crate::hash::Fnv1a64;
use crate::param::Params;
use crate::region::Region;

/// The global parameters of one evaluation request: resolution, region, and the
/// global seed. The target node is a separate argument to [`Graph::evaluate`],
/// since which node is requested is the evaluator's concern, not an operator's.
#[derive(Clone, Debug)]
pub struct EvalRequest {
    /// Requested grid width in cells.
    pub width: usize,
    /// Requested grid height in cells.
    pub height: usize,
    /// World-space region to evaluate.
    pub region: Region,
    /// Global seed; each node's seed is derived from this and its `stable_id`.
    pub seed: u64,
    /// Cancellation signal, threaded into each node's context; defaults to
    /// never-cancel.
    cancel: CancelToken,
}

impl EvalRequest {
    /// Creates an evaluation request with no cancellation attached.
    #[must_use]
    pub fn new(width: usize, height: usize, region: Region, seed: u64) -> Self {
        Self {
            width,
            height,
            region,
            seed,
            cancel: CancelToken::new(),
        }
    }

    /// Attaches a cancellation token. The GUI cancels it when a newer change
    /// supersedes this evaluation; the evaluator polls it between nodes and
    /// long-running operators poll it inside their loops, both aborting with
    /// [`Error::Cancelled`].
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }
}

/// A bounded, cross-evaluation result cache with least-recently-used eviction.
///
/// This is the persistent tier only. Within a single evaluation the active path
/// is pinned separately, so entries here may be evicted freely without ever
/// dropping a result the current pull still needs.
pub struct EvalCache {
    entries: HashMap<NodeId, CacheEntry>,
    capacity: usize,
    tick: u64,
}

struct CacheEntry {
    key: u64,
    outputs: Arc<Vec<Field>>,
    last_used: u64,
}

impl EvalCache {
    /// Creates a cache holding at most `capacity` node results across
    /// evaluations. A capacity of zero keeps nothing between pulls (the active
    /// path is still pinned within each pull).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            tick: 0,
        }
    }

    /// Whether `id`'s cached result is still valid for `key` (read-only; does not
    /// touch recency). Backs [`Graph::cache_status`].
    fn is_valid(&self, id: NodeId, key: u64) -> bool {
        self.entries.get(&id).is_some_and(|e| e.key == key)
    }

    /// Returns the cached outputs for `id` if present and still keyed by `key`.
    fn get(&mut self, id: NodeId, key: u64) -> Option<Arc<Vec<Field>>> {
        let entry = self.entries.get_mut(&id)?;
        if entry.key != key {
            return None;
        }
        self.tick += 1;
        entry.last_used = self.tick;
        Some(Arc::clone(&entry.outputs))
    }

    /// Inserts or replaces `id`'s result, evicting the least-recently-used entry
    /// while over capacity.
    fn insert(&mut self, id: NodeId, key: u64, outputs: Arc<Vec<Field>>) {
        self.tick += 1;
        let last_used = self.tick;
        self.entries.insert(
            id,
            CacheEntry {
                key,
                outputs,
                last_used,
            },
        );

        while self.entries.len() > self.capacity {
            // last_used is unique per access, so the minimum is unambiguous and
            // eviction is deterministic.
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(&id, _)| id)
            else {
                break;
            };
            self.entries.remove(&victim);
        }
    }
}

impl Graph {
    /// Evaluates `target`, returning its output fields (one per output port).
    ///
    /// Results are memoized in `cache` across calls, so re-evaluating after
    /// changing one node recomputes only that node and what is downstream of it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cycle`] if the reachable subgraph has a cycle,
    /// [`Error::NodeNotFound`] if `target` or an upstream node is missing,
    /// [`Error::DisconnectedInput`] if a required input is unconnected, or any
    /// error an operator returns.
    pub fn evaluate(
        &self,
        target: NodeId,
        request: &EvalRequest,
        cache: &mut EvalCache,
    ) -> Result<Arc<Vec<Field>>> {
        let mut computed: HashMap<NodeId, (u64, Arc<Vec<Field>>)> = HashMap::new();
        let mut in_progress: HashSet<NodeId> = HashSet::new();
        let result = self.pull(target, request, cache, &mut computed, &mut in_progress)?;

        // Flush the active path into the persistent cache once the pull is done,
        // so the next evaluation can reuse unchanged nodes.
        for (id, (key, outputs)) in computed {
            cache.insert(id, key, outputs);
        }
        Ok(result)
    }

    fn pull(
        &self,
        id: NodeId,
        request: &EvalRequest,
        cache: &mut EvalCache,
        computed: &mut HashMap<NodeId, (u64, Arc<Vec<Field>>)>,
        in_progress: &mut HashSet<NodeId>,
    ) -> Result<Arc<Vec<Field>>> {
        // Bail as soon as a newer change has superseded this evaluation.
        if request.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Already produced in this pull (e.g. a diamond's shared ancestor).
        if let Some((_, outputs)) = computed.get(&id) {
            return Ok(Arc::clone(outputs));
        }
        // A node re-entered while still on the stack is a back edge: a cycle.
        if !in_progress.insert(id) {
            return Err(Error::Cycle);
        }

        let node = self.node(id).ok_or(Error::NodeNotFound)?;

        // Evaluate each input, collecting its output Arc, the consumed output
        // index, and its cache key (for composing this node's key).
        let mut upstream: Vec<(Arc<Vec<Field>>, usize, u64)> =
            Vec::with_capacity(node.inputs.len());
        for (port, slot) in node.inputs.iter().enumerate() {
            let conn = slot.as_ref().ok_or(Error::DisconnectedInput {
                type_id: node.type_id,
                port,
            })?;
            let outputs = self.pull(conn.source, request, cache, computed, in_progress)?;
            let upstream_key = computed.get(&conn.source).map_or(0, |(key, _)| *key);
            upstream.push((outputs, conn.output, upstream_key));
        }

        let seed = derive_seed(request.seed, node.stable_id);
        let input_keys: Vec<u64> = upstream.iter().map(|(_, _, key)| *key).collect();
        let key = compute_key(node.type_id, &node.params, &input_keys, request, seed);

        // An endpoint produces no field, so there is nothing to memoize: its job
        // is the side effect (e.g. writing a file), which must happen on every
        // pull. Only non-endpoints consult or populate the cache. Its upstream
        // fields are still memoized normally, which is where the savings are.
        let is_endpoint = node.output_count == 0;

        // Reuse a persistent result if its key still matches.
        if !is_endpoint && let Some(outputs) = cache.get(id, key) {
            computed.insert(id, (key, Arc::clone(&outputs)));
            in_progress.remove(&id);
            return Ok(outputs);
        }

        // Gather input field references for the operator.
        let mut inputs: Vec<&Field> = Vec::with_capacity(upstream.len());
        for (outputs, output_index, _) in &upstream {
            let field = outputs.get(*output_index).ok_or(Error::InvalidPort {
                type_id: node.type_id,
                port: *output_index,
            })?;
            inputs.push(field);
        }

        let ctx = EvalContext::new(request.width, request.height, request.region, seed)
            .with_cancel(request.cancel.clone());
        let outputs = Arc::new(node.operator.eval(&inputs, &node.params, &ctx)?);

        // Endpoints are neither pinned nor flushed to the persistent cache, so
        // they re-execute on every pull.
        if !is_endpoint {
            computed.insert(id, (key, Arc::clone(&outputs)));
        }
        in_progress.remove(&id);
        Ok(outputs)
    }

    /// Reports, for every node reachable from `target`, whether its result is
    /// currently cached (`true`) or would recompute (`false`) for `request`.
    ///
    /// This is the read-only signal behind the canvas stale-vs-cached indicators.
    /// It recomputes each node's cache key (cheap, no evaluation) and checks it
    /// against the cache without perturbing eval or recency. Endpoints are never
    /// cached, so they always report `false`.
    ///
    /// # Errors
    ///
    /// Same structural errors as [`evaluate`](Self::evaluate): [`Error::Cycle`],
    /// [`Error::NodeNotFound`], or [`Error::DisconnectedInput`].
    pub fn cache_status(
        &self,
        target: NodeId,
        request: &EvalRequest,
        cache: &EvalCache,
    ) -> Result<HashMap<NodeId, bool>> {
        let mut keys: HashMap<NodeId, u64> = HashMap::new();
        let mut in_progress: HashSet<NodeId> = HashSet::new();
        self.node_key(target, request, &mut keys, &mut in_progress)?;
        Ok(keys
            .iter()
            .map(|(&id, &key)| (id, cache.is_valid(id, key)))
            .collect())
    }

    /// Recomputes a node's cache key (and, memoized in `keys`, its upstream
    /// nodes') without evaluating anything. Mirrors the key composition in
    /// [`pull`](Self::pull): a node's key is built from its upstream keys.
    fn node_key(
        &self,
        id: NodeId,
        request: &EvalRequest,
        keys: &mut HashMap<NodeId, u64>,
        in_progress: &mut HashSet<NodeId>,
    ) -> Result<u64> {
        if let Some(&key) = keys.get(&id) {
            return Ok(key);
        }
        if !in_progress.insert(id) {
            return Err(Error::Cycle);
        }
        let node = self.node(id).ok_or(Error::NodeNotFound)?;

        let mut input_keys: Vec<u64> = Vec::with_capacity(node.inputs.len());
        for (port, slot) in node.inputs.iter().enumerate() {
            let conn = slot.as_ref().ok_or(Error::DisconnectedInput {
                type_id: node.type_id,
                port,
            })?;
            input_keys.push(self.node_key(conn.source, request, keys, in_progress)?);
        }

        let seed = derive_seed(request.seed, node.stable_id);
        let key = compute_key(node.type_id, &node.params, &input_keys, request, seed);
        keys.insert(id, key);
        in_progress.remove(&id);
        Ok(key)
    }
}

/// Derives a node's seed from the global seed and its stable id, via the
/// SplitMix64 finalizer, so a node yields the same result regardless of graph
/// order or unrelated edits.
fn derive_seed(global_seed: u64, stable_id: u64) -> u64 {
    let mut h = global_seed ^ stable_id.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h
}

/// Composes a node's cache key from its type, params, upstream keys, and the
/// request context (resolution, region, derived seed).
fn compute_key(
    type_id: &str,
    params: &Params,
    input_keys: &[u64],
    request: &EvalRequest,
    seed: u64,
) -> u64 {
    let mut h = Fnv1a64::new();
    h.write_str(type_id);
    h.write_u64(params.content_hash().to_u64());
    h.write_usize(input_keys.len());
    for &key in input_keys {
        h.write_u64(key);
    }
    h.write_usize(request.width);
    h.write_usize(request.height);
    request.region.hash_into(&mut h);
    h.write_u64(seed);
    h.finish().to_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers;
    use crate::operator::Operator;
    use crate::param::ParamValue;
    use crate::spec::{NodeSpec, PortSpec};
    use crate::{Layer, Region};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A generator whose output is a constant field driven by the per-node seed,
    /// counting how many times it is evaluated.
    struct CountingGen {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingGen {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.gen",
                category: "test",
                tags: &[],
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: &[&Field], _: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let value = (ctx.seed % 1000) as f32 / 1000.0;
            let layer = Layer::filled(ctx.width, ctx.height, value);
            Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region)
                    .with_layer(layers::HEIGHT, Arc::new(layer)),
            ])
        }
    }

    /// A one-input modifier that adds its `delta` param to the height layer,
    /// counting evaluations.
    struct CountingAdd {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingAdd {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.add",
                category: "test",
                tags: &[],
                inputs: vec![PortSpec::new("in")],
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, inputs: &[&Field], params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let delta = params.get_f64("delta", 0.0) as f32;
            let input = inputs[0];
            let src = input.layer_or(layers::HEIGHT, 0.0);
            let layer = Layer::from_fn(input.width(), input.height(), |x, y| {
                src.get(x, y).unwrap_or(0.0) + delta
            });
            let mut out = input.clone();
            out.set_layer(layers::HEIGHT, Arc::new(layer));
            Ok(vec![out])
        }
    }

    /// A one-input endpoint (no outputs) that counts how often it executes its
    /// side effect.
    struct CountingSink {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingSink {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.sink",
                category: "test",
                tags: &[],
                inputs: vec![PortSpec::new("in")],
                outputs: Vec::new(),
                params: Vec::new(),
            }
        }

        fn eval(&self, _: &[&Field], _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        }
    }

    fn request() -> EvalRequest {
        EvalRequest::new(16, 16, Region::UNIT, 42)
    }

    #[test]
    fn evaluates_a_chain() {
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let add = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new().with("delta", ParamValue::Float(0.25)),
        );
        graph.connect(head, 0, add, 0).unwrap();

        let mut cache = EvalCache::new(16);
        let out = graph.evaluate(add, &request(), &mut cache).unwrap();
        let height = out[0].layer(layers::HEIGHT).unwrap();
        // Generator value plus delta, uniform across the field.
        let base = height.as_slice()[0];
        assert!(height.as_slice().iter().all(|&v| (v - base).abs() < 1e-6));
        assert!(base >= 0.25);
    }

    #[test]
    fn evaluation_is_deterministic() {
        let build = || {
            let mut graph = Graph::new();
            let head = graph.add_op(
                Box::new(CountingGen {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Params::new(),
            );
            let add = graph.add_op(
                Box::new(CountingAdd {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Params::new().with("delta", ParamValue::Float(0.1)),
            );
            graph.connect(head, 0, add, 0).unwrap();
            (graph, add)
        };

        let (g1, t1) = build();
        let (g2, t2) = build();
        let mut c1 = EvalCache::new(16);
        let mut c2 = EvalCache::new(16);
        let a = g1.evaluate(t1, &request(), &mut c1).unwrap();
        let b = g2.evaluate(t2, &request(), &mut c2).unwrap();
        assert_eq!(a[0].content_hash(), b[0].content_hash());
    }

    #[test]
    fn memoization_recomputes_only_downstream_of_a_change() {
        let gen_calls = Arc::new(AtomicUsize::new(0));
        let add_calls = Arc::new(AtomicUsize::new(0));

        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::clone(&gen_calls),
            }),
            Params::new(),
        );
        let add = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::clone(&add_calls),
            }),
            Params::new().with("delta", ParamValue::Float(0.25)),
        );
        graph.connect(head, 0, add, 0).unwrap();

        let mut cache = EvalCache::new(16);

        graph.evaluate(add, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
        assert_eq!(add_calls.load(Ordering::Relaxed), 1);

        // Re-evaluate unchanged: nothing recomputes.
        graph.evaluate(add, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
        assert_eq!(add_calls.load(Ordering::Relaxed), 1);

        // Change only the downstream node: the generator is reused, add recomputes.
        graph
            .set_params(add, Params::new().with("delta", ParamValue::Float(0.5)))
            .unwrap();
        graph.evaluate(add, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
        assert_eq!(add_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cycles_are_reported_not_panicked() {
        let mut graph = Graph::new();
        let a = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let b = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        graph.connect(a, 0, b, 0).unwrap();
        graph.connect(b, 0, a, 0).unwrap();

        let mut cache = EvalCache::new(16);
        let err = graph.evaluate(b, &request(), &mut cache).unwrap_err();
        assert!(matches!(err, Error::Cycle));
    }

    #[test]
    fn active_path_is_pinned_even_with_zero_capacity() {
        // Capacity 0 keeps nothing across pulls, but within one pull the active
        // path must not be evicted, so each node evaluates exactly once.
        let gen_calls = Arc::new(AtomicUsize::new(0));
        let add_calls = Arc::new(AtomicUsize::new(0));

        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::clone(&gen_calls),
            }),
            Params::new(),
        );
        let add = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::clone(&add_calls),
            }),
            Params::new().with("delta", ParamValue::Float(0.25)),
        );
        graph.connect(head, 0, add, 0).unwrap();

        let mut cache = EvalCache::new(0);
        graph.evaluate(add, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
        assert_eq!(add_calls.load(Ordering::Relaxed), 1);

        // Nothing persisted, so a fresh pull recomputes both.
        graph.evaluate(add, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 2);
        assert_eq!(add_calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn endpoints_always_execute_but_upstream_is_memoized() {
        let gen_calls = Arc::new(AtomicUsize::new(0));
        let sink_calls = Arc::new(AtomicUsize::new(0));

        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::clone(&gen_calls),
            }),
            Params::new(),
        );
        let sink = graph.add_op(
            Box::new(CountingSink {
                calls: Arc::clone(&sink_calls),
            }),
            Params::new(),
        );
        graph.connect(head, 0, sink, 0).unwrap();

        let mut cache = EvalCache::new(16);
        graph.evaluate(sink, &request(), &mut cache).unwrap();
        graph.evaluate(sink, &request(), &mut cache).unwrap();

        // The endpoint executes its side effect every pull...
        assert_eq!(sink_calls.load(Ordering::Relaxed), 2);
        // ...while its upstream field is computed once and memoized.
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn disconnected_input_is_an_error() {
        let mut graph = Graph::new();
        let add = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let mut cache = EvalCache::new(16);
        let err = graph.evaluate(add, &request(), &mut cache).unwrap_err();
        assert!(matches!(
            err,
            Error::DisconnectedInput {
                type_id: "test.add",
                port: 0
            }
        ));
    }

    #[test]
    fn graph_evaluates_on_a_worker_thread() {
        // Compiles only if Graph (and thus Box<dyn Operator>) is Send, which the
        // Operator: Send + Sync bound provides.
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let req = request();
        let on_worker = std::thread::spawn(move || {
            let mut cache = EvalCache::new(8);
            graph.evaluate(head, &req, &mut cache).unwrap()[0].content_hash()
        })
        .join()
        .unwrap();

        // Same bytes as evaluating locally (determinism is thread-independent).
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let local = graph
            .evaluate(head, &request(), &mut EvalCache::new(8))
            .unwrap()[0]
            .content_hash();
        assert_eq!(on_worker, local);
    }

    #[test]
    fn cancellation_aborts_evaluation() {
        let cancel = crate::CancelToken::new();
        cancel.cancel();

        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let req = request().with_cancel(cancel);
        let err = graph
            .evaluate(head, &req, &mut EvalCache::new(8))
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }

    #[test]
    fn cache_status_reflects_staleness() {
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let add = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new().with("delta", ParamValue::Float(0.25)),
        );
        graph.connect(head, 0, add, 0).unwrap();

        let req = request();
        let mut cache = EvalCache::new(8);

        // Nothing evaluated yet: both stale.
        let status = graph.cache_status(add, &req, &cache).unwrap();
        assert!(!status[&head] && !status[&add]);

        graph.evaluate(add, &req, &mut cache).unwrap();
        let status = graph.cache_status(add, &req, &cache).unwrap();
        assert!(status[&head] && status[&add], "both cached after eval");

        // Change only the downstream node: the generator stays cached, add goes stale.
        graph
            .set_params(add, Params::new().with("delta", ParamValue::Float(0.5)))
            .unwrap();
        let status = graph.cache_status(add, &req, &cache).unwrap();
        assert!(status[&head], "unchanged upstream stays cached");
        assert!(!status[&add], "changed node is stale");
    }
}
