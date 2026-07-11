//! The node graph: nodes keyed by `NodeId`, edges stored as `NodeId`s.
//!
//! A node holds its resolved operator (behavior) plus its per-instance params and
//! input connections. Persistence (a later step) stores `type_id` and params and
//! rebuilds via the registry; the runtime graph here holds the live operator.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use slotmap::{SlotMap, new_key_type};

use crate::error::{Error, Result};
use crate::hash::Fnv1a64;
use crate::operator::Operator;
use crate::param::Params;
use crate::project::{Connection, FORMAT_VERSION, NodeDocument, ProjectDocument};
use crate::spec::NodeSpec;

new_key_type! {
    /// Runtime handle for a node in a [`Graph`].
    ///
    /// This is a generational slotmap key: removing a node and inserting another
    /// never reuses a live id. It is runtime-only and must never be serialized,
    /// seeded from, or allowed to influence output; the node's `stable_id` is the
    /// only identity that feeds results.
    pub struct NodeId;
}

/// The result of [`Graph::extract_subgraph`] (#106): the new container node plus the
/// identity mapping the editor needs to lay the new interior out (preserve the wrapped
/// nodes' relative positions, and place the boundary markers around them).
#[derive(Debug, Clone)]
pub struct Extraction {
    /// The new container node.
    pub container: NodeId,
    /// Each wrapped node as `(outer stable_id, inner stable_id)`, so the editor can carry
    /// the originals' canvas positions onto their copies inside.
    pub moved: Vec<(u64, u64)>,
    /// Inner Input marker `stable_id`s, in input-port order.
    pub inputs: Vec<u64>,
    /// Inner Output marker `stable_id`s, in output-port order.
    pub outputs: Vec<u64>,
}

/// A connection feeding one input port: which upstream node and which of its
/// output ports.
#[derive(Clone)]
pub(crate) struct InputConn {
    pub(crate) source: NodeId,
    pub(crate) output: usize,
}

/// A node instance: behavior plus per-instance configuration and wiring.
#[derive(Clone)]
pub(crate) struct Node {
    /// Persistent identity, distinct from the runtime [`NodeId`]. Monotonic and
    /// never reused, it is the only identity that feeds the per-node seed.
    pub(crate) stable_id: u64,
    /// The operator's type id, captured from its spec for keys and messages.
    pub(crate) type_id: &'static str,
    /// The operator (behavior).
    pub(crate) operator: Box<dyn Operator>,
    /// This instance's parameters.
    pub(crate) params: Params,
    /// Optional per-instance display-name override, to tell instances of one type
    /// apart. Cosmetic metadata: it is serialized with the graph but never enters a
    /// cache key, the per-node seed, or evaluation, so a rename cannot change output.
    pub(crate) name: Option<String>,
    /// When set, the node is transparent: the evaluator skips its operator and forwards
    /// its input 0 instead, so a node with no input 0 (a generator) emits nothing. A fast
    /// way to toggle a node off without unwiring it. Serialized with the graph.
    pub(crate) bypassed: bool,
    /// One slot per input port, in order; `None` is unconnected.
    pub(crate) inputs: Vec<Option<InputConn>>,
    /// Number of leading required input ports. Ports `[0, required_input_count)` must
    /// be connected to evaluate; `[required_input_count, inputs.len())` are optional.
    /// (Optional ports are declared after required ones.)
    pub(crate) required_input_count: usize,
    /// Number of output ports, for connection validation.
    pub(crate) output_count: usize,
}

/// A directed graph of operator nodes.
///
/// `Clone` produces an independent snapshot (operators are cloned via
/// [`OperatorClone`](crate::OperatorClone), params and wiring deeply): the GUI
/// clones the canonical graph to evaluate it on a worker thread without locking the
/// graph it is editing. A clone evaluates identically to the original, since node
/// identity for seeding is the persistent `stable_id`, which clones unchanged.
#[derive(Clone)]
pub struct Graph {
    nodes: SlotMap<NodeId, Node>,
    next_stable_id: u64,
}

impl Graph {
    /// Creates an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: SlotMap::with_key(),
            next_stable_id: 0,
        }
    }

    /// Adds a node backed by `operator`, returning its runtime handle. The node's
    /// input ports are sized from the operator's spec and start unconnected.
    pub fn add_op(&mut self, operator: Box<dyn Operator>, params: Params) -> NodeId {
        let spec = operator.spec();
        let type_id = spec.type_id;
        let input_count = spec.inputs.len();
        let required_input_count = spec.inputs.iter().filter(|p| !p.optional).count();
        let output_count = spec.outputs.len();

        // Optional ports must trail the required ones, so a single split point
        // separates them (relied on by the evaluator and `Inputs`).
        debug_assert!(
            spec.inputs
                .iter()
                .enumerate()
                .all(|(i, p)| p.optional == (i >= required_input_count)),
            "operator {type_id:?} declares an optional input port before a required one"
        );

        let stable_id = self.next_stable_id;
        // Monotonic and never reused, so a removed node's identity never returns.
        self.next_stable_id += 1;

        self.nodes.insert(Node {
            stable_id,
            type_id,
            operator,
            params,
            name: None,
            bypassed: false,
            inputs: (0..input_count).map(|_| None).collect(),
            required_input_count,
            output_count,
        })
    }

    /// Replaces a node's operator in place, re-deriving its port arity from the new
    /// operator's spec while preserving the node's identity (`stable_id`), name override,
    /// bypass state, and params.
    ///
    /// A node's arity is cached at [`add_op`](Self::add_op) time, but a subgraph
    /// container's ports are dynamic: editing its inner graph swaps in a new operator that
    /// may declare more or fewer ports. This refreshes that cache and keeps the wiring
    /// consistent. Connections into input ports that no longer exist are dropped, and edges
    /// elsewhere that fed from an output port this node no longer has are cleared, so no
    /// connection is left dangling. Surviving ports keep their connections, by index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `id` is not in the graph.
    pub fn set_operator(&mut self, id: NodeId, operator: Box<dyn Operator>) -> Result<()> {
        let spec = operator.spec();
        let type_id = spec.type_id;
        let input_count = spec.inputs.len();
        let required_input_count = spec.inputs.iter().filter(|p| !p.optional).count();
        let output_count = spec.outputs.len();

        // Optional ports must trail the required ones, the same invariant `add_op` checks.
        debug_assert!(
            spec.inputs
                .iter()
                .enumerate()
                .all(|(i, p)| p.optional == (i >= required_input_count)),
            "operator {type_id:?} declares an optional input port before a required one"
        );

        let node = self.nodes.get_mut(id).ok_or(Error::NodeNotFound)?;
        node.operator = operator;
        node.type_id = type_id;
        node.required_input_count = required_input_count;
        node.output_count = output_count;
        // Resize the input slots to the new arity: surviving ports keep their connection
        // (by index), removed ports are dropped, and new ports start unconnected.
        node.inputs.resize_with(input_count, || None);

        // Clear edges anywhere that fed from an output port this node no longer has, so no
        // connection dangles past a shrunk output count.
        for other in self.nodes.values_mut() {
            for slot in &mut other.inputs {
                if slot
                    .as_ref()
                    .is_some_and(|conn| conn.source == id && conn.output >= output_count)
                {
                    *slot = None;
                }
            }
        }
        Ok(())
    }

    /// The inner graph held by a container node (a subgraph), or `None` for an ordinary
    /// node. The editor uses this to detect a container and to read its inner graph when
    /// diving in. A structural query through the operator's
    /// [`Operator::nested`](crate::Operator::nested) hook, not a check on a concrete type.
    #[must_use]
    pub fn nested(&self, id: NodeId) -> Option<&Graph> {
        self.node(id).and_then(|n| n.operator.nested())
    }

    /// Installs `inner` as a container node's inner graph, refreshing the node's ports to
    /// match. The editor uses this to write edits made while diving into a subgraph back
    /// into its container. It goes through the operator's
    /// [`rebuild_nested`](crate::Operator::rebuild_nested) hook and
    /// [`set_operator`](Self::set_operator), so it never names a concrete node type, and the
    /// node's identity, params (including the seed), name, and bypass are preserved. On an
    /// ordinary node the default `rebuild_nested` ignores `inner`, leaving the node as it was.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `id` is not in the graph.
    pub fn set_nested(&mut self, id: NodeId, inner: Graph) -> Result<()> {
        let rebuilt = self
            .node(id)
            .ok_or(Error::NodeNotFound)?
            .operator
            .rebuild_nested(inner);
        self.set_operator(id, rebuilt)
    }

    /// Replaces a node's parameters.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `node` is not in the graph.
    pub fn set_params(&mut self, node: NodeId, params: Params) -> Result<()> {
        let node = self.nodes.get_mut(node).ok_or(Error::NodeNotFound)?;
        node.params = params;
        Ok(())
    }

    /// A node's display-name override, or `None` if it uses its type's name.
    #[must_use]
    pub fn name(&self, node: NodeId) -> Option<&str> {
        self.nodes.get(node).and_then(|n| n.name.as_deref())
    }

    /// Sets or clears a node's display-name override (cosmetic; never affects
    /// evaluation). Pass `None` to revert to the type's name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `node` is not in the graph.
    pub fn set_name(&mut self, node: NodeId, name: Option<String>) -> Result<()> {
        let node = self.nodes.get_mut(node).ok_or(Error::NodeNotFound)?;
        node.name = name;
        Ok(())
    }

    /// Whether a node is bypassed (transparent: the evaluator forwards its input 0 and
    /// skips its operator). `false` for an absent node.
    #[must_use]
    pub fn is_bypassed(&self, node: NodeId) -> bool {
        self.nodes.get(node).is_some_and(|n| n.bypassed)
    }

    /// Sets a node's bypass state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `node` is not in the graph.
    pub fn set_bypassed(&mut self, node: NodeId, bypassed: bool) -> Result<()> {
        let node = self.nodes.get_mut(node).ok_or(Error::NodeNotFound)?;
        node.bypassed = bypassed;
        Ok(())
    }

    /// Connects `source`'s output port to `dest`'s input port.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if either node is absent, or
    /// [`Error::InvalidPort`] if a port index is out of range. Cycles are not
    /// rejected here; they are detected at evaluation.
    pub fn connect(
        &mut self,
        source: NodeId,
        source_port: usize,
        dest: NodeId,
        dest_port: usize,
    ) -> Result<()> {
        let (source_type, source_outputs) = {
            let s = self.nodes.get(source).ok_or(Error::NodeNotFound)?;
            (s.type_id, s.output_count)
        };
        if source_port >= source_outputs {
            return Err(Error::InvalidPort {
                type_id: source_type,
                port: source_port,
            });
        }

        let dest_node = self.nodes.get_mut(dest).ok_or(Error::NodeNotFound)?;
        if dest_port >= dest_node.inputs.len() {
            return Err(Error::InvalidPort {
                type_id: dest_node.type_id,
                port: dest_port,
            });
        }
        dest_node.inputs[dest_port] = Some(InputConn {
            source,
            output: source_port,
        });
        Ok(())
    }

    /// The number of nodes in the graph.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// This node's schema, or `None` if `id` is absent.
    ///
    /// The editor reads the spec to render a node: its `type_id` resolves the
    /// display name through `tr`, and the port counts size its pins. The engine's
    /// internal node storage stays private; callers see only the public schema.
    #[must_use]
    pub fn spec(&self, id: NodeId) -> Option<NodeSpec> {
        self.nodes.get(id).map(|n| n.operator.spec())
    }

    /// This node's current parameters, or `None` if `id` is absent.
    ///
    /// The parameter inspector reads these to show and edit the live values.
    #[must_use]
    pub fn params(&self, id: NodeId) -> Option<&Params> {
        self.nodes.get(id).map(|n| &n.params)
    }

    /// This node's persistent `stable_id`, or `None` if `id` is absent.
    ///
    /// The editor stores `stable_id` (not the runtime [`NodeId`]) as its handle for
    /// a node, so view-state survives a reload; this resolves a live id to it, for
    /// instance when writing that view-state out.
    #[must_use]
    pub fn stable_id(&self, id: NodeId) -> Option<u64> {
        self.nodes.get(id).map(|n| n.stable_id)
    }

    /// The runtime [`NodeId`] currently backing a persistent `stable_id`, or `None`
    /// if no live node has it.
    ///
    /// This is the inverse of [`stable_id`](Self::stable_id): it resolves an
    /// editor-held handle back to the live node for a graph operation. It scans the
    /// node set; that is fine at editor scale, and an index can replace it later
    /// without changing this signature.
    #[must_use]
    pub fn node_id_of(&self, stable_id: u64) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|(_, n)| n.stable_id == stable_id)
            .map(|(id, _)| id)
    }

    /// The `(source node, source output port)` feeding `dest`'s input port, or
    /// `None` if the port is unconnected or out of range (or `dest` is absent).
    ///
    /// The editor reads edges back to render existing wires: when a project loads,
    /// the canvas is populated from core's connections, the same canonical-core
    /// direction as wiring. Core's internal connection storage stays private.
    #[must_use]
    pub fn input_source(&self, dest: NodeId, dest_port: usize) -> Option<(NodeId, usize)> {
        let node = self.nodes.get(dest)?;
        let conn = node.inputs.get(dest_port)?.as_ref()?;
        Some((conn.source, conn.output))
    }

    /// Clears the connection feeding one input port, leaving it unconnected.
    ///
    /// Disconnecting an already-empty port is not an error; the port ends empty
    /// either way.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NodeNotFound`] if `dest` is absent, or [`Error::InvalidPort`]
    /// if `dest_port` is out of range.
    pub fn disconnect(&mut self, dest: NodeId, dest_port: usize) -> Result<()> {
        let node = self.nodes.get_mut(dest).ok_or(Error::NodeNotFound)?;
        if dest_port >= node.inputs.len() {
            return Err(Error::InvalidPort {
                type_id: node.type_id,
                port: dest_port,
            });
        }
        node.inputs[dest_port] = None;
        Ok(())
    }

    /// Removes a node, cascading to any edges that referenced it, and reports
    /// whether a node was actually removed.
    ///
    /// Every input slot pointing at the removed node is cleared, so no connection
    /// is left dangling to a missing source. Removing an absent node is a no-op that
    /// returns `false`.
    pub fn remove_node(&mut self, id: NodeId) -> bool {
        if self.nodes.remove(id).is_none() {
            return false;
        }
        for node in self.nodes.values_mut() {
            for slot in &mut node.inputs {
                if slot.as_ref().is_some_and(|conn| conn.source == id) {
                    *slot = None;
                }
            }
        }
        true
    }

    /// Copies a set of nodes into this graph, minting a fresh `stable_id` and runtime
    /// id for each, and returns a map from every source node's id to its copy.
    ///
    /// This is the shared kernel under multi-node duplicate, "create subgraph from a
    /// selection", and dropping a saved subgraph into a graph: each needs an
    /// independent copy of a node set that preserves how those nodes wire to one
    /// another. Edges *internal* to the set (both endpoints in `nodes`) are reproduced
    /// among the copies; edges crossing the set boundary are dropped, so the result is
    /// a self-contained fragment with the same inner shape and no link to the originals
    /// or their neighbours. Each copy carries the source's operator, params, name
    /// override, and bypass state; only its identity and wiring are new.
    ///
    /// Ids in `nodes` that are absent are skipped, and a repeated id is copied once.
    /// Copies are created in ascending source-`stable_id` order, so the same selection
    /// always yields the same id assignment (determinism the seed and cache depend on),
    /// independent of the order `nodes` is given in.
    pub fn copy_subgraph(&mut self, nodes: &[NodeId]) -> HashMap<NodeId, NodeId> {
        // Deduplicate to the live nodes paired with their stable_id, then order by it,
        // so fresh ids are assigned deterministically regardless of the caller's order.
        let unique: HashSet<NodeId> = nodes.iter().copied().collect();
        let mut sources: Vec<(NodeId, u64)> = unique
            .iter()
            .filter_map(|&id| self.nodes.get(id).map(|n| (id, n.stable_id)))
            .collect();
        sources.sort_by_key(|&(_, stable_id)| stable_id);

        // Pass one: clone each node's payload with a fresh identity and no wiring,
        // recording old -> new so pass two can translate internal edges.
        let mut map: HashMap<NodeId, NodeId> = HashMap::with_capacity(sources.len());
        for &(old, _) in &sources {
            let Some(src) = self.nodes.get(old) else {
                continue;
            };
            let mut clone = src.clone();
            clone.stable_id = self.next_stable_id;
            self.next_stable_id += 1;
            for slot in &mut clone.inputs {
                *slot = None;
            }
            let new = self.nodes.insert(clone);
            map.insert(old, new);
        }

        // Pass two: reproduce edges whose source is also copied, translated onto the
        // copies. An edge from outside the set has no entry in `map` and is left
        // dropped, so the boundary is cut cleanly.
        for &(old, _) in &sources {
            let Some(src) = self.nodes.get(old) else {
                continue;
            };
            let internal: Vec<(usize, NodeId, usize)> = src
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(port, slot)| {
                    let conn = slot.as_ref()?;
                    map.get(&conn.source)
                        .map(|&new_source| (port, new_source, conn.output))
                })
                .collect();
            let Some(&new_dest) = map.get(&old) else {
                continue;
            };
            // The copy has the same input arity as its source, so every `port` here is
            // in range; rewrite the slots directly (no port to revalidate).
            if let Some(dest_node) = self.nodes.get_mut(new_dest) {
                for (port, new_source, output) in internal {
                    dest_node.inputs[port] = Some(InputConn {
                        source: new_source,
                        output,
                    });
                }
            }
        }

        map
    }

    /// Wraps `nodes` into a new subgraph container that replaces them in this graph (#106).
    ///
    /// The selected nodes move into the container's inner graph (operators cloned faithfully,
    /// so a nested subgraph survives being wrapped), Input/Output boundary markers are
    /// generated for every wire that crossed the selection, and the surrounding graph is
    /// rewired to the container's derived ports. Ports follow the boundary: one input port
    /// per selected input pin fed from outside, one output port per selected output pin
    /// feeding outside (fan-out shares one port). Internal wiring is preserved inside; the
    /// originals are removed. Ports are ordered deterministically by `stable_id`. Absent ids
    /// are ignored. Returns an [`Extraction`] (the container plus the identity mapping the
    /// editor needs to lay the interior out).
    ///
    /// # Errors
    ///
    /// Propagates an error only if an internal or external reconnection is rejected, which
    /// does not happen for a well-formed selection (every port is copied from a live node).
    pub fn extract_subgraph(&mut self, nodes: &[NodeId]) -> Result<Extraction> {
        use crate::subgraph::{InputNode, OutputNode, SubgraphNode};

        // Live, de-duplicated selection in ascending stable_id order, for deterministic port
        // and inner-id assignment.
        let mut selected: Vec<(NodeId, u64)> = nodes
            .iter()
            .copied()
            .collect::<HashSet<NodeId>>()
            .into_iter()
            .filter_map(|id| self.nodes.get(id).map(|n| (id, n.stable_id)))
            .collect();
        selected.sort_by_key(|&(_, stable_id)| stable_id);
        let selected: Vec<NodeId> = selected.into_iter().map(|(id, _)| id).collect();
        let set: HashSet<NodeId> = selected.iter().copied().collect();

        // Boundary inputs: a selected input pin fed from outside the selection. One container
        // input port each, in (selected stable_id, port) order.
        let mut boundary_inputs: Vec<(NodeId, usize, NodeId, usize)> = Vec::new();
        for &s in &selected {
            let Some(node) = self.nodes.get(s) else {
                continue;
            };
            for (port, slot) in node.inputs.iter().enumerate() {
                if let Some(conn) = slot.as_ref()
                    && !set.contains(&conn.source)
                {
                    boundary_inputs.push((s, port, conn.source, conn.output));
                }
            }
        }

        // Boundary outputs: a selected output pin feeding outside. Group external consumers
        // by (selected src, out); the ports are ordered by (src stable_id, out).
        let mut consumers_by_output: HashMap<(NodeId, usize), Vec<(NodeId, usize)>> =
            HashMap::new();
        for (dest, node) in self.nodes.iter() {
            if set.contains(&dest) {
                continue;
            }
            for (port, slot) in node.inputs.iter().enumerate() {
                if let Some(conn) = slot.as_ref()
                    && set.contains(&conn.source)
                {
                    consumers_by_output
                        .entry((conn.source, conn.output))
                        .or_default()
                        .push((dest, port));
                }
            }
        }
        let mut output_keys: Vec<(NodeId, usize)> = consumers_by_output.keys().copied().collect();
        output_keys
            .sort_by_key(|&(src, out)| (self.nodes.get(src).map_or(0, |n| n.stable_id), out));

        // Build the inner graph: a faithful copy of each selected node (operator cloned, so
        // nested subgraphs survive), then its internal wiring, then the boundary markers.
        let mut inner = Graph::new();
        let mut inner_of: HashMap<NodeId, NodeId> = HashMap::new();
        for &s in &selected {
            let Some((operator, params, name, bypassed)) = self.nodes.get(s).map(|n| {
                (
                    n.operator.clone_box(),
                    n.params.clone(),
                    n.name.clone(),
                    n.bypassed,
                )
            }) else {
                continue;
            };
            let inner_id = inner.add_op(operator, params);
            inner.set_name(inner_id, name)?;
            inner.set_bypassed(inner_id, bypassed)?;
            inner_of.insert(s, inner_id);
        }
        for &s in &selected {
            let edges: Vec<(usize, NodeId, usize)> = match self.nodes.get(s) {
                Some(node) => node
                    .inputs
                    .iter()
                    .enumerate()
                    .filter_map(|(port, slot)| {
                        let conn = slot.as_ref()?;
                        set.contains(&conn.source)
                            .then_some((port, conn.source, conn.output))
                    })
                    .collect(),
                None => continue,
            };
            let dest = inner_of[&s];
            for (port, src, out) in edges {
                inner.connect(inner_of[&src], out, dest, port)?;
            }
        }
        // Input markers, in boundary-input order, so container input port i is boundary i.
        let mut input_markers = Vec::with_capacity(boundary_inputs.len());
        for &(dest, port, _, _) in &boundary_inputs {
            let marker = inner.add_op(Box::new(InputNode), Params::default());
            inner.connect(marker, 0, inner_of[&dest], port)?;
            input_markers.push(marker);
        }
        // Output markers, in output-port order, so container output port i is output_keys[i].
        let mut output_markers = Vec::with_capacity(output_keys.len());
        for &(src, out) in &output_keys {
            let marker = inner.add_op(Box::new(OutputNode), Params::default());
            inner.connect(inner_of[&src], out, marker, 0)?;
            output_markers.push(marker);
        }

        // Capture the inner identities (by stable_id) the editor needs to lay the interior
        // out, before `inner` is moved into the container.
        let moved: Vec<(u64, u64)> = selected
            .iter()
            .filter_map(|&s| {
                let outer = self.nodes.get(s)?.stable_id;
                let inner_id = inner.stable_id(inner_of[&s])?;
                Some((outer, inner_id))
            })
            .collect();
        let input_ids: Vec<u64> = input_markers
            .iter()
            .filter_map(|&m| inner.stable_id(m))
            .collect();
        let output_ids: Vec<u64> = output_markers
            .iter()
            .filter_map(|&m| inner.stable_id(m))
            .collect();

        // Create the container (its ports derive from the markers just added) and rewire the
        // surrounding graph to it.
        let container = self.add_op(Box::new(SubgraphNode::new(inner)), Params::default());
        for (port, &(_, _, ext_src, ext_out)) in boundary_inputs.iter().enumerate() {
            self.connect(ext_src, ext_out, container, port)?;
        }
        for (port, key) in output_keys.iter().enumerate() {
            let mut consumers = consumers_by_output.get(key).cloned().unwrap_or_default();
            consumers
                .sort_by_key(|&(dest, q)| (self.nodes.get(dest).map_or(0, |n| n.stable_id), q));
            for (dest, q) in consumers {
                self.connect(container, port, dest, q)?;
            }
        }
        // Remove the now-wrapped originals; their external edges were rewired to the
        // container above, and their internal edges live inside it.
        for &s in &selected {
            self.remove_node(s);
        }
        Ok(Extraction {
            container,
            moved,
            inputs: input_ids,
            outputs: output_ids,
        })
    }

    /// Whether connecting `source` into `dest` would create a cycle.
    ///
    /// Connection itself ([`connect`](Self::connect)) is deliberately lenient and
    /// leaves cycle detection to evaluation, which suits programmatic and load-time
    /// use. The editor needs the stricter, pre-flight answer: it must refuse a wire
    /// that would form a loop before the wire is ever shown. Adding an edge that
    /// feeds `source` into `dest` closes a loop exactly when `dest` already lies on
    /// `source`'s upstream cone (so `dest` reaches `source`); a self-edge is the
    /// degenerate case. This walks `source`'s upstream inputs and reports whether
    /// `dest` is reached. The traversal set only gates revisits, so the boolean
    /// result is independent of iteration order.
    #[must_use]
    pub fn would_create_cycle(&self, source: NodeId, dest: NodeId) -> bool {
        if source == dest {
            return true;
        }
        let mut stack = vec![source];
        let mut visited = HashSet::new();
        while let Some(node) = stack.pop() {
            if !visited.insert(node) {
                continue;
            }
            let Some(node) = self.nodes.get(node) else {
                continue;
            };
            for conn in node.inputs.iter().flatten() {
                if conn.source == dest {
                    return true;
                }
                stack.push(conn.source);
            }
        }
        false
    }

    /// Borrows a node by id, for the evaluator.
    pub(crate) fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The runtime node ids of every node whose `type_id` is `type_id`, in ascending
    /// `stable_id` order.
    ///
    /// The subgraph container uses this to find its boundary markers deterministically,
    /// so its derived input/output ports keep a stable order regardless of node insertion
    /// order. The editor uses it to number a marker node, whose index here (by `stable_id`)
    /// matches the container's derived port order.
    #[must_use]
    pub fn nodes_of_type(&self, type_id: &str) -> Vec<NodeId> {
        let mut ids: Vec<(NodeId, u64)> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.type_id == type_id)
            .map(|(id, n)| (id, n.stable_id))
            .collect();
        ids.sort_by_key(|&(_, stable_id)| stable_id);
        ids.into_iter().map(|(id, _)| id).collect()
    }

    /// A canonical, machine-independent content hash of the graph's output-determining
    /// state: each node's `stable_id`, `type_id`, params, bypass, and connections, in
    /// deterministic order (built from the canonical [`to_document`](Self::to_document)).
    ///
    /// Equal graphs hash equal on any machine. A subgraph container folds this into its
    /// cache key (via [`Operator::content_hash`](crate::Operator::content_hash)) so editing
    /// the inner graph invalidates the container's cached output. Display-name overrides are
    /// excluded deliberately: like everywhere else, a rename is cosmetic and never changes
    /// output or a cache key.
    #[must_use]
    pub fn content_hash(&self) -> u64 {
        let doc = self.to_document();
        let mut h = Fnv1a64::new();
        h.write_usize(doc.nodes.len());
        for node in &doc.nodes {
            h.write_u64(node.stable_id);
            h.write_str(&node.type_id);
            h.write_u64(node.params.content_hash().to_u64());
            h.write_u64(u64::from(node.bypassed));
            h.write_usize(node.connections.len());
            for conn in &node.connections {
                h.write_usize(conn.input);
                h.write_u64(conn.source);
                h.write_usize(conn.output);
            }
        }
        h.finish().to_u64()
    }

    /// Serializes the graph into a [`ProjectDocument`]: its persistent state only
    /// (`stable_id`, `type_id`, name, params, connections), never the live operators
    /// or runtime `NodeId`s. Connection sources are translated from `NodeId` to the
    /// source node's `stable_id`, the identity that survives a reload.
    ///
    /// Output is deterministically ordered (nodes by `stable_id`, connections by input
    /// port; params are already name-ordered), so the same graph always produces the
    /// same document and a saved project diffs cleanly.
    #[must_use]
    pub fn to_document(&self) -> ProjectDocument {
        let mut nodes: Vec<NodeDocument> = self
            .nodes
            .values()
            .map(|node| {
                let mut connections: Vec<Connection> = node
                    .inputs
                    .iter()
                    .enumerate()
                    .filter_map(|(input, slot)| {
                        let conn = slot.as_ref()?;
                        // A connection's source is always a live node: removing a node
                        // cascades to clear edges into it (see `remove_node`). The
                        // lookup is therefore infallible; a `None` would mean a broken
                        // invariant, so assert it in debug and drop the dangling edge
                        // in release rather than emit a reference to a missing node.
                        let source = self.nodes.get(conn.source).map(|s| s.stable_id);
                        debug_assert!(source.is_some(), "connection source must be live");
                        Some(Connection {
                            input,
                            source: source?,
                            output: conn.output,
                        })
                    })
                    .collect();
                connections.sort_by_key(|c| c.input);
                NodeDocument {
                    stable_id: node.stable_id,
                    type_id: node.type_id.to_string(),
                    name: node.name.clone(),
                    params: node.params.clone(),
                    connections,
                    bypassed: node.bypassed,
                    // A container captures its inner graph (recursively); an ordinary node
                    // has no nested graph, so this is omitted from the file. Structural, via
                    // the operator's hook, never by naming a concrete type.
                    subgraph: node
                        .operator
                        .nested()
                        .map(|inner| Box::new(inner.to_document())),
                }
            })
            .collect();
        nodes.sort_by_key(|n| n.stable_id);

        ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: self.next_stable_id,
            nodes,
        }
    }

    /// Rebuilds a graph from a [`ProjectDocument`], the inverse of
    /// [`to_document`](Self::to_document).
    ///
    /// Each node's operator is reconstructed from its `type_id` through the registry,
    /// so no concrete node type is named here; its params, name, and persistent
    /// `stable_id` are restored, and `next_stable_id` is carried over so ids assigned
    /// after the load cannot collide with loaded ones. Connections are reapplied by
    /// `stable_id`. A reloaded graph evaluates identically to the saved one, since
    /// seeding derives from `stable_id`, which is preserved.
    ///
    /// Recoverable problems do not fail the load; they degrade so the project always opens (a
    /// node change must never orphan a saved project). See
    /// [`from_document_reporting`](Self::from_document_reporting) for the list, and use that
    /// variant when the warnings should be surfaced to the user.
    ///
    /// # Errors
    ///
    /// - [`Error::UnsupportedFormatVersion`] if the document's version is not the one this build
    ///   understands (a genuine incompatibility that needs a migration).
    pub fn from_document(doc: &ProjectDocument) -> Result<Self> {
        Self::from_document_reporting(doc).map(|(graph, _warnings)| graph)
    }

    /// Rebuilds a graph from a document, returning the graph together with a list of human-
    /// readable warnings for anything that had to degrade. Nothing recoverable aborts the load:
    ///
    /// - An unknown `type_id` (a node removed or renamed since the save) becomes a placeholder
    ///   that preserves the type id, params, and enough ports for its connections to reattach;
    ///   it re-saves faithfully and evaluates to an error rather than producing output.
    /// - A connection to a missing source, or to a port the rebuilt operator no longer has (an
    ///   arity change), is dropped.
    /// - A duplicate `stable_id` keeps the first node; the collision is reported.
    /// - A subgraph that cannot be rebuilt is left empty and reported, its own warnings folded in.
    ///
    /// Only an [`Error::UnsupportedFormatVersion`] is fatal.
    pub fn from_document_reporting(doc: &ProjectDocument) -> Result<(Self, Vec<String>)> {
        if doc.format_version != FORMAT_VERSION {
            return Err(Error::UnsupportedFormatVersion {
                version: doc.format_version,
                expected: FORMAT_VERSION,
            });
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut graph = Graph::new();
        // stable_id -> runtime NodeId, for resolving connection sources in pass two.
        let mut by_stable: HashMap<u64, NodeId> = HashMap::with_capacity(doc.nodes.len());
        // The runtime id of each node, parallel to `doc.nodes`, so pass two need not
        // look the destination up.
        let mut node_ids: Vec<NodeId> = Vec::with_capacity(doc.nodes.len());

        // The highest output index any connection reads from each source, so a placeholder for a
        // missing node gets enough output ports for its downstream wiring to reattach.
        let mut max_output: HashMap<u64, usize> = HashMap::new();
        for nd in &doc.nodes {
            for conn in &nd.connections {
                let slot = max_output.entry(conn.source).or_insert(0);
                *slot = (*slot).max(conn.output);
            }
        }

        // Pass one: create every node, so a connection can resolve its source
        // regardless of node order in the file.
        for nd in &doc.nodes {
            // Rebuild the operator, or substitute a placeholder when its type is unavailable so
            // the project still opens. A placeholder's ports are inferred from the wiring in the
            // file (max input used, max output read downstream) so pass-two connections land.
            let operator: Box<dyn Operator> = match crate::registry::make(&nd.type_id) {
                Some(op) => op,
                None => {
                    let inputs = nd
                        .connections
                        .iter()
                        .map(|c| c.input + 1)
                        .max()
                        .unwrap_or(0);
                    let outputs = max_output.get(&nd.stable_id).map_or(0, |m| m + 1).max(1);
                    warnings.push(format!(
                        "node {} has unavailable type \"{}\"; kept as a placeholder (it will not evaluate)",
                        nd.stable_id, nd.type_id,
                    ));
                    Box::new(crate::missing::MissingOperator::new(
                        crate::missing::intern_type_id(&nd.type_id),
                        inputs,
                        outputs,
                    ))
                }
            };
            let id = graph.add_op(operator, nd.params.clone());
            // `add_op` assigned a fresh stable_id; overwrite it with the persisted one
            // so identity (and the seed derived from it) matches the saved project.
            if let Some(node) = graph.nodes.get_mut(id) {
                node.stable_id = nd.stable_id;
                node.name = nd.name.clone();
                node.bypassed = nd.bypassed;
            }
            // Restore a container's inner graph (recursively), then refresh this node's arity from
            // it so pass-two connections land on the right ports. A subgraph that cannot be
            // rebuilt is left as the container's default inner rather than aborting the load.
            if let Some(inner_doc) = &nd.subgraph {
                match Self::from_document_reporting(inner_doc) {
                    Ok((inner, inner_warnings)) => {
                        for w in inner_warnings {
                            warnings.push(format!("in subgraph {}: {w}", nd.stable_id));
                        }
                        let rebuilt = graph
                            .node(id)
                            .ok_or(Error::NodeNotFound)?
                            .operator
                            .rebuild_nested(inner);
                        graph.set_operator(id, rebuilt)?;
                    }
                    Err(e) => warnings.push(format!(
                        "subgraph {} could not be rebuilt ({e}); left empty",
                        nd.stable_id,
                    )),
                }
            }
            // A duplicate id is corruption; keep the first node and report it rather than abort.
            if let std::collections::hash_map::Entry::Vacant(slot) = by_stable.entry(nd.stable_id) {
                slot.insert(id);
            } else {
                warnings.push(format!(
                    "duplicate node id {}; keeping the first",
                    nd.stable_id,
                ));
            }
            node_ids.push(id);
        }

        // `add_op` bumped the counter once per node; restore the saved value so the
        // next id assigned matches the saved project.
        graph.next_stable_id = doc.next_stable_id;

        // Pass two: reapply connections by stable_id, dropping any that cannot be applied (a
        // missing source, or a port an arity change removed) rather than aborting.
        for (nd, &dest) in doc.nodes.iter().zip(&node_ids) {
            for conn in &nd.connections {
                let Some(&source) = by_stable.get(&conn.source) else {
                    warnings.push(format!(
                        "dropped a connection into node {}: its source node {} is missing",
                        nd.stable_id, conn.source,
                    ));
                    continue;
                };
                if let Err(e) = graph.connect(source, conn.output, dest, conn.input) {
                    warnings.push(format!(
                        "dropped a connection into node {} ({e})",
                        nd.stable_id,
                    ));
                }
            }
        }

        Ok((graph, warnings))
    }

    /// Writes the graph to `writer` as a pretty-printed JSON project document. Pretty
    /// output (not minified) keeps a saved project human-readable and git-diffable, a
    /// stated goal for sharing node networks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`] if serialization or the underlying write fails.
    pub fn save_to_writer(&self, writer: impl Write) -> Result<()> {
        serde_json::to_writer_pretty(writer, &self.to_document())?;
        Ok(())
    }

    /// Reads a graph from `reader`, parsing a JSON project document and rebuilding it
    /// via [`from_document`](Self::from_document).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`] if the bytes are not a valid project document, or any
    /// error from [`from_document`](Self::from_document) (unknown node type,
    /// unsupported version, duplicate id, dangling connection).
    pub fn load_from_reader(reader: impl Read) -> Result<Self> {
        let doc: ProjectDocument = serde_json::from_reader(reader)?;
        Self::from_document(&doc)
    }

    /// Saves the graph to a project file at `path` (pretty JSON), creating or
    /// truncating it. The parent directory must already exist.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be created, or [`Error::Json`] if
    /// writing the document fails.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let file = File::create(path)?;
        self.save_to_writer(BufWriter::new(file))
    }

    /// Loads a graph from a project file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be opened, or any error from
    /// [`load_from_reader`](Self::load_from_reader).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        Self::load_from_reader(BufReader::new(file))
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EvalContext;
    use crate::field::Field;
    use crate::spec::PortSpec;

    /// A test operator with arbitrary arity. Its `eval` is never run here; these
    /// tests exercise graph structure (wiring, removal, cycle queries), not
    /// evaluation.
    #[derive(Clone)]
    struct Stub {
        type_id: &'static str,
        inputs: usize,
        outputs: usize,
    }

    impl Operator for Stub {
        fn spec(&self) -> NodeSpec {
            NodeSpec {
                type_id: self.type_id,
                category: "test",
                inputs: (0..self.inputs)
                    .map(|i| PortSpec::new(format!("in{i}")))
                    .collect(),
                outputs: (0..self.outputs)
                    .map(|i| PortSpec::new(format!("out{i}")))
                    .collect(),
                params: Vec::new(),
            }
        }

        fn eval(&self, _: crate::Inputs, _: &Params, _: &EvalContext) -> Result<Vec<Field>> {
            Ok(Vec::new())
        }
    }

    fn add(graph: &mut Graph, type_id: &'static str, inputs: usize, outputs: usize) -> NodeId {
        graph.add_op(
            Box::new(Stub {
                type_id,
                inputs,
                outputs,
            }),
            Params::default(),
        )
    }

    // Two operators registered with the registry so `from_document` can rebuild them
    // by `type_id`, the same path real nodes take. Scoped to this test binary.
    fn make_test_gen() -> Box<dyn Operator> {
        Box::new(Stub {
            type_id: "test.gen",
            inputs: 0,
            outputs: 1,
        })
    }

    fn make_test_mod() -> Box<dyn Operator> {
        Box::new(Stub {
            type_id: "test.mod",
            inputs: 1,
            outputs: 1,
        })
    }

    inventory::submit! { crate::registry::OperatorEntry { type_id: "test.gen", make: make_test_gen } }
    inventory::submit! { crate::registry::OperatorEntry { type_id: "test.mod", make: make_test_mod } }

    #[test]
    fn spec_and_params_are_readable_by_id() {
        let mut g = Graph::new();
        let n = add(&mut g, "test.mod", 1, 2);
        let spec = g.spec(n).expect("node exists");
        assert_eq!(spec.type_id, "test.mod");
        assert_eq!(spec.inputs.len(), 1);
        assert_eq!(spec.outputs.len(), 2);
        assert!(g.params(n).is_some());
    }

    #[test]
    fn name_override_is_set_cleared_and_validated() {
        let mut g = Graph::new();
        let n = add(&mut g, "test.mod", 1, 1);
        assert_eq!(g.name(n), None, "defaults to the type's name");
        g.set_name(n, Some("My Node".to_string()))
            .expect("node exists");
        assert_eq!(g.name(n), Some("My Node"));
        g.set_name(n, None).expect("node exists");
        assert_eq!(g.name(n), None, "cleared back to the type's name");

        // A missing node errors rather than panicking.
        g.remove_node(n);
        assert!(matches!(
            g.set_name(n, Some("x".to_string())),
            Err(Error::NodeNotFound)
        ));
        assert_eq!(g.name(n), None);
    }

    #[test]
    fn stable_id_round_trips_through_node_id_of() {
        let mut g = Graph::new();
        let a = add(&mut g, "test.head", 0, 1);
        let b = add(&mut g, "test.head", 0, 1);
        let sid_a = g.stable_id(a).expect("a exists");
        let sid_b = g.stable_id(b).expect("b exists");
        assert_ne!(sid_a, sid_b, "stable ids are distinct");
        assert_eq!(g.node_id_of(sid_a), Some(a));
        assert_eq!(g.node_id_of(sid_b), Some(b));
        assert_eq!(g.node_id_of(9999), None);
    }

    #[test]
    fn accessors_return_none_for_absent_nodes() {
        let mut g = Graph::new();
        let n = add(&mut g, "test.head", 0, 1);
        assert!(g.remove_node(n));
        assert!(g.spec(n).is_none());
        assert!(g.params(n).is_none());
        assert!(g.stable_id(n).is_none());
    }

    #[test]
    fn input_source_reads_edges_back() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let modr = add(&mut g, "test.mod", 1, 1);
        assert_eq!(g.input_source(modr, 0), None, "unconnected reads as None");
        g.connect(head, 0, modr, 0).expect("connect");
        assert_eq!(g.input_source(modr, 0), Some((head, 0)));
        // Out-of-range port and absent node both read as None, never panic.
        assert_eq!(g.input_source(modr, 9), None);
        assert!(g.remove_node(head));
        assert_eq!(g.input_source(head, 0), None);
    }

    #[test]
    fn disconnect_clears_one_input_and_validates() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let modr = add(&mut g, "test.mod", 1, 1);
        g.connect(head, 0, modr, 0).expect("connect");
        assert_eq!(
            g.node(modr).expect("mod").inputs[0]
                .as_ref()
                .map(|c| c.source),
            Some(head)
        );

        g.disconnect(modr, 0).expect("disconnect");
        assert!(g.node(modr).expect("mod").inputs[0].is_none());

        // Idempotent on an empty port; erroring conditions are reported.
        g.disconnect(modr, 0).expect("disconnect empty is ok");
        assert!(matches!(
            g.disconnect(modr, 5),
            Err(Error::InvalidPort { .. })
        ));
        assert!(matches!(
            g.disconnect(head, 0),
            Err(Error::InvalidPort { .. })
        ));
    }

    #[test]
    fn remove_node_cascades_to_dependent_edges() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let modr = add(&mut g, "test.mod", 1, 1);
        g.connect(head, 0, modr, 0).expect("connect");

        assert!(g.remove_node(head));
        assert_eq!(g.node_count(), 1);
        // The edge that fed the removed node is gone, not dangling.
        assert!(g.node(modr).expect("mod").inputs[0].is_none());
        // Removing an absent node is a no-op.
        assert!(!g.remove_node(head));
    }

    #[test]
    fn to_document_captures_persistent_state_and_translates_sources() {
        use crate::param::ParamValue;

        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let modr = add(&mut g, "test.mod", 1, 1);
        g.connect(head, 0, modr, 0).expect("connect");
        g.set_name(modr, Some("Shaper".to_string())).expect("name");
        g.set_params(modr, Params::new().with("k", ParamValue::Int(3)))
            .expect("params");

        let sid_head = g.stable_id(head).expect("head sid");
        let sid_mod = g.stable_id(modr).expect("mod sid");

        let doc = g.to_document();
        assert_eq!(doc.format_version, FORMAT_VERSION);
        assert_eq!(doc.next_stable_id, 2);
        // Nodes are emitted in stable_id order.
        let ids: Vec<u64> = doc.nodes.iter().map(|n| n.stable_id).collect();
        assert_eq!(ids, vec![sid_head, sid_mod]);

        let head_doc = &doc.nodes[0];
        assert_eq!(head_doc.type_id, "test.head");
        assert_eq!(head_doc.name, None);
        assert!(head_doc.connections.is_empty());

        let mod_doc = &doc.nodes[1];
        assert_eq!(mod_doc.type_id, "test.mod");
        assert_eq!(mod_doc.name.as_deref(), Some("Shaper"));
        assert_eq!(mod_doc.params.get("k"), Some(&ParamValue::Int(3)));
        // The connection's source is the head node's stable_id, not its NodeId.
        assert_eq!(
            mod_doc.connections,
            vec![Connection {
                input: 0,
                source: sid_head,
                output: 0,
            }]
        );
    }

    #[test]
    fn from_document_round_trips_a_graph_identically() {
        use crate::param::ParamValue;

        let mut g = Graph::new();
        let head = g.add_op(make_test_gen(), Params::new());
        let modr = g.add_op(make_test_mod(), Params::new().with("k", ParamValue::Int(3)));
        g.connect(head, 0, modr, 0).expect("connect");
        g.set_name(modr, Some("Shaper".to_string())).expect("name");

        let doc = g.to_document();
        let rebuilt = Graph::from_document(&doc).expect("rebuild");
        // Document-level equality proves stable_ids, type_ids, params, names,
        // connections, and next_stable_id all round-trip.
        assert_eq!(rebuilt.to_document(), doc);
    }

    #[test]
    fn bypass_state_round_trips_through_a_document() {
        let mut g = Graph::new();
        let head = g.add_op(make_test_gen(), Params::new());
        let modr = g.add_op(make_test_mod(), Params::new());
        g.connect(head, 0, modr, 0).expect("connect");
        g.set_bypassed(modr, true).expect("bypass");

        let rebuilt = Graph::from_document(&g.to_document()).expect("rebuild");
        // The modifier's bypass survives; the untouched generator stays not bypassed.
        let modr_id = rebuilt.node_id_of(g.stable_id(modr).unwrap()).unwrap();
        let head_id = rebuilt.node_id_of(g.stable_id(head).unwrap()).unwrap();
        assert!(rebuilt.is_bypassed(modr_id));
        assert!(!rebuilt.is_bypassed(head_id));
    }

    #[test]
    fn from_document_preserves_the_id_counter() {
        let mut g = Graph::new();
        g.add_op(make_test_gen(), Params::new());
        g.add_op(make_test_gen(), Params::new());
        let doc = g.to_document();
        assert_eq!(doc.next_stable_id, 2);

        let mut rebuilt = Graph::from_document(&doc).expect("rebuild");
        // A node added after loading continues the sequence rather than colliding.
        let added = rebuilt.add_op(make_test_gen(), Params::new());
        assert_eq!(rebuilt.stable_id(added), Some(2));
    }

    #[test]
    fn from_document_rejects_an_unsupported_version() {
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION + 1,
            next_stable_id: 0,
            nodes: Vec::new(),
        };
        assert!(matches!(
            Graph::from_document(&doc),
            Err(Error::UnsupportedFormatVersion { .. })
        ));
    }

    #[test]
    fn from_document_keeps_an_unknown_node_type_as_a_placeholder() {
        // A project referencing a node type this build lacks must still open: the node becomes a
        // placeholder that preserves its type id and params, and the loss is reported.
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 1,
            nodes: vec![NodeDocument {
                stable_id: 0,
                type_id: "test.nonesuch".to_string(),
                name: None,
                params: Params::new(),
                connections: Vec::new(),
                bypassed: false,
                subgraph: None,
            }],
        };
        let (graph, warnings) =
            Graph::from_document_reporting(&doc).expect("loads despite the unknown type");
        assert!(
            warnings.iter().any(|w| w.contains("test.nonesuch")),
            "the unavailable type is reported: {warnings:?}"
        );
        // The placeholder round-trips faithfully, so re-saving does not lose the node.
        assert_eq!(graph.to_document().nodes[0].type_id, "test.nonesuch");
        assert!(
            Graph::from_document(&doc).is_ok(),
            "never orphans a project"
        );
    }

    #[test]
    fn from_document_keeps_the_first_of_duplicate_ids() {
        let node = |stable_id| NodeDocument {
            stable_id,
            type_id: "test.gen".to_string(),
            name: None,
            params: Params::new(),
            connections: Vec::new(),
            bypassed: false,
            subgraph: None,
        };
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 2,
            nodes: vec![node(0), node(0)],
        };
        let (_graph, warnings) =
            Graph::from_document_reporting(&doc).expect("loads despite the duplicate id");
        assert!(
            warnings.iter().any(|w| w.contains("duplicate")),
            "the duplicate id is reported: {warnings:?}"
        );
    }

    #[test]
    fn save_and_load_through_a_byte_buffer_round_trips() {
        use crate::param::ParamValue;

        let mut g = Graph::new();
        let head = g.add_op(make_test_gen(), Params::new());
        let modr = g.add_op(make_test_mod(), Params::new().with("k", ParamValue::Int(5)));
        g.connect(head, 0, modr, 0).expect("connect");

        let mut buf: Vec<u8> = Vec::new();
        g.save_to_writer(&mut buf).expect("save");
        assert!(buf.starts_with(b"{"), "output is JSON");

        let loaded = Graph::load_from_reader(&buf[..]).expect("load");
        assert_eq!(loaded.to_document(), g.to_document());
    }

    #[test]
    fn load_reports_a_malformed_file() {
        let bad = br#"{ "format_version": 1, "nodes": [ this is not json"#;
        assert!(matches!(
            Graph::load_from_reader(&bad[..]),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn from_document_drops_a_dangling_connection() {
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 1,
            nodes: vec![NodeDocument {
                stable_id: 0,
                type_id: "test.mod".to_string(),
                name: None,
                params: Params::new(),
                connections: vec![Connection {
                    input: 0,
                    source: 99,
                    output: 0,
                }],
                bypassed: false,
                subgraph: None,
            }],
        };
        let (graph, warnings) =
            Graph::from_document_reporting(&doc).expect("loads despite the dangling connection");
        assert!(
            warnings.iter().any(|w| w.contains("dropped")),
            "the dropped connection is reported: {warnings:?}"
        );
        // The node is kept, with its now-unwired input left empty.
        let saved = graph.to_document();
        assert_eq!(saved.nodes.len(), 1);
        assert!(saved.nodes[0].connections.is_empty());
    }

    #[test]
    fn from_document_drops_a_connection_to_a_port_that_no_longer_exists() {
        // A saved connection reads an output the operator no longer has (its arity changed since
        // the file was written). The connection is dropped rather than aborting the load.
        let entry = |stable_id, type_id: &str, connections| NodeDocument {
            stable_id,
            type_id: type_id.to_string(),
            name: None,
            params: Params::new(),
            connections,
            bypassed: false,
            subgraph: None,
        };
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 2,
            nodes: vec![
                entry(0, "test.gen", Vec::new()),
                entry(
                    1,
                    "test.mod",
                    vec![Connection {
                        input: 0,
                        source: 0,
                        output: 5, // test.gen has one output; 5 is out of range
                    }],
                ),
            ],
        };
        let (graph, warnings) =
            Graph::from_document_reporting(&doc).expect("loads despite the invalid port");
        assert!(
            warnings.iter().any(|w| w.contains("dropped")),
            "the dropped connection is reported: {warnings:?}"
        );
        let saved = graph.to_document();
        assert_eq!(saved.nodes.len(), 2);
        assert!(saved.nodes.iter().all(|n| n.connections.is_empty()));
    }

    #[test]
    fn a_placeholder_preserves_params_and_wiring_on_re_save() {
        use crate::param::ParamValue;
        // An unknown node carrying params and an input wired from a real upstream node re-saves
        // with its type, params, and connection intact: opening a project with a missing node
        // loses none of that node's data.
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 2,
            nodes: vec![
                NodeDocument {
                    stable_id: 0,
                    type_id: "test.gen".to_string(),
                    name: None,
                    params: Params::new(),
                    connections: Vec::new(),
                    bypassed: false,
                    subgraph: None,
                },
                NodeDocument {
                    stable_id: 1,
                    type_id: "test.gone".to_string(),
                    name: None,
                    params: Params::new().with("k", ParamValue::Int(7)),
                    connections: vec![Connection {
                        input: 0,
                        source: 0,
                        output: 0,
                    }],
                    bypassed: false,
                    subgraph: None,
                },
            ],
        };
        let (graph, _warnings) = Graph::from_document_reporting(&doc).expect("loads");
        let saved = graph.to_document();
        let placeholder = saved
            .nodes
            .iter()
            .find(|n| n.stable_id == 1)
            .expect("placeholder present");
        assert_eq!(placeholder.type_id, "test.gone");
        assert_eq!(placeholder.params.get("k"), Some(&ParamValue::Int(7)));
        assert_eq!(placeholder.connections.len(), 1, "wiring preserved");
    }

    #[test]
    fn set_operator_refreshes_arity_and_preserves_identity() {
        let mut g = Graph::new();
        let n = add(&mut g, "test.mod", 1, 1);
        g.set_name(n, Some("Shaper".to_string())).expect("name");
        g.set_bypassed(n, true).expect("bypass");
        let sid = g.stable_id(n).expect("sid");

        g.set_operator(
            n,
            Box::new(Stub {
                type_id: "test.mod2",
                inputs: 2,
                outputs: 3,
            }),
        )
        .expect("set operator");

        let spec = g.spec(n).expect("spec");
        assert_eq!(spec.type_id, "test.mod2");
        assert_eq!(spec.inputs.len(), 2, "input arity refreshed");
        assert_eq!(spec.outputs.len(), 3, "output arity refreshed");
        // Identity and the cosmetic/eval-neutral fields survive the swap.
        assert_eq!(g.stable_id(n), Some(sid));
        assert_eq!(g.name(n), Some("Shaper"));
        assert!(g.is_bypassed(n));
    }

    #[test]
    fn set_operator_drops_connections_to_removed_input_ports() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let head2 = add(&mut g, "test.head", 0, 1);
        let n = add(&mut g, "test.mod", 2, 1);
        g.connect(head, 0, n, 0).expect("head->n.0");
        g.connect(head2, 0, n, 1).expect("head2->n.1");

        // Shrink to one input: port 0 keeps its wire, port 1 is gone.
        g.set_operator(
            n,
            Box::new(Stub {
                type_id: "test.mod",
                inputs: 1,
                outputs: 1,
            }),
        )
        .expect("set operator");
        assert_eq!(g.input_source(n, 0), Some((head, 0)));
        assert_eq!(g.input_source(n, 1), None, "removed port has no connection");
    }

    #[test]
    fn set_operator_prunes_downstream_edges_to_removed_output_ports() {
        let mut g = Graph::new();
        let src = add(&mut g, "test.src", 0, 2);
        let a = add(&mut g, "test.mod", 1, 1);
        let b = add(&mut g, "test.mod", 1, 1);
        g.connect(src, 0, a, 0).expect("src.0->a");
        g.connect(src, 1, b, 0).expect("src.1->b");

        // Shrink src to one output: the edge from its now-missing output 1 (into b) is cut.
        g.set_operator(
            src,
            Box::new(Stub {
                type_id: "test.src",
                inputs: 0,
                outputs: 1,
            }),
        )
        .expect("set operator");
        assert_eq!(
            g.input_source(a, 0),
            Some((src, 0)),
            "surviving output kept"
        );
        assert_eq!(g.input_source(b, 0), None, "edge to removed output pruned");
    }

    #[test]
    fn set_operator_on_an_absent_node_errors() {
        let mut g = Graph::new();
        let n = add(&mut g, "test.mod", 1, 1);
        assert!(g.remove_node(n));
        assert!(matches!(
            g.set_operator(
                n,
                Box::new(Stub {
                    type_id: "test.mod",
                    inputs: 1,
                    outputs: 1,
                }),
            ),
            Err(Error::NodeNotFound)
        ));
    }

    #[test]
    fn copy_subgraph_clones_payload_with_fresh_identity() {
        use crate::param::ParamValue;

        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let modr = add(&mut g, "test.mod", 1, 1);
        g.connect(head, 0, modr, 0).expect("connect");
        g.set_name(modr, Some("Shaper".to_string())).expect("name");
        g.set_params(modr, Params::new().with("k", ParamValue::Int(7)))
            .expect("params");
        g.set_bypassed(modr, true).expect("bypass");

        let map = g.copy_subgraph(&[head, modr]);
        assert_eq!(map.len(), 2);
        assert_eq!(g.node_count(), 4, "originals plus their copies");

        // The copy has a fresh identity but carries the source's payload.
        let copy_mod = map[&modr];
        assert_ne!(g.stable_id(copy_mod), g.stable_id(modr));
        assert_eq!(g.name(copy_mod), Some("Shaper"));
        assert_eq!(
            g.params(copy_mod).and_then(|p| p.get("k")),
            Some(&ParamValue::Int(7))
        );
        assert!(g.is_bypassed(copy_mod));
        // The original is untouched.
        assert_eq!(g.name(modr), Some("Shaper"));
    }

    #[test]
    fn copy_subgraph_keeps_internal_edges_and_drops_boundary() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        let a = add(&mut g, "test.mod", 1, 1);
        let b = add(&mut g, "test.mod", 1, 1);
        g.connect(head, 0, a, 0).expect("head->a");
        g.connect(a, 0, b, 0).expect("a->b");

        // Select only {a, b}: head->a crosses the boundary, a->b is internal.
        let map = g.copy_subgraph(&[a, b]);
        let (ca, cb) = (map[&a], map[&b]);
        // The boundary edge is not reproduced: the copy of a is unconnected.
        assert_eq!(g.input_source(ca, 0), None);
        // The internal edge is reproduced among the copies.
        assert_eq!(g.input_source(cb, 0), Some((ca, 0)));
        // Originals keep their wiring; copies link to nothing outside the set.
        assert_eq!(g.input_source(a, 0), Some((head, 0)));
        assert_eq!(g.input_source(b, 0), Some((a, 0)));
    }

    #[test]
    fn copy_subgraph_preserves_internal_fan_out() {
        let mut g = Graph::new();
        let src = add(&mut g, "test.head", 0, 1);
        let b = add(&mut g, "test.mod", 1, 1);
        let c = add(&mut g, "test.mod", 1, 1);
        g.connect(src, 0, b, 0).expect("src->b");
        g.connect(src, 0, c, 0).expect("src->c");

        let map = g.copy_subgraph(&[src, b, c]);
        let (cs, cb, cc) = (map[&src], map[&b], map[&c]);
        // One source fanning out to two destinations is preserved on both copies.
        assert_eq!(g.input_source(cb, 0), Some((cs, 0)));
        assert_eq!(g.input_source(cc, 0), Some((cs, 0)));
    }

    #[test]
    fn copy_subgraph_assigns_ids_in_source_order_regardless_of_input_order() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1); // stable_id 0
        let modr = add(&mut g, "test.mod", 1, 1); // stable_id 1

        // Pass the selection reversed; ids are still assigned by source stable_id.
        let map = g.copy_subgraph(&[modr, head]);
        assert_eq!(g.stable_id(map[&head]), Some(2));
        assert_eq!(g.stable_id(map[&modr]), Some(3));
    }

    #[test]
    fn copy_subgraph_ignores_empty_and_absent_ids() {
        let mut g = Graph::new();
        let head = add(&mut g, "test.head", 0, 1);
        assert!(g.copy_subgraph(&[]).is_empty());
        assert_eq!(g.node_count(), 1, "an empty selection copies nothing");

        let removed = add(&mut g, "test.mod", 1, 1);
        assert!(g.remove_node(removed));
        // A stale id is skipped, not panicked on; the live node still copies.
        let map = g.copy_subgraph(&[removed, head]);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&head));
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn extract_subgraph_wraps_a_chain_with_derived_ports() {
        let mut g = Graph::new();
        let feeder = add(&mut g, "test.gen", 0, 1);
        let a = add(&mut g, "test.mod", 1, 1);
        let b = add(&mut g, "test.mod", 1, 1);
        let sink = add(&mut g, "test.sink", 1, 0);
        g.connect(feeder, 0, a, 0).expect("feeder->a");
        g.connect(a, 0, b, 0).expect("a->b");
        g.connect(b, 0, sink, 0).expect("b->sink");

        // Wrap {a, b}: feeder->a is a boundary input, b->sink a boundary output.
        let extraction = g.extract_subgraph(&[a, b]).expect("extract");
        let container = extraction.container;

        // feeder, container, sink remain; a and b are gone.
        assert_eq!(g.node_count(), 3);
        assert!(g.node_id_of(g.stable_id(container).unwrap()).is_some());
        // The container has one derived input and one derived output.
        let spec = g.spec(container).expect("spec");
        assert_eq!(spec.inputs.len(), 1);
        assert_eq!(spec.outputs.len(), 1);
        // The surrounding graph is rewired through the container.
        assert_eq!(g.input_source(container, 0), Some((feeder, 0)));
        assert_eq!(g.input_source(sink, 0), Some((container, 0)));
        // Inside: the two wrapped nodes plus one input and one output marker.
        assert_eq!(g.nested(container).expect("inner").node_count(), 4);
        // The mapping reports both wrapped nodes and one marker on each side, for layout.
        assert_eq!(extraction.moved.len(), 2);
        assert_eq!(extraction.inputs.len(), 1);
        assert_eq!(extraction.outputs.len(), 1);
    }

    #[test]
    fn extract_subgraph_shares_one_output_port_for_fan_out() {
        let mut g = Graph::new();
        let feeder = add(&mut g, "test.gen", 0, 1);
        let a = add(&mut g, "test.mod", 1, 1);
        let c = add(&mut g, "test.mod", 1, 1);
        let d = add(&mut g, "test.mod", 1, 1);
        g.connect(feeder, 0, a, 0).expect("feeder->a");
        g.connect(a, 0, c, 0).expect("a->c");
        g.connect(a, 0, d, 0).expect("a->d");

        // Wrap {a}: its output fans out to c and d, which is one boundary output port.
        let container = g.extract_subgraph(&[a]).expect("extract").container;
        let spec = g.spec(container).expect("spec");
        assert_eq!(spec.inputs.len(), 1);
        assert_eq!(spec.outputs.len(), 1, "fan-out is one shared output port");
        // Both external consumers now read from the container's single output.
        assert_eq!(g.input_source(c, 0), Some((container, 0)));
        assert_eq!(g.input_source(d, 0), Some((container, 0)));
    }

    #[test]
    fn would_create_cycle_detects_loops_and_self_edges() {
        let mut g = Graph::new();
        let a = add(&mut g, "test.mod", 1, 1);
        let b = add(&mut g, "test.mod", 1, 1);
        let c = add(&mut g, "test.mod", 1, 1);
        // Chain a -> b -> c.
        g.connect(a, 0, b, 0).expect("a->b");
        g.connect(b, 0, c, 0).expect("b->c");

        // Closing c back into a would loop; so would any back-edge along the chain.
        assert!(g.would_create_cycle(c, a));
        assert!(g.would_create_cycle(b, a));
        assert!(g.would_create_cycle(c, b));
        // A self-edge is the degenerate loop.
        assert!(g.would_create_cycle(a, a));
        // Forward and sideways edges are acyclic.
        assert!(!g.would_create_cycle(a, c));
        let d = add(&mut g, "test.head", 0, 1);
        assert!(!g.would_create_cycle(d, a));
    }
}
