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
use crate::operator::Inputs;
use crate::param::Params;
use crate::region::Region;

/// The global parameters of one evaluation request: resolution, region, the
/// global seed, and the world extent. The target node is a separate argument to
/// [`Graph::evaluate`], since which node is requested is the evaluator's concern,
/// not an operator's.
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
    /// Physical size of the full `UNIT` region along x, in world units (meters);
    /// threaded into each node's [`EvalContext`]. Defaults to `1.0`.
    world_extent: f64,
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
            world_extent: 1.0,
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

    /// Sets the world's physical size along x, in world units (meters) across the
    /// full `UNIT` region. Defaults to `1.0`. The evaluator threads this into each
    /// node's [`EvalContext`], where scale-aware operators convert world-unit
    /// parameters to cells.
    #[must_use]
    pub fn with_world_extent(mut self, world_extent: f64) -> Self {
        self.world_extent = world_extent;
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
        let required_count = node.required_input_count;

        // A bypassed node is transparent: its output is its input 0's field, forwarded
        // unchanged, with the operator never run. A node with no input 0 (a bypassed
        // generator, or an unconnected input 0) forwards nothing. Its key is the resolved
        // source's, so a downstream diamond reuses and a source change recomputes.
        if node.bypassed {
            let resolved = self.resolve_source(id, 0)?;
            let outputs = match resolved {
                Some((src, out)) => {
                    let upstream = self.pull(src, request, cache, computed, in_progress)?;
                    let field = upstream
                        .get(out)
                        .ok_or(Error::InvalidPort {
                            type_id: node.type_id,
                            port: out,
                        })?
                        .clone();
                    Arc::new(vec![field])
                }
                None => Arc::new(Vec::new()),
            };
            let key = resolved
                .and_then(|(src, _)| computed.get(&src).map(|(key, _)| *key))
                .unwrap_or(0);
            computed.insert(id, (key, Arc::clone(&outputs)));
            in_progress.remove(&id);
            return Ok(outputs);
        }

        // Evaluate each connected input, collecting its output Arc and the consumed
        // output index per port (`None` for an unconnected optional port), plus its
        // cache key. The source is resolved through any bypassed nodes first, so a
        // bypassed node is seen through to the field it forwards; a dead-ended bypass (a
        // bypassed generator upstream) reads as unconnected. A required port that is
        // unconnected is an error.
        let mut upstream: Vec<Option<(Arc<Vec<Field>>, usize)>> =
            Vec::with_capacity(node.inputs.len());
        let mut input_keys: Vec<Option<u64>> = Vec::with_capacity(node.inputs.len());
        for (port, slot) in node.inputs.iter().enumerate() {
            let resolved = match slot {
                Some(conn) => self.resolve_source(conn.source, conn.output)?,
                None => None,
            };
            match resolved {
                Some((src, out)) => {
                    let outputs = self.pull(src, request, cache, computed, in_progress)?;
                    let upstream_key = computed.get(&src).map_or(0, |(key, _)| *key);
                    upstream.push(Some((outputs, out)));
                    input_keys.push(Some(upstream_key));
                }
                None if port < required_count => {
                    return Err(Error::DisconnectedInput {
                        type_id: node.type_id,
                        port,
                    });
                }
                None => {
                    upstream.push(None);
                    input_keys.push(None);
                }
            }
        }

        let seed = derive_seed(request.seed, node.stable_id);
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

        // Resolve each port's consumed field, then split into the required inputs
        // (dense, all present) and the optional ones (one entry per optional port,
        // `None` when unconnected) for the operator.
        let mut required: Vec<&Field> = Vec::with_capacity(required_count);
        let mut optional: Vec<Option<&Field>> = Vec::with_capacity(upstream.len() - required_count);
        for (port, slot) in upstream.iter().enumerate() {
            let field = match slot {
                Some((outputs, output_index)) => {
                    Some(outputs.get(*output_index).ok_or(Error::InvalidPort {
                        type_id: node.type_id,
                        port: *output_index,
                    })?)
                }
                None => None,
            };
            if port < required_count {
                // Unconnected required ports erred above, so this is always present.
                match field {
                    Some(field) => required.push(field),
                    None => {
                        return Err(Error::DisconnectedInput {
                            type_id: node.type_id,
                            port,
                        });
                    }
                }
            } else {
                optional.push(field);
            }
        }

        let ctx = EvalContext::new(request.width, request.height, request.region, seed)
            .with_cancel(request.cancel.clone())
            .with_world_extent(request.world_extent);
        let inputs = Inputs::new(&required, &optional);
        let outputs = Arc::new(node.operator.eval(inputs, &node.params, &ctx)?);

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

    /// The content key of `target`'s output for `request`: a hash that changes if
    /// and only if the previewed output would change (this node's params, anything
    /// upstream of it, the seed, or the resolution). It composes cache keys without
    /// evaluating anything, so it is cheap to call every frame.
    ///
    /// The GUI uses it as a change signal: when the key differs from the last one
    /// submitted, it re-runs the background preview; otherwise the cached result
    /// still stands.
    ///
    /// # Errors
    ///
    /// Same structural errors as [`evaluate`](Self::evaluate): [`Error::Cycle`],
    /// [`Error::NodeNotFound`], or [`Error::DisconnectedInput`].
    pub fn output_key(&self, target: NodeId, request: &EvalRequest) -> Result<u64> {
        let mut keys: HashMap<NodeId, u64> = HashMap::new();
        let mut in_progress: HashSet<NodeId> = HashSet::new();
        self.node_key(target, request, &mut keys, &mut in_progress)
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

        // A bypassed node's output is its resolved source's field, so its key is that
        // source's key (0 for a dead bypass), mirroring `pull`.
        if node.bypassed {
            let key = match self.resolve_source(id, 0)? {
                Some((src, _)) => self.node_key(src, request, keys, in_progress)?,
                None => 0,
            };
            keys.insert(id, key);
            in_progress.remove(&id);
            return Ok(key);
        }

        let required_count = node.required_input_count;

        let mut input_keys: Vec<Option<u64>> = Vec::with_capacity(node.inputs.len());
        for (port, slot) in node.inputs.iter().enumerate() {
            // Resolve through bypassed nodes, exactly as `pull` does, so the key tracks
            // the field actually consumed.
            let resolved = match slot {
                Some(conn) => self.resolve_source(conn.source, conn.output)?,
                None => None,
            };
            match resolved {
                Some((src, _)) => {
                    input_keys.push(Some(self.node_key(src, request, keys, in_progress)?));
                }
                None if port < required_count => {
                    return Err(Error::DisconnectedInput {
                        type_id: node.type_id,
                        port,
                    });
                }
                None => input_keys.push(None),
            }
        }

        let seed = derive_seed(request.seed, node.stable_id);
        let key = compute_key(node.type_id, &node.params, &input_keys, request, seed);
        keys.insert(id, key);
        in_progress.remove(&id);
        Ok(key)
    }

    /// Resolves an edge `(source, output)` through any chain of bypassed nodes to the
    /// node that actually produces the field, or `None` if the chain dead-ends at a
    /// bypassed node with no input 0 (a bypassed generator, or an unconnected input 0):
    /// an absent edge. Bypassed nodes forward their input 0 regardless of which output
    /// port was requested, so the requested `output` is replaced as the walk follows.
    fn resolve_source(
        &self,
        mut source: NodeId,
        mut output: usize,
    ) -> Result<Option<(NodeId, usize)>> {
        // Input-0 edges form a sub-DAG, so the walk terminates; bound it by the node
        // count anyway, so a cycle among bypassed nodes reports rather than spins.
        for _ in 0..=self.node_count() {
            let node = self.node(source).ok_or(Error::NodeNotFound)?;
            if !node.bypassed {
                return Ok(Some((source, output)));
            }
            match node.inputs.first() {
                Some(Some(conn)) => {
                    source = conn.source;
                    output = conn.output;
                }
                _ => return Ok(None),
            }
        }
        Err(Error::Cycle)
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
/// request context (resolution, region, world extent, derived seed).
fn compute_key(
    type_id: &str,
    params: &Params,
    input_keys: &[Option<u64>],
    request: &EvalRequest,
    seed: u64,
) -> u64 {
    let mut h = Fnv1a64::new();
    h.write_str(type_id);
    h.write_u64(params.content_hash().to_u64());
    h.write_usize(input_keys.len());
    for key in input_keys {
        // A presence discriminant so a connected optional input (with its key) never
        // collides with an unconnected one, and connecting/disconnecting invalidates.
        match key {
            Some(key) => {
                h.write_u64(1);
                h.write_u64(*key);
            }
            None => h.write_u64(0),
        }
    }
    h.write_usize(request.width);
    h.write_usize(request.height);
    request.region.hash_into(&mut h);
    h.write_f64_bits(request.world_extent);
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
    #[derive(Clone)]
    struct CountingGen {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingGen {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.gen",
                category: "test",
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
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
    #[derive(Clone)]
    struct CountingAdd {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingAdd {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.add",
                category: "test",
                inputs: vec![PortSpec::new("in")],
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, inputs: Inputs, params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
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
    #[derive(Clone)]
    struct CountingSink {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingSink {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.sink",
                category: "test",
                inputs: vec![PortSpec::new("in")],
                outputs: Vec::new(),
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        }
    }

    /// A two-input modifier that sums the height layers of both inputs, counting
    /// evaluations. The first real merge node (issue #14) will be shaped like this;
    /// here it exercises multi-input gathering and the diamond shared-ancestor case.
    #[derive(Clone)]
    struct CountingMerge {
        calls: Arc<AtomicUsize>,
    }

    impl Operator for CountingMerge {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.merge",
                category: "test",
                inputs: vec![PortSpec::new("a"), PortSpec::new("b")],
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, inputs: Inputs, _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // The evaluator gathers every input slot before calling eval, so both
            // inputs are present here; an unwired port would have failed upstream.
            let a = inputs[0].layer_or(layers::HEIGHT, 0.0);
            let b = inputs[1].layer_or(layers::HEIGHT, 0.0);
            let layer = Layer::from_fn(inputs[0].width(), inputs[0].height(), |x, y| {
                a.get(x, y).unwrap_or(0.0) + b.get(x, y).unwrap_or(0.0)
            });
            let mut out = inputs[0].clone();
            out.set_layer(layers::HEIGHT, Arc::new(layer));
            Ok(vec![out])
        }
    }

    /// A modifier with one required input and one optional input. Its output height
    /// is the required input's height plus `1.0` when the optional input is
    /// connected, and unchanged otherwise, so a test can observe optional presence.
    #[derive(Clone)]
    struct OptionalProbe;

    impl Operator for OptionalProbe {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.optional",
                category: "test",
                inputs: vec![PortSpec::new("in"), PortSpec::optional("extra")],
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, inputs: Inputs, _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            let base = inputs[0];
            let bump = if inputs.optional(0).is_some() {
                1.0
            } else {
                0.0
            };
            let src = base.layer_or(layers::HEIGHT, 0.0);
            let layer = Layer::from_fn(base.width(), base.height(), |x, y| {
                src.get(x, y).unwrap_or(0.0) + bump
            });
            let mut out = base.clone();
            out.set_layer(layers::HEIGHT, Arc::new(layer));
            Ok(vec![out])
        }
    }

    /// A generator that stamps `ctx.meters_per_cell()` as its uniform height, so a
    /// test can read back the world extent the evaluator threaded into the context.
    #[derive(Clone)]
    struct ProbeExtent;

    impl Operator for ProbeExtent {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: "test.probe_extent",
                category: "test",
                inputs: Vec::new(),
                outputs: vec![PortSpec::new("out")],
                params: Vec::new(),
            }
        }

        fn eval(&self, _: Inputs, _: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
            let value = ctx.meters_per_cell() as f32;
            Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region).with_layer(
                    layers::HEIGHT,
                    Arc::new(Layer::filled(ctx.width, ctx.height, value)),
                ),
            ])
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
    fn a_cloned_graph_evaluates_identically() {
        // The GUI clones the canonical graph to evaluate off-thread; a clone must
        // produce byte-identical output, addressed by the persistent stable_id.
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
            Params::new().with("delta", ParamValue::Float(0.3)),
        );
        graph.connect(head, 0, add, 0).unwrap();

        let snapshot = graph.clone();
        let target_sid = graph.stable_id(add).unwrap();
        let snapshot_target = snapshot.node_id_of(target_sid).unwrap();

        let mut c1 = EvalCache::new(16);
        let mut c2 = EvalCache::new(16);
        let original = graph.evaluate(add, &request(), &mut c1).unwrap();
        let cloned = snapshot
            .evaluate(snapshot_target, &request(), &mut c2)
            .unwrap();
        assert_eq!(original[0].content_hash(), cloned[0].content_hash());
    }

    #[test]
    fn output_key_tracks_what_changes_the_output() {
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

        let req = request();
        let key = graph.output_key(add, &req).unwrap();
        // Stable when nothing changes.
        assert_eq!(key, graph.output_key(add, &req).unwrap());

        // An upstream param change changes the key.
        graph
            .set_params(add, Params::new().with("delta", ParamValue::Float(0.5)))
            .unwrap();
        let after_param = graph.output_key(add, &req).unwrap();
        assert_ne!(key, after_param);

        // A different seed changes the key.
        let reseeded = EvalRequest::new(16, 16, Region::UNIT, 999);
        assert_ne!(after_param, graph.output_key(add, &reseeded).unwrap());

        // A structurally broken target reports the error rather than a key.
        let lone = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        assert!(matches!(
            graph.output_key(lone, &req),
            Err(Error::DisconnectedInput { .. })
        ));
    }

    #[test]
    fn world_extent_threads_into_the_operator_context() {
        let mut graph = Graph::new();
        let probe = graph.add_op(Box::new(ProbeExtent), Params::new());
        let req = EvalRequest::new(4096, 4096, Region::UNIT, 0).with_world_extent(2000.0);
        let mut cache = EvalCache::new(8);
        let out = graph.evaluate(probe, &req, &mut cache).unwrap();
        let height = out[0].layer(layers::HEIGHT).unwrap();
        // The operator saw region.width() * extent / width = 2000 / 4096 per cell.
        let expected = (2000.0_f64 / 4096.0) as f32;
        assert!((height.as_slice()[0] - expected).abs() < 1e-6);
    }

    #[test]
    fn changing_world_extent_invalidates_the_cache() {
        let mut graph = Graph::new();
        let probe = graph.add_op(Box::new(ProbeExtent), Params::new());
        let a = EvalRequest::new(64, 64, Region::UNIT, 0).with_world_extent(1000.0);
        let b = EvalRequest::new(64, 64, Region::UNIT, 0).with_world_extent(2000.0);
        // The extent is part of the cache key, so a different extent is a different key.
        assert_ne!(
            graph.output_key(probe, &a).unwrap(),
            graph.output_key(probe, &b).unwrap()
        );
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
    fn diamond_evaluates_the_shared_ancestor_once_and_merges_correctly() {
        // A splits into two branches B and C, which merge into D:
        //   A -> B \
        //   A -> C  > D
        // The shared ancestor A must evaluate once, not once per branch, and the
        // merge must see the same A on both inputs.
        let gen_calls = Arc::new(AtomicUsize::new(0));
        let b_calls = Arc::new(AtomicUsize::new(0));
        let c_calls = Arc::new(AtomicUsize::new(0));
        let merge_calls = Arc::new(AtomicUsize::new(0));

        let mut graph = Graph::new();
        let a = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::clone(&gen_calls),
            }),
            Params::new(),
        );
        let b = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::clone(&b_calls),
            }),
            Params::new().with("delta", ParamValue::Float(0.1)),
        );
        let c = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::clone(&c_calls),
            }),
            Params::new().with("delta", ParamValue::Float(0.2)),
        );
        let d = graph.add_op(
            Box::new(CountingMerge {
                calls: Arc::clone(&merge_calls),
            }),
            Params::new(),
        );
        graph.connect(a, 0, b, 0).unwrap();
        graph.connect(a, 0, c, 0).unwrap();
        graph.connect(b, 0, d, 0).unwrap();
        graph.connect(c, 0, d, 1).unwrap();

        let mut cache = EvalCache::new(16);
        let out = graph.evaluate(d, &request(), &mut cache).unwrap();

        // The shared ancestor evaluated exactly once despite feeding two branches;
        // every other node once.
        assert_eq!(
            gen_calls.load(Ordering::Relaxed),
            1,
            "shared ancestor must evaluate once, not per branch"
        );
        assert_eq!(b_calls.load(Ordering::Relaxed), 1);
        assert_eq!(c_calls.load(Ordering::Relaxed), 1);
        assert_eq!(merge_calls.load(Ordering::Relaxed), 1);

        // Both branches saw the same A, so the merge is (A+0.1) + (A+0.2), uniform
        // across the field. A's own value comes from an independent lone-generator
        // build: it has the same stable_id (0) and seed, so it reproduces A exactly.
        let a_value = {
            let mut g = Graph::new();
            let lone = g.add_op(
                Box::new(CountingGen {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Params::new(),
            );
            let mut c = EvalCache::new(4);
            g.evaluate(lone, &request(), &mut c).unwrap()[0]
                .layer(layers::HEIGHT)
                .unwrap()
                .as_slice()[0]
        };
        let height = out[0].layer(layers::HEIGHT).unwrap();
        let value = height.as_slice()[0];
        assert!(
            height.as_slice().iter().all(|&v| (v - value).abs() < 1e-6),
            "merge output must be uniform"
        );
        assert!(
            (value - (2.0 * a_value + 0.3)).abs() < 1e-6,
            "merge must sum both branches over the same shared ancestor"
        );

        // Re-evaluating the diamond is fully served from cache: nothing recomputes.
        graph.evaluate(d, &request(), &mut cache).unwrap();
        assert_eq!(gen_calls.load(Ordering::Relaxed), 1);
        assert_eq!(b_calls.load(Ordering::Relaxed), 1);
        assert_eq!(c_calls.load(Ordering::Relaxed), 1);
        assert_eq!(merge_calls.load(Ordering::Relaxed), 1);
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
    fn an_unconnected_optional_input_is_not_an_error_and_is_observable() {
        // Probe with only its required input wired: it evaluates (no
        // DisconnectedInput), and the optional input reads as absent.
        let build = || {
            let mut g = Graph::new();
            let head = g.add_op(
                Box::new(CountingGen {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Params::new(),
            );
            let probe = g.add_op(Box::new(OptionalProbe), Params::new());
            g.connect(head, 0, probe, 0).unwrap();
            (g, probe)
        };

        let (g, probe) = build();
        let absent = g
            .evaluate(probe, &request(), &mut EvalCache::new(8))
            .unwrap()[0]
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()[0];

        // Wire the optional input too: the probe now observes it (height bumps by 1).
        let (mut g, probe) = build();
        let extra = g.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        g.connect(extra, 0, probe, 1).unwrap();
        let present = g
            .evaluate(probe, &request(), &mut EvalCache::new(8))
            .unwrap()[0]
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()[0];

        assert!(
            (present - (absent + 1.0)).abs() < 1e-6,
            "optional present must bump the output: {present} vs {absent}+1"
        );
    }

    #[test]
    fn renaming_a_node_does_not_change_its_output_key() {
        // A display-name override is cosmetic: it must never enter the cache key, or
        // a rename would needlessly invalidate the preview (and risk affecting output).
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let req = request();
        let before = graph.output_key(head, &req).unwrap();
        graph
            .set_name(head, Some("Base Terrain".to_string()))
            .unwrap();
        assert_eq!(before, graph.output_key(head, &req).unwrap());
    }

    #[test]
    fn connecting_an_optional_input_changes_the_output_key() {
        // Presence of an optional input is part of the cache key, so wiring one
        // invalidates the previewed output rather than silently reusing the old.
        let mut g = Graph::new();
        let head = g.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let probe = g.add_op(Box::new(OptionalProbe), Params::new());
        g.connect(head, 0, probe, 0).unwrap();

        let req = request();
        let before = g.output_key(probe, &req).unwrap();

        let extra = g.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        g.connect(extra, 0, probe, 1).unwrap();
        assert_ne!(before, g.output_key(probe, &req).unwrap());
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

    /// First height-layer cell of an evaluated output, for terse assertions.
    fn first_height(out: &[Field]) -> f32 {
        out[0].layer(layers::HEIGHT).unwrap().as_slice()[0]
    }

    #[test]
    fn bypassed_modifier_forwards_its_input_unchanged() {
        let mut graph = Graph::new();
        let add_calls = Arc::new(AtomicUsize::new(0));
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
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
        graph.set_bypassed(add, true).unwrap();

        let head_value = {
            let mut cache = EvalCache::new(8);
            first_height(&graph.evaluate(head, &request(), &mut cache).unwrap())
        };
        let mut cache = EvalCache::new(8);
        let out = graph.evaluate(add, &request(), &mut cache).unwrap();
        // The head's value comes through with no delta, and the operator never ran.
        assert!((first_height(&out) - head_value).abs() < 1e-6);
        assert_eq!(
            add_calls.load(Ordering::Relaxed),
            0,
            "a bypassed operator must not be evaluated"
        );
    }

    #[test]
    fn bypassing_a_generator_drops_an_optional_input() {
        let mut graph = Graph::new();
        let base = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let extra = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let probe = graph.add_op(Box::new(OptionalProbe), Params::new());
        graph.connect(base, 0, probe, 0).unwrap();
        graph.connect(extra, 0, probe, 1).unwrap();

        let with_optional = {
            let mut cache = EvalCache::new(8);
            first_height(&graph.evaluate(probe, &request(), &mut cache).unwrap())
        };
        // Bypassing the generator on the optional port makes it read as unconnected, so
        // the probe degrades (no +1) instead of failing.
        graph.set_bypassed(extra, true).unwrap();
        let without_optional = {
            let mut cache = EvalCache::new(8);
            first_height(&graph.evaluate(probe, &request(), &mut cache).unwrap())
        };
        assert!((with_optional - without_optional - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bypassing_a_generator_red_outs_a_required_consumer() {
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
            Params::new(),
        );
        graph.connect(head, 0, add, 0).unwrap();
        graph.set_bypassed(head, true).unwrap();

        // The required input is now fed by nothing, exactly as if unwired.
        let mut cache = EvalCache::new(8);
        let result = graph.evaluate(add, &request(), &mut cache);
        assert!(matches!(result, Err(Error::DisconnectedInput { .. })));
    }

    #[test]
    fn bypass_sees_through_a_chain_of_modifiers() {
        let mut graph = Graph::new();
        let head = graph.add_op(
            Box::new(CountingGen {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new(),
        );
        let a1 = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new().with("delta", ParamValue::Float(0.1)),
        );
        let a2 = graph.add_op(
            Box::new(CountingAdd {
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Params::new().with("delta", ParamValue::Float(0.1)),
        );
        graph.connect(head, 0, a1, 0).unwrap();
        graph.connect(a1, 0, a2, 0).unwrap();
        graph.set_bypassed(a1, true).unwrap();
        graph.set_bypassed(a2, true).unwrap();

        let head_value = {
            let mut cache = EvalCache::new(8);
            first_height(&graph.evaluate(head, &request(), &mut cache).unwrap())
        };
        let mut cache = EvalCache::new(8);
        let out = graph.evaluate(a2, &request(), &mut cache).unwrap();
        // Both modifiers are seen through: the head's value reaches the end untouched.
        assert!((first_height(&out) - head_value).abs() < 1e-6);
    }

    #[test]
    fn toggling_bypass_changes_the_output_key() {
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

        let active = graph.output_key(add, &request()).unwrap();
        graph.set_bypassed(add, true).unwrap();
        let bypassed = graph.output_key(add, &request()).unwrap();
        assert_ne!(
            active, bypassed,
            "a bypass toggle must invalidate the cache key"
        );
    }
}
