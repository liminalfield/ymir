//! The node graph: nodes keyed by `NodeId`, edges stored as `NodeId`s.
//!
//! A node holds its resolved operator (behavior) plus its per-instance params and
//! input connections. Persistence (a later step) stores `type_id` and params and
//! rebuilds via the registry; the runtime graph here holds the live operator.

use slotmap::{SlotMap, new_key_type};

use crate::error::{Error, Result};
use crate::operator::Operator;
use crate::param::Params;

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
pub(crate) struct InputConn {
    pub(crate) source: NodeId,
    pub(crate) output: usize,
}

/// A node instance: behavior plus per-instance configuration and wiring.
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
    /// One slot per input port, in order; `None` is unconnected.
    pub(crate) inputs: Vec<Option<InputConn>>,
    /// Number of output ports, for connection validation.
    pub(crate) output_count: usize,
}

/// A directed graph of operator nodes.
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
        let output_count = spec.outputs.len();

        let stable_id = self.next_stable_id;
        // Monotonic and never reused, so a removed node's identity never returns.
        self.next_stable_id += 1;

        self.nodes.insert(Node {
            stable_id,
            type_id,
            operator,
            params,
            inputs: (0..input_count).map(|_| None).collect(),
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

    /// Borrows a node by id, for the evaluator.
    pub(crate) fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}
