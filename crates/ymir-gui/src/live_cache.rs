//! The editor's live evaluation cache, shared by the preview and thumbnail workers (#382).
//!
//! The two workers evaluate the same graph at the same resolution: the preview evaluates the
//! selected node's chain, and thumbnails evaluate every visible node, which includes that chain.
//! With a cache each they compute the same expensive nodes twice, concurrently, and contend for
//! cores doing it. On a real graph at a preview resolution of 1024, that was a second hydraulic
//! erosion at three and a half seconds, run alongside the first.
//!
//! One cache removes the duplication: whichever worker reaches an upstream node first computes it,
//! and the other gets a hit. That is what makes a thumbnail of an eroded node affordable at the
//! resolution it has to be evaluated at to be truthful.
//!
//! # Why one lock around a whole job
//!
//! The lock is held for the duration of an evaluation rather than per cache entry, and that is
//! deliberate. Fine-grained locking would let both workers look up the same uncached node, miss,
//! and both start computing it, which is the duplicated work this exists to prevent. Holding it
//! across the job means the second worker waits for the first and then finds the result waiting.
//!
//! The waiting worker has nothing useful to do meanwhile: its own results depend on the very node
//! the other one is computing.
//!
//! Thumbnails lock per node rather than per job, so a preview submitted mid-pass waits for one
//! node rather than for a whole graph's worth of them. The preview is what the user is looking at.

use std::sync::{Arc, Mutex, MutexGuard};

use ymir_core::EvalCache;

/// Memory budget for the live cache.
///
/// A byte budget rather than an entry count, because the two are wildly different units here: at a
/// preview resolution of 96 a layer is 36 KiB and at 1024 it is 4 MiB, so a count that bounds one
/// bounds nothing at the other. Sized to hold a working graph's worth of preview-resolution fields
/// while staying well under the build cache's gigabyte, which is evaluating at a much larger size
/// and is not competing with an interactive editor for memory.
const LIVE_MEMORY_BUDGET: usize = 512 << 20; // 512 MiB

/// A handle on the live cache, cloned to each worker.
#[derive(Clone)]
pub(crate) struct LiveCache(Arc<Mutex<EvalCache>>);

impl LiveCache {
    /// Creates the cache. One of these is made per app and cloned to the workers.
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(EvalCache::with_memory_budget(
            LIVE_MEMORY_BUDGET,
        ))))
    }

    /// Locks the cache for the duration of an evaluation.
    ///
    /// A worker that panics mid-evaluation poisons the mutex, and this recovers rather than
    /// propagating: the cache holds only memoized results keyed by content, so a poisoned one is
    /// not in an invalid state, merely one whose last insert may not have happened. Refusing to
    /// hand it back would take out the other worker too, turning one node's failure into a dead
    /// editor. A node that fails is already reported through the preview's error path.
    pub(crate) fn lock(&self) -> MutexGuard<'_, EvalCache> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{EvalRequest, Graph, Params, Region, registry};

    /// The point of the whole module: a node computed through one handle is a hit through another.
    #[test]
    fn a_clone_sees_what_the_original_cached() {
        let cache = LiveCache::new();
        let other = cache.clone();

        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let request = EvalRequest::new(32, 32, Region::UNIT, 0);

        graph
            .evaluate(id, &request, &mut cache.lock())
            .expect("evaluates");
        // Through the other handle, and with no evaluation in between: the entry is only visible
        // if the two share one cache.
        let status = graph
            .cache_status(id, &request, &other.lock())
            .expect("status");
        assert_eq!(
            status.get(&id),
            Some(&true),
            "the second handle must see the first handle's cached result"
        );
    }

    /// Recovering from poison keeps the editor alive when one worker dies.
    #[test]
    fn a_poisoned_cache_is_still_usable() {
        let cache = LiveCache::new();
        let poisoner = cache.clone();
        // Panic while holding the lock, on a thread whose death is contained.
        let died = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("a worker died mid-evaluation");
        })
        .join();
        assert!(died.is_err(), "the thread must actually have panicked");

        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let request = EvalRequest::new(16, 16, Region::UNIT, 0);
        assert!(
            graph.evaluate(id, &request, &mut cache.lock()).is_ok(),
            "the surviving worker must still be able to evaluate"
        );
    }
}
