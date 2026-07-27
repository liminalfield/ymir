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
use ymir_core::Graph;

use crate::canvas::Handle;

/// Spacing of the fallback cascade for a node that has no saved position (a
/// graph-only file). Kept small; the canvas frames to the graph on open anyway.
const CASCADE_STEP: f32 = 36.0;

/// The complete on-disk project: the engine's own file, with this editor's view state in its
/// view slot.
///
/// The envelope, its version, and the world settings live in `ymir-core` (#30): they describe
/// what a graph builds, so anything headless must be able to read them without knowing this
/// editor exists. What stays here is what only an editor cares about.
pub(crate) type ProjectFile = ymir_core::ProjectFile<View>;

/// The world settings, re-exported so this module still reads as the project-file module.
pub(crate) use ymir_core::WorldSettings;

/// The world height (meters that a height of `1.0` represents) for a fresh project. Roughly a
/// quarter of the default world extent, so the default world reads at natural proportions.
pub(crate) const DEFAULT_WORLD_HEIGHT: f64 = 256.0;

/// The default full-Build resolution (square) for a fresh project.
pub(crate) const DEFAULT_BUILD_RES: usize = 1024;

/// The preview resolution when a project's view section does not name one. Reuses the app-level
/// default so a fresh project and a partial file agree.
fn default_preview_res() -> usize {
    crate::PREVIEW_RES
}

/// The sea/base level (normalized height) for a fresh project. Above the very base, so enabling
/// water starts at a sensible level.
pub(crate) const DEFAULT_SEA_LEVEL: f64 = 0.3;

/// Whether to draw the water plane when a project does not say: off. A fresh project turns it on
/// explicitly, so the quiet default is the one that shows the terrain.
fn default_show_water() -> bool {
    false
}

/// Default for a water bool that a partial block leaves out (e.g. `reflection`): on.
fn default_true() -> bool {
    true
}

/// Gerstner crest steepness when a project does not say (#155).
fn default_steepness() -> f32 {
    0.6
}

/// Prevailing wave bearing when a project does not say (#251): the bearing the shader's tallest
/// component travels along, so an unspecified swell matches the fan as authored.
fn default_wave_direction() -> f32 {
    crate::viewport::WAVE_FAN_BEARING_DEG
}

/// Wave fan spread when a project does not say (#251): the fan as authored.
fn default_wave_spread() -> f32 {
    crate::viewport::WAVE_SPREAD_DEFAULT
}

/// Gerstner wavelength scale when a project does not say (#155).
fn default_wavelength() -> f32 {
    1.0
}

/// Wet-shore strength / band width when a project does not say (#156).
fn default_wet() -> f32 {
    0.35
}

fn default_wet_width() -> f32 {
    0.03
}

/// How the 3D viewport renders the water surface: which effect layers are on and their look
/// controls (#157). Travels with the project so a saved world reopens looking as it was tuned.
/// The animation *phase* is a running clock, not a setting, and is deliberately not stored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct WaterSettings {
    /// Depth-shading layer (Tier 0): Beer-Lambert extinction tints and opaques with depth.
    pub depth: bool,
    /// Gerstner wave layer (#155): geometric wave displacement.
    pub waves: bool,
    /// Reflective surface finish: sky Fresnel reflection and sun specular. Toggles independently
    /// of the waves.
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
    /// The bearing the swell travels along, in degrees, and how widely the wave components fan
    /// around it (`0` parallel, `1` the fan as authored) (#251).
    #[serde(default = "default_wave_direction")]
    pub direction: f32,
    #[serde(default = "default_wave_spread")]
    pub spread: f32,
    /// Foam amount and band width (in normalized depth).
    pub foam: f32,
    pub foam_width: f32,
    /// Wet-shore darkening (#156): on/off, strength, and band width (normalized height). Defaulted
    /// for projects saved before it existed.
    #[serde(default = "default_true")]
    pub wet_on: bool,
    #[serde(default = "default_wet")]
    pub wet: f32,
    #[serde(default = "default_wet_width")]
    pub wet_width: f32,
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
            direction: default_wave_direction(),
            spread: default_wave_spread(),
            foam: 0.5,
            foam_width: 0.015,
            wet_on: true,
            wet: 0.35,
            wet_width: 0.03,
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

/// What this editor keeps in the project file's view slot: its own settings, plus the canvas
/// layout.
///
/// Split in two because only the layout is recursive. A subgraph has its own node positions and
/// camera, so [`ViewState`] nests; it has no preview resolution or pane ordering of its own, and
/// writing one into every subgraph would be both noise in the diff and a claim that is not true.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct View {
    /// Editor settings for the project as a whole.
    #[serde(default)]
    pub settings: ViewSettings,
    /// Node positions, camera, frames, and subgraph interiors.
    #[serde(default)]
    pub canvas: ViewState,
}

/// Editor settings that travel with a project: how it is previewed and how its lists read.
///
/// None of these reach evaluation. They are what the user set up while working on this project
/// and would have to set up again on reopening it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct ViewSettings {
    /// The resolution the interactive preview evaluates at (square). A per-project working
    /// choice, so it reopens as the user left it.
    #[serde(default = "default_preview_res")]
    pub preview_res: usize,
    /// Whether the 3D viewport draws the water plane. Saved so a world with a configured sea
    /// reopens showing it.
    #[serde(default = "default_show_water")]
    pub show_water: bool,
    /// How the water is rendered: the effect layers and their look controls (#157). Grouped into
    /// one sub-object so it stays a tidy, git-diffable block and can move as a unit.
    #[serde(default)]
    pub water: WaterSettings,
    /// The node pane's ordering and row density (#281): how the user likes to read this graph,
    /// so it travels with the project. The pane's *filter* deliberately does not, since restoring
    /// one means opening a project to a list that hides most of its nodes with no memory of why.
    #[serde(default)]
    pub node_sort: crate::status::NodeSort,
    #[serde(default)]
    pub node_density: crate::status::Density,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            preview_res: default_preview_res(),
            show_water: default_show_water(),
            water: WaterSettings::default(),
            node_sort: crate::status::NodeSort::default(),
            node_density: crate::status::Density::default(),
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
    /// The restored node-pane ordering and density (#281).
    pub node_sort: crate::status::NodeSort,
    pub node_density: crate::status::Density,
    /// The restored interior layouts of subgraph containers, flattened to a path-keyed map
    /// (the container `stable_id`s from the top) for the editor's in-session layout cache.
    pub subgraph_layouts: HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
    /// Human-readable notes for anything that had to degrade on load (an unavailable node kept as
    /// a placeholder, a dropped connection). Empty on a clean load. The caller surfaces and logs
    /// these so a lossy open is never silent.
    pub warnings: Vec<String>,
}

/// Captures the current session into a project file: the graph as a document, every node's
/// canvas position from `snarl`, the world settings, and this editor's own view settings.
///
/// A free function rather than a method: the envelope is `ymir-core`'s type now, and capture is
/// this editor filling in its slot.
pub(crate) fn capture(
    graph: &Graph,
    snarl: &Snarl<Handle>,
    world: WorldSettings,
    settings: ViewSettings,
    frames: &[Frame],
) -> ProjectFile {
    capture_with(
        graph,
        snarl_positions(snarl),
        world,
        settings,
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
    settings: ViewSettings,
    frames: &[Frame],
    layouts: &HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
) -> ProjectFile {
    ProjectFile {
        format_version: ymir_core::PROJECT_FILE_VERSION,
        world,
        graph: graph.to_document(),
        view: View {
            settings,
            canvas: ViewState {
                nodes,
                // The camera is not captured in the snapshot (panning is not an undoable edit);
                // it is injected only when the project is written to disk.
                camera: None,
                frames: frames.to_vec(),
                subgraphs: subgraph_view(graph, &[], layouts),
            },
        },
    }
}

/// If `before` and `after` describe the same graph and world and differ in the
/// position of exactly one node, returns that node's stable id. `None` for a
/// semantic change (graph or world), or a layout change touching no or several nodes
/// (an added/removed node, or a multi-node move). The undo history uses this to
/// coalesce a run of moves to a *single* node into one step, while a move of a
/// different node opens a fresh step (#82).
pub(crate) fn single_moved_node(before: &ProjectFile, after: &ProjectFile) -> Option<u64> {
    if before.world != after.world
        || before.graph != after.graph
        || before.view.canvas.frames != after.view.canvas.frames
    {
        return None;
    }
    let here = &before.view.canvas.nodes;
    let there = &after.view.canvas.nodes;
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

/// Rebuilds the session from `file`: the engine graph via the registry, and the canvas with
/// each node at its saved position (or a cascade fallback) and its wires reattached from the
/// document's connections.
///
/// # Errors
///
/// Returns [`Error::UnsupportedFormatVersion`](ymir_core::Error::UnsupportedFormatVersion)
/// if the envelope version is not understood, or any error from [`Graph::from_document`].
pub(crate) fn restore(file: &ProjectFile) -> Result<RestoredProject, ymir_core::Error> {
    file.check_version()?;

    let (graph, warnings) = Graph::from_document_reporting(&file.graph)?;
    let snarl = build_snarl(&graph, &file.view.canvas.nodes);

    let mut subgraph_layouts = HashMap::new();
    flatten_subgraphs(&file.view.canvas.subgraphs, &[], &mut subgraph_layouts);

    Ok(RestoredProject {
        graph,
        snarl,
        seed: file.world.seed,
        world_extent: file.world.world_extent,
        world_height: file.world.world_height,
        build_res: file.world.build_res,
        preview_res: file.view.settings.preview_res,
        sea_level: file.world.sea_level,
        show_water: file.view.settings.show_water,
        water: file.view.settings.water,
        camera: file.view.canvas.camera.map(Camera::to_transform),
        frames: file.view.canvas.frames.clone(),
        node_sort: file.view.settings.node_sort,
        node_density: file.view.settings.node_density,
        subgraph_layouts,
        warnings,
    })
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

        let file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 99,
                world_extent: 4096.0,
                world_height: 800.0,
                build_res: 2048,
                sea_level: 0.42,
            },
            ViewSettings {
                preview_res: 384,
                show_water: true,
                ..ViewSettings::default()
            },
            &[],
        );

        // Through JSON, to exercise the real serialization path.
        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);

        let restored = restore(&parsed).expect("restore");

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
        let mut file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                sea_level: DEFAULT_SEA_LEVEL,
            },
            ViewSettings::default(),
            &[],
        );
        file.view.canvas.camera = Some(Camera {
            translation: [12.0, -34.0],
            scale: 1.5,
        });

        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);

        let t = restore(&parsed).expect("restore").camera.expect("camera");
        assert_eq!((t.translation.x, t.translation.y), (12.0, -34.0));
        assert_eq!(t.scaling, 1.5);
    }

    #[test]
    fn a_graph_only_file_restores_with_cascaded_positions() {
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::ZERO).expect("fbm");

        // Drop the view section entirely, as a headless or fragment file would have.
        let mut file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                sea_level: DEFAULT_SEA_LEVEL,
            },
            ViewSettings::default(),
            &[],
        );
        file.view.canvas.nodes.clear();

        let restored = restore(&file).expect("restore");
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
        let mut file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 0,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                sea_level: DEFAULT_SEA_LEVEL,
            },
            ViewSettings::default(),
            &[],
        );
        file.format_version = ymir_core::PROJECT_FILE_VERSION + 1;
        assert!(matches!(
            restore(&file),
            Err(ymir_core::Error::UnsupportedFormatVersion { .. })
        ));
    }

    #[test]
    fn a_version_one_project_is_rejected_rather_than_misread() {
        // Version 1 kept the world settings in the editor's own envelope. Nothing headless could
        // read them, which is why the shape changed; the file is deliberately not migrated. What
        // matters is that opening one says so, rather than parsing partially and building the
        // graph under invented settings.
        let json = r#"{
            "format_version": 1,
            "world": { "seed": 3, "world_extent": 2048.0, "world_height": 256.0,
                "build_res": 1024, "sea_level": 0.3 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize");
        assert!(matches!(
            restore(&file),
            Err(ymir_core::Error::UnsupportedFormatVersion {
                version: 1,
                expected: _
            })
        ));
    }

    #[test]
    fn a_file_with_no_view_section_opens_on_the_defaults() {
        // A graph-only file (one a headless tool wrote, or a fragment shared without layout) is a
        // valid project: the world and the graph are everything the terrain needs. It opens with
        // nodes cascaded, water off so the terrain is what shows, and the pane in its default
        // order.
        let json = r#"{
            "format_version": 2,
            "world": { "seed": 3, "world_extent": 2048.0, "world_height": 256.0,
                "build_res": 1024, "sea_level": 0.3 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize graph-only file");
        let restored = restore(&file).expect("restore");
        assert_eq!(restored.world_extent, 2048.0);
        assert_eq!(restored.sea_level, DEFAULT_SEA_LEVEL);
        assert_eq!(restored.preview_res, crate::PREVIEW_RES);
        assert!(!restored.show_water);
        assert_eq!(restored.water, WaterSettings::default());
        assert!(restored.camera.is_none());
        assert!(restored.frames.is_empty());
    }

    #[test]
    fn a_partial_water_block_fills_in_the_look_it_does_not_name() {
        // The file is meant to be hand-editable and diffable, so a `water` block that names only
        // what someone cared about must not zero the rest. #251's direction and spread in
        // particular have to land on the fan the shader was authored with, since any other value
        // silently swings or narrows the swell.
        let json = r#"{
            "depth": true, "waves": true, "foam_on": true,
            "extinction": 5.0, "color": [0.1, 0.28, 0.42],
            "wave": 0.5, "reflectivity": 0.6, "specular": 0.5,
            "foam": 0.5, "foam_width": 0.015, "speed": 0.4
        }"#;
        let water: WaterSettings = serde_json::from_str(json).expect("deserialize partial water");
        assert!(water.reflection, "`reflection` fills in on");
        assert_eq!(water.direction, crate::viewport::WAVE_FAN_BEARING_DEG);
        assert_eq!(water.spread, crate::viewport::WAVE_SPREAD_DEFAULT);
        assert_eq!(water.steepness, WaterSettings::default().steepness);
        assert_eq!(water.wavelength, WaterSettings::default().wavelength);
    }

    #[test]
    fn the_node_pane_sort_persists_but_a_project_without_it_takes_the_default() {
        // #281: how you like to read a graph travels with it. A filter deliberately does not, so
        // there is nothing here to restore for one; opening a project must never present a list
        // that hides most of its nodes.
        let mut graph = Graph::new();
        let mut snarl = Snarl::<Handle>::new();
        add_node(&mut graph, &mut snarl, "generator.fbm", Pos2::ZERO).expect("fbm");

        let mut file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 1,
                world_extent: 2048.0,
                world_height: 256.0,
                build_res: 1024,
                sea_level: 0.3,
            },
            ViewSettings::default(),
            &[],
        );
        file.view.settings.node_sort = crate::status::NodeSort::StaleFirst;
        file.view.settings.node_density = crate::status::Density::Compact;
        let json = serde_json::to_string(&file).expect("serialize");
        let back: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.view.settings.node_sort,
            crate::status::NodeSort::StaleFirst
        );
        assert_eq!(
            back.view.settings.node_density,
            crate::status::Density::Compact
        );
        let restored = restore(&back).expect("restore");
        assert_eq!(restored.node_sort, crate::status::NodeSort::StaleFirst);
        assert_eq!(restored.node_density, crate::status::Density::Compact);

        // A project whose view section says nothing about the pane opens in dependency order at
        // full density.
        let older = r#"{
            "format_version": 2,
            "world": { "seed": 0, "world_extent": 2048.0, "world_height": 256.0,
                "build_res": 1024, "sea_level": 0.3 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] },
            "view": { "canvas": { "nodes": {} } }
        }"#;
        let older: ProjectFile = serde_json::from_str(older).expect("deserialize older");
        assert_eq!(
            older.view.settings.node_sort,
            crate::status::NodeSort::Dependency
        );
        assert_eq!(
            older.view.settings.node_density,
            crate::status::Density::Comfortable
        );
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
        let file = capture(
            &graph,
            &snarl,
            WorldSettings {
                seed: 1,
                world_extent: 1024.0,
                world_height: 256.0,
                build_res: DEFAULT_BUILD_RES,
                sea_level: DEFAULT_SEA_LEVEL,
            },
            ViewSettings::default(),
            &frames,
        );

        let json = serde_json::to_string(&file).expect("serialize");
        let parsed: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, file);
        assert_eq!(restore(&parsed).expect("restore").frames, frames);
    }

    #[test]
    fn a_file_without_a_frames_field_restores_with_none() {
        // A canvas section that names only `nodes` must still load, with no frames, rather than
        // failing to deserialize. Every part of the view is individually optional, so a file
        // written by hand or by an older editor opens on the defaults for what it omits.
        let json = r#"{
            "format_version": 2,
            "world": { "seed": 0, "world_extent": 1024.0, "world_height": 256.0,
                "build_res": 1024, "sea_level": 0.3 },
            "graph": { "format_version": 1, "next_stable_id": 0, "nodes": [] },
            "view": { "canvas": { "nodes": {} } }
        }"#;
        let file: ProjectFile = serde_json::from_str(json).expect("deserialize partial view");
        assert!(file.view.canvas.frames.is_empty());
        assert!(restore(&file).expect("restore").frames.is_empty());
    }
}
