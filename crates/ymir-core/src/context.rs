//! Per-evaluation context handed to operators.

use crate::cancel::CancelToken;
use crate::region::Region;

/// The context an operator receives for one evaluation.
///
/// It carries the requested resolution, the region being evaluated, the
/// already-derived seed the operator should use, and a cancellation signal. It
/// deliberately does **not** carry the target endpoint: which node the evaluation
/// was requested for is the evaluator's concern, not an operator's.
#[derive(Clone, Debug)]
pub struct EvalContext {
    /// Requested grid width in cells.
    pub width: usize,
    /// Requested grid height in cells.
    pub height: usize,
    /// The world-space region being evaluated.
    pub region: Region,
    /// The seed the operator should use, already derived from the global seed and
    /// the node's stable identity by the evaluator.
    pub seed: u64,
    cancel: CancelToken,
}

impl EvalContext {
    /// Creates an evaluation context with no cancellation attached.
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

    /// Attaches a cancellation token (used by the evaluator to thread the
    /// request's token into each node's context).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Whether evaluation has been asked to cancel. Long-running operators (e.g.
    /// erosion) should poll this inside their loops and return
    /// [`Error::Cancelled`](crate::Error::Cancelled) early when it is `true`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}
