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

    /// Evaluation was cancelled via a [`CancelToken`](crate::CancelToken) before
    /// it completed. The partial result is discarded; a completed evaluation is
    /// never affected, so this does not impact determinism.
    #[error("evaluation cancelled")]
    Cancelled,
}

/// Convenience alias for results carrying the crate [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
