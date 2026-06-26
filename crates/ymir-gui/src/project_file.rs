//! The GUI project file: one git-friendly JSON file holding everything needed to
//! reopen a session.
//!
//! It wraps `ymir-core`'s headless [`ProjectDocument`] (the engine truth: nodes,
//! params, wiring) with the view-state the engine deliberately does not know about:
//! the canvas position of each node, plus world settings (seed, world extent). Both
//! the document and the positions are keyed by the persistent `stable_id`, so a
//! reopened project lines its nodes back up exactly.
//!
//! The `view` section is optional: a graph-only file (one the headless CLI wrote, or
//! a fragment shared without layout) still opens, with nodes auto-placed in a cascade.

use std::collections::{BTreeMap, HashMap};

use eframe::egui::Pos2;
use egui_snarl::{InPinId, NodeId as SnarlNodeId, OutPinId, Snarl};
use serde::{Deserialize, Serialize};
use ymir_core::{Graph, ProjectDocument};

use crate::canvas::Handle;

/// Current GUI project-file version, distinct from the core graph document's own
/// version: the envelope (view/world sections) evolves independently of the graph
/// schema. Bumped on a breaking envelope change, paired with a migration.
pub(crate) const PROJECT_FORMAT_VERSION: u32 = 1;

/// Spacing of the fallback cascade for a node that has no saved position (a
/// graph-only file). Kept small; the canvas frames to the graph on open anyway.
const CASCADE_STEP: f32 = 36.0;

/// The complete on-disk project: the engine graph plus the GUI's view-state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProjectFile {
    /// Envelope version; see [`PROJECT_FORMAT_VERSION`].
    pub format_version: u32,
    /// World-level evaluation settings restored with the project.
    pub world: WorldSettings,
    /// The engine graph (nodes, params, wiring), `ymir-core`'s headless document.
    pub graph: ProjectDocument,
    /// Canvas view-state. Optional, so a graph-only file still loads. Kept last so
    /// layout-only edits localize their diff.
    #[serde(default)]
    pub view: ViewState,
}

/// The world height (meters that a height of `1.0` represents) assumed for a project saved
/// before the field existed, and the app-level default for a fresh project. Roughly a
/// quarter of the default world extent, so the default world reads at natural proportions.
pub(crate) const DEFAULT_WORLD_HEIGHT: f64 = 256.0;

/// The world height for a project file that predates the field (format version 1 without it).
fn default_world_height() -> f64 {
    DEFAULT_WORLD_HEIGHT
}

/// World settings restored with the project.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorldSettings {
    /// The global seed.
    pub seed: u64,
    /// World extent along x, in meters: the footprint's physical width.
    pub world_extent: f64,
    /// World height, in meters: the real elevation a height value of `1.0` represents, the
    /// vertical counterpart to `world_extent`. An interpretation of the normalized height
    /// (for display proportion and export), not an input to evaluation. Defaulted on load so
    /// projects saved before it existed still open.
    #[serde(default = "default_world_height")]
    pub world_height: f64,
}

/// GUI view-state: where each node sits on the canvas, keyed by `stable_id`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ViewState {
    /// Canvas position `[x, y]` per node, keyed by `stable_id`. A `BTreeMap` keeps
    /// the keys ordered for clean diffs.
    pub nodes: BTreeMap<u64, [f32; 2]>,
}

/// The pieces restored from a [`ProjectFile`], ready to install into the app state.
pub(crate) struct RestoredProject {
    /// The rebuilt engine graph.
    pub graph: Graph,
    /// The canvas, with nodes at their saved positions and wires reattached.
    pub snarl: Snarl<Handle>,
    /// The restored global seed.
    pub seed: u64,
    /// The restored world extent (meters).
    pub world_extent: f64,
    /// The restored world height (meters).
    pub world_height: f64,
}

impl ProjectFile {
    /// Captures the current session into a project file: the graph as a document,
    /// every node's canvas position from `snarl`, and the world settings.
    pub(crate) fn capture(
        graph: &Graph,
        snarl: &Snarl<Handle>,
        seed: u64,
        world_extent: f64,
        world_height: f64,
    ) -> Self {
        let nodes = snarl
            .node_ids()
            .filter_map(|(snarl_id, &handle)| {
                let pos = snarl.get_node_info(snarl_id)?.pos;
                Some((handle, [pos.x, pos.y]))
            })
            .collect();
        Self {
            format_version: PROJECT_FORMAT_VERSION,
            world: WorldSettings {
                seed,
                world_extent,
                world_height,
            },
            graph: graph.to_document(),
            view: ViewState { nodes },
        }
    }

    /// If `self` and `other` describe the same graph and world and differ in the
    /// position of exactly one node, returns that node's stable id. `None` for a
    /// semantic change (graph or world), or a layout change touching no or several nodes
    /// (an added/removed node, or a multi-node move). The undo history uses this to
    /// coalesce a run of moves to a *single* node into one step, while a move of a
    /// different node opens a fresh step (#82).
    pub(crate) fn single_moved_node(&self, other: &Self) -> Option<u64> {
        if self.world != other.world || self.graph != other.graph {
            return None;
        }
        let here = &self.view.nodes;
        let there = &other.view.nodes;
        if here.len() != there.len() {
            return None;
        }
        let mut moved = None;
        for (id, pos) in here {
            match there.get(id) {
                Some(other_pos) if other_pos == pos => {}
                // A differing position: the moved node, unless a second one already was.
                Some(_) => {
                    if moved.is_some() {
                        return None;
                    }
                    moved = Some(*id);
                }
                // A key present here but not there: the node sets differ, not a move.
                None => return None,
            }
        }
        moved
    }

    /// Rebuilds the session from this project file: the engine graph via the registry,
    /// and the canvas with each node at its saved position (or a cascade fallback) and
    /// its wires reattached from the document's connections.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFormatVersion`](ymir_core::Error::UnsupportedFormatVersion)
    /// if the envelope version is not understood, or any error from
    /// [`Graph::from_document`].
    pub(crate) fn restore(&self) -> Result<RestoredProject, ymir_core::Error> {
        if self.format_version != PROJECT_FORMAT_VERSION {
            return Err(ymir_core::Error::UnsupportedFormatVersion {
                version: self.format_version,
                expected: PROJECT_FORMAT_VERSION,
            });
        }

        let graph = Graph::from_document(&self.graph)?;

        // Insert every node into the canvas, recording its snarl id so the wires can
        // be reattached by stable_id below.
        let mut snarl = Snarl::<Handle>::new();
        let mut snarl_of: HashMap<u64, SnarlNodeId> =
            HashMap::with_capacity(self.graph.nodes.len());
        for (index, nd) in self.graph.nodes.iter().enumerate() {
            let pos = self
                .view
                .nodes
                .get(&nd.stable_id)
                .map_or_else(|| cascade_pos(index), |p| Pos2::new(p[0], p[1]));
            let snarl_id = snarl.insert_node(pos, nd.stable_id);
            snarl_of.insert(nd.stable_id, snarl_id);
        }

        // Reattach wires. The document's connections are already in stable_id terms,
        // so this needs no lookup into the rebuilt graph.
        for nd in &self.graph.nodes {
            let Some(&dest) = snarl_of.get(&nd.stable_id) else {
                continue;
            };
            for conn in &nd.connections {
                if let Some(&source) = snarl_of.get(&conn.source) {
                    snarl.connect(
                        OutPinId {
                            node: source,
                            output: conn.output,
                        },
                        InPinId {
                            node: dest,
                            input: conn.input,
                        },
                    );
                }
            }
        }

        Ok(RestoredProject {
            graph,
            snarl,
            seed: self.world.seed,
            world_extent: self.world.world_extent,
            world_height: self.world.world_height,
        })
    }
}

/// A staggered fallback position for a node with no saved layout, so a graph-only
/// file does not stack every node on the same point.
fn cascade_pos(index: usize) -> Pos2 {
    let step = index as f32 * CASCADE_STEP;
    Pos2::new(40.0 + step, 40.0 + step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::add_node;

    /// The snarl node id whose handle is `stable_id`.
    fn snarl_id_of(snarl: &Snarl<Handle>, stable_id: u64) -> SnarlNodeId {
        snarl
            .node_ids()
            .find(|&(_, &h)| h == stable_id)
            .map(|(id, _)| id)
            .expect("node present")
    }

    #[test]
    fn capture_restore_round_trips_graph_positions_and_world() {
        // Two real nodes wired in core, positioned on the canvas.
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        let generator = add_node(
            &mut graph,
            &mut snarl,
            "generator.fbm",
            Pos2::new(10.0, 20.0),
        )
        .expect("fbm");
        let erosion = add_node(
            &mut graph,
            &mut snarl,
            "modifier.thermal_erosion",
            Pos2::new(100.0, 200.0),
        )
        .expect("thermal");
        graph.connect(generator, 0, erosion, 0).expect("connect");

        let file = ProjectFile::capture(&graph, &snarl, 99, 4096.0, 800.0);

        // Through JSON, to exercise the real serialization path.
        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);

        let restored = parsed.restore().expect("restore");

        // Engine graph round-trips (nodes, params, wiring).
        assert_eq!(restored.graph.to_document(), graph.to_document());
        // World settings restored.
        assert_eq!(restored.seed, 99);
        assert_eq!(restored.world_extent, 4096.0);
        assert_eq!(restored.world_height, 800.0);

        // Positions restored by stable_id.
        let gen_sid = graph.stable_id(generator).expect("gen sid");
        let erosion_sid = graph.stable_id(erosion).expect("erosion sid");
        let pos_of = |snarl: &Snarl<Handle>, sid| {
            snarl
                .get_node_info(snarl_id_of(snarl, sid))
                .expect("info")
                .pos
        };
        assert_eq!(pos_of(&restored.snarl, gen_sid), Pos2::new(10.0, 20.0));
        assert_eq!(
            pos_of(&restored.snarl, erosion_sid),
            Pos2::new(100.0, 200.0)
        );

        // The wire was reattached on the canvas.
        assert_eq!(restored.snarl.wires().count(), 1);
    }

    #[test]
    fn a_graph_only_file_restores_with_cascaded_positions() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::ZERO).expect("fbm");

        // Drop the view section entirely, as a headless or fragment file would have.
        let mut file = ProjectFile::capture(&graph, &snarl, 0, 1024.0, 256.0);
        file.view.nodes.clear();

        let restored = file.restore().expect("restore");
        assert_eq!(restored.graph.node_count(), 1);
        // The lone node (stable_id 0 in a fresh graph) lands at the first cascade slot
        // rather than an undefined spot.
        let pos = restored
            .snarl
            .get_node_info(snarl_id_of(&restored.snarl, 0))
            .expect("info")
            .pos;
        assert_eq!(pos, cascade_pos(0));
    }

    #[test]
    fn restore_rejects_an_unknown_envelope_version() {
        let graph = Graph::new();
        let snarl = Snarl::<Handle>::new();
        let mut file = ProjectFile::capture(&graph, &snarl, 0, 1024.0, 256.0);
        file.format_version = PROJECT_FORMAT_VERSION + 1;
        assert!(matches!(
            file.restore(),
            Err(ymir_core::Error::UnsupportedFormatVersion { .. })
        ));
    }

    #[test]
    fn world_height_defaults_when_absent_from_an_older_file() {
        // A version-1 project saved before world_height existed: its `world` section has
        // only seed and world_extent. It must still load, taking the default height rather
        // than failing to deserialize.
        let json = r#"{
            "format_version": 1,
            "world": { "seed": 3, "world_extent": 2048.0 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize legacy file");
        assert_eq!(file.world.world_height, DEFAULT_WORLD_HEIGHT);
        let restored = file.restore().expect("restore legacy file");
        assert_eq!(restored.world_extent, 2048.0);
        assert_eq!(restored.world_height, DEFAULT_WORLD_HEIGHT);
    }
}
