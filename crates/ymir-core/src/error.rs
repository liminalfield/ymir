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
}

/// Convenience alias for results carrying the crate [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
