//! Ymir's node editor and viewport.
//!
//! The app shell mounts self-registered panes (ribbon, canvas, parameter
//! inspector, 2D preview) over a single canonical [`Graph`]. The ribbon and
//! palette are generated from the operator and category registries; the canvas
//! ([`canvas`]) is an `egui-snarl` pure view over the graph; the inspector
//! ([`param_ui`]) edits the selected node's params; and the 2D preview
//! ([`preview`]) evaluates the selected node's output on a worker thread. All
//! display strings resolve through `tr(key)`, so this crate holds no node prose.

use std::collections::{BTreeMap, HashMap, HashSet};

use eframe::egui;
use egui_snarl::ui::SnarlWidget;
use egui_snarl::{NodeId as SnarlNodeId, Snarl};
use ymir_core::registry;
use ymir_core::{EvalRequest, Field, FieldStore, Graph, NodeId, ParamValue, Region};
use ymir_nodes::{CategoryDef, categories, find_category, tr};

// The node-editor canvas (GUI step 5, issue #6): egui-snarl as a pure view over the
// core graph, per the policy confirmed by the spike (issue #5).
mod canvas;
use canvas::Handle;
// The parameter inspector: ParamSpec-driven widgets, no per-node code.
mod param_ui;
// The visual curve editor widget (GUI step A2), rendered for ParamKind::Curve.
mod curve_edit;
// Background preview evaluation (GUI step 6b): off-thread, latest-wins.
mod preview;
use preview::PreviewEngine;

mod shade;

mod thumbnails;
use thumbnails::ThumbnailEngine;
// Off-thread full-resolution Build (#7).
mod build;
// The GUI project file: graph + canvas view-state, save/open (#75).
mod project_file;
// The built-in starter graph a fresh session opens with (#76).
mod starter;
// The Ymir Dark brand palette and egui Visuals built from it (#104).
mod theme;
// The 3D viewport: custom wgpu rendering inside an egui pane (#7).
mod viewport;
// Snapshot-based undo/redo over the session (#82).
mod history;
use build::BuildRunner;
use history::EditHistory;

/// Resolution of the interactive 2D preview. Low for responsiveness; it is an
/// approximation of the target-resolution build, never equal to it (erosion is
/// resolution-dependent). The build resolution is decided later (step 6c).
const PREVIEW_RES: usize = 256;

fn main() -> eframe::Result {
    // The window icon: `with_icon` is honoured on X11/Windows/macOS. On Wayland it is
    // ignored (no runtime icon protocol); there the icon comes from a `.desktop` entry
    // matched by `app_id`, so set a stable one. Falls back to eframe's default if the
    // PNG can't be decoded (it is cosmetic, never worth failing startup over).
    let viewport = egui::ViewportBuilder::default().with_app_id("ymir");
    let viewport = match app_icon() {
        Some(icon) => viewport.with_icon(icon),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        // A 24-bit depth buffer on egui's render pass, used by the 3D viewport for correct
        // occlusion (egui clears it to far and never writes it, so it is ours to use).
        depth_buffer: 24,
        ..Default::default()
    };
    eframe::run_native(
        "Ymir",
        options,
        Box::new(|cc| Ok(Box::new(YmirApp::new(cc)))),
    )
}

/// The application's window icon, decoded from the embedded PNG into RGBA. `None` if
/// the PNG can't be decoded (then eframe uses its default icon). winit scales this
/// single image for the titlebar/taskbar, so one size suffices.
fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../../../ymir-icon-512.png").as_slice();
    let Ok(mut reader) = png::Decoder::new(bytes).read_info() else {
        return None;
    };
    let mut buf = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return None;
    };
    buf.truncate(info.buffer_size());
    // eframe wants RGBA; the icon is RGB, so give every pixel an opaque alpha.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Some(egui::IconData {
        rgba,
        width: info.width,
        height: info.height,
    })
}

// ---- application state ------------------------------------------------------

/// Which palette tab is active: a registered category, or the synthesized
/// "Uncategorized" group for nodes whose category has no `CategoryDef`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Category(&'static str),
    Uncategorized,
}

/// Which tab of the Parameters pane is showing: the selected node's inspector, or the
/// global world/build settings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamTab {
    Node,
    World,
}

/// Build-resolution presets offered in the world settings: UE5 landscape sizes
/// (component-based, `N·63 + 1`) and plain powers of two (for texture maps). The
/// field itself accepts any custom value; these are just shortcuts.
const BUILD_RES_PRESETS: &[usize] = &[256, 505, 512, 1009, 1024, 2017, 2048, 4033, 4096, 8129];

/// A sanity cap on how many outputs one Build evaluates, so a stray graph can't spawn
/// an unbounded run. Exceeding it is reported, never silently truncated.
const MAX_BUILD_OUTPUTS: usize = 64;

/// Vertical padding above and below the menu bar, so it is not jammed against the title
/// bar or the node tabs.
const MENU_VPAD: f32 = 4.0;

/// Minimum drag (px) before a left-press on empty canvas counts as a marquee rather than
/// a click; below it, the press is the click that selects/clears (#84).
const MARQUEE_MIN_DRAG: f32 = 4.0;

/// Default physical size of the world along x, in meters. Pairs with the default
/// build resolution to give a clean 1 m/cell, and is the meters-to-cells bridge for
/// world-unit parameters (scale-aware nodes consume it via `EvalContext`).
const DEFAULT_WORLD_EXTENT: f64 = 1024.0;

/// The canvas's pan/zoom view, captured each frame so other panes (the ribbon
/// add) can place a node where the user is actually looking. The transform maps
/// the canvas's local graph space to screen; `rect` is the canvas area on screen.
/// This is the one shared "screen point to graph position" helper the node-creation
/// paths (ribbon add, future tab-menu) build on.
#[derive(Clone, Copy)]
struct CanvasView {
    to_global: egui::emath::TSTransform,
    rect: egui::Rect,
}

impl CanvasView {
    /// Maps a screen position into the canvas's local graph space.
    fn graph_pos(&self, screen: egui::Pos2) -> egui::Pos2 {
        self.to_global.inverse() * screen
    }

    /// The graph-space position at the centre of the visible canvas.
    ///
    /// A degenerate transform (an empty, not-yet-laid-out canvas) maps to a
    /// non-finite point; fall back to the screen-space centre, which is correct
    /// while the transform is the identity-like default it recovers to, and keeps
    /// placement finite (a NaN position panics egui's layout) and on the canvas.
    fn center(&self) -> egui::Pos2 {
        let mapped = self.graph_pos(self.rect.center());
        if mapped.is_finite() {
            mapped
        } else {
            self.rect.center()
        }
    }

    /// Maps a screen position into graph space, falling back to the visible centre
    /// if the transform is degenerate, so a placement is never a non-finite point
    /// (a NaN position panics egui's layout). Used to drop a node at the cursor.
    fn graph_pos_finite(&self, screen: egui::Pos2) -> egui::Pos2 {
        let mapped = self.graph_pos(screen);
        if mapped.is_finite() {
            mapped
        } else {
            self.center()
        }
    }
}

/// The data panes draw from. Panes receive `&mut AppState`, never the app shell,
/// so they stay mount-agnostic.
/// A suspended parent editing context, pushed when diving into a subgraph (#106). The
/// active context lives in [`AppState`]'s `graph`/`snarl`/`frames`/`selection`; this holds
/// what they were at the parent level so popping out can restore them and fold the edited
/// child back into the container.
struct NavFrame {
    /// The parent graph as it was when diving in. On pop, the edited child is installed
    /// back into its container here (via [`Graph::set_nested`]).
    graph: Graph,
    /// The parent's canvas node positions, to rebuild its snarl on the way back.
    positions: BTreeMap<u64, [f32; 2]>,
    /// The parent's canvas frames.
    frames: Vec<project_file::Frame>,
    /// The parent's node selection.
    selection: HashSet<Handle>,
    /// The parent's primary (inspected) node.
    primary: Option<Handle>,
    /// The parent's pinned preview node.
    preview_pin: Option<Handle>,
    /// The `stable_id` of the container node (in `graph`) that was dived into.
    container: Handle,
    /// The container's display name, for the breadcrumb.
    label: String,
}

struct AppState {
    /// The canonical graph being composed. The canvas renders it and edits flow
    /// back into it; the evaluator (step 6) runs it. When diving into a subgraph (#106)
    /// this is the *active* inner graph; the suspended parents live in `nav`.
    graph: Graph,
    /// The canvas view over `graph`: snarl holds only node handles (`stable_id`)
    /// and view-state (positions), never a copy of node data.
    snarl: Snarl<Handle>,
    /// Canvas frames (#94): labelled boxes that group nodes visually and move them
    /// together. Pure view-state, persisted with the project, never seen by `ymir-core`.
    frames: Vec<project_file::Frame>,
    /// The canvas's pan/zoom view from the last frame, for placing new nodes in
    /// view. `None` until the canvas has drawn once.
    canvas_view: Option<CanvasView>,
    /// The set of nodes selected on the canvas (their `stable_id`s), highlighted there
    /// and acted on together by Delete and the like (#84).
    selection: HashSet<Handle>,
    /// The primary (last-clicked) selected node, whose parameters the inspector edits and
    /// whose output drives the preview when no node is pinned (see `preview_pin`). Kept in
    /// sync with `selection`: it is always a member of the set, or `None` when it is empty.
    primary: Option<Handle>,
    /// The screen-space origin of an in-progress marquee box-select (a left-drag begun on
    /// empty canvas), or `None` when not marqueeing (#84).
    marquee_start: Option<egui::Pos2>,
    /// The selected node currently under the cursor during a multi-node drag (the "leader"
    /// snarl moves itself), or `None` when no group drag is in flight. The rest of the
    /// selection follows the leader by the same delta each frame, since snarl moves only the
    /// dragged node.
    group_drag_leader: Option<Handle>,
    /// The selected canvas frame, by index into `frames` (#94): the target of Delete and
    /// the frame inspector. `None` when no frame is selected.
    selected_frame: Option<usize>,
    /// An in-progress frame gesture (#94): the frame being moved or resized (and, for a
    /// move, the nodes it carries, captured at drag-start so none escape). `None` when idle.
    frame_drag: Option<FrameGesture>,
    /// Transient HSVA buffers for the selected frame's fill, border, and label-text colour
    /// pickers (`(index, fill, border, text)`, #94). The pickers edit in HSVA so the hue stays
    /// put while dragging; editing the stored `[u8; _]` re-derives the hue from RGB each frame,
    /// which jumps near gray. Seeded when the selection changes, quantized back to the frame.
    frame_color_edit: Option<(
        usize,
        egui::ecolor::Hsva,
        egui::ecolor::Hsva,
        egui::ecolor::Hsva,
    )>,
    /// The wire snarl reports as armed this frame, mirrored from the viewer each frame for
    /// wire-to-create (#123): when the node menu picks a node, this is the wire it connects.
    /// `None` when no wire is in flight; self-correcting, so a cancelled wire clears it.
    pending_wire: Option<canvas::ArmedWire>,
    /// Set after wire-to-create makes a node, to ask snarl (via the viewer) to drop the
    /// armed wire next frame so its rubber-band clears (#123). One-shot.
    consume_wire: bool,
    /// The node pinned as the preview target, if any (GUI view-state, not core graph
    /// data — issue #39). When set and still previewable, the 2D preview shows this
    /// node instead of the selection, so selection can move upstream to edit while the
    /// pinned downstream result keeps updating (the Houdini display-flag idea).
    preview_pin: Option<Handle>,
    /// The global seed for evaluation, set by the ribbon control. Reseeds the whole
    /// world; each node stays internally stable across edits.
    seed: u64,
    /// Background preview evaluation: submits graph snapshots and shows results
    /// without ever blocking the UI thread.
    preview: PreviewEngine,
    /// Background per-node thumbnail evaluation, drawn in node bodies (#42).
    thumbnails: ThumbnailEngine,
    /// Whether node thumbnails are shown, toggled from the View menu (#74). When off,
    /// no thumbnails are evaluated, uploaded, or drawn.
    thumbnails_enabled: bool,
    /// The off-thread full-resolution Build (#7).
    build: BuildRunner,
    /// The selected palette tab (defaults to the first category on first draw).
    active_tab: Option<ActiveTab>,
    /// The node-search query; when non-empty it overrides the active tab.
    search: String,
    /// The cursor-anchored node-creation menu (issue #51), open only while the user
    /// is picking a node. `None` when closed.
    node_menu: Option<NodeMenu>,
    /// The rename dialog (#61), open while the user edits a node's display name.
    /// `None` when closed.
    rename: Option<RenameDialog>,
    /// The popped-out curve editor: a larger, draggable window for shaping a curve
    /// param with room to be precise and a coordinate readout. `None` when closed.
    curve_popout: Option<CurvePopout>,
    /// A one-shot "zoom to graph" transform to apply on the next frame (#65). The
    /// fit is computed from this frame's node rects (collected during rendering) and
    /// applied via the canvas's `current_transform` override next frame.
    pending_view: Option<egui::emath::TSTransform>,
    /// Which Parameters-pane tab is showing (the node inspector or world settings).
    param_tab: ParamTab,
    /// The resolution a full Build evaluates at (square). Custom, since UE5 landscapes
    /// need specific sizes.
    build_res: usize,
    /// The interactive 2D preview resolution (square); low for responsiveness.
    preview_res: usize,
    /// Physical size of the world along x, in meters. Threaded into the Build and
    /// Preview requests so world-unit parameters resolve to cells consistently. Cells
    /// are square, so the y extent follows the grid aspect.
    world_extent: f64,
    /// Physical world height, in meters: the elevation a height value of `1.0` represents.
    /// The vertical counterpart to `world_extent`, used to show the terrain at true
    /// proportion in the viewport. An interpretation of the normalized height, not an input
    /// to evaluation, so it is not threaded into the eval request.
    world_height: f64,
    /// The project file the session is bound to, if any (#75). `Save` writes here;
    /// `None` until the project is first saved or opened, when `Save` falls back to
    /// `Save As`.
    project_path: Option<std::path::PathBuf>,
    /// Recently opened/saved projects, most recent first, capped at [`RECENT_MAX`]. Shown at
    /// the bottom of the File menu and persisted across sessions. Loaded by the app shell.
    recent: Vec<std::path::PathBuf>,
    /// A one-shot request to frame the canvas to the whole graph on the next render,
    /// set after opening a project so its layout comes into view.
    frame_to_graph_request: bool,
    /// Key of the mesh currently uploaded to the 3D viewport (field content plus the display
    /// settings that shape it), so it re-meshes only when one of those changes. `None` until
    /// the first mesh.
    viewport_mesh: Option<viewport::MeshKey>,
    /// The 3D viewport's orbit camera, persisted across frames so the view holds as the
    /// previewed node changes.
    viewport_camera: viewport::OrbitCamera,
    /// Whether the 3D viewport shows true amplitude (Fixed) or normalizes to fill the relief
    /// (Auto). Fixed by default so terrain reads at its real height.
    viewport_scale: shade::HeightScale,
    /// The 3D viewport's vertical exaggeration: a multiplier on the true world proportion
    /// (`world_height / world_extent`). `1.0` shows real-world proportions; higher values
    /// exaggerate relief to inspect subtle terrain. A non-persisted view aid.
    viewport_exaggeration: f32,
    /// The 3D viewport's sun direction and response (azimuth/elevation degrees, diffuse
    /// intensity, ambient fill). A non-persisted view aid; raking the sun low reads form.
    viewport_lighting: viewport::Lighting,
    /// The build cache's disk store (read view), opened once in the app shell so the viewport
    /// can show build-quality terrain. `None` if the cache directory is unavailable, or in the
    /// test-constructed state, which never touches the filesystem.
    field_store: Option<FieldStore>,
    /// Build-quality outputs for the shown node, loaded from the disk cache and keyed by the
    /// node's build-resolution content hash. The viewport meshes these when present (after a
    /// Build of the unchanged graph), falling back to the coarse preview field otherwise.
    /// Reloaded only when the key changes, not every frame.
    viewport_build: Option<(u64, Vec<Field>)>,
    /// A transient status line shown in the menu bar (e.g. the result of a save or
    /// open). Replaced by the next action.
    status: Option<String>,
    /// Snapshot-based undo/redo over the session (#82). Edits are recorded at a settled
    /// moment (no drag or text edit in flight), so a continuous interaction is one step.
    history: EditHistory,
    /// The session snapshot as of the last save or open: the "clean" point (#83).
    /// `modified` is whether the current session differs from it.
    saved_snapshot: project_file::ProjectFile,
    /// Whether the session has unsaved changes (the current snapshot differs from
    /// `saved_snapshot`). Recomputed at each settled frame; drives the dirty indicator
    /// and the unsaved-changes prompt.
    modified: bool,
    /// A discarding action awaiting the unsaved-changes prompt's outcome (#83). `Some`
    /// while the prompt is open; the action runs once the user chooses Save or Discard.
    pending_action: Option<PendingAction>,
    /// Set once the user has confirmed quitting (saved or discarded), so the next window
    /// close request is allowed through instead of re-raising the prompt (#83).
    allow_close: bool,
    /// The subgraph navigation stack (#106): suspended parent contexts, outermost first.
    /// Empty at the top level. Diving into a subgraph pushes the current context here and
    /// makes the inner graph active; popping folds the edited child back into its container.
    nav: Vec<NavFrame>,
    /// In-session canvas layouts for subgraph interiors, keyed by the navigation path (the
    /// container `stable_id`s from the top). Remembered on exit and restored on re-dive, so
    /// arranging a subgraph's insides is not lost when stepping out and back in. In-memory
    /// only for now; persisting it to the project file is a follow-up.
    subgraph_layouts: HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
}

/// An action that would discard unsaved changes, deferred behind the unsaved-changes
/// prompt until the user resolves it (#83).
#[derive(Clone, PartialEq, Eq, Debug)]
enum PendingAction {
    /// Open another project (prompts for a file once resolved).
    Open,
    /// Open a specific project by path (a recent-projects entry).
    OpenPath(std::path::PathBuf),
    /// Start a new, empty canvas.
    New,
    /// Open the default startup graph (the saved default, or the built-in starter).
    OpenDefault,
    /// Close the app window (quit).
    Quit,
}

/// The node-rename dialog (#61): edits a node's display-name override.
struct RenameDialog {
    /// The node being renamed (its `stable_id`).
    target: Handle,
    /// The editable name buffer, seeded from the current override.
    text: String,
    /// True only on the frame the dialog opened, so its field grabs focus once.
    just_opened: bool,
}

/// Identifies the curve param shown in the popped-out editor window: a specific node
/// and the param name on it. Tied to the node, not the current selection, so the window
/// keeps editing the same curve even after another node is selected.
#[derive(Clone)]
struct CurvePopout {
    /// The node whose curve is being edited (its `stable_id`).
    node: Handle,
    /// The curve param's name on that node.
    param: String,
}

impl AppState {
    fn new() -> Self {
        // Open with the built-in starter chain (#76), so a fresh session has a real
        // graph to preview, build, and edit rather than a blank canvas. Frame it into
        // view on the first render (the same one-shot the open path uses), so its
        // placement does not depend on the canvas's initial transform.
        let (graph, snarl) = starter::starter_graph();
        // Anchor the undo history and the clean point at this initial session, so the
        // first edit records an undo step and marks the session modified.
        let initial = project_file::ProjectFile::capture(
            &graph,
            &snarl,
            0,
            DEFAULT_WORLD_EXTENT,
            project_file::DEFAULT_WORLD_HEIGHT,
            &[],
        );
        let history = EditHistory::new(initial.clone());
        Self {
            graph,
            snarl,
            frames: Vec::new(),
            canvas_view: None,
            selection: HashSet::new(),
            primary: None,
            marquee_start: None,
            group_drag_leader: None,
            selected_frame: None,
            frame_drag: None,
            frame_color_edit: None,
            pending_wire: None,
            consume_wire: false,
            seed: 0,
            preview: PreviewEngine::new(),
            thumbnails: ThumbnailEngine::new(),
            thumbnails_enabled: true,
            build: BuildRunner::new(),
            active_tab: None,
            search: String::new(),
            node_menu: None,
            preview_pin: None,
            rename: None,
            curve_popout: None,
            pending_view: None,
            param_tab: ParamTab::Node,
            build_res: 1024,
            preview_res: PREVIEW_RES,
            world_extent: DEFAULT_WORLD_EXTENT,
            world_height: project_file::DEFAULT_WORLD_HEIGHT,
            project_path: None,
            recent: Vec::new(),
            frame_to_graph_request: false,
            viewport_mesh: None,
            viewport_camera: viewport::OrbitCamera::default(),
            viewport_scale: shade::HeightScale::Fixed,
            viewport_exaggeration: 1.0,
            // Reproduces the previous fixed key light: high from the front-right.
            viewport_lighting: viewport::Lighting {
                azimuth_deg: 35.0,
                elevation_deg: 55.0,
                intensity: 0.75,
                ambient: 0.25,
            },
            field_store: None,
            viewport_build: None,
            status: None,
            history,
            saved_snapshot: initial,
            modified: false,
            pending_action: None,
            allow_close: false,
            nav: Vec::new(),
            subgraph_layouts: HashMap::new(),
        }
    }

    /// Installs a project opened from disk (#75): swaps in the rebuilt graph, canvas,
    /// and world settings, and clears view-state that referenced the old graph's
    /// handles (selection, preview pin, open dialogs). Requests a frame-to-graph so the
    /// loaded layout is visible. The background engines pick up the new graph on the
    /// next frame, so no explicit reset is needed.
    fn install_project(
        &mut self,
        restored: project_file::RestoredProject,
        path: std::path::PathBuf,
    ) {
        self.nav.clear();
        self.subgraph_layouts.clear();
        self.graph = restored.graph;
        self.snarl = restored.snarl;
        self.frames = restored.frames;
        self.selected_frame = None;
        self.frame_drag = None;
        self.frame_color_edit = None;
        self.seed = restored.seed;
        self.world_extent = restored.world_extent;
        self.world_height = restored.world_height;
        self.clear_selection();
        self.preview_pin = None;
        self.node_menu = None;
        self.rename = None;
        self.frame_to_graph_request = true;
        // The name indicator shows which file; the status only needs the action.
        self.status = Some("Opened".to_string());
        self.project_path = Some(path);
        // Undo must not reach back across an Open into the previous project, and the
        // freshly opened session is clean.
        self.reset_history();
        self.mark_clean();
    }

    /// Replaces the session with a fresh untitled one built from `graph`/`snarl`: resets
    /// world settings and view-state, drops the project path, and re-anchors undo and the
    /// clean point. Shared by New (a starter graph) and Close (an empty graph).
    fn install_fresh(
        &mut self,
        graph: Graph,
        snarl: Snarl<Handle>,
        seed: u64,
        world_extent: f64,
        world_height: f64,
    ) {
        self.nav.clear();
        self.subgraph_layouts.clear();
        self.graph = graph;
        self.snarl = snarl;
        self.frames = Vec::new();
        self.selected_frame = None;
        self.frame_drag = None;
        self.frame_color_edit = None;
        self.seed = seed;
        self.world_extent = world_extent;
        self.world_height = world_height;
        self.clear_selection();
        self.preview_pin = None;
        self.node_menu = None;
        self.rename = None;
        self.project_path = None;
        self.frame_to_graph_request = true;
        // No status line: the visible canvas change is feedback enough, and a transient
        // "New"/"Closed" message only clutters the menu bar.
        self.status = None;
        self.reset_history();
        self.mark_clean();
    }

    /// Starts a new, empty project: a blank canvas, untitled.
    fn new_project(&mut self) {
        self.install_fresh(
            Graph::new(),
            Snarl::new(),
            0,
            DEFAULT_WORLD_EXTENT,
            project_file::DEFAULT_WORLD_HEIGHT,
        );
    }

    /// Opens the default startup graph (the saved default if one exists, else the
    /// built-in starter), untitled, the same state as on launch.
    fn open_default(&mut self) {
        let loaded = default_project_path()
            .filter(|p| p.exists())
            .and_then(|p| read_project(&p).ok());
        match loaded {
            Some(r) => self.install_fresh(r.graph, r.snarl, r.seed, r.world_extent, r.world_height),
            None => {
                let (graph, snarl) = starter::starter_graph();
                self.install_fresh(
                    graph,
                    snarl,
                    0,
                    DEFAULT_WORLD_EXTENT,
                    project_file::DEFAULT_WORLD_HEIGHT,
                );
            }
        }
    }

    /// A snapshot of the current session (graph, canvas positions, world settings),
    /// the unit the undo history and the project file both work in.
    ///
    /// Always the effective *top-level* project, even when diving into a subgraph: the
    /// active inner graph is folded back up through `nav`, and the top-level positions and
    /// frames come from the outermost suspended context. So save, dirty tracking, and undo
    /// all operate on the whole project regardless of how deep the user is editing.
    fn snapshot(&self) -> project_file::ProjectFile {
        if self.nav.is_empty() {
            return project_file::ProjectFile::capture(
                &self.graph,
                &self.snarl,
                self.seed,
                self.world_extent,
                self.world_height,
                &self.frames,
            );
        }
        let top = self.top_graph();
        project_file::ProjectFile::capture_with(
            &top,
            self.nav[0].positions.clone(),
            self.seed,
            self.world_extent,
            self.world_height,
            &self.nav[0].frames,
        )
    }

    /// The effective top-level graph: the active graph if at the top, otherwise the active
    /// inner graph folded back up through every suspended parent. The build runs this so it
    /// always produces the whole project, not whatever subgraph is open.
    fn top_graph(&self) -> Graph {
        if self.nav.is_empty() {
            return self.graph.clone();
        }
        // The fold only fails if a container vanished from a parent snapshot, which cannot
        // happen here; fall back to the outermost parent rather than panic.
        fold_to_top(self.graph.clone(), &self.nav).unwrap_or_else(|_| self.nav[0].graph.clone())
    }

    /// The current navigation path: the `stable_id`s of the containers dived through, from
    /// the top. Empty at the top level; identifies the active context for layout memory.
    fn current_path(&self) -> Vec<u64> {
        self.nav.iter().map(|frame| frame.container).collect()
    }

    /// Dives into a subgraph node to edit its inner graph: suspends the current context onto
    /// `nav` and makes the inner graph active, rebuilding the canvas for it (#106). A no-op
    /// if `handle` is not a container.
    fn dive_in(&mut self, handle: Handle) {
        let Some(id) = self.graph.node_id_of(handle) else {
            return;
        };
        let Some(inner) = self.graph.nested(id).cloned() else {
            return; // not a container
        };
        let label = node_display_name(&self.graph, id);
        let mut child_path = self.current_path();
        child_path.push(handle);

        self.nav.push(NavFrame {
            positions: project_file::snarl_positions(&self.snarl),
            frames: std::mem::take(&mut self.frames),
            selection: std::mem::take(&mut self.selection),
            primary: self.primary.take(),
            preview_pin: self.preview_pin.take(),
            container: handle,
            label,
            graph: std::mem::replace(&mut self.graph, inner),
        });

        let positions = self
            .subgraph_layouts
            .get(&child_path)
            .cloned()
            .unwrap_or_default();
        self.snarl = project_file::build_snarl(&self.graph, &positions);
        self.reset_canvas_transients();
        self.frame_to_graph_request = true;
    }

    /// Pops one level out of the current subgraph: remembers its layout, installs the edited
    /// inner graph back into its container, and restores the parent context (#106). A no-op
    /// at the top level.
    fn exit_subgraph(&mut self) {
        let Some(frame) = self.nav.pop() else {
            return;
        };
        // Remember this context's layout (keyed by its full path) for an in-session re-dive.
        let mut active_path: Vec<u64> = self.nav.iter().map(|f| f.container).collect();
        active_path.push(frame.container);
        self.subgraph_layouts
            .insert(active_path, project_file::snarl_positions(&self.snarl));

        let child = std::mem::replace(&mut self.graph, frame.graph);
        if let Some(container_id) = self.graph.node_id_of(frame.container) {
            // The container is present (it is the node we dived into), so this cannot fail;
            // on the impossible error keep the parent without the child's edits, not a panic.
            if self.graph.set_nested(container_id, child).is_err() {
                self.status = Some("Could not save subgraph edits".to_string());
            }
        }
        self.snarl = project_file::build_snarl(&self.graph, &frame.positions);
        self.frames = frame.frames;
        self.selection = frame.selection;
        self.primary = frame.primary;
        self.preview_pin = frame.preview_pin;
        self.reset_canvas_transients();
    }

    /// Pops out until the navigation stack is `depth` deep, the action behind a breadcrumb
    /// click (depth 0 is the top level). A no-op when already at or above `depth`.
    fn exit_to(&mut self, depth: usize) {
        while self.nav.len() > depth {
            self.exit_subgraph();
        }
    }

    /// Clears transient canvas interaction state on a context switch, so an in-flight
    /// gesture or open popup does not carry across into a different graph. Selection,
    /// primary, and the preview pin are handled separately (cleared on dive, restored on
    /// exit), so they are not touched here.
    fn reset_canvas_transients(&mut self) {
        self.selected_frame = None;
        self.frame_drag = None;
        self.frame_color_edit = None;
        self.group_drag_leader = None;
        self.marquee_start = None;
        self.pending_wire = None;
        self.consume_wire = false;
        self.node_menu = None;
        self.rename = None;
    }

    /// Re-anchors the undo history at the current session and clears its stacks, after
    /// the session is replaced wholesale (open a project, load the default).
    fn reset_history(&mut self) {
        let snapshot = self.snapshot();
        self.history.reset(snapshot);
    }

    /// Marks the session saved: the current snapshot becomes the clean point, so it
    /// reads as unmodified until the next edit (#83). Called on save, open, and default.
    fn mark_clean(&mut self) {
        self.saved_snapshot = self.snapshot();
        self.modified = false;
    }

    /// Restores a session snapshot from undo/redo: swaps in its graph, canvas, and world
    /// settings, keeping the current pan/zoom (no reframe) and dropping any selection or
    /// pin that the restored graph no longer contains. The history baseline already
    /// tracks this snapshot, so the end-of-frame record sees no change.
    fn apply_snapshot(&mut self, snapshot: &project_file::ProjectFile) {
        match snapshot.restore() {
            Ok(restored) => {
                // Undo/redo restores the whole (top-level) project, so step back out of any
                // subgraph rather than leave a stale navigation stack over a new graph.
                self.nav.clear();
                self.subgraph_layouts.clear();
                self.graph = restored.graph;
                self.snarl = restored.snarl;
                self.frames = restored.frames;
                self.selected_frame = None;
                self.frame_drag = None;
                self.frame_color_edit = None;
                self.seed = restored.seed;
                self.world_extent = restored.world_extent;
                self.world_height = restored.world_height;
                self.selection
                    .retain(|&h| self.graph.node_id_of(h).is_some());
                self.primary = self.primary.filter(|h| self.selection.contains(h));
                self.preview_pin = self
                    .preview_pin
                    .filter(|&h| self.graph.node_id_of(h).is_some());
                self.node_menu = None;
                self.rename = None;
            }
            // A snapshot we captured ourselves cannot fail to restore; surface it rather
            // than swallow it if the impossible happens.
            Err(err) => self.status = Some(format!("Undo failed: {err}")),
        }
    }

    /// Steps the session back one undo step, if any.
    fn undo(&mut self) {
        if let Some(snapshot) = self.history.undo() {
            self.apply_snapshot(&snapshot);
            self.status = Some("Undo".to_string());
        }
    }

    /// Steps the session forward one redo step, if any.
    fn redo(&mut self) {
        if let Some(snapshot) = self.history.redo() {
            self.apply_snapshot(&snapshot);
            self.status = Some("Redo".to_string());
        }
    }

    /// Records an undo step at a settled moment. Called once per frame after the panes
    /// have applied this frame's edits. While a pointer drag or a text edit is in flight
    /// the snapshot changes every frame, and recording then would make one step per
    /// frame; holding until the interaction settles coalesces it into a single step.
    /// `record` is a no-op when nothing changed, so a settled frame with no edit (or one
    /// just after an undo) costs only a comparison. This also catches keyboard-only edits,
    /// which never register as a pointer interaction. (If the per-settled-frame snapshot
    /// ever shows up in a profile on large graphs, gate it on a cheap content hash.)
    fn sync_history(&mut self, ctx: &egui::Context) {
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.egui_wants_keyboard_input();
        if !busy {
            let snapshot = self.snapshot();
            // Dirty state is a comparison to the clean point, so undoing back to the saved
            // state clears the modified flag (#83).
            self.modified = snapshot != self.saved_snapshot;
            self.history.record(&snapshot);
        }
    }

    /// Whether `handle` resolves to a live node that produces an output, so it can be
    /// previewed (an endpoint or a deleted node cannot).
    fn is_previewable(&self, handle: Handle) -> bool {
        self.graph
            .node_id_of(handle)
            .and_then(|id| self.graph.spec(id))
            .is_some_and(|spec| !spec.outputs.is_empty())
    }

    /// Selects exactly `handle`, replacing any prior selection, and makes it primary.
    fn select_only(&mut self, handle: Handle) {
        self.selection.clear();
        self.selection.insert(handle);
        self.primary = Some(handle);
        // Node and frame selection are mutually exclusive (#94): the inspector shows one.
        self.selected_frame = None;
    }

    /// Toggles `handle` in the selection (Ctrl-click): adding it makes it primary,
    /// removing it moves primary to another selected node or clears it when empty.
    fn toggle_selection(&mut self, handle: Handle) {
        self.selected_frame = None;
        if self.selection.remove(&handle) {
            if self.primary == Some(handle) {
                self.primary = self.selection.iter().copied().next();
            }
        } else {
            self.selection.insert(handle);
            self.primary = Some(handle);
        }
    }

    /// Clears the selection.
    fn clear_selection(&mut self) {
        self.selection.clear();
        self.primary = None;
    }

    /// Sets the selection to `hits` (a marquee result), or adds them to it when `additive`
    /// (#84). The primary stays if still selected, else becomes the last hit.
    fn marquee_select(&mut self, hits: &[Handle], additive: bool) {
        if !additive {
            self.selection.clear();
        }
        self.selection.extend(hits.iter().copied());
        if self.primary.is_none_or(|p| !self.selection.contains(&p)) {
            self.primary = hits
                .last()
                .copied()
                .or_else(|| self.selection.iter().copied().next());
        }
    }

    /// Selects every node in the graph (Ctrl/Cmd-A). Keeps the current primary if it is
    /// still a node, otherwise picks an arbitrary one.
    fn select_all(&mut self) {
        self.selection = self.snarl.node_ids().map(|(_, &h)| h).collect();
        if self.primary.is_none_or(|p| !self.selection.contains(&p)) {
            self.primary = self.selection.iter().copied().next();
        }
    }

    /// Deletes every selected node from the graph and canvas (the Delete key), then
    /// clears the selection.
    fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        // Collect the selected nodes' snarl ids first, since removing mutates the snarl.
        let ids: Vec<SnarlNodeId> = self
            .snarl
            .node_ids()
            .filter(|(_, h)| self.selection.contains(*h))
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            canvas::remove_snarl_node(&mut self.graph, &mut self.snarl, id);
        }
        self.clear_selection();
    }

    /// The node whose output the 2D preview shows: the pinned node when one is set
    /// and still previewable, otherwise the primary selected node. Decouples the preview
    /// target from selection (#39).
    fn preview_target(&self) -> Option<Handle> {
        self.preview_pin
            .filter(|&h| self.is_previewable(h))
            .or_else(|| self.primary.filter(|&h| self.is_previewable(h)))
            .or_else(|| self.preview_sink())
    }

    /// The graph's natural "result" node: a previewable node whose output feeds nothing else
    /// (a sink). With several such sinks, the last-added one (highest stable id) wins,
    /// matching "the last node in the graph". `None` for a graph with no previewable sink (an
    /// empty graph, or one whose only sinks are endpoints). Used as the preview target when
    /// nothing is pinned or selected, so a freshly opened or launched graph shows its result
    /// instead of a blank preview and a flat 3D viewport.
    fn preview_sink(&self) -> Option<Handle> {
        // A node whose output is read by some input port is consumed, so it is not a sink.
        let mut consumed: HashSet<NodeId> = HashSet::new();
        for (_, &handle) in self.snarl.node_ids() {
            let Some(id) = self.graph.node_id_of(handle) else {
                continue;
            };
            let Some(spec) = self.graph.spec(id) else {
                continue;
            };
            for port in 0..spec.inputs.len() {
                if let Some((source, _)) = self.graph.input_source(id, port) {
                    consumed.insert(source);
                }
            }
        }
        self.snarl
            .node_ids()
            .filter_map(|(_, &handle)| {
                let id = self.graph.node_id_of(handle)?;
                (self.is_previewable(handle) && !consumed.contains(&id)).then_some(handle)
            })
            .max()
    }

    /// Evaluates the current preview target once per frame, regardless of which pane or tab is
    /// visible. The 3D viewport meshes the preview's output, so without this the viewport froze
    /// whenever the 2D preview pane was hidden, most visibly when switching to the World tab to
    /// change world settings: evaluation used to run only inside that pane. A no-op when no
    /// node is previewable.
    fn drive_preview(&mut self, ctx: &egui::Context) {
        let Some(id) = self.preview_target().and_then(|h| self.graph.node_id_of(h)) else {
            return;
        };
        let res = self.preview_res;
        let request = EvalRequest::new(res, res, Region::UNIT, self.seed)
            .with_world_extent(self.world_extent)
            .with_world_height(self.world_height);
        let now = ctx.input(|i| i.time);
        self.preview.sync(&self.graph, id, request, now);
        self.preview.poll(ctx);
    }
}

/// The cursor-anchored node-creation menu (issue #51): opened with Space over the
/// canvas, it drops the chosen node at the cursor. An empty search browses the
/// categories (drilling into one to list its nodes); a non-empty search shows flat
/// results across all categories.
struct NodeMenu {
    /// Where the menu opened, in screen space; the created node lands here.
    anchor: egui::Pos2,
    /// The canvas view captured at open time, giving the screen-to-graph mapping for
    /// placement (stable even if the canvas re-lays-out while the menu is open).
    view: CanvasView,
    /// The filter query; non-empty overrides category browsing with flat results.
    search: String,
    /// The category drilled into; `None` is the top-level category list.
    drilled: Option<&'static str>,
    /// The keyboard-highlighted row (index into the current row list). Arrow keys
    /// move it; Enter activates it. Clamped to the row list each frame.
    highlight: usize,
    /// Set when the search field should grab keyboard focus next frame (on open and
    /// after a drill, so typing keeps working without a click).
    focus_search: bool,
    /// The wire to connect when a node is picked, snapshotted at open time for
    /// wire-to-create (#123): the armed wire (Space path) or the dropped wire (drop path).
    /// `None` for a plain create. Holds snarl ids; the source pin outlives the menu.
    pending_wire: Option<canvas::ArmedWire>,
}

// ---- palette: registry-driven node listing (pure, testable) -----------------

/// A node available in the palette, projected from its `NodeSpec`. All fields are
/// `'static` (they come from the spec's static strings); the display name is
/// resolved on demand via [`tr`] so the data layer stays prose-free.
struct NodeEntry {
    type_id: &'static str,
    category: &'static str,
}

/// Every registered operator, projected to a [`NodeEntry`]. Built from the
/// operator registry, never a hand-kept list.
fn node_entries() -> Vec<NodeEntry> {
    registry::entries()
        .map(|entry| {
            let spec = (entry.make)().spec();
            NodeEntry {
                type_id: spec.type_id,
                category: spec.category,
            }
        })
        .collect()
}

/// The registered categories, sorted by `sort` then `id` for a stable palette.
fn categories_sorted() -> Vec<&'static CategoryDef> {
    let mut cats: Vec<_> = categories().collect();
    cats.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.id.cmp(b.id)));
    cats
}

/// Whether any operator declares a category with no registered `CategoryDef`
/// (which the palette shows under an "Uncategorized" tab).
fn has_uncategorized_nodes() -> bool {
    node_entries()
        .iter()
        .any(|e| find_category(e.category).is_none())
}

/// How well a node's display name matched a search query, best variant first. A prefix
/// outranks a mid-word hit, so typing the start of a name surfaces it first (#91).
/// `derive(Ord)` ranks by declaration order, so sorting ascending puts the best first.
/// Search is over the display name only; nodes carry no search tags (#92).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum MatchRank {
    Exact,
    Prefix,
    Substring,
}

/// The best rank at which `entry`'s display name matches the lowercased `query`, or
/// `None` if it does not match.
fn match_rank(entry: &NodeEntry, query: &str) -> Option<MatchRank> {
    let name = tr(&format!("node-{}", entry.type_id)).to_lowercase();
    if name == query {
        Some(MatchRank::Exact)
    } else if name.starts_with(query) {
        Some(MatchRank::Prefix)
    } else if name.contains(query) {
        Some(MatchRank::Substring)
    } else {
        None
    }
}

/// The nodes shown for the current tab/search selection.
fn visible_nodes<'a>(
    entries: &'a [NodeEntry],
    tab: Option<ActiveTab>,
    search: &str,
) -> Vec<&'a NodeEntry> {
    let query = search.trim().to_lowercase();
    if query.is_empty() {
        entries
            .iter()
            .filter(|e| match tab {
                Some(ActiveTab::Category(id)) => e.category == id,
                Some(ActiveTab::Uncategorized) => find_category(e.category).is_none(),
                None => false,
            })
            .collect()
    } else {
        // Rank every match, then sort best-first, breaking ties by display name so the
        // order is stable rather than registry-dependent (#91).
        let mut matched: Vec<(MatchRank, String, &NodeEntry)> = entries
            .iter()
            .filter_map(|e| {
                match_rank(e, &query)
                    .map(|rank| (rank, tr(&format!("node-{}", e.type_id)).to_lowercase(), e))
            })
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        matched.into_iter().map(|(_, _, e)| e).collect()
    }
}

/// One selectable line in the node menu. A flat row list is the single model both
/// the mouse and the keyboard act on, so a highlight index and a click hit the same
/// rows. Each variant carries only `'static` ids, so the list outlives the borrow of
/// the menu state it was built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuRow {
    /// Returns from a drilled-in category to the category list.
    Back,
    /// A category to drill into, by id.
    Category(&'static str),
    /// A node to create, by `type_id`.
    Node(&'static str),
}

/// The rows the node menu shows, in display order. A non-empty search wins (flat
/// node results across all categories); otherwise a drilled-in category lists a
/// `Back` row then its nodes, and the top level lists the categories. Pure, so the
/// navigation logic is unit-tested apart from the egui drawing.
fn menu_rows(entries: &[NodeEntry], search: &str, drilled: Option<&'static str>) -> Vec<MenuRow> {
    if !search.trim().is_empty() {
        visible_nodes(entries, None, search)
            .iter()
            .map(|e| MenuRow::Node(e.type_id))
            .collect()
    } else if let Some(cat) = drilled {
        std::iter::once(MenuRow::Back)
            .chain(
                visible_nodes(entries, Some(ActiveTab::Category(cat)), "")
                    .iter()
                    .map(|e| MenuRow::Node(e.type_id)),
            )
            .collect()
    } else {
        categories_sorted()
            .iter()
            .map(|c| MenuRow::Category(c.id))
            .collect()
    }
}

/// The top-level row index of the category `id`, used to land the highlight on the
/// category just left when stepping back out of it (#89). The top level lists
/// `categories_sorted()` in order, so a category's position there is its row. Falls
/// back to the first row if the id is unknown.
fn category_row_index(id: &str) -> usize {
    categories_sorted()
        .iter()
        .position(|c| c.id == id)
        .unwrap_or(0)
}

/// The display text of a menu row: a bare category or node name, or the Back
/// affordance. A category's disclosure caret is *painted* by [`menu_row`] at the row's
/// right edge rather than baked into the text, so the carets line up in a column (#93).
/// Back leads with a Phosphor left caret so it reads as "up a level" and unlike a node row.
fn menu_row_text(row: MenuRow) -> String {
    match row {
        MenuRow::Back => format!("{}  Back", egui_phosphor::regular::CARET_LEFT),
        MenuRow::Category(id) => tr(&format!("category-{id}")).to_string(),
        MenuRow::Node(type_id) => tr(&format!("node-{type_id}")).to_string(),
    }
}

/// A spawn position (in graph space) for the next node placed from the ribbon.
///
/// Anchored at the centre of the currently visible canvas so the node lands where
/// the user is looking, not at a fixed graph-space origin that may be off-screen
/// when panned or zoomed. A small per-add cascade keeps successive adds from
/// stacking exactly. Falls back to a fixed point before the canvas has drawn once.
fn spawn_pos(view: Option<CanvasView>, node_count: usize) -> egui::Pos2 {
    let step = (node_count % 12) as f32 * 24.0;
    let base = view.map_or(egui::pos2(40.0, 40.0), |v| v.center());
    // Offset up-left of centre so the node body sits roughly centred, then cascade.
    base + egui::vec2(step - 60.0, step - 24.0)
}

// ---- pane kinds (self-registered by id) -------------------------------------

/// A pane kind: a stable id plus a mount-agnostic draw function. Pane kinds
/// self-register with `inventory::submit!`, so adding one is a single
/// registration beside the pane and touches no layout code.
struct PaneKind {
    id: &'static str,
    draw: fn(&mut egui::Ui, &mut AppState),
}

inventory::collect!(PaneKind);

/// Looks up a registered pane kind by id.
fn pane_kind(id: &str) -> Option<&'static PaneKind> {
    inventory::iter::<PaneKind>().find(|p| p.id == id)
}

/// Draws the pane registered under `id`, or a visible placeholder if the id is
/// unknown — graceful degradation, never a panic.
fn draw_pane(id: &str, ui: &mut egui::Ui, state: &mut AppState) {
    match pane_kind(id) {
        Some(kind) => (kind.draw)(ui, state),
        None => {
            let error = ui.visuals().error_fg_color;
            ui.colored_label(error, format!("unknown pane: {id:?}"));
        }
    }
}

fn menu_bar_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // A little breathing room under the title bar and above the node tabs.
    ui.add_space(MENU_VPAD);
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked() {
                request_new(state);
                ui.close();
            }
            if ui.button("Open...").clicked() {
                request_open(state);
                ui.close();
            }
            if ui.button("Open Default Graph").clicked() {
                request_open_default(state);
                ui.close();
            }
            ui.separator();
            if ui.button("Save").clicked() {
                save_project(state, state.project_path.clone());
                ui.close();
            }
            if ui.button("Save As...").clicked() {
                save_project(state, None);
                ui.close();
            }
            if ui.button("Save as Default Startup Graph").clicked() {
                save_as_default(state);
                ui.close();
            }
            // Recent projects (most recent first), each opening through the unsaved-changes
            // guard. Shown only when there are any; the file name labels the entry, the full
            // path is on hover.
            if !state.recent.is_empty() {
                ui.separator();
                ui.weak("Open Recent");
                for path in state.recent.clone() {
                    let label = path.file_name().map_or_else(
                        || path.to_string_lossy().into_owned(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    if ui
                        .button(label)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        request_open_path(state, path);
                        ui.close();
                    }
                }
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                request_quit(ui.ctx(), state);
                ui.close();
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui
                .add_enabled(state.history.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                state.undo();
                ui.close();
            }
            if ui
                .add_enabled(state.history.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                state.redo();
                ui.close();
            }
        });
        ui.menu_button("View", |ui| {
            ui.checkbox(&mut state.thumbnails_enabled, "Node thumbnails");
        });
        ui.menu_button("Graph", |ui| {
            ui.weak("(empty)");
        });
        ui.menu_button("Help", |ui| {
            ui.weak("(empty)");
        });
        // The project name and unsaved-changes marker live in the OS title bar now (#83, #87);
        // the menu bar carries only the transient status, pushed to the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(status) = &state.status {
                ui.weak(status);
            }
        });
    });
    ui.add_space(MENU_VPAD);
}
inventory::submit! { PaneKind { id: "menu-bar", draw: menu_bar_pane } }

/// File filter shared by the open and save dialogs.
const PROJECT_EXTENSIONS: &[&str] = &["ymir", "json"];

/// Begins opening another project, guarded by the unsaved-changes prompt (#83): with
/// unsaved changes it defers to the prompt, otherwise it opens straight away.
fn request_open(state: &mut AppState) {
    if state.modified {
        state.pending_action = Some(PendingAction::Open);
    } else {
        open_project(state);
    }
}

/// Begins opening a specific project by path (a recent-projects entry), guarded by the
/// unsaved-changes prompt.
fn request_open_path(state: &mut AppState, path: std::path::PathBuf) {
    if state.modified {
        state.pending_action = Some(PendingAction::OpenPath(path));
    } else {
        open_project_path(state, path);
    }
}

/// Begins a fresh project (the starter graph), guarded by the unsaved-changes prompt.
fn request_new(state: &mut AppState) {
    if state.modified {
        state.pending_action = Some(PendingAction::New);
    } else {
        state.new_project();
    }
}

/// Opens the default startup graph, guarded by the unsaved-changes prompt.
fn request_open_default(state: &mut AppState) {
    if state.modified {
        state.pending_action = Some(PendingAction::OpenDefault);
    } else {
        state.open_default();
    }
}

/// Requests quitting the app (the File menu's Exit), guarded by the unsaved-changes
/// prompt; a clean session closes immediately. Shares the close path with the window's
/// own close button (#83).
fn request_quit(ctx: &egui::Context, state: &mut AppState) {
    if state.modified {
        state.pending_action = Some(PendingAction::Quit);
    } else {
        state.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The user's choice in the unsaved-changes prompt.
enum UnsavedChoice {
    Save,
    SaveAs,
    Discard,
    Cancel,
}

/// Draws the unsaved-changes prompt while a discarding action is pending (#83), and
/// applies the choice: Save (or Save As) then carry out the action, Discard and carry it
/// out, or Cancel and stay. A failed or cancelled save aborts the action so no changes
/// are lost. A no-op when nothing is pending.
fn unsaved_changes_dialog(ctx: &egui::Context, state: &mut AppState) {
    let Some(action) = state.pending_action.clone() else {
        return;
    };
    let mut choice = None;
    egui::Window::new("Unsaved changes")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("This project has unsaved changes.");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(UnsavedChoice::Save);
                }
                if ui.button("Save As...").clicked() {
                    choice = Some(UnsavedChoice::SaveAs);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(UnsavedChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(UnsavedChoice::Cancel);
                }
            });
        });

    // Whether to carry out the pending action. A save must actually write; a cancelled
    // Save As or a write error keeps the changes and aborts, so nothing is lost.
    let proceed = match choice {
        None => return, // still open, no choice yet
        Some(UnsavedChoice::Cancel) => {
            state.pending_action = None;
            return;
        }
        Some(UnsavedChoice::Discard) => true,
        Some(UnsavedChoice::Save) => save_project(state, state.project_path.clone()),
        Some(UnsavedChoice::SaveAs) => save_project(state, None),
    };
    state.pending_action = None;
    if !proceed {
        return;
    }
    match action {
        PendingAction::Open => open_project(state),
        PendingAction::OpenPath(path) => open_project_path(state, path),
        PendingAction::New => state.new_project(),
        PendingAction::OpenDefault => state.open_default(),
        PendingAction::Quit => {
            // Allow the close that follows through, then request it.
            state.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Prompts for a project file and opens it (#75). A cancelled dialog is a no-op; a
/// failed read or parse leaves the current project untouched and reports the reason
/// in the status line.
fn open_project(state: &mut AppState) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Ymir project", PROJECT_EXTENSIONS)
        .pick_file()
    else {
        return;
    };
    open_project_path(state, path);
}

/// Opens a known project path (the Open dialog's pick, or a recent-projects entry). On
/// success it becomes the most recent project; a failed read leaves the current project
/// untouched, reports why, and drops the path from the recent list so a stale entry clears
/// itself.
fn open_project_path(state: &mut AppState, path: std::path::PathBuf) {
    match read_project(&path) {
        Ok(restored) => {
            state.install_project(restored, path.clone());
            record_recent(state, path);
        }
        Err(err) => {
            state.status = Some(format!("Could not open {}: {err}", path.display()));
            state.recent.retain(|p| p != &path);
            save_recent(state);
        }
    }
}

/// Saves the project to `path`, or prompts for one (Save As) when `path` is `None`.
/// On success the path becomes the session's project path so a later `Save` reuses it,
/// and the session is marked clean. Returns whether the project was written (a cancelled
/// Save As dialog or a write error returns `false`), so the caller can decide whether a
/// pending discard action is safe to proceed.
fn save_project(state: &mut AppState, path: Option<std::path::PathBuf>) -> bool {
    let path = match path.or_else(prompt_save_path) {
        Some(path) => path,
        None => return false,
    };
    let file = state.snapshot();
    match write_project(&path, &file) {
        Ok(()) => {
            state.status = Some("Saved".to_string());
            state.project_path = Some(path.clone());
            state.mark_clean();
            record_recent(state, path);
            true
        }
        Err(err) => {
            state.status = Some(format!("Could not save {}: {err}", path.display()));
            false
        }
    }
}

/// Prompts for a save location, defaulting the name and extension.
fn prompt_save_path() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Ymir project", PROJECT_EXTENSIONS)
        .set_file_name("untitled.ymir")
        .save_file()
}

/// Reads and rebuilds a project from `path`. Errors are surfaced as a message for the
/// status line rather than a typed error, since the only consumer is the GUI.
fn read_project(path: &std::path::Path) -> Result<project_file::RestoredProject, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file: project_file::ProjectFile =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    file.restore().map_err(|e| e.to_string())
}

/// Writes `file` to `path` as pretty JSON.
fn write_project(path: &std::path::Path, file: &project_file::ProjectFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// The path to the user's default startup graph (#76): `$XDG_CONFIG_HOME/ymir/
/// default.ymir`, falling back to `$HOME/.config/ymir/default.ymir`. `None` if
/// neither variable is set, in which case the default-graph feature is simply
/// unavailable rather than an error. Reading env vars is safe in edition 2024; only
/// `set_var` is the forbidden-unsafe one, which this never needs.
fn default_project_path() -> Option<std::path::PathBuf> {
    config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "default.ymir",
    )
}

/// The path to the recent-projects list: `…/ymir/recent.json` under the XDG config base,
/// or `None` when no config directory can be resolved.
fn recent_projects_path() -> Option<std::path::PathBuf> {
    config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "recent.json",
    )
}

/// Resolves a config file path from the XDG config base, given the `XDG_CONFIG_HOME`
/// and `HOME` values and a `filename`. Pure (the env read lives in the caller), so the
/// precedence is unit-tested without touching the process environment.
fn config_path(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    filename: &str,
) -> Option<std::path::PathBuf> {
    let base = xdg
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            home.filter(|s| !s.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("ymir").join(filename))
}

/// How many recent projects to remember.
const RECENT_MAX: usize = 5;

/// The recent-projects list, persisted as JSON next to the default graph. A small wrapper so
/// the on-disk shape can evolve without changing the bare list type.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct RecentProjects {
    /// Project paths, most recent first.
    paths: Vec<std::path::PathBuf>,
}

/// Moves `path` to the front of the recent list, removing any earlier occurrence and
/// trimming to [`RECENT_MAX`]. Pure, so the ordering and cap are unit-tested directly.
fn push_recent(list: &mut Vec<std::path::PathBuf>, path: std::path::PathBuf) {
    list.retain(|p| p != &path);
    list.insert(0, path);
    list.truncate(RECENT_MAX);
}

/// Loads the recent-projects list, or an empty list when it is absent or unreadable (a
/// missing or stale list is the normal first-run case, not an error).
fn load_recent() -> Vec<std::path::PathBuf> {
    let Some(path) = recent_projects_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice::<RecentProjects>(&bytes)
        .map(|r| r.paths)
        .unwrap_or_default()
}

/// Records `path` as the most recent project: moves it to the front of `state.recent` and
/// persists the list.
fn record_recent(state: &mut AppState, path: std::path::PathBuf) {
    push_recent(&mut state.recent, path);
    save_recent(state);
}

/// Persists `state.recent` to the config file. A failed write is reported on the status line
/// rather than silently dropped, but never blocks the open/save that triggered it.
fn save_recent(state: &mut AppState) {
    let Some(config) = recent_projects_path() else {
        return;
    };
    if let Some(parent) = config.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        state.status = Some(format!("Could not save recent list: {err}"));
        return;
    }
    let recent = RecentProjects {
        paths: state.recent.clone(),
    };
    match serde_json::to_string_pretty(&recent) {
        Ok(json) => {
            if let Err(err) = std::fs::write(&config, json) {
                state.status = Some(format!("Could not save recent list: {err}"));
            }
        }
        Err(err) => state.status = Some(format!("Could not serialize recent list: {err}")),
    }
}

/// Saves the current session as the user's default startup graph (#76), at the
/// fixed config path (no dialog, since the location is not the user's to pick).
/// Creates the config directory if it does not exist. Reports the outcome on the
/// status line.
fn save_as_default(state: &mut AppState) {
    let Some(path) = default_project_path() else {
        state.status =
            Some("Could not locate a config directory for the default graph.".to_string());
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        state.status = Some(format!("Could not create {}: {err}", parent.display()));
        return;
    }
    let file = state.snapshot();
    match write_project(&path, &file) {
        Ok(()) => {
            state.status = Some(format!(
                "Saved as default startup graph ({})",
                path.display()
            ));
        }
        Err(err) => state.status = Some(format!("Could not save default: {err}")),
    }
}

/// Overlays the user's saved default startup graph onto a fresh session, if one
/// exists (#76), replacing the built-in starter. The default is a template, not the
/// session's save target, so `project_path` stays `None` and the first `Save`
/// prompts for a location. An absent default is the normal first-run case (the
/// starter stands); a corrupt one is reported and the starter stands.
fn apply_default(state: &mut AppState) {
    let Some(path) = default_project_path() else {
        return;
    };
    if !path.exists() {
        return;
    }
    match read_project(&path) {
        Ok(restored) => {
            state.graph = restored.graph;
            state.snarl = restored.snarl;
            state.seed = restored.seed;
            state.world_extent = restored.world_extent;
            state.world_height = restored.world_height;
            state.frame_to_graph_request = true;
            // Anchor undo and the clean point at the default, not the starter it replaced.
            state.reset_history();
            state.mark_clean();
        }
        Err(err) => {
            state.status = Some(format!("Default startup graph could not be loaded: {err}"));
        }
    }
}

/// A category tab whose width is reserved at its full text width, so the per-state
/// difference in egui's button frame margin (it subtracts the state's border width from
/// the padding) cannot change a tab's footprint and reflow the row on hover or selection.
/// Sets the button's `min_size` to `text + 2·button_padding`, which no state's natural
/// width exceeds, pinning every state to that one width.
fn category_tab(
    ui: &mut egui::Ui,
    active: &mut Option<ActiveTab>,
    value: Option<ActiveTab>,
    label: &str,
) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, egui::Color32::WHITE)
        .rect
        .width();
    let width = text_w + 2.0 * ui.spacing().button_padding.x;
    let height = ui.spacing().interact_size.y;
    let selected = *active == value;
    if ui
        .add(egui::Button::selectable(selected, label).min_size(egui::vec2(width, height)))
        .clicked()
    {
        *active = value;
    }
}

/// A new default canvas frame with its top-left at `pos` (graph space): a subtle
/// translucent tint with a matching border and a placeholder label, ready to recolour and
/// relabel in the inspector (#94).
fn new_frame(pos: egui::Pos2) -> project_file::Frame {
    /// Default frame size in graph units.
    const SIZE: egui::Vec2 = egui::vec2(240.0, 160.0);
    let c = theme::LINE_STRONG;
    let t = theme::TEXT_PRIMARY;
    project_file::Frame {
        rect: [pos.x, pos.y, pos.x + SIZE.x, pos.y + SIZE.y],
        // A low alpha so the fill tints the canvas grid rather than hiding it.
        fill: [c.r(), c.g(), c.b(), 36],
        border: [c.r(), c.g(), c.b()],
        text: [t.r(), t.g(), t.b()],
        label: "Frame".to_string(),
        label_placement: project_file::LabelPlacement::TopLeft,
    }
}

fn ribbon_pane(ui: &mut egui::Ui, state: &mut AppState) {
    let cats = categories_sorted();
    if state.active_tab.is_none() {
        state.active_tab = cats.first().map(|c| ActiveTab::Category(c.id));
    }

    // Two full-width bands with equal padding, so they are equal height with their content
    // vertically centred: the categories/search/Build bar, then the node list below.
    egui::Frame::new()
        .fill(scale_color(ui.visuals().panel_fill, 1.5))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                for cat in &cats {
                    let key = format!("category-{}", cat.id);
                    category_tab(
                        ui,
                        &mut state.active_tab,
                        Some(ActiveTab::Category(cat.id)),
                        tr(&key),
                    );
                }
                if has_uncategorized_nodes() {
                    category_tab(
                        ui,
                        &mut state.active_tab,
                        Some(ActiveTab::Uncategorized),
                        "Uncategorized",
                    );
                }
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(&mut state.search)
                        .hint_text("search nodes")
                        .desired_width(160.0),
                );
                // Clear button, shown only when there is a query (#56).
                if !state.search.is_empty()
                    && ui.small_button("×").on_hover_text("Clear search").clicked()
                {
                    state.search.clear();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Build the selected outputs at the world-tab resolution, off-thread.
                    state.build.poll(ui.ctx());
                    let building = state.build.is_building();
                    if ui
                        .add_enabled(!building, egui::Button::new("Build"))
                        .clicked()
                    {
                        // Build the whole project: the effective top-level graph, even if a
                        // subgraph is currently open on the canvas.
                        let top = state.top_graph();
                        let targets = included_endpoints(&top);
                        if targets.is_empty() {
                            state
                                .build
                                .report("No outputs selected to build.".to_string());
                        } else if targets.len() > MAX_BUILD_OUTPUTS {
                            state.build.report(format!(
                                "Too many outputs ({}); the limit is {MAX_BUILD_OUTPUTS}.",
                                targets.len()
                            ));
                        } else {
                            let res = state.build_res;
                            let request = EvalRequest::new(res, res, Region::UNIT, state.seed)
                                .with_world_extent(state.world_extent)
                                .with_world_height(state.world_height);
                            state.build.start(top, targets, request);
                        }
                    }
                    state.build.show(ui);
                });
            });
        });

    // The node list, a touch lighter so it reads as distinct from the bar above.
    let entries = node_entries();
    let shown = visible_nodes(&entries, state.active_tab, &state.search);
    egui::Frame::new()
        .fill(scale_color(ui.visuals().panel_fill, 1.8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                for entry in shown {
                    let key = format!("node-{}", entry.type_id);
                    if ui.button(tr(&key)).clicked() {
                        let pos = spawn_pos(state.canvas_view, state.graph.node_count());
                        if let Some(id) =
                            canvas::add_node(&mut state.graph, &mut state.snarl, entry.type_id, pos)
                            && let Some(handle) = state.graph.stable_id(id)
                        {
                            // Select the new node so the inspector shows it immediately (#62).
                            state.select_only(handle);
                        }
                    }
                }
            });
        });
}
inventory::submit! { PaneKind { id: "ribbon", draw: ribbon_pane } }

/// The display-name override for some edited text: `None` when the text is empty or
/// whitespace (revert to the type name), else the raw text. Shared by the inspector
/// Name field and the Rename dialog (#59, #61).
fn name_override(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// The `stable_id`s of the output endpoints a Build should write: nodes with no outputs
/// whose `build` flag is on (default on). Iterates the graph itself (via its document, in
/// `stable_id` order) so it works on the effective top-level graph even while a subgraph is
/// open on the canvas.
fn included_endpoints(graph: &Graph) -> Vec<u64> {
    graph
        .to_document()
        .nodes
        .iter()
        .filter_map(|nd| {
            let id = graph.node_id_of(nd.stable_id)?;
            let spec = graph.spec(id)?;
            let included = spec.outputs.is_empty()
                && graph.params(id).is_none_or(|p| p.get_bool("build", true));
            included.then_some(nd.stable_id)
        })
        .collect()
}

/// Folds an active inner graph back up through its suspended parents to the top-level
/// graph (#106): for each parent from innermost out, installs the child into its container
/// via [`Graph::set_nested`] and continues with that parent. With an empty stack this is
/// just `active`. Used for save, dirty tracking, and Build, which all act on the whole
/// project regardless of which subgraph is open.
///
/// # Errors
///
/// Returns the error from [`Graph::set_nested`] if a container `stable_id` is no longer in
/// its parent graph, which cannot happen for a live navigation stack.
fn fold_to_top(active: Graph, nav: &[NavFrame]) -> Result<Graph, ymir_core::Error> {
    let mut active = active;
    for frame in nav.iter().rev() {
        let mut parent = frame.graph.clone();
        let container = parent
            .node_id_of(frame.container)
            .ok_or(ymir_core::Error::NodeNotFound)?;
        parent.set_nested(container, active)?;
        active = parent;
    }
    Ok(active)
}

/// A node's display name: its per-instance override if set (#59), else its type's
/// name via `tr`. Mirrors the canvas title for the preview header.
fn node_display_name(graph: &Graph, id: NodeId) -> String {
    if let Some(name) = graph.name(id) {
        return name.to_string();
    }
    graph.spec(id).map_or_else(
        || "?".to_string(),
        |spec| tr(&format!("node-{}", spec.type_id)).to_string(),
    )
}

/// The right column: a tab header (Node / World) over the selected tab's content. The Node
/// tab shows the preview over the node inspector; the World tab shows the world settings. A
/// collapse control will live at the far right of the header later.
fn right_panel_pane(ui: &mut egui::Ui, state: &mut AppState) {
    header_strip(ui, |ui| {
        ui.selectable_value(&mut state.param_tab, ParamTab::Node, "Node");
        ui.selectable_value(&mut state.param_tab, ParamTab::World, "World");
    });
    match state.param_tab {
        ParamTab::Node => {
            egui::Panel::top("preview-panel")
                // Fixed height so the preview does not change size between having a node
                // selected (a tall image) and not (a placeholder).
                .exact_size(346.0)
                .show_separator_line(false)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(4, 2)),
                )
                .show_inside(ui, |ui| preview_2d_pane(ui, state));
            // A selected frame shows the frame inspector here instead of the node inspector
            // (selection is mutually exclusive, #94).
            egui::CentralPanel::default().show_inside(ui, |ui| match state.selected_frame {
                Some(index) => frame_inspector(ui, state, index),
                None => node_inspector(ui, state),
            });
        }
        ParamTab::World => {
            egui::CentralPanel::default().show_inside(ui, |ui| world_settings(ui, state));
        }
    }
}
inventory::submit! { PaneKind { id: "right-panel", draw: right_panel_pane } }

/// The selected node's inspector: its display-name override and parameter widgets.
fn node_inspector(ui: &mut egui::Ui, state: &mut AppState) {
    // The inspector edits the primary (last-clicked) selected node; nothing selected (or
    // it was deleted) shows a hint, not an error.
    let Some(handle) = state.primary else {
        ui.weak("Select a node to edit its parameters.");
        return;
    };
    let Some(id) = state.graph.node_id_of(handle) else {
        ui.weak("Select a node to edit its parameters.");
        return;
    };
    let Some(spec) = state.graph.spec(id) else {
        ui.weak("Select a node to edit its parameters.");
        return;
    };

    // Display-name override (#59): edit at the top. The type name is the hint, so an
    // empty field shows what the node falls back to, and a weak line below always
    // shows the underlying type even when renamed.
    let type_name = tr(&format!("node-{}", spec.type_id)).to_string();
    let mut name = state.graph.name(id).unwrap_or("").to_string();
    ui.horizontal(|ui| {
        ui.label("Name");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text(type_name.as_str())
                .desired_width(f32::INFINITY),
        );
        if resp.changed()
            && let Err(err) = state.graph.set_name(id, name_override(&name))
        {
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    });
    ui.weak(type_name.as_str());
    ui.separator();

    if spec.params.is_empty() {
        ui.weak("This node has no parameters.");
        return;
    }

    // The input distribution for the histogram behind the curve editor (#15), copied to
    // an owned buffer so it does not borrow `state.preview` across the params write below.
    // `None` unless this node is the one being previewed, so the histogram matches.
    let histogram: Option<Vec<f32>> = state
        .primary
        .and_then(|h| state.preview.input_histogram(h).map(|h| h.to_vec()));

    // Edit against a clone of the current params, then write back once if anything
    // changed. The graph stays the single source of truth.
    let mut params = state.graph.params(id).cloned().unwrap_or_default();
    let mut changed = false;
    for (index, pspec) in spec.params.iter().enumerate() {
        // A little vertical breathing room between parameters, so the panel does not read as a
        // dense stack (#90). Between rows only: none before the first or after the last.
        if index > 0 {
            ui.add_space(6.0);
        }
        let current = param_ui::current_value(&params, pspec);
        // The curve editor's corner pop-out icon (#70-style) reports through this flag,
        // opening the larger, draggable window for this node's curve param.
        let mut popout = false;
        if let Some(new_value) =
            param_ui::edit(ui, pspec, &current, histogram.as_deref(), &mut popout)
        {
            params.insert(pspec.name.clone(), new_value);
            changed = true;
        }
        if popout {
            state.curve_popout = Some(CurvePopout {
                node: handle,
                param: pspec.name.clone(),
            });
        }
    }

    if changed && let Err(err) = state.graph.set_params(id, params) {
        // The node would have to vanish mid-frame to reach here; surface it rather
        // than swallow it.
        ui.colored_label(ui.visuals().error_fg_color, err.to_string());
    }
}

/// The selected frame's inspector (#94): edits its label, fill colour and opacity, border
/// colour, and label placement, plus a delete action. Shown in place of the node inspector
/// while a frame is selected.
fn frame_inspector(ui: &mut egui::Ui, state: &mut AppState, index: usize) {
    use egui::widgets::color_picker::{Alpha, color_edit_button_hsva};

    // The frame can vanish (deleted, or a session swapped in) between selection and draw.
    if index >= state.frames.len() {
        ui.weak("No frame selected.");
        return;
    }

    // Seed the HSVA edit buffers when the selected frame changes, so the colour pickers edit
    // in HSVA (hue preserved) rather than re-deriving the hue from the stored RGB each frame,
    // which jumps around near gray.
    if state.frame_color_edit.map(|(i, ..)| i) != Some(index) {
        let frame = &state.frames[index];
        let opaque =
            |c: [u8; 3]| egui::ecolor::Hsva::from_srgba_unmultiplied([c[0], c[1], c[2], 255]);
        let fill = egui::ecolor::Hsva::from_srgba_unmultiplied(frame.fill);
        let border = opaque(frame.border);
        let text = opaque(frame.text);
        state.frame_color_edit = Some((index, fill, border, text));
    }
    // Just seeded above, so this is always `Some`; fall through harmlessly if not.
    let Some((_, mut fill_hsva, mut border_hsva, mut text_hsva)) = state.frame_color_edit else {
        return;
    };

    ui.horizontal(|ui| {
        ui.label("Label");
        ui.add(
            egui::TextEdit::singleline(&mut state.frames[index].label)
                .hint_text("Frame")
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Fill");
        // OnlyBlend keeps the alpha a normal 0..1 opacity (no additive/HDR mode).
        if color_edit_button_hsva(ui, &mut fill_hsva, Alpha::OnlyBlend).changed() {
            state.frames[index].fill = fill_hsva.to_srgba_unmultiplied();
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Border");
        if color_edit_button_hsva(ui, &mut border_hsva, Alpha::Opaque).changed() {
            let c = border_hsva.to_srgba_unmultiplied();
            state.frames[index].border = [c[0], c[1], c[2]];
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Label text");
        // A dark text colour stays readable on a bright header where the light default does not.
        if color_edit_button_hsva(ui, &mut text_hsva, Alpha::Opaque).changed() {
            let c = text_hsva.to_srgba_unmultiplied();
            state.frames[index].text = [c[0], c[1], c[2]];
        }
    });
    // Persist the (possibly dragged) HSVA buffers so the hue carries to the next frame.
    state.frame_color_edit = Some((index, fill_hsva, border_hsva, text_hsva));

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Label position");
        let current = state.frames[index].label_placement;
        let label = match current {
            project_file::LabelPlacement::TopLeft => "Top left",
            project_file::LabelPlacement::TopCenter => "Top centre",
        };
        egui::ComboBox::from_id_salt("frame-label-placement")
            .selected_text(label)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.frames[index].label_placement,
                    project_file::LabelPlacement::TopLeft,
                    "Top left",
                );
                ui.selectable_value(
                    &mut state.frames[index].label_placement,
                    project_file::LabelPlacement::TopCenter,
                    "Top centre",
                );
            });
    });

    ui.add_space(10.0);
    if ui.button("Delete frame").clicked() {
        state.frames.remove(index);
        state.selected_frame = None;
        state.frame_color_edit = None;
    }
}

/// The world/build settings: the global eval-request inputs (seed, resolutions) that
/// apply to the whole graph. Outputs selection and the Build action land here in
/// later steps.
fn world_settings(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label("Seed");
        ui.add(egui::DragValue::new(&mut state.seed).speed(1.0));
    });

    ui.separator();
    ui.label("World extent");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut state.world_extent)
                .speed(8.0)
                .range(1.0..=1_000_000.0)
                .suffix(" m"),
        );
        // The meters-to-cells bridge made tangible. Cells are square, so this is the
        // size along both axes; it follows from extent / build resolution.
        let m_per_cell = state.world_extent / state.build_res as f64;
        ui.weak(format!("≈ {m_per_cell:.3} m/cell at build"));
    });

    ui.separator();
    ui.label("World height");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut state.world_height)
                .speed(2.0)
                .range(1.0..=100_000.0)
                .suffix(" m"),
        );
        // The vertical:horizontal ratio a height of 1.0 reaches over the footprint: what the
        // viewport shows at 1x exaggeration. A value of 1.0 would be as tall as it is wide.
        let proportion = state.world_height / state.world_extent;
        ui.weak(format!("≈ {proportion:.2}× footprint at full height"));
    });

    ui.separator();
    ui.label("Build resolution");
    ui.horizontal(|ui| {
        // Custom value (UE5 landscapes need specific sizes), with presets as
        // shortcuts.
        ui.add(
            egui::DragValue::new(&mut state.build_res)
                .speed(8.0)
                .range(16..=8192),
        );
        egui::ComboBox::from_id_salt("build-res-presets")
            .selected_text("presets")
            .show_ui(ui, |ui| {
                for &preset in BUILD_RES_PRESETS {
                    if ui.selectable_label(false, preset.to_string()).clicked() {
                        state.build_res = preset;
                    }
                }
            });
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Preview resolution");
        ui.add(
            egui::DragValue::new(&mut state.preview_res)
                .speed(4.0)
                .range(32..=1024),
        );
    });

    ui.separator();
    ui.label("Outputs");
    ui.weak("Endpoints a Build will write; tick to include.");
    // Endpoints are nodes with no outputs. Collect them first (releasing the snarl
    // borrow) before mutating params below.
    let endpoints: Vec<NodeId> = state
        .snarl
        .node_ids()
        .filter_map(|(_, &handle)| state.graph.node_id_of(handle))
        .filter(|&id| state.graph.spec(id).is_some_and(|s| s.outputs.is_empty()))
        .collect();
    if endpoints.is_empty() {
        ui.weak("No output nodes in the graph.");
        return;
    }
    for id in endpoints {
        let mut params = state.graph.params(id).cloned().unwrap_or_default();
        let mut include = params.get_bool("build", true);
        let name = node_display_name(&state.graph, id);
        let path = params.get_str("path", "").to_string();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut include, name).changed() {
                params.insert("build".to_string(), ParamValue::Bool(include));
                if let Err(err) = state.graph.set_params(id, params) {
                    ui.colored_label(ui.visuals().error_fg_color, err.to_string());
                }
            }
            if !path.is_empty() {
                ui.weak(path);
            }
        });
    }
}

fn preview_2d_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // Drop a pin left pointing at a deleted node, so it never sticks the preview on
    // nothing.
    if state
        .preview_pin
        .is_some_and(|h| state.graph.node_id_of(h).is_none())
    {
        state.preview_pin = None;
    }

    // The preview shows the pinned node if one is set, else the selection. Only nodes with
    // an output qualify; evaluating an endpoint would run its side effect. When nothing is
    // selected the same layout is drawn with placeholder content (a neutral header and a
    // black image), so the pane never collapses or swaps to a bare box.
    let target = state.preview_target();
    let id = target.and_then(|t| state.graph.node_id_of(t));
    let is_pinned = target.is_some() && state.preview_pin == target;

    // Row 1: the status dot (colour = up-to-date/evaluating/error, words on hover), the
    // previewed node's name, a pinned/bypassed marker, and the Pin/Unpin toggle right-aligned.
    // A plain row on the regular background (the tab header above is the panel's header).
    ui.horizontal(|ui| {
        let color = match id {
            Some(_) => state.preview.status_color(ui.visuals()),
            None => ui.visuals().weak_text_color(),
        };
        let d = ui.text_style_height(&egui::TextStyle::Body) * 0.6;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), d * 0.5, color);
        if id.is_some() {
            resp.on_hover_text(state.preview.status_label());
        }

        match id {
            Some(id) => {
                ui.label(node_display_name(&state.graph, id));
                if is_pinned {
                    ui.weak("· pinned");
                }
                // A bypassed node shows its input, not its own output (#105).
                if state.graph.is_bypassed(id) {
                    ui.weak("· bypassed");
                }
            }
            None => {
                ui.weak("No node selected");
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match (target, is_pinned) {
                (Some(_), true) => {
                    if ui.button("Unpin").clicked() {
                        state.preview_pin = None;
                    }
                }
                (Some(t), false) => {
                    if ui.button("Pin").clicked() {
                        state.preview_pin = Some(t);
                    }
                }
                // Nothing to pin, but keep the control for a stable header.
                (None, _) => {
                    ui.add_enabled(false, egui::Button::new("Pin"));
                }
            }
        });
    });

    // Output picker: when the previewed node has more than one output (e.g. hydraulic
    // erosion's heightfield/water/sediment), choose which output to view. Hidden for a
    // single-output node, so it appears exactly when there is a choice to make.
    let output_names: Vec<String> = id
        .and_then(|id| state.graph.spec(id))
        .map(|spec| spec.outputs.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    if output_names.len() > 1 {
        let mut selected = state.preview.display_output().min(output_names.len() - 1);
        ui.horizontal(|ui| {
            ui.label("Output");
            egui::ComboBox::from_id_salt("preview-output")
                .selected_text(output_names[selected].clone())
                .show_ui(ui, |ui| {
                    for (index, name) in output_names.iter().enumerate() {
                        ui.selectable_value(&mut selected, index, name);
                    }
                });
        });
        state.preview.set_display_output(selected);
    }

    // Row 2: shading toggle (relief makes subtle height changes legible, #40); in relief the
    // light dial, in height the Auto/Fixed scale toggle. These are persistent display
    // settings, so they show even with no node selected.
    let mut mode = state.preview.mode();
    let mut scale = state.preview.scale();
    ui.horizontal(|ui| {
        // Reserve the dial's height in both modes so the image never jumps when toggling
        // Height/Relief.
        ui.set_min_height(preview::LIGHT_DIAL_SIZE);
        ui.selectable_value(&mut mode, shade::ShadeMode::Height, "Height");
        ui.selectable_value(&mut mode, shade::ShadeMode::Relief, "Relief");
        // The mode-specific control sits at the right, separated from Height/Relief.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if mode == shade::ShadeMode::Relief {
                state.preview.light_indicator(ui);
            } else {
                // Auto stretches the actual range (shape); Fixed maps [0, 1] (true amplitude,
                // clips out of range). right_to_left places the first-added widget rightmost,
                // so add Fixed then Auto to keep the pair reading "Auto  Fixed".
                ui.selectable_value(&mut scale, shade::HeightScale::Fixed, "Fixed")
                    .on_hover_text("Map a fixed [0, 1]: true height, clips out of range");
                ui.selectable_value(&mut scale, shade::HeightScale::Auto, "Auto")
                    .on_hover_text("Stretch the field's actual range to black/white");
            }
        });
    });
    state.preview.set_mode(mode);
    state.preview.set_scale(scale);

    // The image: the previewed node's output (evaluated each frame by `drive_preview`,
    // independent of this pane being visible), or a black placeholder when there is no
    // previewable target.
    match id {
        Some(_) => preview_box(ui, |ui| state.preview.show(ui)),
        None => preview_box(ui, preview_black_image),
    }
}

/// A black square the size a preview image would occupy, drawn when nothing is selected so
/// the preview reads as an empty image rather than a missing one.
fn preview_black_image(ui: &mut egui::Ui) {
    let side = ui.available_width().min(ui.available_height()).max(0.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
}

/// Multiplies an opaque colour's channels by `factor` (clamped), keeping it opaque. Unlike
/// [`egui::Color32::gamma_multiply`], which changes opacity, this darkens (`factor < 1`) or
/// lightens (`factor > 1`) the colour itself.
fn scale_color(c: egui::Color32, factor: f32) -> egui::Color32 {
    let s = |v: u8| (f32::from(v) * factor).clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

/// Draws a pane header strip: a full-width band a touch darker than the pane body, with
/// `contents` laid out horizontally inside it. Shared by the preview and node-list panes so
/// their headers match.
fn header_strip(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(scale_color(ui.visuals().panel_fill, 0.5))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            // Fill the width so the strip spans the pane even when its content is short.
            ui.set_min_width(ui.available_width());
            ui.horizontal(contents);
        });
}

/// Draws the preview image in a border that hugs it (no inset), with a little space below so
/// it does not sit flush against the pane's lower edge. The image sizes itself to fit the
/// fixed-height pane, and the border is drawn at its exact edge.
fn preview_box(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(2)
        .show(ui, contents);
    ui.add_space(8.0);
}

/// Draws the cursor-anchored node-creation menu when open, and applies its outcome:
/// drilling into a category, creating a node at the cursor, or closing. A no-op when
/// the menu is closed.
fn node_menu_ui(ui: &mut egui::Ui, state: &mut AppState) {
    // Esc closes without creating. Checked before borrowing the menu so it can clear
    // `node_menu` outright.
    if state.node_menu.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.node_menu = None;
    }
    let Some(menu) = state.node_menu.as_mut() else {
        return;
    };

    let entries = node_entries();
    let rows = menu_rows(&entries, &menu.search, menu.drilled);

    // Keyboard navigation: Up/Down move the highlight (wrapping), Enter activates it,
    // Left/Right step back and forth through the category hierarchy. Read before the
    // draw so the highlight shown is this frame's. A single-line search field ignores
    // Up/Down, so they are free; Left/Right are used for navigation only when the
    // search is empty (otherwise they edit the query's caret as usual).
    let (up, down, left, right, enter) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::Enter),
        )
    });
    if rows.is_empty() {
        menu.highlight = 0;
    } else {
        let n = rows.len();
        if down {
            menu.highlight = (menu.highlight + 1) % n;
        }
        if up {
            menu.highlight = (menu.highlight + n - 1) % n;
        }
        menu.highlight = menu.highlight.min(n - 1);
    }
    // The activated row: Enter takes the highlight; a click overrides with its row.
    let mut activated = (enter && !rows.is_empty()).then_some(menu.highlight);
    // Left/Right step through the hierarchy when not editing a query: Right drills
    // into the highlighted category, Left returns via the Back row (always row 0 of a
    // drilled-in list). Both reuse the row-activation path below.
    if menu.search.trim().is_empty() {
        if right && matches!(rows.get(menu.highlight), Some(MenuRow::Category(_))) {
            activated = Some(menu.highlight);
        }
        if left && menu.drilled.is_some() {
            activated = Some(0);
        }
    }

    let area = egui::Area::new(egui::Id::new("ymir-node-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(menu.anchor)
        .constrain(true);
    // A fixed menu width: rows fill it through a justified layout, so nothing
    // queries `available_width`.
    const MENU_WIDTH: f32 = 180.0;
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::menu(ui.style()).show(ui, |ui| {
            ui.set_width(MENU_WIDTH);
            // Taller rows make calmer, less-overshootable targets (#64).
            ui.spacing_mut().button_padding = egui::vec2(8.0, 6.0);

            // A bare search field: a hint, not a label, so it costs no row after the
            // first use and vanishes the moment you type.
            let edit = egui::TextEdit::singleline(&mut menu.search)
                .hint_text("filter nodes")
                .desired_width(f32::INFINITY);
            let resp = ui.add(edit);
            if menu.focus_search {
                resp.request_focus();
                menu.focus_search = false;
            }
            // Editing the query resets the highlight to the first result.
            if resp.changed() {
                menu.highlight = 0;
            }
            ui.separator();

            // The list sizes to its content each frame. No scroll area: a scroll
            // area must size its viewport from the previous frame's content, so it
            // lags (and, while egui is idle, sticks) when the list shrinks then grows
            // across a drill-in/back. The current lists are short enough to fit; a
            // scroll area is the deliberate addition for when a category can overflow
            // the screen.
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                // The blue selection is the only row highlight; suppress egui's grey
                // hover fill so it never competes with the blue when the content
                // changes under a stationary pointer (e.g. just after drilling into a
                // category). Hover still drives the blue highlight (below) on movement.
                let hovered_vis = &mut ui.visuals_mut().widgets.hovered;
                hovered_vis.weak_bg_fill = egui::Color32::TRANSPARENT;
                hovered_vis.bg_fill = egui::Color32::TRANSPARENT;
                if rows.is_empty() {
                    ui.weak("no matches");
                }
                // Pointer movement makes hover drive the single (blue) highlight, so it
                // follows the mouse instead of a separate grey hover fighting the
                // keyboard highlight. When the pointer is still, arrow keys own the
                // highlight without hover stomping it.
                let pointer_moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                let mut hovered = None;
                for (i, &row) in rows.iter().enumerate() {
                    let resp = menu_row(ui, row, i == menu.highlight);
                    if resp.hovered() {
                        hovered = Some(i);
                    }
                    if resp.clicked() {
                        activated = Some(i);
                    }
                }
                if pointer_moved
                    && let Some(h) = hovered
                    && h != menu.highlight
                {
                    menu.highlight = h;
                    // Redraw so the highlight lands on the hovered row promptly.
                    ui.ctx().request_repaint();
                }
            });
        });
    });

    // The placement view, anchor, and the snapshotted wire-to-create wire are Copy; read
    // them out so the menu borrow can end before the state is mutated below.
    let anchor = menu.anchor;
    let view = menu.view;
    let pending_wire = menu.pending_wire;
    let menu_rect = area_resp.response.rect;

    // A primary press outside the menu dismisses it.
    let clicked_outside = ui.input(|i| {
        i.pointer.primary_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !menu_rect.contains(p))
    });

    if clicked_outside {
        state.node_menu = None;
    } else if let Some(&row) = activated.and_then(|i| rows.get(i)) {
        match row {
            // Drilling keeps the menu open: reset the highlight (skipping the Back row
            // when entering a category) and refocus the search so typing continues.
            MenuRow::Back => {
                if let Some(menu) = state.node_menu.as_mut() {
                    // Land on the category just left, not the top of the list, so a
                    // mistaken drill-in is one keystroke to undo (#89). The top-level
                    // rows are `categories_sorted()` in order, so the category's index
                    // there is the row to highlight.
                    let from = menu.drilled;
                    menu.drilled = None;
                    menu.highlight = from.map_or(0, category_row_index);
                    menu.focus_search = true;
                }
            }
            MenuRow::Category(id) => {
                if let Some(menu) = state.node_menu.as_mut() {
                    menu.drilled = Some(id);
                    menu.highlight = 1;
                    menu.focus_search = true;
                }
            }
            MenuRow::Node(type_id) => {
                let pos = view.graph_pos_finite(anchor);
                if let Some(id) = canvas::add_node(&mut state.graph, &mut state.snarl, type_id, pos)
                    && let Some(handle) = state.graph.stable_id(id)
                {
                    // Select the new node so the inspector shows it immediately (#62).
                    state.select_only(handle);
                    // Wire-to-create (#123): if a wire was snapshotted when the menu opened
                    // (Space path or drop path), connect it to the new node's default
                    // opposite port (its first input for a wire pulled from an output, its
                    // first output otherwise), then ask snarl to drop the armed wire.
                    // Degrades to no wire if the new node has no port on that side.
                    if let Some(wire) = pending_wire
                        && let Some(new_node) = canvas::snarl_node_of(&state.snarl, handle)
                    {
                        let has_input = state.graph.spec(id).is_some_and(|s| !s.inputs.is_empty());
                        let has_output =
                            state.graph.spec(id).is_some_and(|s| !s.outputs.is_empty());
                        if wire.from_output && has_input {
                            canvas::connect_pins(
                                &mut state.graph,
                                &mut state.snarl,
                                wire.node,
                                wire.port,
                                new_node,
                                0,
                            );
                        } else if !wire.from_output && has_output {
                            canvas::connect_pins(
                                &mut state.graph,
                                &mut state.snarl,
                                new_node,
                                0,
                                wire.node,
                                wire.port,
                            );
                        }
                        // The gesture consumed the wire; clear it next frame so its
                        // rubber-band stops following the cursor.
                        state.consume_wire = true;
                    }
                }
                state.node_menu = None;
            }
        }
    }
}

/// One menu row: a full-width selectable button showing the row's [`menu_row_text`],
/// with a disclosure chevron painted at the right edge for a category, so the chevrons
/// align in a column instead of trailing each name at a different x (#93). `selected`
/// draws the keyboard/hover highlight.
fn menu_row(ui: &mut egui::Ui, row: MenuRow, selected: bool) -> egui::Response {
    let resp = ui.add(egui::Button::selectable(selected, menu_row_text(row)));
    if matches!(row, MenuRow::Category(_)) {
        // Pin the disclosure caret to the right edge (inset by the row padding), in the
        // row's current text colour so it tracks the selection highlight.
        let color = ui.style().interact_selectable(&resp, selected).text_color();
        let x = resp.rect.right() - ui.spacing().button_padding.x;
        ui.painter().text(
            egui::pos2(x, resp.rect.center().y),
            egui::Align2::RIGHT_CENTER,
            egui_phosphor::regular::CARET_RIGHT,
            egui::TextStyle::Button.resolve(ui.style()),
            color,
        );
    }
    resp
}

/// The pan/zoom transform that frames every node within `canvas` (#65): the
/// graph-space union of the node rects, scaled (with a little padding, clamped to the
/// zoom bounds) and centred in the canvas. `None` for an empty or degenerate graph.
fn fit_view(
    node_rects: &[(Handle, egui::Rect)],
    canvas: egui::Rect,
    min_scale: f32,
    max_scale: f32,
) -> Option<egui::emath::TSTransform> {
    let mut bounds: Option<egui::Rect> = None;
    for (_, rect) in node_rects {
        bounds = Some(bounds.map_or(*rect, |b| b.union(*rect)));
    }
    let bounds = bounds?;
    if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return None;
    }

    // Fit the bounds in the canvas with a little breathing room, within the zoom
    // clamp, then translate so the bounds centre lands at the canvas centre.
    let margin = 0.85;
    let scale = ((canvas.width() / bounds.width()).min(canvas.height() / bounds.height()) * margin)
        .clamp(min_scale, max_scale);
    let translation = canvas.center().to_vec2() - scale * bounds.center().to_vec2();
    Some(egui::emath::TSTransform::new(translation, scale))
}

/// A fresh node-creation menu anchored at `anchor` (screen space), placing into
/// `view`. Shared by the Space gesture and the right-click "Add node" (#51, #60).
/// `pending_wire` is the wire to connect when a node is picked (wire-to-create, #123),
/// or `None` for a plain create.
fn open_node_menu(
    anchor: egui::Pos2,
    view: CanvasView,
    pending_wire: Option<canvas::ArmedWire>,
) -> NodeMenu {
    NodeMenu {
        anchor,
        view,
        search: String::new(),
        drilled: None,
        highlight: 0,
        focus_search: true,
        pending_wire,
    }
}

/// What a canvas click landed on, resolved during rendering (while the node rects are
/// borrowed) and applied to the selection after the viewer borrow ends. A click on a
/// node's collapse chevron yields no hit, so it toggles the node without selecting it.
enum ClickHit {
    Node(Handle),
    Empty,
}

fn canvas_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // Breadcrumb while inside a subgraph (#106): "Project › Mountain › …", each earlier
    // segment a link that pops back out to that level. Shown only when dived in, so the
    // top-level canvas is unchanged. Runs first so a click swaps the active context before
    // the canvas below renders it this frame.
    if !state.nav.is_empty() {
        let depth = state.nav.len();
        let mut exit_target: Option<usize> = None;
        ui.horizontal(|ui| {
            if ui.link("Project").clicked() {
                exit_target = Some(0);
            }
            for (i, frame) in state.nav.iter().enumerate() {
                ui.weak("›");
                if i + 1 == depth {
                    ui.strong(&frame.label); // the current context: not a link
                } else if ui.link(&frame.label).clicked() {
                    exit_target = Some(i + 1);
                }
            }
        });
        ui.separator();
        if let Some(target) = exit_target {
            state.exit_to(target);
        }
    }

    // While the node menu is open it owns the pointer: skip canvas selection so a
    // click on a menu row does not also select or clear under it.
    let menu_open = state.node_menu.is_some();

    // The previewed node's status dot, drawn on the node the preview is showing (the
    // pinned node if any, else the selection). The pinned node also gets a ring
    // marker. Both computed before the disjoint borrow below, from read-only fields.
    let status = state
        .preview_target()
        .map(|h| (h, state.preview.status_color(ui.visuals())));
    let pinned = state.preview_pin.filter(|&h| state.is_previewable(h));
    // A one-shot "zoom to graph" view to apply this frame (#65), Copy, read before
    // the disjoint borrow.
    let pending_view = state.pending_view;
    // The thumbnail toggle (#74), read before the disjoint borrow. Gates whether nodes
    // get a thumbnail footer at all.
    let show_thumbnails = state.thumbnails_enabled;
    // A one-shot request to frame the whole graph this frame (#75), set after opening a
    // project. Taken so it fires once; the canvas computes the fit from this frame's
    // node rects.
    let frame_to_graph = std::mem::take(&mut state.frame_to_graph_request);

    // Per-node thumbnails (#42): evaluate every output-producing node at thumbnail
    // resolution off-thread, and draw each result in its node body below.
    let thumb_request = EvalRequest::new(
        thumbnails::THUMB_RES,
        thumbnails::THUMB_RES,
        Region::UNIT,
        state.seed,
    )
    .with_world_extent(state.world_extent)
    .with_world_height(state.world_height);
    // The working set is culled to the last-frame view (#74): off-screen nodes and a
    // zoomed-out canvas (where a thumbnail is too small to read) are skipped, so a
    // large graph evaluates only what is on screen. Disabled entirely from the View
    // menu.
    let visible: Vec<canvas::Handle> = if state.thumbnails_enabled {
        // Output-producing nodes paired with their canvas position.
        let candidates: Vec<(canvas::Handle, egui::Pos2)> = state
            .snarl
            .node_ids()
            .filter_map(|(snarl_id, &h)| {
                let produces_output = state
                    .graph
                    .node_id_of(h)
                    .and_then(|id| state.graph.spec(id))
                    .is_some_and(|spec| !spec.outputs.is_empty());
                let pos = state.snarl.get_node_info(snarl_id).map(|info| info.pos);
                produces_output.then_some(()).and(pos).map(|p| (h, p))
            })
            .collect();
        match state.canvas_view {
            Some(view) => canvas::cull_to_viewport(
                &candidates,
                view.to_global,
                view.rect,
                canvas::THUMB_MIN_SCALE,
                canvas::THUMB_CULL_MARGIN,
            ),
            // Before the canvas has reported a view (first frame), evaluate all, so
            // thumbnails appear at once rather than after a blank frame.
            None => candidates.into_iter().map(|(h, _)| h).collect(),
        }
    } else {
        Vec::new()
    };
    let now = ui.input(|i| i.time);
    state
        .thumbnails
        .sync(&state.graph, &visible, &thumb_request, now);
    state.thumbnails.poll(ui.ctx());

    // The selection to highlight this frame, cloned so the click handling below can apply
    // changes through the state after the disjoint borrow ends.
    let selection = state.selection.clone();
    // Read before the disjoint borrow below: whether to drop the armed wire this frame
    // (set last frame by wire-to-create, #123), and the selected frame (#94).
    let consume_wire = state.consume_wire;
    let selected_frame = state.selected_frame;
    // Disjoint borrows: the viewer holds the graph while snarl is rendered. Both
    // are distinct fields of the state, so this split is sound.
    let AppState {
        graph,
        snarl,
        thumbnails,
        frames,
        ..
    } = &mut *state;
    let mut viewer = canvas::GraphViewer {
        graph,
        selection,
        frames: frames.as_slice(),
        selected_frame,
        node_rects: Vec::new(),
        to_global: egui::emath::TSTransform::IDENTITY,
        wire_click: false,
        pending_wire: None,
        dropped_wire: None,
        node_dropped_on_wire: None,
        consume_wire,
        status,
        pinned,
        add_node_at: None,
        add_frame_at: None,
        select_after: Vec::new(),
        rename_request: None,
        dive_request: None,
        pin_request: None,
        bypass_request: None,
        pending_view,
        frame_all_request: frame_to_graph,
        zoom: None,
        thumbnails: Some(&*thumbnails),
        show_thumbnails,
    };
    // The canvas's screen rect comes from the ui, not snarl's response: snarl
    // returns an unbounded `EVERYTHING` rect, so it cannot be used for hit-testing
    // or to locate the visible centre.
    let canvas_rect = ui.max_rect();
    // Wires default to ~1px in a muted colour, which is hard to read; thicken them and
    // colour them (and the pin fill, which wires inherit) with accent-frost, the brand's
    // wire/connection accent (#104). Width/colour become user settings later (#57).
    let style = egui_snarl::ui::SnarlStyle {
        wire_width: Some(2.5),
        pin_fill: Some(theme::ACCENT_FROST),
        // The canvas backdrop reads as the base layer, a step darker than the surface
        // panels, with a subtle line grid, so the graph sits below the panels (#104).
        bg_frame: Some(egui::Frame::new().fill(theme::BG_BASE)),
        bg_pattern_stroke: Some(egui::Stroke::new(1.0, theme::LINE)),
        // Clamp zoom so the graph can't shrink to an unfindable speck (snarl's
        // default min is 0.2 = 5x out); "zoom to graph" handles seeing a big graph
        // whole (#65).
        min_scale: Some(canvas::MIN_SCALE),
        max_scale: Some(canvas::MAX_SCALE),
        ..egui_snarl::ui::SnarlStyle::new()
    };
    // Ports stack in snarl's top-down pin layout, so the gap between them is the
    // canvas ui's vertical item spacing (~3px by default, which reads as jammed).
    // Roughly double it for breathing room between ports and node rows (#58). Scoped
    // to this ui, so it only affects the snarl widget, not other panes or the menu.
    ui.spacing_mut().item_spacing.y = 6.0;

    // The canvas is a pan/zoom egui Scene, so node content sits at fractional,
    // transform-dependent coordinates by design. egui's `show_unaligned` debug overlay
    // (on by default in debug builds) flags any non-pixel-aligned widget edge with
    // orange "Unaligned" lines, which is a false positive here: it briefly flashes on
    // the thumbnail footer as a node resizes. The overlay earns its keep on the static
    // panes (catching blurry text), so disable it only for this ui, not globally.
    ui.style_mut().debug.show_unaligned = false;

    // Plain scroll wheel zooms the canvas about the cursor (#36). snarl's egui Scene
    // would scroll-pan instead, so suppress its scroll-pan and hand the zoom to the
    // viewer's `current_transform`. Only when the pointer is over the canvas, so other
    // panes keep normal scroll-to-pan.
    viewer.zoom = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| canvas_rect.contains(*p))
        .and_then(|cursor| {
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            (scroll != egui::Vec2::ZERO).then(|| {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                // Match egui Scene's exponential zoom feel (scroll_zoom_speed 1/200).
                let factor = ((scroll.x + scroll.y) / 200.0).exp();
                (factor, cursor)
            })
        });

    SnarlWidget::new()
        .style(style)
        .id_salt("ymir-canvas")
        .show(snarl, &mut viewer, ui);

    // Capture the view now (Copy data, no borrows); stored on the state at the end,
    // once the graph/snarl/selected borrows are released.
    let view = CanvasView {
        to_global: viewer.to_global,
        rect: canvas_rect,
    };
    // A graph-space spot from a right-click "Add node", if any (#60).
    let add_node_at = viewer.add_node_at;
    // A graph-space spot from a right-click "Add frame", if any (#94).
    let add_frame_at = viewer.add_frame_at;
    // Nodes the viewer asks to select (e.g. duplicates, #61/#84).
    let select_after = std::mem::take(&mut viewer.select_after);
    // A node the viewer asks to rename (context-menu "Rename", #61).
    let rename_request = viewer.rename_request;
    // A subgraph container the viewer asks to dive into (context-menu "Edit subgraph",
    // #106). Applied at the end of the pane, after this frame's other edits.
    let dive_request = viewer.dive_request;
    // A preview-pin change the viewer requests (context-menu Pin/Unpin, #39).
    let pin_request = viewer.pin_request;
    // Whether this frame's primary click was a click-to-wire gesture on a pin (#50). When
    // it was, the click already wired (or armed a wire), so it must not also select the
    // node under the pin.
    let wire_click = viewer.wire_click;
    // The wire snarl reports as armed this frame, for wire-to-create (#123). Applied to
    // the state below, once the viewer's borrow of the graph has ended.
    let pending_wire = viewer.pending_wire;
    // A wire dropped on empty canvas this frame (#123 step 2): drop point (graph space)
    // and source pin. Opens the node menu there, once the viewer borrow has ended.
    let dropped_wire = viewer.dropped_wire;
    // A node dropped on a wire this frame (#124): the node and the wire's endpoints.
    // Spliced below, once the viewer borrow has ended.
    let node_dropped_on_wire = viewer.node_dropped_on_wire;
    // A node the viewer asks to toggle bypass on (context-menu Bypass, #105).
    let bypass_request = viewer.bypass_request;
    // If "zoom to graph" was chosen, compute the fit from this frame's node rects to
    // apply next frame (#65). Done here, while the viewer (and its node rects) is
    // still borrowed.
    let frame_all_fit = viewer.frame_all_request.then(|| {
        fit_view(
            &viewer.node_rects,
            canvas_rect,
            canvas::MIN_SCALE,
            canvas::MAX_SCALE,
        )
    });

    // Take the node rects and transform out of the viewer for the click and marquee
    // handling below; this is the viewer's (and the disjoint borrow's) last use, so the
    // selection can then be edited through the state.
    let to_global = viewer.to_global;
    let node_rects = std::mem::take(&mut viewer.node_rects);

    // Resolve selection from a plain click (snarl 0.10 only selects on shift-click).
    // A click is a press-and-release without movement, so drags — wiring from or to
    // a pin, and node moves — are excluded automatically and keep their behavior.
    // Reading the pointer rather than registering an interaction leaves snarl's own
    // widgets (the collapse chevron, the pins) their clicks.
    let click = ui
        .ctx()
        .input(|i| {
            i.pointer
                .primary_clicked()
                .then(|| i.pointer.interact_pos())
        })
        .flatten();
    // Hit-test the click into a node body (excluding the collapse chevron) or empty
    // canvas. The selection change is applied below, through the state.
    let click_hit = click
        .filter(|_| !menu_open)
        // A click that wired (or armed a wire) on a pin is not a selection click (#50).
        .filter(|_| !wire_click)
        .filter(|p| canvas_rect.contains(*p))
        .filter(|p| over_canvas_surface(ui, *p))
        .and_then(|screen_pos| {
            // Node rects are in the canvas's local space; map the screen click back into
            // it through the inverse pan/zoom transform before hit-testing.
            let pos = to_global.inverse() * screen_pos;
            match node_rects.iter().find(|(_, rect)| rect.contains(pos)) {
                Some((handle, rect)) => {
                    // The collapse chevron toggles the node; a click there is not a select.
                    let chevron = egui::Rect::from_min_size(
                        rect.min,
                        egui::Vec2::splat(ui.spacing().icon_width + 12.0),
                    );
                    (!chevron.contains(pos)).then_some(ClickHit::Node(*handle))
                }
                None => Some(ClickHit::Empty),
            }
        });

    // Keep the view as long as its rect is finite; a degenerate transform is handled
    // by CanvasView::center falling back to the screen centre, so placement stays
    // finite (a NaN position panics egui's layout) and on the canvas.
    state.canvas_view = view.rect.is_finite().then_some(view);

    // Mirror snarl's armed wire for wire-to-create (#123), and clear the one-shot consume
    // flag: this frame's render already acted on it (snarl dropped the wire if it was set).
    state.pending_wire = pending_wire;
    state.consume_wire = false;
    // Splice a node dropped on a wire into that connection (#124). snarl reports the exact
    // node and wire on the drop frame, so this is just the splice.
    if let Some((node, out_pin, in_pin)) = node_dropped_on_wire {
        canvas::splice_node_into_wire(&mut state.graph, &mut state.snarl, node, out_pin, in_pin);
    }

    // The pending "zoom to graph" view was consumed by this frame's render; replace
    // it with a freshly requested fit (or clear it). One-shot, so it does not fight
    // subsequent pan/zoom (#65).
    state.pending_view = frame_all_fit.flatten();

    // Apply the click to the selection: a plain click selects one node (or clears on
    // empty canvas), Ctrl/Cmd-click toggles a node in or out of the set (#84).
    if let Some(hit) = click_hit {
        let additive = ui.input(|i| i.modifiers.command);
        match hit {
            ClickHit::Node(handle) if additive => state.toggle_selection(handle),
            ClickHit::Node(handle) => state.select_only(handle),
            ClickHit::Empty if !additive => state.clear_selection(),
            ClickHit::Empty => {}
        }
    }
    // A viewer-requested selection (e.g. duplicates) wins over the click (#61). All the
    // requested handles become the new selection (a multi-node duplicate, #84).
    if !select_after.is_empty() {
        state.marquee_select(&select_after, false);
        state.selected_frame = None;
    }

    // Frame select/move (#94): a press on a frame's title bar selects and drags it (with
    // its contents). Runs before the marquee so a press on a frame handle does not also
    // start a box-select; nodes still take precedence (it skips a press over a node).
    let frame_owns = handle_frame_interaction(ui, state, canvas_rect, to_global, &node_rects);

    // Marquee box-select on left-drag over empty canvas (panning moved to middle-mouse
    // by the snarl patch, #84). Runs after the click so a drag that began on a node
    // (a move) is excluded by the node_at test at its origin; a frame-handle press is
    // excluded by `frame_owns`.
    handle_marquee(
        ui,
        state,
        canvas_rect,
        to_global,
        &node_rects,
        menu_open || frame_owns,
    );

    // Drag one selected node and the rest of the selection follows (#84 follow-up). Runs
    // after the marquee, which is mutually exclusive (a marquee begins on empty canvas, a
    // group drag on a selected node).
    handle_group_drag(ui, state, to_global, &node_rects);

    // Keep the selection consistent with the graph: a node deleted this frame (e.g. via
    // the context menu's Delete) is dropped from the set and the primary (#84).
    state
        .selection
        .retain(|h| state.graph.node_id_of(*h).is_some());
    state.primary = state.primary.filter(|h| state.selection.contains(h));

    // Apply a viewer-requested preview-pin change (context-menu Pin/Unpin, #39).
    if let Some(new_pin) = pin_request {
        state.preview_pin = new_pin;
    }

    // Toggle bypass on a node the context menu asked to bypass (#105).
    if let Some(handle) = bypass_request
        && let Some(id) = state.graph.node_id_of(handle)
    {
        let bypassed = state.graph.is_bypassed(id);
        if let Err(err) = state.graph.set_bypassed(id, !bypassed) {
            // The node was live a moment ago; surface it rather than swallow it.
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    }

    // Open the rename dialog for a node the context menu asked to rename (#61),
    // seeding its field with the node's current override (empty if none).
    if let Some(handle) = rename_request
        && state.rename.is_none()
    {
        let text = state
            .graph
            .node_id_of(handle)
            .and_then(|id| state.graph.name(id))
            .map(str::to_string)
            .unwrap_or_default();
        state.rename = Some(RenameDialog {
            target: handle,
            text,
            just_opened: true,
        });
    }

    // Wire-to-create by drop (#123 step 2): a wire released on empty canvas opens the node
    // menu at the drop point, carrying the dropped wire to connect on pick. The drop point
    // is in graph space; map it back to screen for the anchor.
    if state.node_menu.is_none()
        && let Some((graph_pos, wire)) = dropped_wire
        && let Some(view) = state.canvas_view
    {
        let anchor = view.to_global * graph_pos;
        if anchor.is_finite() {
            state.node_menu = Some(open_node_menu(anchor, view, Some(wire)));
        }
    }

    // Right-click "Add frame" drops a default frame at the clicked graph spot (#94).
    if let Some(graph_pos) = add_frame_at {
        state.frames.push(new_frame(graph_pos));
    }

    // Right-click "Add node" (snarl graph menu) opens the node menu at the clicked
    // graph spot, mapped back to screen for the anchor (#60).
    if state.node_menu.is_none()
        && let Some(graph_pos) = add_node_at
        && let Some(view) = state.canvas_view
    {
        let anchor = view.to_global * graph_pos;
        if anchor.is_finite() {
            // Right-click "Add node" is a plain create, not a wiring gesture.
            state.node_menu = Some(open_node_menu(anchor, view, None));
        }
    }

    // Space over the canvas opens the node-creation menu at the cursor (issue #51),
    // unless a text field already has focus (so a space typed elsewhere never opens
    // it). The view captured above gives the screen-to-graph mapping for placement.
    if state.node_menu.is_none() && !ui.ctx().egui_wants_keyboard_input() {
        let anchor = ui
            .input(|i| {
                i.key_pressed(egui::Key::Space)
                    .then(|| i.pointer.hover_pos())
                    .flatten()
            })
            .filter(|p| canvas_rect.contains(*p));
        if let (Some(anchor), Some(view)) = (anchor, state.canvas_view) {
            // Wire-to-create (#123): if a wire is armed, snapshot it so picking a node
            // connects it. Otherwise a plain Space create.
            state.node_menu = Some(open_node_menu(anchor, view, state.pending_wire));
        }
    }
    node_menu_ui(ui, state);
    rename_dialog_ui(ui, state);

    // Dive into a subgraph the context menu asked to edit (#106). Done last, after this
    // frame's other edits have been applied to the current context; the inner graph becomes
    // active for the next frame.
    if let Some(handle) = dive_request {
        state.dive_in(handle);
    }
}
inventory::submit! { PaneKind { id: "canvas", draw: canvas_pane } }

/// True if `screen_pos` lands on a node body, given the canvas node rects (in graph
/// space) and the graph-to-screen transform. Used to keep a marquee from starting on a
/// node (that left-drag is a node move).
fn node_at(
    screen_pos: egui::Pos2,
    node_rects: &[(Handle, egui::Rect)],
    to_global: egui::emath::TSTransform,
) -> bool {
    let graph_pos = to_global.inverse() * screen_pos;
    node_rects.iter().any(|(_, rect)| rect.contains(graph_pos))
}

/// True when `pos` lands on the canvas surface itself rather than a window floating over
/// it (the curve pop-out, a dialog). The click and marquee handlers read raw global
/// pointer state, so geometry alone cannot tell a press on the bare canvas from one on a
/// window that happens to sit inside the canvas rect; without this, dragging in the curve
/// editor draws a marquee and clicking its empty space clears the selection.
///
/// The canvas pane and snarl's node sublayer share the pane's order
/// ([`egui::Order::Background`]); an [`egui::Window`] floats at [`egui::Order::Middle`] or
/// above. Comparing the order lets a press on the bare canvas or a node through while
/// rejecting one on a window, and so cannot reject node clicks the way layer identity would
/// (snarl draws nodes in a same-order sublayer with a distinct id).
fn over_canvas_surface(ui: &egui::Ui, pos: egui::Pos2) -> bool {
    ui.ctx()
        .layer_id_at(pos)
        .is_none_or(|layer| layer.order <= ui.layer_id().order)
}

/// Drives the marquee box-select on the canvas: a left-drag that begins on empty canvas
/// draws a selection rectangle and, on release, selects every node it intersects (#84).
/// Ctrl/Cmd held adds to the existing selection rather than replacing it. Panning is on
/// the middle button (the snarl patch), so left-drag here is unambiguous.
fn handle_marquee(
    ui: &mut egui::Ui,
    state: &mut AppState,
    canvas_rect: egui::Rect,
    to_global: egui::emath::TSTransform,
    node_rects: &[(Handle, egui::Rect)],
    menu_open: bool,
) {
    let pointer = ui.input(|i| PointerDrag {
        primary_pressed: i.pointer.primary_pressed(),
        primary_released: i.pointer.primary_released(),
        press_origin: i.pointer.press_origin(),
        current: i.pointer.interact_pos(),
    });

    // The viewport panel stacked below the canvas is resizable, so its resize handle straddles
    // the canvas's bottom edge. Keep marquee starts out of that bottom grab strip, or dragging
    // the divider also begins a marquee (#120).
    let grab = ui.style().interaction.resize_grab_radius_side;
    let marquee_region = egui::Rect::from_min_max(
        canvas_rect.min,
        egui::pos2(canvas_rect.max.x, canvas_rect.max.y - grab),
    );

    // Decide whether to begin a marquee once, on the press-down edge, while the node is
    // still under the cursor. Testing every frame instead would misfire: dragging a node
    // moves it out from under the fixed press origin, so `node_at(origin)` would flip to
    // false mid-drag and start a marquee over the node being moved (#84).
    if state.marquee_start.is_none()
        && pointer.primary_pressed
        && !menu_open
        && let Some(origin) = pointer.press_origin
        && marquee_region.contains(origin)
        && over_canvas_surface(ui, origin)
        && !node_at(origin, node_rects, to_global)
    {
        state.marquee_start = Some(origin);
    }

    let Some(origin) = state.marquee_start else {
        return;
    };
    let Some(current) = pointer.current.or(Some(origin)) else {
        return;
    };

    // The marquee is only a marquee once the pointer has actually moved; below the
    // threshold it stays a (possibly aborted) click and draws nothing.
    let rect = egui::Rect::from_two_pos(origin, current);
    let is_drag = rect.width() > MARQUEE_MIN_DRAG || rect.height() > MARQUEE_MIN_DRAG;
    if is_drag {
        let visible = rect.intersect(canvas_rect);
        let painter = ui.painter_at(canvas_rect);
        let stroke = egui::Stroke::new(1.0, ui.visuals().selection.stroke.color);
        painter.rect_filled(
            visible,
            0.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.25),
        );
        painter.rect_stroke(visible, 0.0, stroke, egui::StrokeKind::Inside);
    }

    if pointer.primary_released {
        if is_drag {
            // Map the screen-space marquee into graph space and select every node whose
            // rect it overlaps.
            let graph_rect = egui::Rect::from_two_pos(
                to_global.inverse() * origin,
                to_global.inverse() * current,
            );
            let hits: Vec<Handle> = node_rects
                .iter()
                .filter(|(_, r)| graph_rect.intersects(*r))
                .map(|(h, _)| *h)
                .collect();
            let additive = ui.input(|i| i.modifiers.command);
            state.marquee_select(&hits, additive);
        }
        state.marquee_start = None;
    }
}

/// Snapshot of the pointer state a marquee needs in one frame, read in a single
/// `ui.input` closure.
struct PointerDrag {
    primary_pressed: bool,
    primary_released: bool,
    press_origin: Option<egui::Pos2>,
    current: Option<egui::Pos2>,
}

/// Moves the whole selection when one of its nodes is dragged. egui-snarl moves only the
/// node under the cursor (it keys group moves off its own selection, which the canvas does
/// not drive), so without this a multi-selection drag leaves the rest behind. On the press
/// that begins a no-modifier drag on a selected node, that node becomes the leader (snarl
/// moves it); every frame the drag is held, the other selected nodes follow by the same
/// delta. The pointer delta is screen space, so it is divided by the zoom to move nodes in
/// graph space.
fn handle_group_drag(
    ui: &egui::Ui,
    state: &mut AppState,
    to_global: egui::emath::TSTransform,
    node_rects: &[(Handle, egui::Rect)],
) {
    let (pressed, down, delta, origin, plain) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.delta(),
            i.pointer.press_origin(),
            !i.modifiers.shift && !i.modifiers.command,
        )
    });

    // Begin a group drag only when the press lands on an already-selected node that is part
    // of a multi-selection, with no modifier (matching snarl's own move gate). Decided once
    // on the press edge, since a drag moves the node out from under a later hit-test.
    if state.group_drag_leader.is_none()
        && pressed
        && plain
        && state.selection.len() > 1
        && let Some(origin) = origin
    {
        let graph_pos = to_global.inverse() * origin;
        if let Some((handle, _)) = node_rects
            .iter()
            .find(|(h, r)| r.contains(graph_pos) && state.selection.contains(h))
        {
            state.group_drag_leader = Some(*handle);
        }
    }

    let Some(leader) = state.group_drag_leader else {
        return;
    };
    if !down {
        state.group_drag_leader = None;
        return;
    }

    // Snarl already moved the leader by this frame's drag; move the rest by the same delta.
    let scale = to_global.scaling;
    if delta == egui::Vec2::ZERO || scale == 0.0 {
        return;
    }
    offset_selected_except(&mut state.snarl, &state.selection, leader, delta / scale);
}

/// Offsets every selected node except `leader` by `delta` (graph space). The leader is
/// skipped because snarl moves the dragged node itself, so it must not be moved twice.
fn offset_selected_except(
    snarl: &mut Snarl<Handle>,
    selection: &HashSet<Handle>,
    leader: Handle,
    delta: egui::Vec2,
) {
    // Collect ids first: iterating borrows the snarl, but moving a node needs it mutably.
    let ids: Vec<SnarlNodeId> = snarl
        .node_ids()
        .filter(|(_, h)| **h != leader && selection.contains(h))
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        if let Some(node) = snarl.get_node_info_mut(id) {
            node.pos += delta;
        }
    }
}

/// Width (graph units) of a frame's edge/corner resize bands, just inside the border (#94).
const FRAME_RESIZE_BAND: f32 = 8.0;
/// Smallest a frame can be resized to on either axis (graph units, #94).
const FRAME_MIN_SIZE: f32 = 60.0;

/// What part of a frame a press grabbed (#94): the title bar moves it, an edge or corner
/// resizes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameHandle {
    /// The title bar: moves the frame and its contents.
    Move,
    North,
    South,
    East,
    West,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

/// An in-progress frame gesture (#94): the frame, what was grabbed, and (for a move) the
/// nodes it carries.
struct FrameGesture {
    /// Index into [`AppState::frames`].
    index: usize,
    /// Which part of the frame is being dragged.
    handle: FrameHandle,
    /// Nodes contained when a *move* began, carried by the same delta. Empty for a resize.
    contained: Vec<Handle>,
}

/// A frame's bounds as an egui rect (graph space).
fn frame_rect(frame: &project_file::Frame) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(frame.rect[0], frame.rect[1]),
        egui::pos2(frame.rect[2], frame.rect[3]),
    )
}

/// The handle at `graph_pos` within `rect`: an edge/corner band (corners win where two
/// bands meet), else the title bar (the move handle), else `None` for the body interior or
/// outside the frame.
fn frame_handle_at(graph_pos: egui::Pos2, rect: egui::Rect) -> Option<FrameHandle> {
    if !rect.contains(graph_pos) {
        return None;
    }
    let west = graph_pos.x - rect.left() <= FRAME_RESIZE_BAND;
    let east = rect.right() - graph_pos.x <= FRAME_RESIZE_BAND;
    let north = graph_pos.y - rect.top() <= FRAME_RESIZE_BAND;
    let south = rect.bottom() - graph_pos.y <= FRAME_RESIZE_BAND;
    Some(match (west, east, north, south) {
        (true, _, true, _) => FrameHandle::NorthWest,
        (_, true, true, _) => FrameHandle::NorthEast,
        (true, _, _, true) => FrameHandle::SouthWest,
        (_, true, _, true) => FrameHandle::SouthEast,
        (true, _, _, _) => FrameHandle::West,
        (_, true, _, _) => FrameHandle::East,
        (_, _, true, _) => FrameHandle::North,
        (_, _, _, true) => FrameHandle::South,
        // Not on an edge band: the title bar moves the frame; the body is not a handle.
        _ if graph_pos.y - rect.top() <= canvas::FRAME_TITLE_H => FrameHandle::Move,
        _ => return None,
    })
}

/// The topmost frame and the handle at `graph_pos`, if any. Later frames draw on top, so
/// they are searched first.
fn frame_handle_hit(
    graph_pos: egui::Pos2,
    frames: &[project_file::Frame],
) -> Option<(usize, FrameHandle)> {
    frames
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, frame)| frame_handle_at(graph_pos, frame_rect(frame)).map(|h| (i, h)))
}

/// The cursor that signals what a frame handle does.
fn frame_cursor(handle: FrameHandle) -> egui::CursorIcon {
    match handle {
        FrameHandle::Move => egui::CursorIcon::Grab,
        FrameHandle::North | FrameHandle::South => egui::CursorIcon::ResizeVertical,
        FrameHandle::East | FrameHandle::West => egui::CursorIcon::ResizeHorizontal,
        FrameHandle::NorthWest | FrameHandle::SouthEast => egui::CursorIcon::ResizeNwSe,
        FrameHandle::NorthEast | FrameHandle::SouthWest => egui::CursorIcon::ResizeNeSw,
    }
}

/// Applies a resize `delta` (graph space) to `rect` (`[min_x, min_y, max_x, max_y]`) for the
/// dragged `handle`, moving the relevant edge(s) and clamping to [`FRAME_MIN_SIZE`].
fn resize_frame(rect: &mut [f32; 4], handle: FrameHandle, d: egui::Vec2) {
    use FrameHandle::{East, North, NorthEast, NorthWest, South, SouthEast, SouthWest, West};
    if matches!(handle, West | NorthWest | SouthWest) {
        rect[0] += d.x;
    }
    if matches!(handle, East | NorthEast | SouthEast) {
        rect[2] += d.x;
    }
    if matches!(handle, North | NorthWest | NorthEast) {
        rect[1] += d.y;
    }
    if matches!(handle, South | SouthWest | SouthEast) {
        rect[3] += d.y;
    }
    // Keep at least the minimum span: a moved edge cannot cross the opposite one.
    rect[0] = rect[0].min(rect[2] - FRAME_MIN_SIZE);
    rect[1] = rect[1].min(rect[3] - FRAME_MIN_SIZE);
    rect[2] = rect[2].max(rect[0] + FRAME_MIN_SIZE);
    rect[3] = rect[3].max(rect[1] + FRAME_MIN_SIZE);
}

/// Frame select, move, and resize (#94). A press on a frame's title bar selects and moves
/// it (carrying the nodes contained at drag-start); a press on an edge/corner resizes it;
/// nodes take precedence (drawn on top), and a press on the bare canvas clears the frame
/// selection. While not dragging, the hovered handle sets a matching cursor so the handles
/// are discoverable. Returns whether a frame owns this gesture, so the caller suppresses the
/// marquee.
fn handle_frame_interaction(
    ui: &egui::Ui,
    state: &mut AppState,
    canvas_rect: egui::Rect,
    to_global: egui::emath::TSTransform,
    node_rects: &[(Handle, egui::Rect)],
) -> bool {
    let (pressed, down, origin, delta, hover) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.delta(),
            i.pointer.hover_pos(),
        )
    });

    // Only presses on the canvas count: a press in the side panel (e.g. the frame
    // inspector's own controls) must not be read as a canvas click that deselects the
    // frame. `over_canvas_surface` only excludes floating windows, not the sibling panels.
    if pressed
        && let Some(origin) = origin
        && canvas_rect.contains(origin)
        && over_canvas_surface(ui, origin)
        && !node_at(origin, node_rects, to_global)
    {
        let graph_pos = to_global.inverse() * origin;
        if let Some((index, handle)) = frame_handle_hit(graph_pos, &state.frames) {
            // Select the frame (replacing any node selection). A move captures the nodes
            // whose centre is inside the frame right now; a resize carries no nodes.
            state.selected_frame = Some(index);
            state.clear_selection();
            let contained = if handle == FrameHandle::Move {
                let rect = frame_rect(&state.frames[index]);
                node_rects
                    .iter()
                    .filter(|(_, r)| rect.contains(r.center()))
                    .map(|(handle, _)| *handle)
                    .collect()
            } else {
                Vec::new()
            };
            state.frame_drag = Some(FrameGesture {
                index,
                handle,
                contained,
            });
        } else {
            state.selected_frame = None;
        }
    }

    // Apply or end an in-progress gesture. Copy the fields out first so the frames and snarl
    // can be mutated without the `frame_drag` borrow held.
    let active = state
        .frame_drag
        .as_ref()
        .map(|g| (g.index, g.handle, g.contained.clone()));
    if let Some((index, handle, contained)) = active {
        let scale = to_global.scaling;
        if !down {
            state.frame_drag = None;
        } else if delta != egui::Vec2::ZERO && scale != 0.0 {
            // The pointer delta is screen space; divide by the zoom to move in graph space.
            let d = delta / scale;
            if let Some(frame) = state.frames.get_mut(index) {
                if handle == FrameHandle::Move {
                    frame.rect[0] += d.x;
                    frame.rect[1] += d.y;
                    frame.rect[2] += d.x;
                    frame.rect[3] += d.y;
                } else {
                    resize_frame(&mut frame.rect, handle, d);
                }
            }
            if handle == FrameHandle::Move {
                for handle in &contained {
                    if let Some(snarl_id) = canvas::snarl_node_of(&state.snarl, *handle)
                        && let Some(node) = state.snarl.get_node_info_mut(snarl_id)
                    {
                        node.pos += d;
                    }
                }
            }
        }
        ui.ctx().set_cursor_icon(frame_cursor(handle));
    } else if let Some(hover) = hover
        && canvas_rect.contains(hover)
        && over_canvas_surface(ui, hover)
        && !node_at(hover, node_rects, to_global)
        && let Some((_, handle)) = frame_handle_hit(to_global.inverse() * hover, &state.frames)
    {
        // Not dragging: show the handle's cursor so resize/move zones are discoverable.
        ui.ctx().set_cursor_icon(frame_cursor(handle));
    }

    state.frame_drag.is_some()
}

/// Draws the node-rename dialog when open, and applies its result. A no-op when the
/// dialog is closed (#61).
fn rename_dialog_ui(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(dialog) = state.rename.as_mut() else {
        return;
    };

    let mut apply = false;
    let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));

    egui::Window::new("Rename node")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label("Display name (empty reverts to the type name):");
            let resp = ui.add(egui::TextEdit::singleline(&mut dialog.text).desired_width(220.0));
            if dialog.just_opened {
                resp.request_focus();
                dialog.just_opened = false;
            }
            // Enter in the field confirms.
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                apply = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

    if apply {
        // Read out the Copy/clone values so the dialog borrow ends before the graph
        // and the dialog state are mutated.
        let target = dialog.target;
        let name = name_override(&dialog.text);
        if let Some(id) = state.graph.node_id_of(target)
            && let Err(err) = state.graph.set_name(id, name)
        {
            // Unreachable (the node existed when the menu opened); surface, never swallow.
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
        state.rename = None;
    } else if close {
        state.rename = None;
    }
}

/// Refreshes the cached build-quality outputs for the shown node. Recomputes the node's
/// build-resolution content key (cheap, no evaluation) and, only when that key changes, loads
/// the matching fields from the disk cache: a hit becomes the viewport's source, a miss leaves
/// `None` so the viewport falls back to the live preview. As the graph is edited the key drifts
/// and misses, so the viewport shows the live preview; after a Build, the new key hits and the
/// viewport shows build-quality terrain until the next edit.
fn refresh_viewport_build(state: &mut AppState) {
    let key = (|| {
        let id = state.graph.node_id_of(state.primary?)?;
        let res = state.build_res;
        let request = EvalRequest::new(res, res, Region::UNIT, state.seed)
            .with_world_extent(state.world_extent)
            .with_world_height(state.world_height);
        state.graph.output_key(id, &request).ok()
    })();
    let Some(key) = key else {
        state.viewport_build = None;
        return;
    };
    if state
        .viewport_build
        .as_ref()
        .is_some_and(|(k, _)| *k == key)
    {
        return; // already loaded for this key
    }
    state.viewport_build = state
        .field_store
        .as_ref()
        .and_then(|store| store.load(key))
        .map(|fields| (key, fields));
}

fn viewport_3d_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // The pane rect, captured before `show` consumes it, anchors the floating control HUD.
    let rect = ui.available_rect_before_wrap();

    // True world proportion: a height of 1.0 rises to (world_height / world_extent) over the
    // unit footprint. The exaggeration multiplies it, so 1x is real-world proportion.
    let true_proportion = (state.world_height / state.world_extent.max(f64::EPSILON)) as f32;
    let settings = viewport::ViewSettings {
        fixed_range: state.viewport_scale == shade::HeightScale::Fixed,
        vertical_scale: true_proportion * state.viewport_exaggeration,
    };
    // Prefer build-quality fields for the shown node when the disk cache has them (after a
    // Build of the unchanged graph), else the live preview field.
    refresh_viewport_build(state);
    let display = state.preview.display_output();
    let build_field = state
        .viewport_build
        .as_ref()
        .and_then(|(_, fields)| fields.get(display.min(fields.len().saturating_sub(1))));
    let showing_build = build_field.is_some();
    let field = build_field.or_else(|| state.preview.field());
    viewport::show(
        ui,
        &mut state.viewport_camera,
        field,
        settings,
        state.viewport_lighting,
        &mut state.viewport_mesh,
    );

    // A small control HUD overlaid at the top-left of the viewport. A temporary home: the
    // design calls for a vertical toolbar down the left edge, not yet built.
    let mut scale = state.viewport_scale;
    let mut exaggeration = state.viewport_exaggeration;
    let mut light = state.viewport_lighting;
    egui::Area::new(ui.id().with("viewport-hud"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_top() + egui::vec2(8.0, 8.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                // Whether the viewport is meshing the full build result or the coarse preview,
                // so it is clear which fidelity is on screen while tuning.
                ui.weak(if showing_build {
                    "Showing: build"
                } else {
                    "Showing: preview"
                })
                .on_hover_text(
                    "Build quality appears after a Build, until the graph changes; otherwise the live preview",
                );
                ui.horizontal(|ui| {
                    // Fixed shows true amplitude; Auto normalizes to fill the relief (and so
                    // hides amplitude). Mirrors the 2D preview's Auto/Fixed toggle.
                    ui.selectable_value(&mut scale, shade::HeightScale::Fixed, "Fixed")
                        .on_hover_text("Show true height (clips out of range)");
                    ui.selectable_value(&mut scale, shade::HeightScale::Auto, "Auto")
                        .on_hover_text("Stretch the field's actual range to fill the relief");
                });
                ui.horizontal(|ui| {
                    ui.label("Exaggeration").on_hover_text(
                        "Vertical exaggeration; 1x is real-world proportion (set by World height)",
                    );
                    ui.add(
                        egui::Slider::new(&mut exaggeration, 0.25..=8.0)
                            .logarithmic(true)
                            .fixed_decimals(2)
                            .custom_formatter(|v, _| format!("{v:.2}x")),
                    );
                });
                // Lighting tucks under a collapsing header so the HUD stays compact.
                ui.collapsing("Light", |ui| {
                    egui::Grid::new("viewport-light")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Azimuth")
                                .on_hover_text("Compass direction the sun comes from");
                            ui.add(
                                egui::Slider::new(&mut light.azimuth_deg, 0.0..=360.0)
                                    .fixed_decimals(0)
                                    .suffix("°"),
                            );
                            ui.end_row();
                            ui.label("Elevation")
                                .on_hover_text("Sun height above the horizon; low rakes the form");
                            ui.add(
                                egui::Slider::new(&mut light.elevation_deg, 0.0..=90.0)
                                    .fixed_decimals(0)
                                    .suffix("°"),
                            );
                            ui.end_row();
                            ui.label("Intensity");
                            ui.add(
                                egui::Slider::new(&mut light.intensity, 0.0..=2.0)
                                    .fixed_decimals(2),
                            );
                            ui.end_row();
                            ui.label("Ambient");
                            ui.add(
                                egui::Slider::new(&mut light.ambient, 0.0..=1.0).fixed_decimals(2),
                            );
                            ui.end_row();
                        });
                });
            });
        });
    state.viewport_scale = scale;
    state.viewport_exaggeration = exaggeration;
    state.viewport_lighting = light;
}
inventory::submit! { PaneKind { id: "viewport-3d", draw: viewport_3d_pane } }

// ---- layout description + fixed-panel backend -------------------------------

/// The footer (Section 5): status, hints, and context. A placeholder for now.
fn footer_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.add_space(MENU_VPAD);
        ui.weak("Ready");
    });
}

inventory::submit! { PaneKind { id: "footer", draw: footer_pane } }

/// Which pane kind (by id) fills each slot of the default layout. This is the
/// data the v1 backend reads; a future workspace tree and docking backend will
/// replace the fixed slots without touching any pane internals.
struct Layout {
    menu_bar: &'static str,
    /// Pane 1: node category tabs, Build, and (later) the node icon grid.
    palette: &'static str,
    canvas: &'static str,
    /// The main viewport, stacked with the canvas in the workspace.
    viewport: &'static str,
    /// The right column: tabbed Node (preview + inspector) / World (settings).
    right_panel: &'static str,
    footer: &'static str,
}

fn default_layout() -> Layout {
    Layout {
        menu_bar: "menu-bar",
        palette: "ribbon",
        canvas: "canvas",
        viewport: "viewport-3d",
        right_panel: "right-panel",
        footer: "footer",
    }
}

/// The v1 layout backend: mounts the panes named by `layout` into the five-section
/// structure (menu, workspace, side column, footer, beneath the OS title bar).
///
/// Separation between panes comes mostly from their fills, not lines, so egui's automatic
/// panel separators are turned off and only a few deliberate borders are drawn: a heavier
/// one between the workspace and the side column, and lighter ones above the footer, between
/// the node bar and the canvas, and between the canvas and the viewport.
fn mount(layout: &Layout, ui: &mut egui::Ui, state: &mut AppState) {
    let color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let line = egui::Stroke::new(1.0, color);
    let heavy = egui::Stroke::new(2.0, color);

    // Section 2: the menu strip, directly under the OS title bar (Section 1).
    let menu = egui::Panel::top("menu-bar-panel")
        .show_separator_line(false)
        .show_inside(ui, |ui| draw_pane(layout.menu_bar, ui, state));

    // Section 5: the footer (its top border is drawn below), a bit darker than the body.
    let footer = egui::Panel::bottom("footer-panel")
        .show_separator_line(false)
        .frame(
            egui::Frame::side_top_panel(ui.style()).fill(scale_color(ui.visuals().panel_fill, 0.7)),
        )
        .show_inside(ui, |ui| draw_pane(layout.footer, ui, state));

    // Section 4: the right column — the preview over the inspector/world tabs. Fixed width
    // (not resizable): sized to the square preview image plus a small margin, since the
    // contents do not reflow nicely at other widths. Halved horizontal padding (4 here plus
    // the preview's 4) so the image sits close to the edges.
    let section_4 = egui::Panel::right("section-4")
        // Not resizable: exact_size alone leaves the panel resizable, so the edge still shows
        // a resize cursor even though dragging does nothing.
        .resizable(false)
        .exact_size(260.0)
        .show_separator_line(false)
        .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(4, 2)))
        .show_inside(ui, |ui| draw_pane(layout.right_panel, ui, state));

    // Section 3: the workspace. Palette on top, then the canvas and the main viewport
    // stacked. No frame margin, so the canvas hugs the section borders.
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            let palette = egui::Panel::top("palette-panel")
                .show_separator_line(false)
                // No inner margin (the ribbon draws its own full-width bands), filled to match
                // the category band so the spacing between the two bands does not show the
                // darker default fill through the gap.
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .fill(scale_color(ui.visuals().panel_fill, 1.5))
                        .inner_margin(0),
                )
                .show_inside(ui, |ui| draw_pane(layout.palette, ui, state));
            let viewport = egui::Panel::bottom("viewport-panel")
                .resizable(true)
                .default_size(ui.available_height() * 0.4)
                .show_separator_line(false)
                .show_inside(ui, |ui| draw_pane(layout.viewport, ui, state));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| draw_pane(layout.canvas, ui, state));

            // A heavier border under the node bar (like the workspace/side-column one), and
            // a lighter one between the canvas and the viewport.
            let painter = ui.painter();
            painter.hline(
                palette.response.rect.x_range(),
                palette.response.rect.bottom(),
                heavy,
            );
            painter.hline(
                viewport.response.rect.x_range(),
                viewport.response.rect.top(),
                line,
            );
        });

    // Section borders, drawn last so they sit on top: the full-width line under the menu,
    // the footer's top edge, and the heavier workspace/side-column boundary.
    let painter = ui.painter();
    painter.hline(
        menu.response.rect.x_range(),
        menu.response.rect.bottom(),
        line,
    );
    painter.hline(
        footer.response.rect.x_range(),
        footer.response.rect.top(),
        line,
    );
    painter.vline(
        section_4.response.rect.left(),
        section_4.response.rect.y_range(),
        heavy,
    );
}

// ---- app shell --------------------------------------------------------------

struct YmirApp {
    state: AppState,
    /// The window title last pushed to the OS, so it is only re-sent when it changes (the
    /// OS title bar is Section 1 of the layout, not an in-app strip).
    window_title: String,
}

impl YmirApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Install the Phosphor icon font as a fallback in the proportional family, so an
        // icon const (a disclosure caret, etc.) renders anywhere text does. The default
        // egui font has no triangle glyphs, which is why the node menu used ASCII chevrons.
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        // Calmer, more deliberate menus (#63, #64). Two global dials: a touch more
        // animation so hover highlights glide and popups ease open/closed instead of
        // snapping, and more menu inner margin so text is not jammed against the
        // border (where the pointer's corner sits). Menu row height is set per-menu
        // (button_padding) so the ribbon's buttons are not bloated.
        cc.egui_ctx.global_style_mut(|style| {
            // The Ymir Dark theme (#104): a depth ramp and visible borders so menus,
            // panels, and the canvas read as distinct surfaces instead of dark-on-dark.
            style.visuals = theme::visuals();
            style.animation_time = 0.15;
            style.spacing.menu_margin = egui::Margin::same(8);
        });
        // Build the env-free initial state (the built-in starter), then overlay the
        // user's saved default startup graph if one exists (#76). The overlay lives
        // here, not in AppState::new, so the state the tests construct never reads the
        // process environment or the filesystem.
        let mut state = AppState::new();
        apply_default(&mut state);
        // Load the recent-projects list from config (empty on first run), here in the app
        // shell so the test-constructed state never touches the filesystem.
        state.recent = load_recent();
        // Open the build cache's disk store (read view) for the viewport, in the app shell so
        // the test-constructed state never touches the filesystem.
        state.field_store = build::open_store();
        // Set up the 3D viewport's wgpu pipeline once, now that the wgpu device exists.
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            viewport::init(render_state);
        }
        // Empty so the first frame always pushes the real title to the OS bar.
        Self {
            state,
            window_title: String::new(),
        }
    }

    /// Reflects the current project (and a trailing "*" for unsaved changes) in the OS title
    /// bar, re-sending only when it changes so the platform is not spammed every frame.
    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let name = self
            .state
            .project_path
            .as_deref()
            .and_then(std::path::Path::file_name)
            .map_or_else(
                || "untitled".to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
        let marker = if self.state.modified { " *" } else { "" };
        let title = format!("Ymir — {name}{marker}");
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

/// Draws the popped-out curve editor as a floating, draggable, resizable window when one
/// is open (#70-style pop-out). It edits the specific node and param it was opened for,
/// independent of the current selection, and applies changes through the graph exactly
/// like the inline editor, so the same undo/preview machinery picks them up. A no-op when
/// closed; closing the window (or the node vanishing) clears the pop-out.
fn curve_popout_window(ctx: &egui::Context, state: &mut AppState) {
    let Some(popout) = state.curve_popout.clone() else {
        return;
    };
    let Some(id) = state.graph.node_id_of(popout.node) else {
        state.curve_popout = None;
        return;
    };

    // Title from the node's display-name override, else its translated type name.
    let title = match state.graph.spec(id) {
        Some(spec) => {
            let name = state
                .graph
                .name(id)
                .map(str::to_string)
                .unwrap_or_else(|| tr(&format!("node-{}", spec.type_id)).to_string());
            format!("Curve — {name}")
        }
        None => "Curve".to_string(),
    };

    // Snapshot the current curve and the previewed input histogram (owned, so neither
    // borrows `state` across the apply below).
    let identity = ymir_core::Curve::identity();
    let params = state.graph.params(id).cloned().unwrap_or_default();
    let curve = params.get_curve(&popout.param, &identity).clone();
    let histogram: Option<Vec<f32>> = state
        .preview
        .input_histogram(popout.node)
        .map(|h| h.to_vec());

    let mut open = true;
    egui::Window::new(title)
        // A stable id so the window keeps its position when the title changes (the title
        // carries the node's name, which is live-editable in the inspector; without this
        // each keystroke would read as a new window and re-auto-place it).
        .id(egui::Id::new("curve-popout-window"))
        .open(&mut open)
        .resizable(true)
        .default_size(egui::vec2(480.0, 520.0))
        .show(ctx, |ui| {
            // Absorb drags on the window body so only the title bar moves the window. egui
            // makes the whole window area a move handle, and the curve editor only senses
            // clicks, so a drag on the body (or empty editor space) would otherwise fall
            // through to that handle and drag the window. A drag-sensing guard over the body,
            // registered before the content so the curve's point handles (added after, on
            // top) keep their own drags, eats those drags; clicks still pass through to add a
            // point, since egui resolves the click target and drag target independently.
            ui.interact(
                ui.max_rect(),
                ui.id().with("curve-popout-body-drag-guard"),
                egui::Sense::drag(),
            );
            ui.weak("Drag a point to move it. Click empty space to add one, right-click a point to remove it.");
            ui.add_space(6.0);
            // A near-square editor that grows with the window, so there is real room to be
            // precise. Clamped so it never collapses or overruns a small window.
            let side = ui.available_width().clamp(280.0, 720.0);
            let size = egui::vec2(side, (side * 0.8).max(240.0));
            if let Some(new_curve) =
                curve_edit::curve_editor_sized(ui, &curve, histogram.as_deref(), size, true, None)
            {
                let mut next = params.clone();
                next.insert(popout.param.clone(), ParamValue::Curve(new_curve));
                if let Err(err) = state.graph.set_params(id, next) {
                    ui.colored_label(ui.visuals().error_fg_color, err.to_string());
                }
            }
        });

    if !open {
        state.curve_popout = None;
    }
}

impl eframe::App for YmirApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Undo/redo shortcuts run before the panes draw, so a restore is reflected in
        // this frame's render (#82). They are suppressed while a text field has focus, so
        // Ctrl+Z keeps editing text there instead of undoing the graph.
        handle_shortcuts(ui.ctx(), &mut self.state);
        self.sync_window_title(ui.ctx());
        // Evaluate the preview before the panes draw, so the 3D viewport and 2D preview both
        // render this frame's result regardless of which pane or tab is visible.
        self.state.drive_preview(ui.ctx());
        mount(&default_layout(), ui, &mut self.state);
        // The popped-out curve editor floats over the panes when open (#70-style).
        curve_popout_window(ui.ctx(), &mut self.state);
        // Intercept a window close with unsaved changes: cancel it and raise the prompt
        // (#83). An already-confirmed close (allow_close) or a clean session goes through.
        if ui.ctx().input(|i| i.viewport().close_requested())
            && self.state.modified
            && !self.state.allow_close
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.state.pending_action = Some(PendingAction::Quit);
        }
        // The unsaved-changes prompt draws on top of the panes when a discard action is
        // pending (#83).
        unsaved_changes_dialog(ui.ctx(), &mut self.state);
        // Record an edit at the end of the frame, once the panes have applied it.
        self.state.sync_history(ui.ctx());
    }
}

/// Applies the global keyboard shortcuts: undo/redo (Ctrl/Cmd+Z, +Shift or +Y),
/// Select All (Ctrl/Cmd+A), and Delete/Backspace to remove the selection. A no-op while
/// a text field has keyboard focus, so it never steals that field's own editing keys.
fn handle_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    use egui::{Key, KeyboardShortcut, Modifiers};
    let undo = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
    let redo_z = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    let redo_y = KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
    let select_all = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);
    // Check redo first: its Shift+Z would otherwise also satisfy the plain-Z undo.
    if ctx.input_mut(|i| i.consume_shortcut(&redo_z) || i.consume_shortcut(&redo_y)) {
        state.redo();
    } else if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
        state.undo();
    }
    if ctx.input_mut(|i| i.consume_shortcut(&select_all)) {
        state.select_all();
    }
    // Delete and Backspace remove the selected frame if one is selected (leaving its nodes
    // in place, #94), otherwise the selected nodes.
    if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
        if let Some(index) = state.selected_frame.take() {
            if index < state.frames.len() {
                state.frames.remove(index);
            }
        } else {
            state.delete_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registries_populate_the_palette() {
        // Proves ymir-nodes is linked: its operator and category registrations
        // are visible. If 0, the crate did not link.
        assert!(
            node_entries().len() >= 3,
            "operators not registered in the GUI"
        );
        assert!(
            categories_sorted().len() >= 3,
            "categories not registered in the GUI"
        );
    }

    /// Builds an Input -> Output inner graph for a subgraph, via the registry and public
    /// graph API (the GUI never names the concrete subgraph types).
    #[cfg(test)]
    fn identity_inner() -> ymir_core::Graph {
        let mut inner = ymir_core::Graph::new();
        let i = inner.add_op(
            ymir_core::registry::make("subgraph.input").expect("input op"),
            ymir_core::Params::default(),
        );
        let o = inner.add_op(
            ymir_core::registry::make("subgraph.output").expect("output op"),
            ymir_core::Params::default(),
        );
        inner.connect(i, 0, o, 0).expect("wire inner");
        inner
    }

    #[test]
    fn diving_into_a_subgraph_and_exiting_folds_inner_edits_back() {
        let mut state = AppState::new();
        state.new_project(); // empty top-level canvas

        // A subgraph container with an Input -> Output inner graph.
        let sg = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "subgraph",
            egui::Pos2::ZERO,
        )
        .expect("add subgraph");
        let handle = state.graph.stable_id(sg).expect("handle");
        state
            .graph
            .set_nested(sg, identity_inner())
            .expect("install inner");
        assert_eq!(state.graph.spec(sg).expect("spec").inputs.len(), 1);

        // Dive in: the inner graph becomes the active canvas.
        state.dive_in(handle);
        assert_eq!(state.nav.len(), 1, "one level deep");
        assert_eq!(
            state.graph.node_count(),
            2,
            "active graph is the inner graph"
        );

        // Add a second Input marker inside the subgraph.
        canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "subgraph.input",
            egui::Pos2::ZERO,
        )
        .expect("add inner input");

        // Exit: the edit folds back into the container, which now has two input ports.
        state.exit_subgraph();
        assert_eq!(state.nav.len(), 0, "back at the top");
        let sg = state.graph.node_id_of(handle).expect("container survives");
        assert_eq!(
            state.graph.spec(sg).expect("spec").inputs.len(),
            2,
            "the inner edit folded back into the container"
        );
    }

    #[test]
    fn snapshot_reflects_the_top_project_while_dived_in() {
        let mut state = AppState::new();
        state.new_project();
        let sg = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "subgraph",
            egui::Pos2::ZERO,
        )
        .expect("add subgraph");
        let handle = state.graph.stable_id(sg).expect("handle");

        state.dive_in(handle);
        // Edit inside the (empty) subgraph: add one Input marker.
        canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "subgraph.input",
            egui::Pos2::ZERO,
        )
        .expect("add inner input");

        // The snapshot (the unit save/undo/dirty use) is the whole project, with the
        // container's inner graph carrying the edit, even though the canvas shows the inner.
        let snap = state.snapshot();
        let container = snap
            .graph
            .nodes
            .iter()
            .find(|n| n.stable_id == handle)
            .expect("container in snapshot");
        let inner = container.subgraph.as_ref().expect("inner graph captured");
        assert_eq!(
            inner.nodes.len(),
            1,
            "the inner edit is in the top snapshot"
        );
    }

    #[test]
    fn subgraph_layout_is_remembered_across_a_re_dive() {
        let mut state = AppState::new();
        state.new_project();
        let sg = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "subgraph",
            egui::Pos2::ZERO,
        )
        .expect("add subgraph");
        let handle = state.graph.stable_id(sg).expect("handle");
        state
            .graph
            .set_nested(sg, identity_inner())
            .expect("install inner");

        // Dive in, move the first inner node to a distinctive spot, exit.
        state.dive_in(handle);
        let (snarl_id, inner_handle) = state
            .snarl
            .node_ids()
            .map(|(id, &h)| (id, h))
            .next()
            .expect("an inner node");
        let moved = egui::Pos2::new(321.0, 123.0);
        if let Some(info) = state.snarl.get_node_info_mut(snarl_id) {
            info.pos = moved;
        }
        state.exit_subgraph();

        // Re-dive: the moved node returns to where it was left.
        state.dive_in(handle);
        let re_id = canvas::snarl_node_of(&state.snarl, inner_handle).expect("node back");
        let pos = state.snarl.get_node_info(re_id).expect("info").pos;
        assert!(
            (pos - moved).length() < 1e-3,
            "inner layout remembered across re-dive"
        );
    }

    #[test]
    fn categories_are_sorted_by_sort_then_id() {
        let ids: Vec<&str> = categories_sorted().iter().map(|c| c.id).collect();
        // sort 0, 10, 20, 25, 30, 40, 50, 90
        assert_eq!(
            ids,
            [
                "generator",
                "selector",
                "adjust",
                "filter",
                "combine",
                "geology",
                "utility",
                "output"
            ]
        );
    }

    #[test]
    fn nodes_filter_by_category() {
        let entries = node_entries();
        let generators = visible_nodes(&entries, Some(ActiveTab::Category("generator")), "");
        assert!(generators.iter().all(|e| e.category == "generator"));
        assert!(generators.iter().any(|e| e.type_id == "generator.fbm"));
    }

    #[test]
    fn search_matches_display_names() {
        let entries = node_entries();
        // Matches on the display name ("fBm Noise", "Thermal Erosion"), case-insensitive.
        assert!(
            visible_nodes(&entries, None, "fbm")
                .iter()
                .any(|e| e.type_id == "generator.fbm")
        );
        assert!(
            visible_nodes(&entries, None, "thermal")
                .iter()
                .any(|e| e.type_id == "modifier.thermal_erosion")
        );
        // Nodes carry no search tags, so a word only in an old tag no longer matches.
        assert!(visible_nodes(&entries, None, "talus").is_empty());
        assert!(visible_nodes(&entries, None, "zzznotanode").is_empty());
    }

    #[test]
    fn search_ranks_exact_and_prefix_names_first() {
        let entries = node_entries();
        let ids: Vec<&str> = visible_nodes(&entries, None, "gradient")
            .iter()
            .map(|e| e.type_id)
            .collect();
        // "Gradient" is an exact name match; "Radial Gradient" only contains it, so the
        // exact match leads the substring match (#91, name-only ranking).
        let grad = ids
            .iter()
            .position(|&id| id == "generator.gradient")
            .expect("Gradient present");
        let radial = ids
            .iter()
            .position(|&id| id == "generator.radial")
            .expect("Radial Gradient present");
        assert!(grad < radial, "exact name should outrank a substring match");
    }

    #[test]
    fn all_operators_are_categorized() {
        assert!(!has_uncategorized_nodes());
    }

    #[test]
    fn canvas_view_maps_screen_to_graph() {
        // With a pan+zoom transform, the screen->graph map is the inverse: a 2x
        // zoom and a (100, 50) pan put graph-space (0,0) at screen (100, 50).
        let view = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(100.0, 50.0), 2.0),
            rect: egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(200.0, 200.0)),
        };
        assert_eq!(
            view.graph_pos(egui::pos2(100.0, 50.0)),
            egui::pos2(0.0, 0.0)
        );
        // The visible centre maps to the graph-space centre of the view.
        assert_eq!(view.center(), egui::pos2(50.0, 50.0));
    }

    #[test]
    fn a_degenerate_view_falls_back_to_the_screen_centre() {
        // A zero-scale transform inverts to non-finite coordinates; center() falls
        // back to the screen-space centre so placement stays finite (a NaN position
        // panics egui's layout) and on the canvas, not at a fixed off-screen origin.
        // Regression for the ribbon-add crash and the off-screen placement on a
        // fresh, not-yet-laid-out canvas.
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
        let view = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(0.0, 0.0), 0.0),
            rect,
        };
        assert!(view.center().is_finite());
        assert_eq!(view.center(), rect.center());
    }

    #[test]
    fn spawn_pos_anchors_on_the_visible_centre() {
        let view = CanvasView {
            to_global: egui::emath::TSTransform::IDENTITY,
            rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
        };
        // Near the visible centre (400, 300), not the fixed origin.
        let p = spawn_pos(Some(view), 0);
        assert!((p.x - (400.0 - 60.0)).abs() < 1e-3 && (p.y - (300.0 - 24.0)).abs() < 1e-3);
        // Without a view yet, falls back near the origin.
        let fallback = spawn_pos(None, 0);
        assert!(fallback.x < 100.0 && fallback.y < 100.0);
    }

    #[test]
    fn graph_pos_finite_falls_back_when_degenerate() {
        // The node menu places at an arbitrary cursor point; a degenerate transform
        // must not yield a non-finite position (which would panic egui's layout).
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let degenerate = CanvasView {
            to_global: egui::emath::TSTransform::new(egui::vec2(0.0, 0.0), 0.0),
            rect,
        };
        let p = degenerate.graph_pos_finite(egui::pos2(123.0, 456.0));
        assert!(p.is_finite());
        assert_eq!(p, degenerate.center());
        // A valid transform maps the point straight through.
        let view = CanvasView {
            to_global: egui::emath::TSTransform::IDENTITY,
            rect,
        };
        assert_eq!(
            view.graph_pos_finite(egui::pos2(123.0, 456.0)),
            egui::pos2(123.0, 456.0)
        );
    }

    #[test]
    fn menu_rows_browse_categories_then_drill_in() {
        let entries = node_entries();
        // Empty search, no drill: the top-level category list, in palette order.
        assert_eq!(
            menu_rows(&entries, "", None),
            [
                MenuRow::Category("generator"),
                MenuRow::Category("selector"),
                MenuRow::Category("adjust"),
                MenuRow::Category("filter"),
                MenuRow::Category("combine"),
                MenuRow::Category("geology"),
                MenuRow::Category("utility"),
                MenuRow::Category("output"),
            ]
        );
        // Drilled into a category: a Back row, then that category's nodes only.
        let drilled = menu_rows(&entries, "", Some("adjust"));
        assert_eq!(drilled[0], MenuRow::Back);
        assert!(drilled.contains(&MenuRow::Node("modifier.curve")));
        assert!(drilled.contains(&MenuRow::Node("modifier.invert")));
        // No node row outside the drilled category.
        assert!(!drilled.contains(&MenuRow::Node("generator.fbm")));
    }

    #[test]
    fn back_lands_on_the_category_just_left() {
        // Stepping back out of a drilled-in category highlights that category in the
        // top-level list (its row index there), not the top of the list (#89).
        let top = menu_rows(&node_entries(), "", None);
        for id in ["selector", "geology", "output"] {
            let idx = category_row_index(id);
            assert_eq!(
                top[idx],
                MenuRow::Category(id),
                "back from {id:?} should highlight its own category row"
            );
        }
        // An unknown id degrades to the first row rather than panicking.
        assert_eq!(category_row_index("does-not-exist"), 0);
    }

    #[test]
    fn menu_rows_search_is_flat_and_ignores_drill() {
        let entries = node_entries();
        // A non-empty search overrides drill state with flat node results across all
        // categories, and shows no Back row.
        let rows = menu_rows(&entries, "fbm", Some("adjust"));
        assert!(rows.contains(&MenuRow::Node("generator.fbm")));
        assert!(!rows.contains(&MenuRow::Back));
    }

    #[test]
    fn menu_row_text_is_bare_names_with_a_back_caret() {
        // Category carets are painted by `menu_row`, not baked into the text, so a category
        // and a node read as bare names; Back leads with the Phosphor left caret so it reads
        // as "up a level".
        let back = menu_row_text(MenuRow::Back);
        assert!(back.starts_with(egui_phosphor::regular::CARET_LEFT));
        assert!(back.ends_with("Back"));
        assert_eq!(menu_row_text(MenuRow::Category("adjust")), "Adjust");
        assert_eq!(menu_row_text(MenuRow::Node("modifier.invert")), "Invert");
    }

    #[test]
    fn preview_target_prefers_a_valid_pin_then_selection() {
        let mut state = AppState::new();
        let pos = egui::Pos2::ZERO;
        let gen_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        let out_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "endpoint.export", pos).unwrap();
        // Wire the generator into the endpoint so the generator is consumed (not a sink) and
        // the endpoint is not previewable: the graph then has no previewable sink, isolating
        // this test to the pin/selection precedence the sink fallback is checked separately.
        state.graph.connect(gen_id, 0, out_id, 0).unwrap();
        let generator = state.graph.stable_id(gen_id).unwrap();
        let endpoint = state.graph.stable_id(out_id).unwrap();

        // Nothing selected or pinned, and no previewable sink: no target.
        assert_eq!(state.preview_target(), None);

        // A previewable selection is the target; an endpoint (no output) is not.
        state.select_only(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.select_only(endpoint);
        assert_eq!(state.preview_target(), None);

        // A valid pin wins over the selection.
        state.preview_pin = Some(generator);
        state.select_only(endpoint);
        assert_eq!(state.preview_target(), Some(generator));

        // An invalid pin (an endpoint, or a missing node) is ignored, falling back to
        // the selection.
        state.preview_pin = Some(endpoint);
        state.select_only(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.preview_pin = Some(99_999);
        assert_eq!(state.preview_target(), Some(generator));
    }

    #[test]
    fn push_recent_dedupes_orders_and_caps() {
        use std::path::PathBuf;
        let mut list: Vec<PathBuf> = Vec::new();

        // Most recent goes to the front.
        push_recent(&mut list, PathBuf::from("a.ymir"));
        push_recent(&mut list, PathBuf::from("b.ymir"));
        assert_eq!(list, [PathBuf::from("b.ymir"), PathBuf::from("a.ymir")]);

        // Re-opening an existing entry moves it to the front, without duplicating.
        push_recent(&mut list, PathBuf::from("a.ymir"));
        assert_eq!(list, [PathBuf::from("a.ymir"), PathBuf::from("b.ymir")]);

        // The list never grows past RECENT_MAX; the oldest drops off.
        for i in 0..RECENT_MAX + 3 {
            push_recent(&mut list, PathBuf::from(format!("p{i}.ymir")));
        }
        assert_eq!(list.len(), RECENT_MAX);
        assert_eq!(list[0], PathBuf::from(format!("p{}.ymir", RECENT_MAX + 2)));
    }

    #[test]
    fn preview_target_falls_back_to_the_sink_when_nothing_is_selected() {
        let mut state = AppState::new();
        // A clean two-node chain so the sink is unambiguous: generator -> modifier.
        state.graph = Graph::new();
        state.snarl = Snarl::new();
        let pos = egui::Pos2::ZERO;
        let gen_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        let mod_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "modifier.invert", pos).unwrap();
        state.graph.connect(gen_id, 0, mod_id, 0).unwrap();
        let generator = state.graph.stable_id(gen_id).unwrap();
        let modifier = state.graph.stable_id(mod_id).unwrap();

        // Nothing pinned or selected: the previewable sink (the downstream modifier, which
        // feeds nothing) is the target, so the graph previews its result on its own.
        assert_eq!(state.preview_target(), Some(modifier));

        // A selection and a pin still take precedence over the sink fallback.
        state.select_only(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.preview_pin = Some(generator);
        state.clear_selection();
        assert_eq!(state.preview_target(), Some(generator));
    }

    #[test]
    fn fit_view_centres_and_scales_the_node_bounds() {
        // Two 100x100 nodes spanning a 300x300 bounds centred at (150, 150).
        let rects = [
            (
                1u64,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
            ),
            (
                2u64,
                egui::Rect::from_min_size(egui::pos2(200.0, 200.0), egui::vec2(100.0, 100.0)),
            ),
        ];
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 600.0));
        let t = fit_view(&rects, canvas, 0.1, 4.0).expect("fit");

        // scale = (600/300) * 0.85 margin = 1.7, within the clamp.
        assert!((t.scaling - 1.7).abs() < 1e-4);
        // The bounds centre maps to the canvas centre.
        let mapped = t * egui::pos2(150.0, 150.0);
        assert!((mapped - egui::pos2(300.0, 300.0)).length() < 1e-3);

        // The clamp caps the scale for a tiny graph.
        let tiny = [(
            1u64,
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
        )];
        assert_eq!(fit_view(&tiny, canvas, 0.1, 2.0).expect("fit").scaling, 2.0);

        // No nodes: nothing to frame.
        assert!(fit_view(&[], canvas, 0.1, 4.0).is_none());
    }

    #[test]
    fn name_override_is_none_when_blank_else_the_raw_text() {
        assert_eq!(name_override(""), None);
        assert_eq!(name_override("   "), None);
        assert_eq!(
            name_override("Base Terrain"),
            Some("Base Terrain".to_string())
        );
        // Non-blank text is stored raw (surrounding spaces preserved, not trimmed).
        assert_eq!(name_override("  Hi "), Some("  Hi ".to_string()));
    }

    #[test]
    fn config_path_prefers_xdg_then_home_then_none() {
        use std::ffi::OsString;
        use std::path::PathBuf;
        // XDG_CONFIG_HOME wins when set and non-empty.
        assert_eq!(
            config_path(
                Some(OsString::from("/xdg")),
                Some(OsString::from("/home/u")),
                "default.ymir"
            ),
            Some(PathBuf::from("/xdg/ymir/default.ymir"))
        );
        // An empty XDG value falls through to HOME/.config.
        assert_eq!(
            config_path(
                Some(OsString::new()),
                Some(OsString::from("/home/u")),
                "default.ymir"
            ),
            Some(PathBuf::from("/home/u/.config/ymir/default.ymir"))
        );
        // No XDG: HOME/.config.
        assert_eq!(
            config_path(None, Some(OsString::from("/home/u")), "default.ymir"),
            Some(PathBuf::from("/home/u/.config/ymir/default.ymir"))
        );
        // Neither set (or both empty): no path, so the feature is unavailable.
        assert_eq!(config_path(None, None, "default.ymir"), None);
        assert_eq!(
            config_path(Some(OsString::new()), Some(OsString::new()), "default.ymir"),
            None
        );
    }

    #[test]
    fn write_then_read_project_round_trips_through_a_file() {
        // Exercises the real file I/O wrappers (the in-memory serde path is covered in
        // project_file): write a session to disk, read it back, confirm it matches.
        let (graph, snarl) = starter::starter_graph();
        let file = project_file::ProjectFile::capture(&graph, &snarl, 7, 2048.0, 640.0, &[]);
        let path =
            std::env::temp_dir().join(format!("ymir-default-test-{}.ymir", std::process::id()));

        write_project(&path, &file).expect("write project");
        let restored = read_project(&path).expect("read project");
        std::fs::remove_file(&path).expect("remove temp file");

        assert_eq!(restored.graph.to_document(), graph.to_document());
        assert_eq!(restored.seed, 7);
        assert_eq!(restored.world_extent, 2048.0);
        assert_eq!(restored.world_height, 640.0);
    }

    #[test]
    fn opening_with_unsaved_changes_defers_to_the_prompt() {
        // With unsaved changes, Open does not open immediately; it raises the prompt.
        let mut state = AppState::new();
        state.modified = true;
        request_open(&mut state);
        assert_eq!(state.pending_action, Some(PendingAction::Open));
    }

    #[test]
    fn new_with_unsaved_changes_defers_to_the_prompt() {
        let mut state = AppState::new();
        state.modified = true;
        request_new(&mut state);
        assert_eq!(state.pending_action, Some(PendingAction::New));
    }

    #[test]
    fn new_project_yields_an_empty_untitled_canvas() {
        let mut state = AppState::new();
        state.modified = true;
        state.project_path = Some(std::path::PathBuf::from("somewhere.ymir"));
        state.new_project();
        assert_eq!(state.graph.node_count(), 0); // a blank canvas
        assert!(state.project_path.is_none());
        assert!(!state.modified);
    }

    #[test]
    fn open_default_is_untitled_and_clean() {
        // Open Default loads the default startup graph, whose contents depend on the
        // machine's saved default, so only the untitled/clean properties are asserted.
        let mut state = AppState::new();
        state.modified = true;
        state.project_path = Some(std::path::PathBuf::from("somewhere.ymir"));
        state.open_default();
        assert!(state.project_path.is_none());
        assert!(!state.modified);
    }

    #[test]
    fn selection_set_and_primary_track_clicks() {
        let mut state = AppState::new();
        // A plain click selects exactly one node and makes it primary.
        state.select_only(1);
        assert!(state.selection.contains(&1));
        assert_eq!(state.primary, Some(1));

        // Ctrl-click adds another and makes it the primary.
        state.toggle_selection(2);
        assert!(state.selection.contains(&1) && state.selection.contains(&2));
        assert_eq!(state.primary, Some(2));

        // Ctrl-click the primary removes it; primary moves to a remaining member.
        state.toggle_selection(2);
        assert!(!state.selection.contains(&2));
        assert_eq!(state.primary, Some(1));

        // Removing the last selected node clears the primary.
        state.toggle_selection(1);
        assert!(state.selection.is_empty());
        assert_eq!(state.primary, None);

        // A plain click replaces the whole set.
        state.toggle_selection(5);
        state.toggle_selection(6);
        state.select_only(9);
        assert_eq!(state.selection.len(), 1);
        assert!(state.selection.contains(&9));
        assert_eq!(state.primary, Some(9));
    }

    #[test]
    fn marquee_select_replaces_or_adds() {
        let mut state = AppState::new();
        // A non-additive marquee replaces the whole selection with its hits.
        state.select_only(1);
        state.marquee_select(&[5, 6], false);
        assert_eq!(state.selection.len(), 2);
        assert!(state.selection.contains(&5) && state.selection.contains(&6));
        assert!(!state.selection.contains(&1));
        // The primary becomes the last hit.
        assert_eq!(state.primary, Some(6));

        // An additive marquee unions its hits into the existing set, keeping the
        // primary when it stays selected.
        state.marquee_select(&[7], true);
        assert_eq!(state.selection.len(), 3);
        assert!(state.selection.contains(&7));
        assert_eq!(state.primary, Some(6));

        // An empty marquee with no prior primary picks nothing.
        state.clear_selection();
        state.marquee_select(&[], false);
        assert!(state.selection.is_empty());
        assert_eq!(state.primary, None);
    }

    #[test]
    fn group_drag_moves_selected_nodes_except_the_leader() {
        let mut snarl: Snarl<Handle> = Snarl::new();
        let leader = snarl.insert_node(egui::pos2(0.0, 0.0), 1);
        let follower = snarl.insert_node(egui::pos2(10.0, 0.0), 2);
        let unselected = snarl.insert_node(egui::pos2(20.0, 0.0), 3);

        // The selection contains the leader; the leader is skipped (snarl moves it itself),
        // the other selected node follows, and the unselected node is untouched.
        let selection: HashSet<Handle> = [1, 2].into_iter().collect();
        offset_selected_except(&mut snarl, &selection, 1, egui::vec2(5.0, -3.0));

        assert_eq!(
            snarl.get_node_info(leader).unwrap().pos,
            egui::pos2(0.0, 0.0)
        );
        assert_eq!(
            snarl.get_node_info(follower).unwrap().pos,
            egui::pos2(15.0, -3.0)
        );
        assert_eq!(
            snarl.get_node_info(unselected).unwrap().pos,
            egui::pos2(20.0, 0.0)
        );
    }

    #[test]
    fn select_all_then_delete_clears_the_graph() {
        let mut state = AppState::new(); // the starter chain: 3 nodes
        state.select_all();
        assert_eq!(state.selection.len(), 3);
        assert!(state.primary.is_some());

        state.delete_selection();
        assert_eq!(state.graph.node_count(), 0);
        assert!(state.selection.is_empty());
        assert!(state.primary.is_none());
    }

    #[test]
    fn delete_selection_removes_only_selected_nodes() {
        let mut state = AppState::new(); // 3 nodes
        let one = state.snarl.node_ids().next().map(|(_, &h)| h).unwrap();
        state.select_only(one);
        state.delete_selection();
        assert_eq!(state.graph.node_count(), 2);
        assert!(state.selection.is_empty());
    }

    #[test]
    fn mark_clean_resets_the_modified_state() {
        let mut state = AppState::new();
        state.modified = true;
        state.mark_clean();
        assert!(!state.modified);
        // The clean point now equals the current session, so it reads as unmodified.
        assert_eq!(state.saved_snapshot, state.snapshot());
    }

    #[test]
    fn pane_kinds_are_registered_and_unique() {
        let l = default_layout();
        for id in [
            l.menu_bar,
            l.palette,
            l.canvas,
            l.viewport,
            l.right_panel,
            l.footer,
        ] {
            assert!(pane_kind(id).is_some(), "pane {id:?} is not registered");
        }
        let mut ids: Vec<&str> = inventory::iter::<PaneKind>().map(|p| p.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate pane-kind id");
    }
}

#[cfg(test)]
mod icon_test {
    #[test]
    fn app_icon_decodes_to_rgba() {
        let icon = super::app_icon().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }
}
