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
use eframe::egui::emath::TSTransform;
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

/// The default full-Build resolution (square), and the value assumed for a project saved before
/// the field was persisted.
pub(crate) const DEFAULT_BUILD_RES: usize = 1024;

/// The build resolution for a project file that predates the field.
fn default_build_res() -> usize {
    DEFAULT_BUILD_RES
}

/// The preview resolution for a project file that predates the field being persisted. Reuses the
/// app-level default so a fresh project and an older file agree.
fn default_preview_res() -> usize {
    crate::PREVIEW_RES
}

/// The sea/base level (normalized height) for a fresh project, and the value assumed for a
/// project saved before the field existed. Matches the app-level default so enabling water on an
/// older project starts at a sensible level rather than the very base.
pub(crate) const DEFAULT_SEA_LEVEL: f64 = 0.3;

/// The sea level for a project file that predates the field.
fn default_sea_level() -> f64 {
    DEFAULT_SEA_LEVEL
}

/// Whether to draw the water plane for a project that predates the toggle: off, so an older
/// project opens looking as it did before water existed. A fresh project turns it on explicitly.
fn default_show_water() -> bool {
    false
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
    /// The resolution a full Build evaluates at (square). Project intent (a UE5 export wants a
    /// specific size), so it travels with the project. Defaulted on load for older files.
    #[serde(default = "default_build_res")]
    pub build_res: usize,
    /// The resolution the interactive preview evaluates at (square). A per-project working choice
    /// like `build_res`, so it travels with the project and reopens as the user left it. Defaulted
    /// on load for files saved before it was persisted.
    #[serde(default = "default_preview_res")]
    pub preview_res: usize,
    /// The sea/base level as a normalized height in `[0, 1]`: the 3D viewport draws water at it,
    /// and it feeds evaluation as base level. A world global that travels with the project.
    /// Defaulted on load for files saved before it existed.
    #[serde(default = "default_sea_level")]
    pub sea_level: f64,
    /// Whether the 3D viewport draws the water plane. Saved so a world with a configured sea
    /// reopens showing it. Defaulted off for files that predate the toggle.
    #[serde(default = "default_show_water")]
    pub show_water: bool,
    /// How the water is rendered: the effect layers and their look controls (#157). Grouped into
    /// one sub-object so it stays a tidy, git-diffable block and can move as a unit. Defaulted on
    /// load, so a project saved before it existed opens with the standard look (no format bump,
    /// like `world_height`).
    #[serde(default)]
    pub water: WaterSettings,
}

/// Default for a bool setting added after `WaterSettings` shipped (e.g. `reflection`): on.
fn default_true() -> bool {
    true
}

/// Gerstner crest steepness for a project saved before the control existed (#155).
fn default_steepness() -> f32 {
    0.6
}

/// Gerstner wavelength scale for a project saved before the control existed (#155).
fn default_wavelength() -> f32 {
    1.0
}

/// How the 3D viewport renders the water surface: which effect layers are on and their look
/// controls (#157). Travels with the project so a saved world reopens looking as it was tuned.
/// The animation *phase* is a running clock, not a setting, and is deliberately not stored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct WaterSettings {
    /// Depth-shading layer (Tier 0): Beer-Lambert extinction tints and opaques with depth.
    pub depth: bool,
    /// Gerstner wave layer (#155): geometric wave displacement. Aliased from the old `surface` key,
    /// which used to gate both waves and reflection, so older projects keep their setting.
    #[serde(alias = "surface")]
    pub waves: bool,
    /// Reflective surface finish: sky Fresnel reflection and sun specular. Split out from `surface`
    /// so it toggles independently of the waves; defaulted on for projects saved before the split.
    #[serde(default = "default_true")]
    pub reflection: bool,
    /// Shoreline foam layer.
    pub foam_on: bool,
    /// Depth falloff (Beer-Lambert extinction): higher clears to opaque faster.
    pub extinction: f32,
    /// Water tint (linear RGB).
    pub color: [f32; 3],
    /// Surface ripple strength, sky reflectivity, and specular intensity (all `0..1`).
    pub wave: f32,
    pub reflectivity: f32,
    pub specular: f32,
    /// Gerstner wave shaping (#155): crest steepness (`0..1`) and wavelength scale (a multiplier on
    /// the base wavelengths). Defaulted on load for projects saved before they existed.
    #[serde(default = "default_steepness")]
    pub steepness: f32,
    #[serde(default = "default_wavelength")]
    pub wavelength: f32,
    /// Foam amount and band width (in normalized depth).
    pub foam: f32,
    pub foam_width: f32,
    /// Animation speed multiplier for the ripples and foam; `0` freezes the surface.
    pub speed: f32,
}

impl Default for WaterSettings {
    /// The standard water look, shared with `AppState::new` so a fresh session and a project saved
    /// without water settings agree. A calm default speed, since the raw shader rates read frantic.
    fn default() -> Self {
        Self {
            depth: true,
            waves: true,
            reflection: true,
            foam_on: true,
            extinction: 5.0,
            color: [0.10, 0.28, 0.42],
            wave: 0.5,
            reflectivity: 0.6,
            specular: 0.5,
            steepness: 0.6,
            wavelength: 1.0,
            foam: 0.5,
            foam_width: 0.015,
            speed: 0.4,
        }
    }
}

/// The default frame label colour: the brand's light text, readable on the dark default
/// header. A frame saved before [`Frame::text`] existed restores to this, and a new frame
/// starts here, leaving a dark choice available for a bright header.
fn default_frame_text() -> [u8; 3] {
    [0xd6, 0xe0, 0xf0]
}

/// Where a frame's label sits. The first cut renders over the top border; modelled as an
/// enum so placement can grow without a format change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LabelPlacement {
    /// Over the top border, left-aligned (a title-bar feel).
    #[default]
    TopLeft,
    /// Over the top border, centred.
    TopCenter,
}

/// A canvas frame (#94): a labelled, translucent box drawn behind a set of nodes that
/// groups them visually and moves them together. Pure view-state, never a graph node, so
/// `ymir-core` never learns about it. Stored in [`ViewState`], persisted with the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frame {
    /// Bounds in canvas (graph) space: `[min_x, min_y, max_x, max_y]`.
    pub rect: [f32; 4],
    /// Fill colour `[r, g, b, a]`; the alpha gives the translucent tint over the grid.
    pub fill: [u8; 4],
    /// Border colour `[r, g, b]`.
    pub border: [u8; 3],
    /// Label text colour `[r, g, b]`. Defaulted (the brand's light text) so a frame saved
    /// before it existed stays readable, and so it can be set dark for a bright header.
    #[serde(default = "default_frame_text")]
    pub text: [u8; 3],
    /// The frame's text label.
    pub label: String,
    /// Where the label sits relative to the frame. Optional so a future placement value
    /// added to an entry stays backward-compatible.
    #[serde(default)]
    pub label_placement: LabelPlacement,
}

/// The saved canvas camera: the pan/zoom of the view, so a project reopens looking exactly as it
/// was left. Stored as plain data (translation and a uniform scale) rather than an egui transform,
/// and converted at the boundary. Optional on [`ViewState`]: a project saved before this existed,
/// or a graph-only file, has none, and the editor fits the graph to the screen instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Camera {
    /// Canvas translation `[x, y]`: the screen offset of the graph origin.
    pub translation: [f32; 2],
    /// Uniform zoom scale.
    pub scale: f32,
}

impl Camera {
    /// The camera as an egui view transform, for applying it to the canvas.
    pub(crate) fn to_transform(self) -> TSTransform {
        TSTransform::new(self.translation.into(), self.scale)
    }

    /// Captures an egui view transform as a saveable camera.
    pub(crate) fn from_transform(t: TSTransform) -> Self {
        Self {
            translation: [t.translation.x, t.translation.y],
            scale: t.scaling,
        }
    }
}

/// GUI view-state: where each node sits on the canvas, keyed by `stable_id`, plus the canvas
/// camera and any canvas frames.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ViewState {
    /// Canvas position `[x, y]` per node, keyed by `stable_id`. A `BTreeMap` keeps
    /// the keys ordered for clean diffs.
    pub nodes: BTreeMap<u64, [f32; 2]>,
    /// The saved canvas camera (pan/zoom). Optional and defaulted, so an older project (or a
    /// graph-only file) opens by fitting the graph to the screen instead. Not part of the undo
    /// snapshot (panning is not an edit); set only when the project is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    /// Canvas frames (#94), in creation order. Optional and defaulted, so a project saved
    /// before frames existed opens with none (no format bump, like `world_height`). Kept
    /// last so adding or moving a frame localizes its diff.
    #[serde(default)]
    pub frames: Vec<Frame>,
    /// Interior layouts of subgraph containers (#106), keyed by the container's `stable_id`,
    /// recursively mirroring the graph's nesting: each entry is the inner graph's own
    /// view-state. Only visited subgraphs appear (an unopened one cascades on first dive).
    /// Optional and defaulted, so projects without subgraphs are unchanged and the format
    /// version does not bump.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subgraphs: BTreeMap<u64, ViewState>,
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
    /// The restored full-Build resolution (square).
    pub build_res: usize,
    /// The restored interactive preview resolution (square).
    pub preview_res: usize,
    /// The restored sea/base level (normalized height).
    pub sea_level: f64,
    /// Whether the restored project draws the water plane.
    pub show_water: bool,
    /// The restored water rendering look and effect layers (#157).
    pub water: WaterSettings,
    /// The restored canvas camera (pan/zoom), if the project saved one. `None` for an older
    /// project or a graph-only file, in which case the editor fits the graph to the screen.
    pub camera: Option<TSTransform>,
    /// The restored canvas frames (#94).
    pub frames: Vec<Frame>,
    /// The restored interior layouts of subgraph containers, flattened to a path-keyed map
    /// (the container `stable_id`s from the top) for the editor's in-session layout cache.
    pub subgraph_layouts: HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
    /// Human-readable notes for anything that had to degrade on load (an unavailable node kept as
    /// a placeholder, a dropped connection). Empty on a clean load. The caller surfaces and logs
    /// these so a lossy open is never silent.
    pub warnings: Vec<String>,
}

impl ProjectFile {
    /// Captures the current session into a project file: the graph as a document,
    /// every node's canvas position from `snarl`, and the world settings.
    pub(crate) fn capture(
        graph: &Graph,
        snarl: &Snarl<Handle>,
        world: WorldSettings,
        frames: &[Frame],
    ) -> Self {
        Self::capture_with(
            graph,
            snarl_positions(snarl),
            world,
            frames,
            &HashMap::new(),
        )
    }

    /// Captures a project from a graph, an explicit top-level node-position map, and the
    /// interior layouts of its subgraphs (path-keyed by container `stable_id`s, #106).
    ///
    /// Used when diving into a subgraph: the active canvas shows the inner graph, so the
    /// top-level snapshot is built from the folded top graph and the saved top-level
    /// positions, and the subgraph interiors come from `layouts` rather than a live snarl.
    pub(crate) fn capture_with(
        graph: &Graph,
        nodes: BTreeMap<u64, [f32; 2]>,
        world: WorldSettings,
        frames: &[Frame],
        layouts: &HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
    ) -> Self {
        Self {
            format_version: PROJECT_FORMAT_VERSION,
            world,
            graph: graph.to_document(),
            view: ViewState {
                nodes,
                // The camera is not captured in the snapshot (panning is not an undoable edit);
                // it is injected only when the project is written to disk.
                camera: None,
                frames: frames.to_vec(),
                subgraphs: subgraph_view(graph, &[], layouts),
            },
        }
    }

    /// If `self` and `other` describe the same graph and world and differ in the
    /// position of exactly one node, returns that node's stable id. `None` for a
    /// semantic change (graph or world), or a layout change touching no or several nodes
    /// (an added/removed node, or a multi-node move). The undo history uses this to
    /// coalesce a run of moves to a *single* node into one step, while a move of a
    /// different node opens a fresh step (#82).
    pub(crate) fn single_moved_node(&self, other: &Self) -> Option<u64> {
        if self.world != other.world
            || self.graph != other.graph
            || self.view.frames != other.view.frames
        {
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

        let (graph, warnings) = Graph::from_document_reporting(&self.graph)?;
        let snarl = build_snarl(&graph, &self.view.nodes);

        let mut subgraph_layouts = HashMap::new();
        flatten_subgraphs(&self.view.subgraphs, &[], &mut subgraph_layouts);

        Ok(RestoredProject {
            graph,
            snarl,
            seed: self.world.seed,
            world_extent: self.world.world_extent,
            world_height: self.world.world_height,
            build_res: self.world.build_res,
            preview_res: self.world.preview_res,
            sea_level: self.world.sea_level,
            show_water: self.world.show_water,
            water: self.world.water,
            camera: self.view.camera.map(Camera::to_transform),
            frames: self.view.frames.clone(),
            subgraph_layouts,
            warnings,
        })
    }
}

/// A staggered fallback position for a node with no saved layout, so a graph-only
/// file does not stack every node on the same point.
fn cascade_pos(index: usize) -> Pos2 {
    let step = index as f32 * CASCADE_STEP;
    Pos2::new(40.0 + step, 40.0 + step)
}

/// Builds a fresh canvas snarl for `graph`, placing each node at its saved position in
/// `positions` (keyed by `stable_id`) or a cascade fallback, and reattaching every wire
/// from the graph's connections.
///
/// Shared by project restore and by diving into or out of a subgraph, which both rebuild
/// the canvas for a different graph (the inner graph, or the parent on the way back).
pub(crate) fn build_snarl(graph: &Graph, positions: &BTreeMap<u64, [f32; 2]>) -> Snarl<Handle> {
    let doc = graph.to_document();
    let mut snarl = Snarl::<Handle>::new();
    let mut snarl_of: HashMap<u64, SnarlNodeId> = HashMap::with_capacity(doc.nodes.len());
    for (index, nd) in doc.nodes.iter().enumerate() {
        let pos = positions
            .get(&nd.stable_id)
            .map_or_else(|| cascade_pos(index), |p| Pos2::new(p[0], p[1]));
        snarl_of.insert(nd.stable_id, snarl.insert_node(pos, nd.stable_id));
    }
    // The document's connections are already in stable_id terms, so no lookup into the
    // rebuilt graph is needed.
    for nd in &doc.nodes {
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
    snarl
}

/// Builds the recursive subgraph view-state for `graph` (#106): for each container node,
/// if a layout is known for its path (in `layouts`) or any deeper subgraph is, an entry
/// mirroring the inner graph's view-state. `path` is the container `stable_id`s from the
/// top to `graph`. Interior frames are not persisted yet, so each entry's `frames` is empty.
fn subgraph_view(
    graph: &Graph,
    path: &[u64],
    layouts: &HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
) -> BTreeMap<u64, ViewState> {
    let mut out = BTreeMap::new();
    for nd in &graph.to_document().nodes {
        let Some(id) = graph.node_id_of(nd.stable_id) else {
            continue;
        };
        let Some(inner) = graph.nested(id) else {
            continue; // only container nodes have an interior
        };
        let mut child_path = path.to_vec();
        child_path.push(nd.stable_id);
        let nodes = layouts.get(&child_path).cloned().unwrap_or_default();
        let nested = subgraph_view(inner, &child_path, layouts);
        // Skip a container with no known interior layout (and no nested one): it cascades
        // on first dive, and omitting it keeps the file small and the diff clean.
        if !nodes.is_empty() || !nested.is_empty() {
            out.insert(
                nd.stable_id,
                ViewState {
                    nodes,
                    // Subgraph interior cameras are not persisted yet; they fit on first dive.
                    camera: None,
                    frames: Vec::new(),
                    subgraphs: nested,
                },
            );
        }
    }
    out
}

/// Flattens a recursive subgraph view-state into a path-keyed layout map (the inverse of
/// [`subgraph_view`]), for the editor's in-session layout cache. `path` is the container
/// `stable_id`s from the top to `subgraphs`.
fn flatten_subgraphs(
    subgraphs: &BTreeMap<u64, ViewState>,
    path: &[u64],
    out: &mut HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
) {
    for (container, view) in subgraphs {
        let mut child_path = path.to_vec();
        child_path.push(*container);
        if !view.nodes.is_empty() {
            out.insert(child_path.clone(), view.nodes.clone());
        }
        flatten_subgraphs(&view.subgraphs, &child_path, out);
    }
}

/// Captures each node's canvas position from `snarl`, keyed by `stable_id`, for saving or
/// for suspending a context when diving into a subgraph.
pub(crate) fn snarl_positions(snarl: &Snarl<Handle>) -> BTreeMap<u64, [f32; 2]> {
    snarl
        .node_ids()
        .filter_map(|(snarl_id, &handle)| {
            let pos = snarl.get_node_info(snarl_id)?.pos;
            Some((handle, [pos.x, pos.y]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::add_node;

    #[test]
    fn view_state_with_nested_subgraphs_round_trips() {
        let mut inner = ViewState::default();
        inner.nodes.insert(5, [1.0, 2.0]);
        let mut view = ViewState::default();
        view.nodes.insert(0, [0.0, 0.0]);
        view.subgraphs.insert(9, inner);

        let json = serde_json::to_string(&view).expect("serialize");
        let back: ViewState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(view, back, "recursive subgraph view-state round-trips");
    }

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

        let file = ProjectFile::capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 99,
                world_extent: 4096.0,
                world_height: 800.0,
                build_res: 2048,
                preview_res: 384,
                sea_level: 0.42,
                show_water: true,
                water: WaterSettings::default(),
            },
            &[],
        );

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
        assert_eq!(restored.build_res, 2048);
        assert_eq!(restored.preview_res, 384);
        assert_eq!(restored.sea_level, 0.42);
        assert!(restored.show_water);
        // No camera was saved, so the load will fit the graph to the screen.
        assert!(restored.camera.is_none());

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
    fn saved_camera_round_trips_to_a_transform() {
        // A project that saved a camera restores that exact pan/zoom (so it reopens as left),
        // rather than falling back to fitting the graph.
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::new(0.0, 0.0)).expect("fbm");
        let mut file = ProjectFile::capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                preview_res: crate::PREVIEW_RES,
                sea_level: DEFAULT_SEA_LEVEL,
                show_water: false,
                water: WaterSettings::default(),
            },
            &[],
        );
        file.view.camera = Some(Camera {
            translation: [12.0, -34.0],
            scale: 1.5,
        });

        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);

        let t = parsed.restore().expect("restore").camera.expect("camera");
        assert_eq!((t.translation.x, t.translation.y), (12.0, -34.0));
        assert_eq!(t.scaling, 1.5);
    }

    #[test]
    fn a_graph_only_file_restores_with_cascaded_positions() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::ZERO).expect("fbm");

        // Drop the view section entirely, as a headless or fragment file would have.
        let mut file = ProjectFile::capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                preview_res: crate::PREVIEW_RES,
                sea_level: DEFAULT_SEA_LEVEL,
                show_water: false,
                water: WaterSettings::default(),
            },
            &[],
        );
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
        let mut file = ProjectFile::capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                preview_res: crate::PREVIEW_RES,
                sea_level: DEFAULT_SEA_LEVEL,
                show_water: false,
                water: WaterSettings::default(),
            },
            &[],
        );
        file.format_version = PROJECT_FORMAT_VERSION + 1;
        assert!(matches!(
            file.restore(),
            Err(ymir_core::Error::UnsupportedFormatVersion { .. })
        ));
    }

    #[test]
    fn world_height_defaults_when_absent_from_an_older_file() {
        // A version-1 project saved before world_height (and sea level) existed: its `world`
        // section has only seed and world_extent. It must still load, taking the defaults rather
        // than failing to deserialize, and open looking as it did before water existed.
        let json = r#"{
            "format_version": 1,
            "world": { "seed": 3, "world_extent": 2048.0 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize legacy file");
        assert_eq!(file.world.world_height, DEFAULT_WORLD_HEIGHT);
        assert_eq!(file.world.sea_level, DEFAULT_SEA_LEVEL);
        assert!(!file.world.show_water);
        // Water settings, added later, default in on an older file that never stored them.
        assert_eq!(file.world.water, WaterSettings::default());
        // The preview resolution, persisted later, defaults on an older file too.
        assert_eq!(file.world.preview_res, crate::PREVIEW_RES);
        let restored = file.restore().expect("restore legacy file");
        assert_eq!(restored.world_extent, 2048.0);
        assert_eq!(restored.world_height, DEFAULT_WORLD_HEIGHT);
        assert_eq!(restored.sea_level, DEFAULT_SEA_LEVEL);
        assert!(!restored.show_water);
    }

    #[test]
    fn water_surface_key_migrates_to_waves_and_reflection_defaults_on() {
        // Projects saved before the waves/reflection split stored a single `surface` bool. It must
        // load with `waves` taking that value (via the serde alias) and `reflection` defaulting on,
        // so an existing project keeps its wave setting rather than silently resetting.
        let json = r#"{
            "format_version": 1,
            "world": {
                "seed": 0, "world_extent": 2048.0,
                "water": { "depth": true, "surface": false, "foam_on": true,
                    "extinction": 5.0, "color": [0.1, 0.28, 0.42],
                    "wave": 0.5, "reflectivity": 0.6, "specular": 0.5,
                    "foam": 0.5, "foam_width": 0.015, "speed": 0.4 }
            },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize pre-split water");
        assert!(
            !file.world.water.waves,
            "old `surface: false` carries to `waves`"
        );
        assert!(file.world.water.reflection, "`reflection` defaults on");
    }

    #[test]
    fn frames_round_trip_through_json_and_restore() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::ZERO).expect("fbm");

        let frames = vec![Frame {
            rect: [10.0, 20.0, 110.0, 90.0],
            fill: [30, 39, 56, 64],
            border: [43, 54, 80],
            text: [12, 14, 20],
            label: "Generators".to_string(),
            label_placement: LabelPlacement::TopCenter,
        }];
        let file = ProjectFile::capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 1,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                preview_res: crate::PREVIEW_RES,
                sea_level: DEFAULT_SEA_LEVEL,
                show_water: false,
                water: WaterSettings::default(),
            },
            &frames,
        );

        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);
        assert_eq!(parsed.restore().expect("restore").frames, frames);
    }

    #[test]
    fn a_file_without_a_frames_field_restores_with_none() {
        // A project saved before frames existed has a `view` with only `nodes`. It must
        // still load, with no frames, rather than failing to deserialize (the additive
        // optional field, no format bump).
        let json = r#"{
            "format_version": 1,
            "world": { "seed": 0, "world_extent": 1024.0, "world_height": 256.0 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] },
            "view": { "nodes": {} }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize pre-frames file");
        assert!(file.view.frames.is_empty());
        assert!(file.restore().expect("restore").frames.is_empty());
    }
}
