//! Evaluation progress reporting (#283).
//!
//! The evaluator walks a graph one node at a time in dependency order and, without this, says
//! nothing while it does. A long build is then a spinner: no way to tell which node is running,
//! which were skipped because the cache already held them, or whether anything is happening at
//! all.
//!
//! A [`ProgressSink`] attached to an [`EvalRequest`](crate::EvalRequest) is told as each node
//! starts, finishes, or is served from the cache. It is **purely observational**: nothing the
//! evaluator does depends on whether a sink is attached, so an evaluation with one produces
//! byte-identical results to one without, and the determinism contract is untouched.
//!
//! Two deliberate omissions:
//!
//! - **No timing.** Events carry no durations or timestamps. The consumer stamps them as they
//!   arrive, which keeps the clock out of the engine entirely rather than relying on a rule that
//!   its readings never reach a result.
//! - **No node identity beyond `stable_id`.** A consumer should not need the graph to make sense
//!   of an event, and a runtime `NodeId` means nothing once the graph has been edited.

/// What the evaluator reports about one node, as it walks a pull.
///
/// A node produces exactly one of these per pull: `Cached` when its result was reused, or
/// `Started` followed by `Finished`. A node whose evaluation fails or is cancelled reports
/// `Started` with no `Finished`, so a consumer showing "running" must treat the end of a build as
/// closing anything still open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// The operator is about to run for this node.
    Started {
        /// The node's persistent id.
        node: u64,
    },
    /// The operator finished and its result is in hand.
    Finished {
        /// The node's persistent id.
        node: u64,
    },
    /// How far through its own work a node is, in whole percent. Only nodes that choose to
    /// report get this: a bar is a claim about progress, and a node that cannot make that claim
    /// honestly shows elapsed time instead of an invented fraction.
    ///
    /// Whole percent rather than a float so the event is cheap, comparable, and bounded: a node
    /// reports at most a hundred of these however many iterations it runs.
    Fraction {
        /// The node's persistent id.
        node: u64,
        /// Completion in percent, `0..=100`.
        percent: u8,
    },
    /// The node was not run: a cached result still keyed to what it would produce now was
    /// reused. Reported because memoization means most nodes in a rebuild are skipped, and a
    /// monitor that showed them flicking past would read as broken rather than as fast.
    Cached {
        /// The node's persistent id.
        node: u64,
    },
}

/// A sink the evaluator reports [`Progress`] to.
///
/// Implementations must be cheap and must not block: this is called from inside the evaluation
/// walk, so anything slow here slows the build it is describing. Sending on a channel is the
/// expected shape. `Send + Sync` because evaluation runs on worker threads.
pub trait ProgressSink: std::fmt::Debug + Send + Sync + 'static {
    /// Called once per event, in the order the evaluator walks the graph.
    fn report(&self, progress: Progress);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A sink that records what it was told, for tests that assert on the sequence.
    #[derive(Debug, Default)]
    pub(crate) struct Recorder(Mutex<Vec<Progress>>);

    impl ProgressSink for Recorder {
        fn report(&self, progress: Progress) {
            // A poisoned lock would mean a test thread panicked while holding it; there is
            // nothing to recover, and the panic that poisoned it is the real failure.
            if let Ok(mut events) = self.0.lock() {
                events.push(progress);
            }
        }
    }

    #[test]
    fn a_sink_records_what_it_is_told_in_order() {
        let recorder = Recorder::default();
        recorder.report(Progress::Started { node: 1 });
        recorder.report(Progress::Finished { node: 1 });
        recorder.report(Progress::Cached { node: 2 });
        let events = recorder.0.lock().expect("not poisoned").clone();
        assert_eq!(
            events,
            vec![
                Progress::Started { node: 1 },
                Progress::Finished { node: 1 },
                Progress::Cached { node: 2 },
            ]
        );
    }
}
