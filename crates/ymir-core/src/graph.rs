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
            inputs: (0..input_count).map(|_| None).collect(),
            required_input_count,
            output_count,
        })
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
    /// # Errors
    ///
    /// - [`Error::UnsupportedFormatVersion`] if the document's version is not the one
    ///   this build understands.
    /// - [`Error::UnknownNodeType`] if a node's `type_id` is not in the registry.
    /// - [`Error::DuplicateStableId`] if two nodes share a `stable_id`.
    /// - [`Error::DanglingConnection`] if a connection names a source `stable_id` no
    ///   node has.
    /// - [`Error::InvalidPort`] if a connection references a port the rebuilt operator
    ///   does not have (e.g. its arity changed since the file was written).
    pub fn from_document(doc: &ProjectDocument) -> Result<Self> {
        if doc.format_version != FORMAT_VERSION {
            return Err(Error::UnsupportedFormatVersion {
                version: doc.format_version,
                expected: FORMAT_VERSION,
            });
        }

        let mut graph = Graph::new();
        // stable_id -> runtime NodeId, for resolving connection sources in pass two.
        let mut by_stable: HashMap<u64, NodeId> = HashMap::with_capacity(doc.nodes.len());
        // The runtime id of each node, parallel to `doc.nodes`, so pass two need not
        // look the destination up.
        let mut node_ids: Vec<NodeId> = Vec::with_capacity(doc.nodes.len());

        // Pass one: create every node, so a connection can resolve its source
        // regardless of node order in the file.
        for nd in &doc.nodes {
            let operator =
                crate::registry::make(&nd.type_id).ok_or_else(|| Error::UnknownNodeType {
                    type_id: nd.type_id.clone(),
                })?;
            let id = graph.add_op(operator, nd.params.clone());
            // `add_op` assigned a fresh stable_id; overwrite it with the persisted one
            // so identity (and the seed derived from it) matches the saved project.
            if let Some(node) = graph.nodes.get_mut(id) {
                node.stable_id = nd.stable_id;
                node.name = nd.name.clone();
            }
            if by_stable.insert(nd.stable_id, id).is_some() {
                return Err(Error::DuplicateStableId {
                    stable_id: nd.stable_id,
                });
            }
            node_ids.push(id);
        }

        // `add_op` bumped the counter once per node; restore the saved value so the
        // next id assigned matches the saved project.
        graph.next_stable_id = doc.next_stable_id;

        // Pass two: reapply connections by stable_id.
        for (nd, &dest) in doc.nodes.iter().zip(&node_ids) {
            for conn in &nd.connections {
                let source = *by_stable
                    .get(&conn.source)
                    .ok_or(Error::DanglingConnection {
                        source_id: conn.source,
                        dest: nd.stable_id,
                    })?;
                graph.connect(source, conn.output, dest, conn.input)?;
            }
        }

        Ok(graph)
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
                tags: &[],
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
    fn from_document_rejects_an_unknown_node_type() {
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 1,
            nodes: vec![NodeDocument {
                stable_id: 0,
                type_id: "test.nonesuch".to_string(),
                name: None,
                params: Params::new(),
                connections: Vec::new(),
            }],
        };
        assert!(matches!(
            Graph::from_document(&doc),
            Err(Error::UnknownNodeType { .. })
        ));
    }

    #[test]
    fn from_document_rejects_duplicate_stable_ids() {
        let node = |stable_id| NodeDocument {
            stable_id,
            type_id: "test.gen".to_string(),
            name: None,
            params: Params::new(),
            connections: Vec::new(),
        };
        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 2,
            nodes: vec![node(0), node(0)],
        };
        assert!(matches!(
            Graph::from_document(&doc),
            Err(Error::DuplicateStableId { stable_id: 0 })
        ));
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
    fn from_document_rejects_a_dangling_connection() {
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
            }],
        };
        assert!(matches!(
            Graph::from_document(&doc),
            Err(Error::DanglingConnection {
                source_id: 99,
                dest: 0
            })
        ));
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
