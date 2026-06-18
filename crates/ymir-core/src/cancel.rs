//! Cooperative cancellation for evaluation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cheap, clonable cancellation flag.
///
/// The GUI sets it (from any thread) when a newer change supersedes an in-flight
/// evaluation; the evaluator polls it between nodes and long-running operators
/// poll it inside their loops, aborting early with [`Error::Cancelled`]. It only
/// stops work in progress — a completed evaluation is never affected — so it does
/// not impact determinism.
///
/// [`Error::Cancelled`]: crate::Error::Cancelled
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Creates a fresh, uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Thread-safe; may be called from a different thread
    /// than the one evaluating.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Returns `true` once [`cancel`](Self::cancel) has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}
