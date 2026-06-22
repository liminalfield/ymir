//! The single crate error type.
//!
//! The engine surfaces per-node failures as values rather than panicking, so
//! these are ordinary returned errors, not aborts.

use thiserror::Error;

/// Errors produced by the engine and its I/O.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// An underlying I/O failure (creating a file, writing bytes).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A layer that a node genuinely cannot proceed without was absent.
    ///
    /// Reserve this for required layers only, such as an export endpoint asked
    /// to write a field that has no `height` layer at all. Optional layers must
    /// still be read through [`Field::layer_or`](crate::Field::layer_or) and must
    /// never raise this, or the soft-layer-contract rule weakens one node at a
    /// time.
    #[error("required layer {name:?} is missing")]
    MissingLayer {
        /// The name of the absent required layer.
        name: String,
    },

    /// The PNG encoder rejected the image or failed to write it.
    #[error("PNG encoding failed: {0}")]
    PngEncode(#[from] png::EncodingError),

    /// Evaluation reached a node already on the current path: the graph has a
    /// cycle and cannot be pulled. Reported, never a panic or stack overflow.
    #[error("graph contains a cycle")]
    Cycle,

    /// A required input port had no connection at evaluation time.
    #[error("operator {type_id:?} input port {port} is not connected")]
    DisconnectedInput {
        /// The consuming node's type id.
        type_id: &'static str,
        /// The unconnected input port index.
        port: usize,
    },

    /// A connection or output index referenced a port the node does not have.
    #[error("operator {type_id:?} has no port {port}")]
    InvalidPort {
        /// The node's type id.
        type_id: &'static str,
        /// The out-of-range port index.
        port: usize,
    },

    /// A `NodeId` did not refer to a node in the graph (e.g. it was removed).
    #[error("node not found in graph")]
    NodeNotFound,

    /// A project file's format version is not one this build can load. The version
    /// is preserved so a future migration can recognize and upgrade the file.
    #[error("unsupported project format version {version} (this build expects {expected})")]
    UnsupportedFormatVersion {
        /// The version found in the file.
        version: u32,
        /// The version this build reads.
        expected: u32,
    },

    /// A loaded node named a `type_id` that is not in the registry, so its operator
    /// cannot be rebuilt (the node was removed from the build, or a plugin is absent).
    #[error("unknown node type {type_id:?}")]
    UnknownNodeType {
        /// The unrecognized type id from the file.
        type_id: String,
    },

    /// Two loaded nodes shared a `stable_id`, which must be unique. The file is
    /// corrupt or was hand-edited incorrectly.
    #[error("duplicate stable id {stable_id} in project")]
    DuplicateStableId {
        /// The id that appeared more than once.
        stable_id: u64,
    },

    /// A loaded connection named a source `stable_id` that no node in the project
    /// has, so the wire cannot be reattached.
    #[error("connection into node {dest} references missing source node {source_id}")]
    DanglingConnection {
        /// The `stable_id` of the missing source node. (Named `source_id` rather than
        /// `source` so `thiserror` does not treat it as the error's cause.)
        source_id: u64,
        /// The `stable_id` of the node whose input named it.
        dest: u64,
    },

    /// Evaluation was cancelled via a [`CancelToken`](crate::CancelToken) before
    /// it completed. The partial result is discarded; a completed evaluation is
    /// never affected, so this does not impact determinism.
    #[error("evaluation cancelled")]
    Cancelled,
}

/// Convenience alias for results carrying the crate [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
