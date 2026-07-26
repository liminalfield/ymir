//! What each node is doing during a build (#285).
//!
//! This is the *overlay*, deliberately separate from the status model in [`crate::status`]. The
//! model is derived by walking and keying the graph, which is affordable only because it happens
//! when the graph changes. Build progress changes many times a second, so it cannot go through
//! that derivation: it is a small map updated from the evaluator's channel, costing what the
//! events cost and never what the graph costs.
//!
//! A row draws its left half from the model and its trailing slot from this, which is the
//! mechanical reason the left half is fixed while a build runs. See
//! `design/node-status-and-build-monitor.md`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ymir_core::Progress;

use crate::canvas::Handle;

/// What a build is doing with one node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NodeBuild {
    /// In this build, not started.
    Queued,
    /// Running. `percent` is present only for a node that reports its own progress; the rest show
    /// the time they have been running, never an invented fraction.
    Active {
        percent: Option<u8>,
        started: Instant,
    },
    /// Ran, and how long it took.
    Done { took: Duration },
    /// Skipped: the cache already held a result still keyed to what it would produce now.
    Cached,
}

/// Every node's build state, updated from the evaluator's events.
#[derive(Debug, Default)]
pub(crate) struct BuildProgress {
    nodes: HashMap<Handle, NodeBuild>,
    /// When the build started, for the summary's elapsed time.
    started: Option<Instant>,
    /// Which nodes have finished or been skipped. A set, not a counter: an endpoint re-executes
    /// on every pull, so the same node can settle more than once and the summary counts nodes.
    settled: HashSet<Handle>,
    /// How many nodes the build expects to touch, from the cone walked at the start.
    expected: usize,
    /// How long the build took, set when it stops.
    total: Option<Duration>,
}

impl BuildProgress {
    /// Begins a build over `cone`, the nodes it is expected to touch, all of them queued.
    ///
    /// The cone is walked once here rather than inferred from events, so a node reads as queued
    /// before anything reaches it. Without that a build looks like it starts empty and fills up,
    /// which says nothing about how much is left.
    pub(crate) fn begin(&mut self, cone: &HashSet<Handle>) {
        self.nodes = cone.iter().map(|&h| (h, NodeBuild::Queued)).collect();
        self.started = Some(Instant::now());
        self.settled.clear();
        self.expected = cone.len();
        self.total = None;
    }

    /// Clears everything, so rows fall back to their idle state.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.started = None;
        self.settled.clear();
        self.expected = 0;
        self.total = None;
    }

    /// Stops the elapsed clock, keeping the per-node results on screen. A finished build should
    /// leave what happened visible, not blank the list the moment it stops.
    pub(crate) fn finish(&mut self) {
        if let Some(started) = self.started.take() {
            self.total = Some(started.elapsed());
        }
    }

    /// How long the finished build took, once it has stopped.
    pub(crate) fn total(&self) -> Option<Duration> {
        self.total
    }

    /// Whether a build's states are being shown.
    pub(crate) fn is_active(&self) -> bool {
        self.started.is_some()
    }

    /// Whether any build's states are on screen, running or finished.
    pub(crate) fn is_shown(&self) -> bool {
        !self.nodes.is_empty()
    }

    /// Applies one event. Timing is stamped here rather than in the engine: the events carry no
    /// clock readings, which keeps time out of the evaluator entirely (#283).
    pub(crate) fn apply(&mut self, event: Progress) {
        match event {
            Progress::Started { node } => {
                self.nodes.insert(
                    node,
                    NodeBuild::Active {
                        percent: None,
                        started: Instant::now(),
                    },
                );
            }
            Progress::Fraction { node, percent } => {
                // A fraction for a node not yet seen starting still means it is running.
                let started = match self.nodes.get(&node) {
                    Some(NodeBuild::Active { started, .. }) => *started,
                    _ => Instant::now(),
                };
                self.nodes.insert(
                    node,
                    NodeBuild::Active {
                        percent: Some(percent),
                        started,
                    },
                );
            }
            Progress::Finished { node } => {
                let took = match self.nodes.get(&node) {
                    Some(NodeBuild::Active { started, .. }) => started.elapsed(),
                    _ => Duration::ZERO,
                };
                self.settled.insert(node);
                self.nodes.insert(node, NodeBuild::Done { took });
            }
            Progress::Cached { node } => {
                self.settled.insert(node);
                self.nodes.insert(node, NodeBuild::Cached);
            }
        }
    }

    /// One node's build state, or `None` when this build does not touch it.
    pub(crate) fn get(&self, node: Handle) -> Option<NodeBuild> {
        self.nodes.get(&node).copied()
    }

    /// How many nodes have settled, out of how many the build expects to touch.
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.settled.len(), self.expected)
    }

    /// How long the build has been running.
    pub(crate) fn elapsed(&self) -> Option<Duration> {
        self.started.map(|t| t.elapsed())
    }
}

/// Formats a duration the way a build reads it: seconds under a minute, then `m:ss`.
pub(crate) fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else if secs >= 10 {
        format!("{secs}s")
    } else {
        // Under ten seconds the tenths are what tell you it is moving.
        format!("{:.1}s", d.as_secs_f32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cone(handles: &[Handle]) -> HashSet<Handle> {
        handles.iter().copied().collect()
    }

    #[test]
    fn a_build_starts_with_every_node_it_will_touch_queued() {
        // A build that filled up from empty would say nothing about how much is left.
        let mut progress = BuildProgress::default();
        progress.begin(&cone(&[1, 2, 3]));
        assert_eq!(progress.get(1), Some(NodeBuild::Queued));
        assert_eq!(progress.counts(), (0, 3));
        assert_eq!(
            progress.get(9),
            None,
            "a node outside the build is untouched"
        );
    }

    #[test]
    fn a_node_runs_then_settles_and_the_count_follows() {
        let mut progress = BuildProgress::default();
        progress.begin(&cone(&[1, 2]));
        progress.apply(Progress::Started { node: 1 });
        assert!(matches!(
            progress.get(1),
            Some(NodeBuild::Active { percent: None, .. })
        ));
        progress.apply(Progress::Fraction {
            node: 1,
            percent: 40,
        });
        assert!(matches!(
            progress.get(1),
            Some(NodeBuild::Active {
                percent: Some(40),
                ..
            })
        ));
        progress.apply(Progress::Finished { node: 1 });
        assert!(matches!(progress.get(1), Some(NodeBuild::Done { .. })));
        assert_eq!(progress.counts(), (1, 2));
    }

    #[test]
    fn a_reused_node_settles_without_ever_running() {
        // Memoization means most of a rebuild is skipped, and those nodes must count as settled
        // or the summary would stall at a number that never reaches its total.
        let mut progress = BuildProgress::default();
        progress.begin(&cone(&[1, 2]));
        progress.apply(Progress::Cached { node: 1 });
        progress.apply(Progress::Cached { node: 2 });
        assert_eq!(progress.get(1), Some(NodeBuild::Cached));
        assert_eq!(progress.counts(), (2, 2));
    }

    #[test]
    fn a_repeated_event_does_not_count_twice() {
        // An endpoint re-executes on every pull, so the same node can settle more than once.
        let mut progress = BuildProgress::default();
        progress.begin(&cone(&[1]));
        progress.apply(Progress::Started { node: 1 });
        progress.apply(Progress::Finished { node: 1 });
        progress.apply(Progress::Started { node: 1 });
        progress.apply(Progress::Finished { node: 1 });
        assert_eq!(progress.counts(), (1, 1), "settled is nodes, not events");
    }

    #[test]
    fn durations_read_as_a_build_does() {
        assert_eq!(format_duration(Duration::from_millis(400)), "0.4s");
        assert_eq!(format_duration(Duration::from_secs(9)), "9.0s");
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2:05");
    }
}
