//! Undo/redo for the editor, kept as snapshots of the whole session.
//!
//! A snapshot is a [`ProjectFile`] (the graph, canvas node positions, and world
//! settings), the same envelope save/open uses. Undo is therefore just "swap a
//! previous snapshot back in", with no per-edit bookkeeping and no command objects to
//! keep invertible. Snapshots are small and the history is bounded, so the memory cost
//! is negligible; in exchange the model is trivially correct and reuses the
//! deterministic `to_document`/`from_document` round-trip wholesale.
//!
//! The GUI feeds the current snapshot in at a *settled* moment (the end of a drag or a
//! text edit, see the app loop), and [`EditHistory::record`] only pushes a step when the
//! snapshot actually differs from the baseline. That is what coalesces a continuous
//! interaction (a slider drag, a node move) into a single undo step rather than one per
//! frame: while the interaction is in flight the GUI does not call `record`, and when it
//! settles the one net change is captured.
//!
//! A *run* of position-only edits coalesces further (see [`EditHistory::record`]): moving
//! nodes around while thinking amends one step instead of pushing one per drop, so layout
//! fiddling does not bury the meaningful edits.

use std::collections::VecDeque;

use crate::project_file::ProjectFile;

/// Maximum number of undo steps retained. Snapshots are small, but the history is
/// bounded so a long session cannot grow it without limit; the oldest step drops first.
const MAX_HISTORY: usize = 100;

/// The editor's undo/redo history: a baseline (the current state) flanked by stacks of
/// past and undone snapshots.
pub(crate) struct EditHistory {
    /// The state the last `record`/`undo`/`redo` settled on. Undo and redo move away
    /// from this, and `record` compares against it to detect a change.
    baseline: ProjectFile,
    /// Past states, oldest at the front. `undo` takes from the back; the front is
    /// dropped when the history exceeds [`MAX_HISTORY`].
    undo: VecDeque<ProjectFile>,
    /// Undone states, for `redo`. Any fresh edit clears it (history forks).
    redo: Vec<ProjectFile>,
}

impl EditHistory {
    /// Starts a history anchored at the session's initial snapshot, with nothing to
    /// undo or redo.
    pub(crate) fn new(initial: ProjectFile) -> Self {
        Self {
            baseline: initial,
            undo: VecDeque::new(),
            redo: Vec::new(),
        }
    }

    /// Whether there is a past state to step back to.
    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is an undone state to step forward to.
    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Records `current` as the new baseline if it differs from the present one: the old
    /// baseline becomes an undo step and the redo stack is cleared (a fresh edit forks
    /// history). A no-op when nothing changed, which is what coalesces a continuous
    /// interaction into one step. Returns whether a step was recorded.
    ///
    /// Layout-only changes (a node moved, with the graph and world untouched) coalesce
    /// further: a *run* of them amends one step rather than pushing one per drop, so
    /// fiddling with node positions while thinking does not flood the history. The run
    /// still opens with a step, so a move is undoable back to where it started; a
    /// semantic edit (structure, params, world) closes the run.
    pub(crate) fn record(&mut self, current: &ProjectFile) -> bool {
        if current == &self.baseline {
            return false;
        }
        // Continue an in-progress layout run when this change touches only positions and
        // the current step does too: amend the baseline in place instead of pushing.
        let continues_layout_run = current.differs_only_in_layout(&self.baseline)
            && self
                .undo
                .back()
                .is_some_and(|previous| previous.differs_only_in_layout(&self.baseline));
        if continues_layout_run {
            self.baseline = current.clone();
        } else {
            let previous = std::mem::replace(&mut self.baseline, current.clone());
            self.undo.push_back(previous);
            if self.undo.len() > MAX_HISTORY {
                self.undo.pop_front();
            }
        }
        self.redo.clear();
        true
    }

    /// Re-anchors the baseline and clears both stacks, after the GUI installs an
    /// unrelated session (open a project, load the default) so undo does not reach back
    /// across that boundary.
    pub(crate) fn reset(&mut self, initial: ProjectFile) {
        self.baseline = initial;
        self.undo.clear();
        self.redo.clear();
    }

    /// Steps back one snapshot, returning the state to restore. The current baseline
    /// moves onto the redo stack. `None` when there is nothing to undo.
    pub(crate) fn undo(&mut self) -> Option<ProjectFile> {
        let previous = self.undo.pop_back()?;
        let current = std::mem::replace(&mut self.baseline, previous);
        self.redo.push(current);
        Some(self.baseline.clone())
    }

    /// Steps forward one snapshot, the inverse of [`undo`](Self::undo). The current
    /// baseline moves back onto the undo stack. `None` when there is nothing to redo.
    pub(crate) fn redo(&mut self) -> Option<ProjectFile> {
        let next = self.redo.pop()?;
        let current = std::mem::replace(&mut self.baseline, next);
        self.undo.push_back(current);
        Some(self.baseline.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_snarl::Snarl;
    use ymir_core::Graph;

    use crate::canvas::Handle;

    /// A distinct snapshot per `seed` (an empty graph differing only in world seed), so
    /// the tests can drive the history without building real graphs.
    fn snap(seed: u64) -> ProjectFile {
        ProjectFile::capture(&Graph::new(), &Snarl::<Handle>::new(), seed, 1024.0)
    }

    /// `base` with node 0 placed at `(x, 0)`: a layout-only variation, with the same
    /// graph and world but a different node position.
    fn moved(base: &ProjectFile, x: f32) -> ProjectFile {
        let mut snapshot = base.clone();
        snapshot.view.nodes.insert(0, [x, 0.0]);
        snapshot
    }

    #[test]
    fn record_pushes_only_on_a_real_change() {
        let mut h = EditHistory::new(snap(0));
        assert!(!h.can_undo());
        // An identical snapshot (a frame where nothing changed) records nothing.
        assert!(!h.record(&snap(0)));
        assert!(!h.can_undo());
        // A changed snapshot records one step.
        assert!(h.record(&snap(1)));
        assert!(h.can_undo());
    }

    #[test]
    fn undo_and_redo_walk_the_history() {
        let mut h = EditHistory::new(snap(0));
        h.record(&snap(1));
        h.record(&snap(2));

        assert_eq!(h.undo(), Some(snap(1)));
        assert!(h.can_redo());
        assert_eq!(h.undo(), Some(snap(0)));
        assert!(!h.can_undo());
        assert_eq!(h.undo(), None);

        assert_eq!(h.redo(), Some(snap(1)));
        assert_eq!(h.redo(), Some(snap(2)));
        assert!(!h.can_redo());
        assert_eq!(h.redo(), None);
    }

    #[test]
    fn a_fresh_edit_after_undo_clears_redo() {
        let mut h = EditHistory::new(snap(0));
        h.record(&snap(1));
        assert_eq!(h.undo(), Some(snap(0)));
        assert!(h.can_redo());
        // Editing after an undo forks history: the redo branch is dropped.
        assert!(h.record(&snap(2)));
        assert!(!h.can_redo());
    }

    #[test]
    fn reset_clears_both_stacks() {
        let mut h = EditHistory::new(snap(0));
        h.record(&snap(1));
        h.undo();
        h.reset(snap(9));
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        // The new baseline is the anchor: an identical snapshot records nothing.
        assert!(!h.record(&snap(9)));
    }

    #[test]
    fn a_run_of_layout_only_edits_coalesces_to_one_step() {
        let base = snap(0);
        let mut h = EditHistory::new(base.clone());
        // The first move opens a step; further moves amend it rather than piling up.
        assert!(h.record(&moved(&base, 1.0)));
        assert!(h.record(&moved(&base, 2.0)));
        assert!(h.record(&moved(&base, 3.0)));
        // One undo returns to the pre-fiddle layout, with nothing more to undo.
        assert_eq!(h.undo(), Some(base));
        assert!(!h.can_undo());
    }

    #[test]
    fn a_semantic_edit_closes_a_layout_run() {
        let base = snap(0);
        let mut h = EditHistory::new(base.clone());
        h.record(&moved(&base, 1.0)); // a layout step
        h.record(&snap(7)); // a semantic change (different world): a separate step
        // The two are distinct steps: the move is not folded into the semantic edit.
        assert_eq!(h.undo(), Some(moved(&base, 1.0)));
        assert_eq!(h.undo(), Some(base));
    }

    #[test]
    fn history_is_bounded() {
        let mut h = EditHistory::new(snap(0));
        for i in 1..=(MAX_HISTORY as u64 + 10) {
            h.record(&snap(i));
        }
        // The cap limits how far back undo reaches: exactly MAX_HISTORY steps, not all
        // of them, so the oldest states have been dropped.
        let mut steps = 0;
        while h.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, MAX_HISTORY);
    }
}
