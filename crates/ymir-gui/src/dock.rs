//! The left dock: a collapsible column of project- and global-scoped panes (#106).
//!
//! Archetype 2 (see the layout note in `mount`): the dock flanks the canvas on the left, below
//! the full-width ribbon, mirroring the right inspector pane. "Left = project/global sources and
//! tools" is the scope rule; the subgraph library is its first pane, with a node outliner and a
//! build list expected later.
//!
//! A dock pane is a [`DockPane`]: a stable id, a rail icon, a title, and a mount-agnostic draw
//! function. Panes self-register with `inventory::submit!` (like the window's [`crate::PaneKind`]),
//! so adding one is a single registration and touches no dock code. The dock collapses to a narrow
//! icon rail; clicking a rail icon opens it to that pane.

use eframe::egui;

use crate::AppState;

/// A dock pane: its id, the icon shown on the collapsed rail and the open switcher, a title (the
/// rail tooltip and open-header label), and a draw function for its body. The draw function takes
/// the whole [`AppState`] so a pane can read and edit anything, exactly like a window pane.
pub(crate) struct DockPane {
    /// A stable id, used as the active-pane key in [`DockState`].
    pub id: &'static str,
    /// A Phosphor glyph for the rail button and the open switcher.
    pub icon: &'static str,
    /// A human title: the rail-button tooltip and the open header.
    pub title: &'static str,
    /// The pane body's draw function.
    pub draw: fn(&mut egui::Ui, &mut AppState),
}

inventory::collect!(DockPane);

/// All registered dock panes, sorted by id for a stable rail order (registration order is
/// unspecified). One pane today (the library); the sort keeps the rail from reshuffling as more
/// are added.
pub(crate) fn dock_panes() -> Vec<&'static DockPane> {
    let mut panes: Vec<&'static DockPane> = inventory::iter::<DockPane>().collect();
    panes.sort_by_key(|pane| pane.id);
    panes
}

/// Looks up a registered dock pane by id.
pub(crate) fn dock_pane(id: &str) -> Option<&'static DockPane> {
    inventory::iter::<DockPane>().find(|pane| pane.id == id)
}

/// The dock's open/collapsed state and which pane is active. Defaults to open with no active pane;
/// the renderer resolves an empty or stale `active` to the first registered pane, so the dock
/// always has a sensible pane to show. Persisting this across sessions is deferred (it is window
/// state, and belongs under `$XDG_STATE_HOME` per #127).
#[derive(Debug, Clone)]
pub(crate) struct DockState {
    /// Whether the dock is expanded (a full pane) or collapsed (a narrow icon rail).
    pub open: bool,
    /// The active pane's id. Empty (or naming a pane that no longer exists) resolves to the
    /// first registered pane at render time.
    pub active: String,
}

impl Default for DockState {
    fn default() -> Self {
        // Open on launch so the library is visible without a click; the switcher's collapse
        // button hides it to the rail.
        Self {
            open: true,
            active: String::new(),
        }
    }
}
