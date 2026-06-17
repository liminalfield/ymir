//! Per-evaluation context handed to operators.

use crate::region::Region;

/// The context an operator receives for one evaluation.
///
/// It carries the requested resolution, the region being evaluated, and the
/// already-derived seed the operator should use. It deliberately does **not**
/// carry the target endpoint: which node the evaluation was requested for is the
/// evaluator's concern, not an operator's, so that is an argument to the
/// evaluator (step 5), not part of this context.
#[derive(Clone, Copy, Debug, PartialEq)]
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
}

impl EvalContext {
    /// Creates an evaluation context.
    #[must_use]
    pub fn new(width: usize, height: usize, region: Region, seed: u64) -> Self {
        Self {
            width,
            height,
            region,
            seed,
        }
    }
}
