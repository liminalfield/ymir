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
use ymir_core::{
    BrushShape, EvalCache, EvalRequest, Extraction, Field, FieldStore, Graph, INPUT_TYPE_ID,
    NodeId, OUTPUT_TYPE_ID, ParamKind, ParamValue, Params, ProjectDocument, Region,
    SUBGRAPH_TYPE_ID, Stroke, StrokeMode, StrokePoint, Strokes, marker_port_label,
};
use ymir_nodes::{CategoryDef, categories, find_category, node_group, tr};

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

mod sun;

mod thumbnails;
use thumbnails::ThumbnailEngine;
// Off-thread full-resolution Build (#7).
mod build;
// The subgraph library: saved subgraphs as standalone files (#106).
mod library;
// The collapsible left dock hosting project-scoped panes, the library first (#106).
mod dock;

mod preferences;
// The GUI project file: graph + canvas view-state, save/open (#75).
mod project_file;
// The built-in starter graph a fresh session opens with (#76).
mod starter;
// The Ymir Dark brand palette and egui Visuals built from it (#104).
mod theme;
// The 3D viewport: custom wgpu rendering inside an egui pane (#7).
mod pick;
mod viewport;
mod viewport2d;
mod viewport2d_gpu;
// Snapshot-based undo/redo over the session (#82).
mod history;
use build::BuildRunner;
use history::EditHistory;

/// Resolution of the interactive 2D preview. Low for responsiveness; it is an
/// approximation of the target-resolution build, never equal to it (erosion is
/// resolution-dependent). The build resolution is decided later (step 6c).
const PREVIEW_RES: usize = 256;

fn main() -> eframe::Result {
    // Diagnostics (a project loaded with degradations, an evaluation problem) log to stderr and to
    // a logfile beside the other config, so an issue is recorded even when the status line is
    // missed. Stderr-only if the logfile path is unavailable.
    ymir_core::logging::init(log_path().as_deref(), log::LevelFilter::Info);

    // The window icon: `with_icon` is honoured on X11/Windows/macOS. On Wayland it is
    // ignored (no runtime icon protocol); there the icon comes from a `.desktop` entry
    // matched by `app_id`, so set a stable one. Falls back to eframe's default if the
    // PNG can't be decoded (it is cosmetic, never worth failing startup over).
    // Open at a workable default size rather than the tiny window some WMs otherwise give a
    // surfaceless app; remembering the last size/position across sessions is window state, deferred
    // to #127 (XDG_STATE). A minimum keeps the ribbon and both side panels usable.
    let viewport = egui::ViewportBuilder::default()
        .with_app_id("ymir")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([900.0, 600.0]);
    let viewport = match app_icon() {
        Some(icon) => viewport.with_icon(icon),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        // No shared depth on egui's pass: the 3D viewport renders to its own offscreen
        // color+depth and composites the result as a texture (#138), so egui's pass is
        // colour-only and the composite blit needs no depth attachment.
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

/// Inner padding for the always-visible search fields (the node palette and the subgraph
/// library), with more vertical room than egui's default so the text is not cramped against the
/// box borders.
const SEARCH_FIELD_MARGIN: egui::Margin = egui::Margin {
    left: 6,
    right: 6,
    top: 5,
    bottom: 5,
};

/// Inner padding for the ribbon's buttons (category tabs and node buttons), well above egui's
/// cramped default so each has whitespace on every side in both its resting and hovered state,
/// and the two ribbon bands stand a little taller. Padding is a minimum, so a tab grows past
/// `interact_size` to fit it too.
const RIBBON_BUTTON_PADDING: egui::Vec2 = egui::vec2(8.0, 5.0);

/// Top/bottom (and left/right) inner margin of each ribbon band, between its content and its
/// edges. A touch tighter vertically than the button padding above, so the bands are snug.
const RIBBON_BAND_MARGIN: egui::Margin = egui::Margin::symmetric(8, 4);

/// The content height of the top ribbon row (tabs / search / Build). Forcing it up front makes every
/// item in the row centre on the same height; without it the shorter tabs and the vertical divider
/// are placed before the taller search field and end up top-aligned.
const RIBBON_ROW_H: f32 = 26.0;

/// The height of the library inspector's thumbnail slot (the handoff's 150px landscape preview). It
/// spans the pane width. The offline subgraph render (a later step) will fill the same box (cover),
/// so reserving it now keeps the inspector from reflowing when it lands; until then it shows an empty
/// state.
const LIBRARY_THUMB_HEIGHT: f32 = 150.0;

/// Minimum drag (px) before a left-press on empty canvas counts as a marquee rather than
/// a click; below it, the press is the click that selects/clears (#84).
const MARQUEE_MIN_DRAG: f32 = 4.0;

/// Default physical size of the world along x, in meters. Pairs with the default
/// build resolution to give a clean 1 m/cell, and is the meters-to-cells bridge for
/// world-unit parameters (scale-aware nodes consume it via `EvalContext`).
const DEFAULT_WORLD_EXTENT: f64 = 1024.0;

/// The world settings for a fresh, untitled project: the app-level defaults, with the water plane
/// shown. Used to anchor a new session's clean point and to reset on New/Close/open-default.
fn fresh_world_settings() -> project_file::WorldSettings {
    project_file::WorldSettings {
        seed: 0,
        world_extent: DEFAULT_WORLD_EXTENT,
        world_height: project_file::DEFAULT_WORLD_HEIGHT,
        build_res: project_file::DEFAULT_BUILD_RES,
        preview_res: PREVIEW_RES,
        sea_level: project_file::DEFAULT_SEA_LEVEL,
        show_water: true,
        water: project_file::WaterSettings::default(),
    }
}

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
/// The real fields feeding the subgraph currently being edited, used to evaluate its
/// interior with live inputs instead of the Input markers' flat zero stand-in (#106), so a
/// node's thumbnail and the 2D preview show real data while diving in. Carries a clone of the
/// parent graph and, per inner Input marker, the parent source that feeds the matching
/// container port; bound fields are computed off-thread by the preview/thumbnail workers.
#[derive(Clone)]
pub(crate) struct SubgraphInputs {
    /// The immediate parent graph (a snapshot), evaluated to produce the input fields.
    parent: Graph,
    /// Per inner Input marker: `(marker handle, parent source stable_id, source output port)`.
    markers: Vec<(Handle, u64, usize)>,
}

impl SubgraphInputs {
    /// Evaluates each parent source at `request` and pairs the field with its inner Input
    /// marker's node id in `inner`, ready to bind via
    /// [`Graph::evaluate_bound`](ymir_core::Graph::evaluate_bound). A source that fails (or a
    /// marker no longer present) is skipped, so its marker falls back to its zero stand-in.
    /// Called on a worker thread, so the parent evaluation never blocks the UI.
    pub(crate) fn bound_fields(
        &self,
        inner: &Graph,
        request: &EvalRequest,
    ) -> Vec<(NodeId, Field)> {
        let mut cache = EvalCache::new(THUMB_INPUT_CACHE_CAP);
        self.markers
            .iter()
            .filter_map(|&(marker, source, output)| {
                let source_id = self.parent.node_id_of(source)?;
                let field = self
                    .parent
                    .evaluate(source_id, request, &mut cache)
                    .ok()?
                    .get(output)?
                    .clone();
                Some((inner.node_id_of(marker)?, field))
            })
            .collect()
    }
}

/// Worker-cache capacity for evaluating a subgraph's parent-side input fields.
const THUMB_INPUT_CACHE_CAP: usize = 64;

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
    /// The parent canvas's pan/zoom at dive time, restored on the way back so popping out
    /// returns to the same view rather than wherever the interior was last scrolled to.
    view: Option<egui::emath::TSTransform>,
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
    /// Set when the user clears the preview by clicking empty canvas, so the viewport goes blank
    /// rather than falling back to the graph's result node (`preview_sink`). Reset by any
    /// `clear_selection` (so a freshly opened or loaded graph still shows its result), then re-set
    /// only by the background-click deselect.
    preview_dismissed: bool,
    /// The global seed for evaluation, set by the ribbon control. Reseeds the whole
    /// world; each node stays internally stable across edits.
    seed: u64,
    /// Background preview evaluation: submits graph snapshots and shows results
    /// without ever blocking the UI thread.
    preview: PreviewEngine,
    /// Background per-node thumbnail evaluation, drawn in node bodies (#42).
    thumbnails: ThumbnailEngine,
    /// Whether node thumbnails are shown. When off, no thumbnails are evaluated, uploaded, or
    /// drawn. The View-menu toggle that flipped this is temporarily hidden (it caused a flashing
    /// build-status frame on every node when switched; see the tracking issue), so this stays at
    /// its `true` default. The capability is kept so the toggle can be restored once that is fixed
    /// (#135).
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
    /// The "Save to library" dialog (#106), open while documenting a subgraph being saved.
    /// `None` when closed.
    library_save: Option<LibrarySave>,
    /// The active tab of the Save/Edit-to-library dialog, reset to Details when it opens.
    library_save_tab: LibraryTab,
    /// The subgraph library listing (#106): the saved subgraphs the left dock browses. Loaded
    /// from disk in the app shell (never in `AppState::new`) and refreshed after a save.
    library: library::LibraryListing,
    /// The left dock's open/collapsed state and active pane (#106).
    dock: dock::DockState,
    /// The library entry selected for the detail view, by its file path (#106). `None` when
    /// nothing is selected; a stale path (the file was removed by a reload) resolves to `None`
    /// at render time. Selecting an entry reveals its documentation and an Insert action.
    library_selection: Option<std::path::PathBuf>,
    /// The library entry whose Delete has been armed, by its file path (#106). The detail section
    /// shows a confirm prompt while this matches the selection; the second click removes the file.
    /// Cleared when the selection changes or the delete resolves, so it never lingers on another
    /// entry.
    library_pending_delete: Option<std::path::PathBuf>,
    /// The library browser's search query (#106). Filters the entry list by name, category, and
    /// description; empty shows the full category-grouped listing. Mirrors the node search.
    library_search: String,
    /// The user's app-global preferences (the author profile, #106), loaded from config at
    /// startup and edited via the Settings dialog. Persists across projects.
    preferences: preferences::Preferences,
    /// The Settings dialog's editable draft, `Some` while it is open. Committed to
    /// `preferences` (and written to disk) on Save, discarded on Cancel.
    settings_edit: Option<preferences::Preferences>,
    /// Whether the About window (Help -> About Ymir) is open. Transient UI state only.
    about_open: bool,
    /// The popped-out curve editor: a larger, draggable window for shaping a curve
    /// param with room to be precise and a coordinate readout. `None` when closed.
    curve_popout: Option<CurvePopout>,
    /// The current paint brush, edited in a Paint node's inspector.
    paint_brush: PaintBrush,
    /// The Paint node currently in paint mode (`stable_id`), so a drag on the 2D map brushes into
    /// its strokes. `None` when not painting.
    paint_target: Option<Handle>,
    /// A one-shot "zoom to graph" transform to apply on the next frame (#65). The
    /// fit is computed from this frame's node rects (collected during rendering) and
    /// applied via the canvas's `current_transform` override next frame.
    pending_view: Option<egui::emath::TSTransform>,
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
    /// The sea/base level as a normalized height in `[0, 1]`. A world global: the 3D viewport
    /// draws a water plane at it, and coastal/stream nodes will read it as base level once wired.
    sea_level: f64,
    /// Whether the 3D viewport draws the water plane at [`sea_level`](Self::sea_level). A view
    /// aid, on by default so the sea reads on a fresh launch.
    show_water: bool,
    /// Water effect layers (#157, #155): depth shading, Gerstner waves, a reflective finish (sky
    /// Fresnel + specular), and foam. Ephemeral view state. Off is cheaper and, for the animated
    /// layers (waves, foam), stops the viewport's per-frame repaint so still water idles the fans.
    water_depth: bool,
    water_waves: bool,
    water_reflection: bool,
    water_foam_on: bool,
    /// Water depth falloff (Beer-Lambert extinction) for the 3D viewport water (#154). Ephemeral
    /// view state for now (not persisted); a higher value clears to opaque faster.
    water_extinction: f32,
    /// Water tint (linear RGB) for the 3D viewport water. Ephemeral view state.
    water_color: [f32; 3],
    /// Tier 1 water surface controls (ephemeral): ripple strength, sky reflectivity, specular.
    water_wave: f32,
    water_reflectivity: f32,
    water_specular: f32,
    /// Gerstner wave shaping (#155): crest steepness and wavelength scale.
    water_steepness: f32,
    water_wavelength: f32,
    /// Shoreline foam controls (ephemeral): amount and band width (#156).
    water_foam: f32,
    water_foam_width: f32,
    /// Wet-shore darkening (#156): toggle, strength, and band width (normalized height).
    water_wet_on: bool,
    water_wet: f32,
    water_wet_width: f32,
    /// Water animation speed multiplier (#157), scaling how fast the ripples and foam scroll.
    /// Ephemeral view state; `0` freezes the surface.
    water_speed: f32,
    /// Accumulated water animation phase in seconds of motion. Advanced each frame by the real
    /// frame delta times [`water_speed`](Self::water_speed), so the speed control changes future
    /// motion without jumping the waves. Not persisted (it is a running clock, not a setting).
    water_phase: f32,
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
    /// Which projection the main viewport draws: the 3D relief or the flat 2D map (#134).
    /// A view aid, not persisted; 3D by default.
    viewport_mode: viewport2d::Mode,
    /// The 2D map view's state (texture, pan, zoom, shading), used when `viewport_mode` is
    /// `TwoD`. Draws the same field the 3D view meshes, flat and pannable.
    viewport_2d: viewport2d::View2d,
    /// The 3D viewport's orbit camera, persisted across frames so the view holds as the
    /// previewed node changes.
    viewport_camera: viewport::OrbitCamera,
    /// eframe's wgpu render state (device, queue, renderer), stashed for the 2D map's GPU shading
    /// (#167). `None` in a headless/test build with no wgpu backend, where the map falls back to a
    /// black fill. Set by the app shell once the device exists, never by `AppState::new`.
    render_state: Option<eframe::egui_wgpu::RenderState>,
    /// Whether the 3D viewport shows true amplitude (Fixed) or normalizes to fill the relief
    /// (Auto). Fixed by default so terrain reads at its real height.
    viewport_scale: shade::HeightScale,
    /// The 3D viewport's vertical exaggeration: a multiplier on the true world proportion
    /// (`world_height / world_extent`). `1.0` shows real-world proportions; higher values
    /// exaggerate relief to inspect subtle terrain. A non-persisted view aid.
    viewport_exaggeration: f32,
    /// Free-fly camera speed (#161), in world units per second (the footprint is 1.0 wide). A
    /// non-persisted view preference.
    viewport_fly_speed: f32,
    /// The 3D viewport's sun direction and response (azimuth/elevation degrees, diffuse
    /// intensity, ambient fill). A non-persisted view aid; raking the sun low reads form.
    viewport_lighting: viewport::Lighting,
    /// The workspace layout mode (#): Split shows both panes over a draggable divider; Graph
    /// maximizes the node graph (the viewport collapses to a labeled bottom bar); Preview maximizes
    /// the 2D/3D viewport (the graph collapses to a labeled top bar). Set by the top-right layout
    /// switcher and by clicking a collapsed bar to restore.
    workspace_mode: WorkspaceMode,
    /// The mode drawn last frame, so [`mount`] can tell when the mode just changed and force the
    /// viewport panel to its target height for that frame (egui otherwise reloads its own persisted
    /// size, which would leave the divider at the previous mode's extreme). `None` before the first
    /// frame, so the initial frame forces the default split rather than inheriting a stale size.
    workspace_mode_prev: Option<WorkspaceMode>,
    /// The remembered viewport fraction of the workspace body while in Split, so restoring from a
    /// maximized mode reopens the divider at the position it last had. Updated from divider drags.
    viewport_frac: f32,
    /// The viewport panel height at the start of the current collapse/expand animation, the value it
    /// eases from toward the new mode's target.
    viewport_anim_from: f32,
    /// The egui time (seconds) the current collapse/expand animation began, so its progress is a
    /// function of elapsed time rather than a frame count.
    viewport_anim_start: f64,
    /// The viewport panel height rendered last frame, captured as the `from` value when a mode
    /// change starts an animation.
    viewport_last_h: f32,
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

/// Where the subgraph a save dialog will write comes from: a live canvas container, or an
/// existing library file being edited in place (#106). The documentation is edited either way;
/// this is what supplies the graph, seed, and interior layout that the documentation wraps.
enum SubgraphSource {
    /// Saving a live container node (its `stable_id`): the graph, seed, and interior layout are
    /// read from the canvas.
    Container(Handle),
    /// Editing an existing library file: its graph, seed, and layout are preserved verbatim while
    /// only the documentation is edited. `original_path` is the file the entry came from, so an
    /// unchanged name overwrites it directly (no guard) and a renamed one removes it after writing.
    Existing {
        original_path: std::path::PathBuf,
        graph: ymir_core::ProjectDocument,
        seed: i64,
        view: project_file::ViewState,
    },
}

impl SubgraphSource {
    /// The file this source was loaded from, if it is an edit of an existing entry. `None` for a
    /// fresh save of a canvas container.
    fn original_path(&self) -> Option<&std::path::Path> {
        match self {
            SubgraphSource::Container(_) => None,
            SubgraphSource::Existing { original_path, .. } => Some(original_path),
        }
    }
}

/// The "Save to library" dialog (#106): a documentation template for a subgraph being saved or
/// edited, pre-filled from its source and edited by the author. `None` when closed.
/// The active tab of the Save/Edit-to-library dialog (design 1c). Kept on [`AppState`] so the
/// overwrite-reconfirm flow, which rebuilds the dialog, does not reset it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LibraryTab {
    #[default]
    Details,
    Ports,
    Attribution,
}

/// Common SPDX license ids offered as quick picks in the license combobox. The field stays free
/// text, so any other license (or none) is always typeable; this only shortcuts the frequent few.
const LICENSE_SUGGESTIONS: &[&str] = &[
    "CC0-1.0",
    "MIT",
    "Apache-2.0",
    "GPL-3.0-or-later",
    "GPL-3.0-only",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
    "Unlicense",
];

struct LibrarySave {
    /// Where the subgraph being documented comes from: a live container, or an existing entry.
    source: SubgraphSource,
    /// The library display name (defaults to the node's name).
    name: String,
    /// A free-text category for grouping in the browser.
    category: String,
    /// What the subgraph produces.
    description: String,
    /// Per-input-port documentation, pre-filled with the port names.
    inputs: Vec<library::PortDoc>,
    /// Per-output-port documentation, pre-filled with the port names.
    outputs: Vec<library::PortDoc>,
    /// The author identity, pre-filled from the user's profile and editable for this subgraph.
    author: preferences::AuthorProfile,
    /// A license for the subgraph (SPDX id or free text), blank by default.
    license: String,
    /// A save error to show in the dialog (e.g. a blank name or a write failure).
    error: Option<String>,
    /// Set once the user has been warned that this name already has a library file, so the next
    /// Save overwrites it instead of warning again. Re-armed (cleared) whenever the name is edited,
    /// so a fresh name is never silently overwritten.
    confirm_overwrite: bool,
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

/// The current paint brush, shared across paint sessions. Radius is a fraction of the region width
/// (canvas-relative), matching the Paint node's stroke model; strength is opacity, hardness the edge.
struct PaintBrush {
    radius: f32,
    strength: f32,
    hardness: f32,
    mode: StrokeMode,
}

impl Default for PaintBrush {
    fn default() -> Self {
        Self {
            radius: 0.08,
            strength: 1.0,
            hardness: 0.5,
            mode: StrokeMode::Paint,
        }
    }
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
        let initial =
            project_file::ProjectFile::capture(&graph, &snarl, fresh_world_settings(), &[]);
        let history = EditHistory::new(initial.clone());
        // The water look/effect defaults, taken from one source so the fresh-session fields below
        // and the persisted `WaterSettings::default` (used for older project files) stay in step.
        let water_defaults = project_file::WaterSettings::default();
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
            preview_dismissed: false,
            rename: None,
            library_save: None,
            library_save_tab: LibraryTab::Details,
            // Env-free defaults (empty listing, collapsed dock); the real library is loaded in
            // the app shell so the test-constructed state never touches the filesystem, matching
            // apply_default.
            library: library::LibraryListing::default(),
            dock: dock::DockState::default(),
            library_selection: None,
            library_pending_delete: None,
            library_search: String::new(),
            // Env-free default (empty profile); the real file is overlaid in the app shell so
            // the test-constructed state never touches the filesystem, matching apply_default.
            preferences: preferences::Preferences::default(),
            settings_edit: None,
            about_open: false,
            curve_popout: None,
            paint_brush: PaintBrush::default(),
            paint_target: None,
            pending_view: None,
            build_res: project_file::DEFAULT_BUILD_RES,
            preview_res: PREVIEW_RES,
            world_extent: DEFAULT_WORLD_EXTENT,
            world_height: project_file::DEFAULT_WORLD_HEIGHT,
            sea_level: project_file::DEFAULT_SEA_LEVEL,
            show_water: true,
            // Water look and effect defaults, taken from the persisted form's `Default` so a fresh
            // session and a project saved without water settings can never drift apart (#157). All
            // layers on, a calm speed. The phase is a running clock, started at zero.
            water_depth: water_defaults.depth,
            water_waves: water_defaults.waves,
            water_reflection: water_defaults.reflection,
            water_foam_on: water_defaults.foam_on,
            water_extinction: water_defaults.extinction,
            water_color: water_defaults.color,
            water_wave: water_defaults.wave,
            water_reflectivity: water_defaults.reflectivity,
            water_specular: water_defaults.specular,
            water_steepness: water_defaults.steepness,
            water_wavelength: water_defaults.wavelength,
            water_foam: water_defaults.foam,
            water_foam_width: water_defaults.foam_width,
            water_wet_on: water_defaults.wet_on,
            water_wet: water_defaults.wet,
            water_wet_width: water_defaults.wet_width,
            water_speed: water_defaults.speed,
            water_phase: 0.0,
            project_path: None,
            recent: Vec::new(),
            // The built-in starter has no saved camera, so fit it to the screen on the first
            // render (the comment above; this is the one-shot the open path also uses).
            frame_to_graph_request: true,
            viewport_mesh: None,
            viewport_mode: viewport2d::Mode::default(),
            viewport_2d: viewport2d::View2d::default(),
            viewport_camera: viewport::OrbitCamera::default(),
            render_state: None,
            viewport_scale: shade::HeightScale::Fixed,
            viewport_exaggeration: 1.0,
            viewport_fly_speed: 0.6,
            // Reproduces the previous fixed key light: high from the front-right.
            viewport_lighting: viewport::Lighting {
                azimuth_deg: 35.0,
                elevation_deg: 55.0,
                intensity: 0.75,
                ambient: 0.25,
            },
            workspace_mode: WorkspaceMode::Split,
            workspace_mode_prev: None,
            viewport_frac: 0.4,
            viewport_anim_from: 0.0,
            viewport_anim_start: 0.0,
            viewport_last_h: 0.0,
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

    /// Applies a restored canvas camera: when the project saved one, restore that pan/zoom so it
    /// reopens exactly as it was left; otherwise fit the graph to the screen (an older project, a
    /// graph-only file, or the built-in starter). Both routes go through the same one-shot the
    /// canvas already applies next frame.
    fn apply_restored_view(&mut self, camera: Option<egui::emath::TSTransform>) {
        match camera {
            Some(transform) => {
                self.pending_view = Some(transform);
                self.frame_to_graph_request = false;
            }
            None => self.frame_to_graph_request = true,
        }
    }

    /// Installs a project opened from disk (#75): swaps in the rebuilt graph, canvas,
    /// and world settings, and clears view-state that referenced the old graph's
    /// handles (selection, preview pin, open dialogs). Restores the saved camera, or fits the
    /// graph when none was saved. The background engines pick up the new graph on the next
    /// frame, so no explicit reset is needed.
    fn install_project(
        &mut self,
        restored: project_file::RestoredProject,
        path: std::path::PathBuf,
    ) {
        self.nav.clear();
        self.graph = restored.graph;
        self.snarl = restored.snarl;
        self.frames = restored.frames;
        self.subgraph_layouts = restored.subgraph_layouts;
        self.selected_frame = None;
        self.frame_drag = None;
        self.frame_color_edit = None;
        self.seed = restored.seed;
        self.world_extent = restored.world_extent;
        self.world_height = restored.world_height;
        self.build_res = restored.build_res;
        self.preview_res = restored.preview_res;
        self.sea_level = restored.sea_level;
        self.show_water = restored.show_water;
        self.apply_water_settings(restored.water);
        self.clear_selection();
        self.preview_pin = None;
        self.node_menu = None;
        self.rename = None;
        self.library_save = None;
        self.apply_restored_view(restored.camera);
        // The name indicator shows which file; the status only needs the action.
        self.status = Some("Opened".to_string());
        self.project_path = Some(path);
        // Undo must not reach back across an Open into the previous project, and the
        // freshly opened session is clean.
        self.reset_history();
        self.mark_clean();
    }

    /// Replaces the session with a fresh untitled one built from `graph`/`snarl`: installs the
    /// given `world` settings (seed, extent, height, build resolution, sea level, water toggle),
    /// resets view-state, drops the project path, and re-anchors undo and the clean point. Shared
    /// by New (a starter graph) and Close (an empty graph).
    fn install_fresh(
        &mut self,
        graph: Graph,
        snarl: Snarl<Handle>,
        world: project_file::WorldSettings,
        subgraph_layouts: HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>>,
    ) {
        self.nav.clear();
        self.subgraph_layouts = subgraph_layouts;
        self.graph = graph;
        self.snarl = snarl;
        self.frames = Vec::new();
        self.selected_frame = None;
        self.frame_drag = None;
        self.frame_color_edit = None;
        self.seed = world.seed;
        self.world_extent = world.world_extent;
        self.world_height = world.world_height;
        self.build_res = world.build_res;
        self.preview_res = world.preview_res;
        self.sea_level = world.sea_level;
        self.show_water = world.show_water;
        self.apply_water_settings(world.water);
        self.clear_selection();
        self.preview_pin = None;
        self.node_menu = None;
        self.rename = None;
        self.library_save = None;
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
            fresh_world_settings(),
            HashMap::new(),
        );
    }

    /// Opens the default startup graph (the saved default if one exists, else the
    /// built-in starter), untitled, the same state as on launch.
    fn open_default(&mut self) {
        let loaded = default_project_path()
            .filter(|p| p.exists())
            .and_then(|p| read_project(&p).ok());
        match loaded {
            Some(r) => {
                let world = project_file::WorldSettings {
                    seed: r.seed,
                    world_extent: r.world_extent,
                    world_height: r.world_height,
                    build_res: r.build_res,
                    preview_res: r.preview_res,
                    sea_level: r.sea_level,
                    show_water: r.show_water,
                    water: r.water,
                };
                self.install_fresh(r.graph, r.snarl, world, r.subgraph_layouts);
            }
            None => {
                let (graph, snarl) = starter::starter_graph();
                self.install_fresh(graph, snarl, fresh_world_settings(), HashMap::new());
            }
        }
    }

    /// The current world settings (seed, extent, height, build resolution, sea level), bundled
    /// for a [`project_file`] capture. Everything here is part of the saved project and the dirty
    /// check, so changing the sea level or the water toggle marks the project modified.
    fn world_settings(&self) -> project_file::WorldSettings {
        project_file::WorldSettings {
            seed: self.seed,
            world_extent: self.world_extent,
            world_height: self.world_height,
            build_res: self.build_res,
            preview_res: self.preview_res,
            sea_level: self.sea_level,
            show_water: self.show_water,
            water: self.water_settings(),
        }
    }

    /// Collects the ephemeral water look and effect controls into the persisted form (#157), so
    /// the current look travels with a saved project.
    fn water_settings(&self) -> project_file::WaterSettings {
        project_file::WaterSettings {
            depth: self.water_depth,
            waves: self.water_waves,
            reflection: self.water_reflection,
            foam_on: self.water_foam_on,
            extinction: self.water_extinction,
            color: self.water_color,
            wave: self.water_wave,
            reflectivity: self.water_reflectivity,
            specular: self.water_specular,
            steepness: self.water_steepness,
            wavelength: self.water_wavelength,
            foam: self.water_foam,
            foam_width: self.water_foam_width,
            wet_on: self.water_wet_on,
            wet: self.water_wet,
            wet_width: self.water_wet_width,
            speed: self.water_speed,
        }
    }

    /// Applies restored water settings back onto the ephemeral controls. The animation phase is a
    /// running clock, not a stored setting, so it is left as-is (the surface simply carries on).
    fn apply_water_settings(&mut self, w: project_file::WaterSettings) {
        self.water_depth = w.depth;
        self.water_waves = w.waves;
        self.water_reflection = w.reflection;
        self.water_foam_on = w.foam_on;
        self.water_extinction = w.extinction;
        self.water_color = w.color;
        self.water_wave = w.wave;
        self.water_reflectivity = w.reflectivity;
        self.water_specular = w.specular;
        self.water_steepness = w.steepness;
        self.water_wavelength = w.wavelength;
        self.water_foam = w.foam;
        self.water_foam_width = w.foam_width;
        self.water_wet_on = w.wet_on;
        self.water_wet = w.wet;
        self.water_wet_width = w.wet_width;
        self.water_speed = w.speed;
    }

    /// A snapshot of the current session (graph, canvas positions, world settings),
    /// the unit the undo history and the project file both work in.
    ///
    /// Always the effective *top-level* project, even when diving into a subgraph: the
    /// active inner graph is folded back up through `nav`, and the top-level positions and
    /// frames come from the outermost suspended context. So save, dirty tracking, and undo
    /// all operate on the whole project regardless of how deep the user is editing.
    fn snapshot(&self) -> project_file::ProjectFile {
        let layouts = self.all_known_layouts();
        if self.nav.is_empty() {
            project_file::ProjectFile::capture_with(
                &self.graph,
                project_file::snarl_positions(&self.snarl),
                self.world_settings(),
                &self.frames,
                &layouts,
            )
        } else {
            project_file::ProjectFile::capture_with(
                &self.top_graph(),
                self.nav[0].positions.clone(),
                self.world_settings(),
                &self.nav[0].frames,
                &layouts,
            )
        }
    }

    /// The project file to write to disk: the current [`snapshot`](Self::snapshot) plus the live
    /// canvas camera (pan/zoom). The camera is added only here, not in `snapshot`, because panning
    /// is not an undoable edit and must not dirty the project; it is captured at write time so a
    /// reopened project restores the exact view.
    fn project_for_save(&self) -> project_file::ProjectFile {
        let mut file = self.snapshot();
        file.view.camera = self
            .canvas_view
            .map(|v| project_file::Camera::from_transform(v.to_global));
        file
    }

    /// Every known subgraph interior layout, keyed by navigation path: the remembered
    /// (exited) ones, plus the currently-suspended parents and the active context, so a save
    /// while diving in captures the live interior arrangements too (#106).
    fn all_known_layouts(&self) -> HashMap<Vec<u64>, BTreeMap<u64, [f32; 2]>> {
        let mut layouts = self.subgraph_layouts.clone();
        // Suspended subgraph parents. `nav[0]` is the top level (not a subgraph); `nav[i]`
        // for i >= 1 is the graph reached through the first `i` containers.
        for i in 1..self.nav.len() {
            let path: Vec<u64> = self.nav[..i].iter().map(|f| f.container).collect();
            layouts.insert(path, self.nav[i].positions.clone());
        }
        // The active context, when inside a subgraph: its live canvas positions.
        if !self.nav.is_empty() {
            layouts.insert(
                self.current_path(),
                project_file::snarl_positions(&self.snarl),
            );
        }
        layouts
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

    /// The live inputs feeding the subgraph currently being edited (#106): a clone of the
    /// parent graph plus, per inner Input marker, the parent source feeding the matching
    /// container port. `None` at the top level or when no input is wired, in which case the
    /// interior evaluates with the markers' zero stand-in as before. The preview and thumbnail
    /// engines bind these so the inside shows real data.
    fn subgraph_inputs(&self) -> Option<SubgraphInputs> {
        let frame = self.nav.last()?;
        let container = frame.graph.node_id_of(frame.container)?;
        let mut markers = Vec::new();
        for (port, &marker_id) in self.graph.nodes_of_type(INPUT_TYPE_ID).iter().enumerate() {
            if let Some(marker) = self.graph.stable_id(marker_id)
                && let Some((source, output)) = frame.graph.input_source(container, port)
                && let Some(source_stable) = frame.graph.stable_id(source)
            {
                markers.push((marker, source_stable, output));
            }
        }
        (!markers.is_empty()).then(|| SubgraphInputs {
            parent: frame.graph.clone(),
            markers,
        })
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
            // Remember the current pan/zoom so popping back out restores this view.
            view: self.canvas_view.map(|v| v.to_global),
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

    /// Wraps the given nodes into a new subgraph container in the active graph (#106): the
    /// top-down "Create subgraph" path. The container takes the selection's place (positioned
    /// at its centroid) with ports derived from the boundary-crossing wires, and becomes the
    /// new selection. A no-op if none of the handles resolve.
    fn create_subgraph_from(&mut self, handles: &[Handle]) {
        let node_ids: Vec<NodeId> = handles
            .iter()
            .filter_map(|&h| self.graph.node_id_of(h))
            .collect();
        if node_ids.is_empty() {
            return;
        }

        // Place the container at the centroid of the wrapped nodes' canvas positions.
        let mut positions = project_file::snarl_positions(&self.snarl);
        let mut sum = egui::Vec2::ZERO;
        let mut count = 0.0_f32;
        for &h in handles {
            if let Some(p) = positions.get(&h) {
                sum += egui::vec2(p[0], p[1]);
                count += 1.0;
            }
        }
        let centroid = if count > 0.0 {
            (sum / count).to_pos2()
        } else {
            egui::Pos2::ZERO
        };

        match self.graph.extract_subgraph(&node_ids) {
            Ok(extraction) => {
                if let Some(handle) = self.graph.stable_id(extraction.container) {
                    positions.insert(handle, [centroid.x, centroid.y]);
                    // Lay the new interior out so diving in shows an untangled graph: the
                    // wrapped nodes keep their relative positions, Input markers to their
                    // left and Output markers to their right (#106), rather than cascading on
                    // top of each other.
                    let mut child_path = self.current_path();
                    child_path.push(handle);
                    self.subgraph_layouts.insert(
                        child_path,
                        subgraph_interior_layout(&extraction, &positions),
                    );
                    self.snarl = project_file::build_snarl(&self.graph, &positions);
                    self.select_only(handle);
                }
            }
            Err(err) => self.status = Some(format!("Create subgraph failed: {err}")),
        }
    }

    /// Opens the "Save to library" dialog for a container, pre-filled from it (#106): its
    /// name, and a documentation row per port seeded with the port name. A no-op for a
    /// non-container.
    fn open_library_save(&mut self, handle: Handle) {
        let Some(id) = self.graph.node_id_of(handle) else {
            return;
        };
        if self.graph.nested(id).is_none() {
            return; // not a container
        }
        self.library_save_tab = LibraryTab::Details;
        let Some(spec) = self.graph.spec(id) else {
            return;
        };
        let docs = |ports: &[ymir_core::PortSpec]| {
            ports
                .iter()
                .enumerate()
                .map(|(index, port)| library::PortDoc {
                    index,
                    name: port.name.clone(),
                    description: String::new(),
                })
                .collect()
        };
        self.library_save = Some(LibrarySave {
            source: SubgraphSource::Container(handle),
            name: node_display_name(&self.graph, id),
            category: String::new(),
            description: String::new(),
            inputs: docs(&spec.inputs),
            outputs: docs(&spec.outputs),
            // Pre-fill the author from the user's profile; they can edit or clear it per save.
            author: self.preferences.author.clone(),
            license: String::new(),
            error: None,
            confirm_overwrite: false,
        });
    }

    /// Opens the save dialog to edit an existing library entry in place (#106): pre-filled with its
    /// current documentation, preserving its graph, seed, and interior layout. Saving under the same
    /// name overwrites the file; a new name renames it (the old file is removed). A no-op if the
    /// entry is no longer in the listing.
    fn open_library_edit(&mut self, path: &std::path::Path) {
        let Some(entry) = self.library.entries.iter().find(|e| e.path == path) else {
            return;
        };
        self.library_save_tab = LibraryTab::Details;
        let file = &entry.file;
        self.library_save = Some(LibrarySave {
            source: SubgraphSource::Existing {
                original_path: entry.path.clone(),
                graph: file.graph.clone(),
                seed: file.seed,
                view: file.view.clone(),
            },
            name: file.name.clone(),
            category: file.category.clone(),
            description: file.description.clone(),
            inputs: file.inputs.clone(),
            outputs: file.outputs.clone(),
            author: file.author.clone(),
            license: file.license.clone(),
            error: None,
            confirm_overwrite: false,
        });
    }

    /// Builds a [`library::SubgraphFile`] from the dialog's documentation over its source's graph,
    /// seed, and interior layout. For a container source these come from the live canvas; for an
    /// existing-entry edit they are preserved verbatim, so editing the documentation never disturbs
    /// the saved graph.
    fn build_subgraph_file(&self, dialog: &LibrarySave) -> Result<library::SubgraphFile, String> {
        let (graph, seed, view) = match &dialog.source {
            SubgraphSource::Container(handle) => {
                let id = self
                    .graph
                    .node_id_of(*handle)
                    .ok_or_else(|| "the subgraph node is gone".to_string())?;
                let inner = self
                    .graph
                    .nested(id)
                    .ok_or_else(|| "not a subgraph".to_string())?;
                let seed = self.graph.params(id).map_or(0, |p| p.get_i64("seed", 0));
                // The container's interior layout is stored under its path in the active context.
                let mut path = self.current_path();
                path.push(*handle);
                let view = project_file::ViewState {
                    nodes: self
                        .subgraph_layouts
                        .get(&path)
                        .cloned()
                        .unwrap_or_default(),
                    ..Default::default()
                };
                (inner.to_document(), seed, view)
            }
            SubgraphSource::Existing {
                graph, seed, view, ..
            } => (graph.clone(), *seed, view.clone()),
        };
        // Reconcile the edited port names into the graph's boundary markers, so a copy dropped from
        // the library derives the same names the library card shows. Done in place on the document
        // (its nodes stay in stable-id order, the order ports derive in), so nothing else moves.
        let mut graph = graph;
        let inputs = reconcile_port_names(&mut graph, INPUT_TYPE_ID, &dialog.inputs);
        let outputs = reconcile_port_names(&mut graph, OUTPUT_TYPE_ID, &dialog.outputs);
        Ok(library::SubgraphFile {
            format_version: library::SUBGRAPH_FORMAT_VERSION,
            name: dialog.name.trim().to_string(),
            category: dialog.category.trim().to_string(),
            description: dialog.description.trim().to_string(),
            inputs,
            outputs,
            author: dialog.author.clone(),
            license: dialog.license.trim().to_string(),
            seed,
            graph,
            view,
        })
    }

    /// Writes the dialog's subgraph to the user library, returning the file path on success.
    fn write_subgraph_file(&self, dialog: &LibrarySave) -> Result<std::path::PathBuf, String> {
        let file = self.build_subgraph_file(dialog)?;
        let path =
            library_target_path(&dialog.name).ok_or_else(|| "no library directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        library::write_subgraph(&path, &file)?;
        Ok(path)
    }

    /// Saves the open dialog's subgraph to the library: validates the name, guards against
    /// silently overwriting an existing file, writes on confirmation, and closes on success or
    /// shows the error in the dialog.
    fn save_subgraph_to_library(&mut self) {
        let Some(dialog) = self.library_save.take() else {
            return;
        };
        // Whether the write would clobber a *different* existing file than the one being edited.
        // Resolved once so the decision is pure. A missing library directory yields `false`: the
        // write itself then reports it. Re-saving an entry in place under its own name is not a
        // conflict, so an in-place metadata edit never has to confirm.
        let target = library_target_path(&dialog.name);
        let target_exists = target.as_ref().is_some_and(|p| p.exists());
        let conflict = is_foreign_overwrite(
            target_exists,
            target.as_deref(),
            dialog.source.original_path(),
        );
        match save_decision(&dialog.name, conflict, dialog.confirm_overwrite) {
            SaveDecision::NameRequired => {
                self.library_save = Some(LibrarySave {
                    error: Some("A name is required.".to_string()),
                    ..dialog
                });
            }
            SaveDecision::ConfirmOverwrite => {
                let name = dialog.name.trim().to_string();
                self.library_save = Some(LibrarySave {
                    confirm_overwrite: true,
                    error: Some(format!(
                        "A subgraph named \"{name}\" already exists. Saving again overwrites it."
                    )),
                    ..dialog
                });
            }
            SaveDecision::Write => match self.write_subgraph_file(&dialog) {
                Ok(path) => {
                    // A rename (editing an existing entry to a new name) leaves the old file behind;
                    // remove it so the rename does not orphan a duplicate. A failed removal is a
                    // soft problem: the new file is correct, so report it but still finish the save.
                    let mut note = format!("Saved to library: {}", path.display());
                    if let Some(original) = dialog.source.original_path()
                        && original != path
                        && let Err(err) = std::fs::remove_file(original)
                    {
                        note = format!("Saved, but the old file could not be removed: {err}");
                    }
                    self.status = Some(note);
                    // Refresh the browser so the entry appears (or moves) without a restart, and
                    // keep an edited entry selected so its detail follows the change.
                    self.reload_library();
                    if matches!(dialog.source, SubgraphSource::Existing { .. }) {
                        self.library_selection = Some(path);
                    }
                }
                Err(err) => {
                    self.library_save = Some(LibrarySave {
                        error: Some(err),
                        ..dialog
                    });
                }
            },
        }
    }

    /// Rescans the library directory into `self.library`. Called from the app shell at startup and
    /// after a save, never from `AppState::new` (which must stay env-free).
    fn reload_library(&mut self) {
        self.library = library::load_library();
    }

    /// Inserts a saved library subgraph into the active canvas as a container node (#106): rebuilds
    /// the saved inner graph, nests it, applies the saved seed and the library name, and registers
    /// the saved interior layout so diving in opens to the author's arrangement. The new node is
    /// selected and reported on the status line. A no-op (with a note) if the saved graph cannot be
    /// rebuilt (a corrupt or version-incompatible file) or the container type is unregistered,
    /// never a panic.
    fn insert_subgraph_from_library(&mut self, file: &library::SubgraphFile) {
        // Rebuild the inner graph first, so an unloadable document adds nothing to the canvas.
        let inner = match Graph::from_document(&file.graph) {
            Ok(inner) => inner,
            Err(err) => {
                self.status = Some(format!("Could not insert \"{}\": {err}", file.name));
                return;
            }
        };
        let pos = spawn_pos(self.canvas_view, self.graph.node_count());
        let Some(id) = canvas::add_node(&mut self.graph, &mut self.snarl, SUBGRAPH_TYPE_ID, pos)
        else {
            self.status = Some("The subgraph node type is unavailable.".to_string());
            return;
        };
        // The container was just created, so these edits cannot fail; on the impossible error,
        // report it rather than panic, leaving the harmless empty container in place.
        let params = Params::new().with("seed", ParamValue::Int(file.seed));
        if self.graph.set_nested(id, inner).is_err()
            || self.graph.set_params(id, params).is_err()
            || self.graph.set_name(id, name_override(&file.name)).is_err()
        {
            self.status = Some(format!("Could not finish inserting \"{}\".", file.name));
            return;
        }
        // Register the saved interior layout under the new container's path so a dive-in restores
        // the author's arrangement. The node exists, so `stable_id` is present; guard, never unwrap.
        if let Some(handle) = self.graph.stable_id(id) {
            let mut path = self.current_path();
            path.push(handle);
            self.subgraph_layouts.insert(path, file.view.nodes.clone());
            self.select_only(handle);
        }
        self.status = Some(format!("Inserted \"{}\".", file.name));
    }

    /// Deletes a library entry's file from disk (#106), then clears the selection and refreshes the
    /// listing. The display name for the status line is read from the current listing, falling back
    /// to the file stem. A file already gone (removed outside the app) is treated as done rather
    /// than an error; a real removal failure is reported and the selection kept so the user can
    /// retry. Never panics.
    fn delete_library_entry(&mut self, path: &std::path::Path) {
        let name = self
            .library
            .entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.file.name.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let result = std::fs::remove_file(path);
        // The armed confirmation is consumed whatever the outcome, so a failed delete does not
        // leave the prompt stuck open.
        self.library_pending_delete = None;
        match result {
            Ok(()) => {
                self.status = Some(format!("Deleted \"{name}\" from the library."));
                self.library_selection = None;
                self.reload_library();
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Already removed elsewhere; drop it from the view rather than report a failure.
                self.status = Some(format!("\"{name}\" was already removed."));
                self.library_selection = None;
                self.reload_library();
            }
            Err(err) => {
                self.status = Some(format!("Could not delete \"{name}\": {err}"));
            }
        }
    }

    /// Commits the open Settings dialog: installs its draft as the live preferences and writes
    /// them to config. Reports the outcome on the status line and closes the dialog. A no-op if
    /// the dialog is not open.
    fn commit_settings(&mut self) {
        let Some(draft) = self.settings_edit.take() else {
            return;
        };
        self.preferences = draft;
        match save_preferences(&self.preferences) {
            Ok(()) => self.status = Some("Settings saved.".to_string()),
            Err(err) => self.status = Some(format!("Settings could not be saved: {err}")),
        }
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
        // Restore the parent's pan/zoom so popping out returns to the same view; without
        // this the canvas keeps the interior's scroll, losing sight of the parent graph.
        self.pending_view = frame.view;
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
        self.library_save = None;
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
                self.graph = restored.graph;
                self.snarl = restored.snarl;
                self.frames = restored.frames;
                self.subgraph_layouts = restored.subgraph_layouts;
                self.selected_frame = None;
                self.frame_drag = None;
                self.frame_color_edit = None;
                self.seed = restored.seed;
                self.world_extent = restored.world_extent;
                self.world_height = restored.world_height;
                self.sea_level = restored.sea_level;
                self.show_water = restored.show_water;
                self.apply_water_settings(restored.water);
                self.selection
                    .retain(|&h| self.graph.node_id_of(h).is_some());
                self.primary = self.primary.filter(|h| self.selection.contains(h));
                self.preview_pin = self
                    .preview_pin
                    .filter(|&h| self.graph.node_id_of(h).is_some());
                self.node_menu = None;
                self.rename = None;
                self.library_save = None;
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

    /// Whether `handle` resolves to a live node that can be previewed in the 2D pane: one
    /// that produces an output, or a subgraph Output marker (an endpoint, but its preview is
    /// the field feeding it — the subgraph's result, #106). A deleted node cannot.
    fn is_previewable(&self, handle: Handle) -> bool {
        self.graph
            .node_id_of(handle)
            .and_then(|id| self.graph.spec(id))
            .is_some_and(|spec| !spec.outputs.is_empty() || spec.type_id == OUTPUT_TYPE_ID)
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
        // Default to the result-node fallback; the background-click deselect re-sets the dismiss
        // flag after this, so only an explicit canvas click blanks the preview.
        self.preview_dismissed = false;
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

    /// The node whose output the 2D preview shows: the pinned node when one is set and still
    /// previewable, else the primary selected node, else — when the selection is an output-less
    /// endpoint — the node feeding that endpoint (what it exports), else the graph's result sink.
    /// Decouples the preview target from selection (#39).
    fn preview_target(&self) -> Option<Handle> {
        self.preview_pin
            .filter(|&h| self.is_previewable(h))
            .or_else(|| self.primary.filter(|&h| self.is_previewable(h)))
            .or_else(|| self.primary.and_then(|h| self.endpoint_input_source(h)))
            // Fall back to the graph's result node only when the preview was not explicitly
            // dismissed (clicking empty canvas), so deselecting goes blank while a freshly opened
            // graph still shows its result.
            .or_else(|| {
                (!self.preview_dismissed)
                    .then(|| self.preview_sink())
                    .flatten()
            })
    }

    /// The previewable node feeding a selected endpoint's input, if any. An output-less endpoint
    /// (an export) is not itself previewable, so selecting it should show *what it exports* — the
    /// node wired into its first input — rather than falling through to an unrelated graph sink
    /// (#133). Returns `None` for a node that has outputs (it previews its own output), for an
    /// endpoint with nothing wired, or when the source is itself not previewable.
    fn endpoint_input_source(&self, handle: Handle) -> Option<Handle> {
        let id = self.graph.node_id_of(handle)?;
        if !self.graph.spec(id)?.outputs.is_empty() {
            return None;
        }
        let (source, _) = self.graph.input_source(id, 0)?;
        let source = self.graph.stable_id(source)?;
        self.is_previewable(source).then_some(source)
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

    /// Evaluates the current preview target once per frame, regardless of which pane is visible.
    /// The 3D viewport meshes the preview's output, so without this the viewport froze whenever
    /// the 2D preview pane was hidden: evaluation used to run only inside that pane. A no-op when
    /// no node is previewable.
    fn drive_preview(&mut self, ctx: &egui::Context) {
        let Some(id) = self.preview_target().and_then(|h| self.graph.node_id_of(h)) else {
            // No target (an empty graph, or the preview was dismissed by clicking empty canvas):
            // blank the preview so the viewport and inspector show nothing, not a stale field.
            self.preview.clear();
            return;
        };
        let res = self.preview_res;
        let request = EvalRequest::new(res, res, Region::UNIT, self.seed)
            .with_world_extent(self.world_extent)
            .with_world_height(self.world_height)
            .with_sea_level(self.sea_level);
        let now = ctx.input(|i| i.time);
        // Inside a subgraph, bind the live input fields so the 2D preview shows real data
        // rather than the Input markers' zero stand-in (#106). `None` at the top level.
        let binding = self.subgraph_inputs();
        self.preview
            .sync(&self.graph, id, request, now, binding.as_ref());
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

/// Whether a node type is flagged experimental (functional but rough or artifact-prone), read from
/// its operator's [`experimental`](ymir_core::Operator::experimental). Constructs the operator (a
/// zero-cost unit struct) to read the flag, cheap enough to call per palette row.
fn is_experimental(type_id: &str) -> bool {
    registry::make(type_id).is_some_and(|op| op.experimental())
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
        let mut shown: Vec<&NodeEntry> = entries
            .iter()
            .filter(|e| match tab {
                Some(ActiveTab::Category(id)) => e.category == id,
                Some(ActiveTab::Uncategorized) => find_category(e.category).is_none(),
                None => false,
            })
            .collect();
        // Order within a category by each node's registered palette-group sort, so like
        // nodes sit together; a stable sort keeps ungrouped nodes (sort MAX) in registry
        // order. Groups end up contiguous, so the ribbon and menu draw a separator wherever
        // the group id changes.
        shown.sort_by_key(|e| node_group(e.type_id).map_or(i32::MAX, |g| g.sort));
        shown
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
    /// A non-selectable divider between palette groups within a drilled-in category.
    /// Keyboard navigation and activation skip it; it renders as a plain separator line.
    Separator,
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
        // Back, then the category's nodes in palette-group order, with a separator row
        // wherever the group changes (matching the ribbon's group dividers).
        let mut rows = vec![MenuRow::Back];
        let mut prev_group: Option<&str> = None;
        for e in visible_nodes(entries, Some(ActiveTab::Category(cat)), "") {
            let group = node_group(e.type_id).map(|g| g.group);
            if let (Some(p), Some(c)) = (prev_group, group)
                && p != c
            {
                rows.push(MenuRow::Separator);
            }
            prev_group = group;
            rows.push(MenuRow::Node(e.type_id));
        }
        rows
    } else {
        categories_sorted()
            .iter()
            .map(|c| MenuRow::Category(c.id))
            .collect()
    }
}

/// The next selectable row index from `from` in the given direction, skipping the
/// non-selectable [`MenuRow::Separator`] dividers so keyboard navigation never lands on
/// one. Returns `from` if there is no other selectable row.
fn step_highlight(rows: &[MenuRow], from: usize, forward: bool) -> usize {
    let n = rows.len();
    if n == 0 {
        return 0;
    }
    let mut i = from;
    for _ in 0..n {
        i = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        if !matches!(rows[i], MenuRow::Separator) {
            return i;
        }
    }
    from
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
        // A divider carries no text; it is drawn as a separator line, never as a labelled row.
        MenuRow::Separator => String::new(),
    }
}

/// Whether a node type can be added in the current context. The subgraph boundary markers
/// (#106) only make sense inside a subgraph, so outside one they are *disabled* rather than
/// hidden: the palette and node menu still show them (so their existence and category stay
/// discoverable) but they cannot be created at the top level.
fn node_addable(type_id: &str, inside_subgraph: bool) -> bool {
    inside_subgraph || (type_id != INPUT_TYPE_ID && type_id != OUTPUT_TYPE_ID)
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
            ui.separator();
            if ui.button("Settings…").clicked() {
                // Open the dialog on a copy of the live preferences, so Cancel discards edits.
                state.settings_edit = Some(state.preferences.clone());
                ui.close();
            }
        });
        ui.menu_button("View", |ui| {
            // The "Node thumbnails" toggle is temporarily hidden: flipping it flashed a
            // build-status frame on every node (#135). The capability is kept (`thumbnails_enabled`
            // stays true by default); the menu returns once that is fixed.
            ui.weak("(empty)");
        });
        ui.menu_button("Graph", |ui| {
            ui.weak("(empty)");
        });
        ui.menu_button("Help", |ui| {
            if ui.button("About Ymir").clicked() {
                state.about_open = true;
                ui.close();
            }
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
            let warnings = restored.warnings.clone();
            state.install_project(restored, path.clone());
            record_recent(state, path.clone());
            // A lossy open (a node kept as a placeholder, a dropped connection) is never silent:
            // each note goes to the log for a headless record, and the count is shown in the UI.
            if !warnings.is_empty() {
                for warning in &warnings {
                    log::warn!("opening {}: {warning}", path.display());
                }
                let n = warnings.len();
                state.status = Some(format!(
                    "Opened with {n} issue{}; see the log for details",
                    if n == 1 { "" } else { "s" },
                ));
            }
        }
        Err(err) => {
            log::warn!("could not open {}: {err}", path.display());
            state.status = Some(format!("Could not open {}: {err}", path.display()));
            // Only forget the project when the file itself is gone; a real file that failed to
            // parse (a corrupt or future-version project) stays in recent so the entry is not lost.
            if !path.exists() {
                state.recent.retain(|p| p != &path);
                save_recent(state);
            }
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
    let file = state.project_for_save();
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

/// The path to the session logfile: `…/ymir/ymir.log` under the XDG config base. `None` if no
/// config base is available (then logging is stderr-only).
fn log_path() -> Option<std::path::PathBuf> {
    config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "ymir.log",
    )
}

/// Resolves an XDG base directory: the `xdg` override if set and non-empty, otherwise
/// `$HOME/<home_subdir>` (e.g. `.config`, `.local/share`). `None` if neither is available.
/// Pure (the env read lives in the caller), so precedence is unit-tested without touching
/// the process environment.
fn xdg_base(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    home_subdir: &str,
) -> Option<std::path::PathBuf> {
    xdg.filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            home.filter(|s| !s.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(home_subdir))
        })
}

/// Resolves a file path under the XDG *config* base (`$XDG_CONFIG_HOME`, else
/// `$HOME/.config`): `…/ymir/<rel>`. For user configuration and settings.
fn config_path(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    rel: &str,
) -> Option<std::path::PathBuf> {
    Some(xdg_base(xdg, home, ".config")?.join("ymir").join(rel))
}

/// Resolves a path under the XDG *data* base (`$XDG_DATA_HOME`, else
/// `$HOME/.local/share`): `…/ymir/<rel>`. For user-authored content the user would not
/// want swept as cache, such as the subgraph library.
fn data_path(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    rel: &str,
) -> Option<std::path::PathBuf> {
    Some(xdg_base(xdg, home, ".local/share")?.join("ymir").join(rel))
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
    let file = state.project_for_save();
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
            state.build_res = restored.build_res;
            state.preview_res = restored.preview_res;
            state.sea_level = restored.sea_level;
            state.show_water = restored.show_water;
            state.apply_water_settings(restored.water);
            state.apply_restored_view(restored.camera);
            // Anchor undo and the clean point at the default, not the starter it replaced.
            state.reset_history();
            state.mark_clean();
        }
        Err(err) => {
            state.status = Some(format!("Default startup graph could not be loaded: {err}"));
        }
    }
}

/// Writes the given preferences to the config directory, creating it if needed.
///
/// # Errors
///
/// Returns a message if no config directory can be resolved, or the directory or file write
/// fails.
fn save_preferences(prefs: &preferences::Preferences) -> Result<(), String> {
    let path = preferences::preferences_path().ok_or_else(|| "no config directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    preferences::write_preferences(&path, prefs)
}

/// Overlays the user's saved preferences onto `state` at startup, if a preferences file exists
/// (#106). An absent file is the normal first-run case, leaving the empty default. A present but
/// unreadable file is reported on the status line rather than failing the launch. Lives in the
/// app shell, not `AppState::new`, so the test-constructed state never reads the filesystem.
fn apply_preferences(state: &mut AppState) {
    let Some(path) = preferences::preferences_path() else {
        return;
    };
    if !path.exists() {
        return;
    }
    match preferences::read_preferences(&path) {
        Ok(prefs) => state.preferences = prefs,
        Err(err) => state.status = Some(format!("Preferences could not be loaded: {err}")),
    }
}

/// A category tab in the Frost style: frameless text with a 2px accent underline when active (no
/// fill highlight). The active tab reads in primary ink, inactive in secondary; the weight stays
/// the same across states so a tab never changes width and reflows the row.
fn category_tab(
    ui: &mut egui::Ui,
    active: &mut Option<ActiveTab>,
    value: Option<ActiveTab>,
    label: &str,
) {
    let selected = *active == value;
    let color = if selected {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    let resp = ui.add(
        egui::Button::new(egui::RichText::new(label).color(color))
            .frame(false)
            .min_size(egui::vec2(0.0, RIBBON_ROW_H)),
    );
    if selected {
        let r = resp.rect;
        ui.painter().hline(
            r.left()..=r.right(),
            r.bottom(),
            egui::Stroke::new(2.0, theme::ACCENT_PRIMARY),
        );
    }
    if resp.clicked() {
        *active = value;
    }
}

/// A vertical divider between palette groups in the ribbon, drawn with a stronger line than
/// egui's default separator, which is near-invisible against the ribbon fill.
fn ribbon_group_separator(ui: &mut egui::Ui) {
    ui.scope(|ui| {
        ui.visuals_mut().widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, theme::LINE_STRONG);
        ui.add(egui::Separator::default().vertical());
    });
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

    // Roomier tabs and node buttons than egui's default (#—): both bands inherit this, so the
    // buttons carry whitespace on every side in both states and the ribbon stands a touch taller.
    ui.spacing_mut().button_padding = RIBBON_BUTTON_PADDING;
    // The two bands sit flush so the hairline divider drawn between them (below) reads as one line.
    ui.spacing_mut().item_spacing.y = 0.0;

    // Two full-width bands: the categories/search/Build bar, then the node list below, separated by
    // a hairline. The row forces a fixed height so tabs, the divider, and the search field all
    // centre on it.
    let top_band = egui::Frame::new()
        .fill(theme::BG_SURFACE)
        .inner_margin(RIBBON_BAND_MARGIN)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(RIBBON_ROW_H);
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
                let search_bg = theme::BG_ABYSS;
                ui.add(
                    egui::TextEdit::singleline(&mut state.search)
                        .hint_text("search nodes")
                        .margin(SEARCH_FIELD_MARGIN)
                        .background_color(search_bg)
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
                    // The one prominent element: an accent-filled button with dark text (per the
                    // Frost spec), so Build reads as the primary action against the dark chrome.
                    let build_btn = egui::Button::new(
                        egui::RichText::new("Build").color(theme::BG_ABYSS).strong(),
                    )
                    .fill(theme::ACCENT_PRIMARY);
                    if ui.add_enabled(!building, build_btn).clicked() {
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
                                .with_world_height(state.world_height)
                                .with_sea_level(state.sea_level);
                            state.build.start(top, targets, request, ui.ctx().clone());
                        }
                    }
                    state.build.show(ui);
                });
            });
        });
    // A hairline between the category row and the node-chip row below it.
    let sep_y = top_band.response.rect.bottom();
    ui.painter().hline(
        top_band.response.rect.x_range(),
        sep_y,
        egui::Stroke::new(1.0, theme::LINE_STRONG),
    );

    // The node list, a touch lighter so it reads as distinct from the bar above.
    let entries = node_entries();
    let shown = visible_nodes(&entries, state.active_tab, &state.search);
    egui::Frame::new()
        .fill(theme::BG_SURFACE)
        .inner_margin(RIBBON_BAND_MARGIN)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Subgraph boundary markers are disabled (shown greyed) outside a subgraph (#106).
            let inside_subgraph = !state.nav.is_empty();
            ui.horizontal_wrapped(|ui| {
                // A touch tighter between nodes within a group than egui's default.
                ui.spacing_mut().item_spacing.x = (ui.spacing().item_spacing.x - 1.0).max(0.0);
                // A vertical separator wherever the palette group changes, so like generators
                // read as a cluster. Ungrouped nodes (no registered group) get none.
                let mut prev_group: Option<&str> = None;
                for entry in shown {
                    let group = node_group(entry.type_id).map(|g| g.group);
                    if let (Some(p), Some(c)) = (prev_group, group)
                        && p != c
                    {
                        ribbon_group_separator(ui);
                    }
                    prev_group = group;
                    let key = format!("node-{}", entry.type_id);
                    let enabled = node_addable(entry.type_id, inside_subgraph);
                    let clicked = ui
                        .add_enabled(enabled, egui::Button::new(tr(&key)))
                        .on_disabled_hover_text("Available inside a subgraph")
                        .clicked();
                    if clicked {
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

/// The interior canvas layout for a freshly created subgraph (#106): each wrapped node keeps
/// its original canvas position (carried from `outer_positions` via the extraction's
/// outer->inner mapping), with the Input markers stacked in a column to the left of the
/// wrapped cluster and the Output markers to its right. Returns an empty map (so the dive
/// falls back to a cascade) when no wrapped node had a known position.
fn subgraph_interior_layout(
    extraction: &Extraction,
    outer_positions: &BTreeMap<u64, [f32; 2]>,
) -> BTreeMap<u64, [f32; 2]> {
    /// Horizontal gap from the wrapped cluster to a marker column.
    const MARKER_GAP: f32 = 220.0;
    /// Vertical spacing between stacked markers.
    const MARKER_ROW: f32 = 110.0;

    let mut layout = BTreeMap::new();
    let (mut min_x, mut min_y, mut max_x) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for &(outer, inner) in &extraction.moved {
        if let Some(&p) = outer_positions.get(&outer) {
            layout.insert(inner, p);
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
        }
    }
    // No positioned wrapped nodes to anchor the markers to: let the dive cascade instead.
    if !min_x.is_finite() {
        return BTreeMap::new();
    }
    for (i, &marker) in extraction.inputs.iter().enumerate() {
        layout.insert(marker, [min_x - MARKER_GAP, min_y + i as f32 * MARKER_ROW]);
    }
    for (i, &marker) in extraction.outputs.iter().enumerate() {
        layout.insert(marker, [max_x + MARKER_GAP, min_y + i as f32 * MARKER_ROW]);
    }
    layout
}

/// A filesystem-safe stem for a library file, derived from the subgraph name: alphanumerics,
/// dash, and underscore survive; anything else becomes a dash. Falls back to `subgraph` for
/// an otherwise-empty result, so a name of only punctuation still yields a valid file.
fn sanitize_filename(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // An empty or all-dash stem carries no real name (an empty or punctuation-only input), so
    // fall back rather than write a meaningless "-.ymirsub".
    if stem.chars().all(|c| c == '-') {
        "subgraph".to_string()
    } else {
        stem
    }
}

/// The library file path a subgraph named `name` would be written to, or `None` when there is no
/// library directory. Shared by the save write and the overwrite guard so both resolve the same
/// file from the same sanitized stem.
fn library_target_path(name: &str) -> Option<std::path::PathBuf> {
    library::library_dir().map(|dir| dir.join(format!("{}.ymirsub", sanitize_filename(name))))
}

/// The outcome of validating a save-to-library request, computed without touching the filesystem
/// (the existence check is passed in) so the guard is unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum SaveDecision {
    /// The name is blank; report it and do not write.
    NameRequired,
    /// A file already exists at this name and the overwrite is not yet confirmed; warn and arm
    /// the confirmation rather than clobber it.
    ConfirmOverwrite,
    /// Nothing blocks the save; write the file.
    Write,
}

/// Decides what a save request should do, given the entered name, whether its target file already
/// exists, and whether the user has confirmed overwriting it. A blank name is rejected before the
/// overwrite check, so an empty name never arms an overwrite.
fn save_decision(name: &str, target_exists: bool, confirmed: bool) -> SaveDecision {
    if name.trim().is_empty() {
        SaveDecision::NameRequired
    } else if target_exists && !confirmed {
        SaveDecision::ConfirmOverwrite
    } else {
        SaveDecision::Write
    }
}

/// Whether writing to `target` would clobber a *different* existing file than the one being edited.
/// Editing an entry in place (its target equals its `original` path) is never a conflict, so an
/// in-place metadata edit does not have to confirm; a fresh save has no original and so conflicts
/// with any existing target.
fn is_foreign_overwrite(
    target_exists: bool,
    target: Option<&std::path::Path>,
    original: Option<&std::path::Path>,
) -> bool {
    target_exists && target != original
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

/// The right column: the 2D preview over the node inspector. Dedicated to the selected node;
/// world settings moved to the left dock (#128). The header keeps a label (and will host a
/// collapse control at its far right later).
fn right_panel_pane(ui: &mut egui::Ui, state: &mut AppState) {
    header_strip(ui, |ui| {
        ui.label("Node Inspector");
    });
    egui::Panel::top("preview-panel")
        // Fixed height so the preview does not change size between having a node selected (a tall
        // image) and not (a placeholder).
        .exact_size(346.0)
        .show_separator_line(false)
        .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(4, 2)))
        .show_inside(ui, |ui| preview_2d_pane(ui, state));
    // A selected frame shows the frame inspector here instead of the node inspector (selection is
    // mutually exclusive, #94). The body scrolls: a node with many parameters, or a subgraph with
    // its input/output port lists, can exceed the panel height below the fixed preview, and without
    // a scroll area that overflow is clipped and unreachable.
    egui::CentralPanel::default().show_inside(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match state.selected_frame {
                Some(index) => frame_inspector(ui, state, index),
                None => node_inspector(ui, state),
            });
    });
}
inventory::submit! { PaneKind { id: "right-panel", draw: right_panel_pane } }

/// The subgraph library dock pane (#106): the saved subgraphs, grouped by category, for the user
/// to browse and insert. The scrolling list sits above a detail section that appears for the
/// selected entry (its documentation and an Insert action); corrupt files are surfaced at the
/// bottom of the list rather than hidden, so a bad file never silently disappears.
fn library_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // The dock body no longer insets its panes (so panels reach both borders), so the pane pads its
    // own content. Values captured up front as owned copies, to avoid holding a borrow of `ui`
    // across the panel bodies (which take it mutably).
    let style = ui.style().clone();
    let pad = egui::Margin::symmetric(8, 6);
    // A slightly sunken fill sets the inspector apart from the browser as a distinct pane.
    let inspector_fill = theme::BG_SURFACE;
    let divider = ui.visuals().widgets.noninteractive.bg_stroke.color;

    let listing = &state.library;
    if listing.entries.is_empty() && listing.errors.is_empty() {
        egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
            ui.add_space(2.0);
            ui.weak("No saved subgraphs yet.");
            ui.add_space(2.0);
            ui.weak(
                "Right-click a subgraph container on the canvas and choose \"Save to library\".",
            );
        });
        return;
    }

    // A selection whose file is gone (removed by a reload) resolves to none, so the inspector
    // never references an entry the list no longer shows.
    let has_selection = state
        .library_selection
        .as_ref()
        .is_some_and(|path| state.library.entries.iter().any(|e| &e.path == path));
    if !has_selection {
        state.library_selection = None;
        state.library_pending_delete = None;
    }

    // The browser (search over the entry list) gets the top two-thirds; the selected entry's
    // inspector fills the bottom third, resizable. The bottom panel is added before the central
    // region, as egui requires, and each carries its own content padding.
    //
    // The inspector body is a filling scroll area, and a resizable panel sized around filling
    // content collapses to its `min_size` (there is no intrinsic content height to hold a larger
    // `default_size`). So `min_size` is what actually governs the resting height here: set it to a
    // usable third of the dock, floored so a bad first-frame height cannot shrink it to a sliver.
    // The user can still drag the divider to make it larger.
    let third = (ui.available_height() / 3.0).max(180.0);
    // The browser and the inspector each read as their own subpane: the browser opens with its
    // SUBGRAPH LIBRARY section label, the inspector with its own SUBGRAPH eyebrow + name heading (see
    // `library_inspector`). The full-width divider (below) marks the split; the panel rects stay full
    // width, so it reaches both borders even though the content is padded.
    let inspector = egui::Panel::bottom("library-inspector")
        .resizable(true)
        .min_size(third)
        .show_separator_line(false)
        .frame(
            egui::Frame::side_top_panel(&style)
                .fill(inspector_fill)
                .inner_margin(pad),
        )
        .show_inside(ui, |ui| {
            library_inspector(ui, state);
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::side_top_panel(&style).inner_margin(pad))
        .show_inside(ui, |ui| {
            section_heading(ui, "Subgraph Library");
            library_browser(ui, state);
        });

    // A solid divider spanning the full dock width (the panes now reach both borders), matching the
    // canvas/viewport border rather than egui's faint default separator, so the inspector reads as
    // a distinct pane below the browser.
    let divider_y = inspector.response.rect.top();
    let x_range = inspector.response.rect.x_range();
    ui.painter()
        .hline(x_range, divider_y, egui::Stroke::new(1.0, divider));
    // The same grab handle as the workspace divider, so this browser/inspector resize reads the same.
    divider_handle(ui, divider_y, x_range);
}
inventory::submit! {
    dock::DockPane {
        id: "library",
        order: 10,
        icon: egui_phosphor::regular::BOOKS,
        title: "Library",
        draw: library_pane,
    }
}

/// The World settings dock pane (#128): the project-global seed, world extent and height, and
/// build resolution. Moved out of the right column so that panel stays dedicated to the selected
/// node. The dock draws the pane's "World" title header; this pads and renders the settings body
/// (which is [`world_settings`], unchanged from when it lived in the right column).
fn world_dock_pane(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            world_settings(ui, state);
        });
}
inventory::submit! {
    dock::DockPane {
        id: "world",
        order: 0,
        icon: egui_phosphor::regular::GLOBE_HEMISPHERE_WEST,
        title: "World",
        draw: world_dock_pane,
    }
}

/// Whether a subgraph file matches a lowercased search query, across its name, category, and
/// description. An empty query matches everything. Pure, so the filter is unit-tested apart from
/// the egui drawing.
fn library_matches(file: &library::SubgraphFile, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    file.name.to_lowercase().contains(query)
        || file.category.to_lowercase().contains(query)
        || file.description.to_lowercase().contains(query)
}

/// The browser below the inspector: a search field over the entry list. With no query the list is
/// grouped by category (name-sorted within each) with any load errors beneath; with a query it
/// flattens to the matching entries, name-sorted, like the node search. Clicking an entry selects
/// it (clicking the selected one again clears the selection), which fills the inspector above.
fn library_browser(ui: &mut egui::Ui, state: &mut AppState) {
    // The search field mirrors the node search: a query box with a clear button that appears only
    // when there is a query. The box fills the width, leaving room for the button.
    ui.horizontal(|ui| {
        let has_query = !state.library_search.is_empty();
        let clear_width = if has_query { 24.0 } else { 0.0 };
        let search_bg = theme::BG_ABYSS;
        ui.add(
            egui::TextEdit::singleline(&mut state.library_search)
                .hint_text("search subgraphs")
                .margin(SEARCH_FIELD_MARGIN)
                .background_color(search_bg)
                .desired_width((ui.available_width() - clear_width).max(0.0)),
        );
        if has_query && ui.small_button("×").on_hover_text("Clear search").clicked() {
            state.library_search.clear();
        }
    });
    ui.add_space(4.0);

    let query = state.library_search.trim().to_lowercase();
    let selection = state.library_selection.clone();
    // The command a row produced this frame (a click selecting it, a double-click inserting it, or
    // a context-menu action), applied after the list borrow ends (the closure only reads the
    // listing, so it cannot also mutate `state`).
    let mut command: Option<LibraryCommand> = None;
    {
        let listing = &state.library;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if query.is_empty() {
                    // Group by category, non-empty categories alphabetically first, then a
                    // trailing "Uncategorized" group for the blank ones.
                    let mut by_category: BTreeMap<&str, Vec<&library::LibraryEntry>> =
                        BTreeMap::new();
                    for entry in &listing.entries {
                        by_category
                            .entry(entry.file.category.trim())
                            .or_default()
                            .push(entry);
                    }
                    for (category, entries) in by_category.iter().filter(|(c, _)| !c.is_empty()) {
                        library_category(ui, category, entries, selection.as_deref(), &mut command);
                    }
                    if let Some(entries) = by_category.get("") {
                        library_category(
                            ui,
                            "Uncategorized",
                            entries,
                            selection.as_deref(),
                            &mut command,
                        );
                    }

                    if !listing.errors.is_empty() {
                        ui.add_space(8.0);
                        ui.separator();
                        let error = ui.visuals().error_fg_color;
                        ui.colored_label(error, "Could not load:");
                        for (path, err) in &listing.errors {
                            let name = path.file_name().map_or_else(
                                || path.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            ui.colored_label(error, format!("• {name}"))
                                .on_hover_text(err);
                        }
                    }
                } else {
                    // A query flattens the list to its matches, name-sorted (entries already are).
                    let matches: Vec<&library::LibraryEntry> = listing
                        .entries
                        .iter()
                        .filter(|e| library_matches(&e.file, &query))
                        .collect();
                    if matches.is_empty() {
                        ui.add_space(6.0);
                        ui.weak("No subgraphs match your search.");
                    } else {
                        for entry in &matches {
                            library_item(ui, entry, selection.as_deref(), &mut command);
                        }
                    }
                }
            });
    }
    if let Some(command) = command {
        apply_library_command(state, command);
    }
}

/// A collapsible category group in the browser: a header row (caret + name + count) that toggles a
/// persisted open state, with its entries listed indented beneath when open. Only drawn in the
/// unsearched, grouped view.
fn library_category(
    ui: &mut egui::Ui,
    category: &str,
    entries: &[&library::LibraryEntry],
    selection: Option<&std::path::Path>,
    command: &mut Option<LibraryCommand>,
) {
    let id = ui.make_persistent_id(("library-category", category));
    let mut open = ui.data_mut(|d| *d.get_temp_mut_or(id, true));
    if library_category_header(ui, category, entries.len(), open).clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    if open {
        // A small left inset sets the items under their category header, matching the mock.
        ui.indent(id, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for entry in entries {
                library_item(ui, entry, selection, command);
            }
        });
        ui.add_space(2.0);
    }
}

/// A category header row: a disclosure caret, the category name, and its entry count in muted ink.
/// The whole row is one click target that toggles the group; returns its response.
fn library_category_header(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    open: bool,
) -> egui::Response {
    let height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let caret = if open {
        egui_phosphor::regular::CARET_DOWN
    } else {
        egui_phosphor::regular::CARET_RIGHT
    };
    let painter = ui.painter();
    let mut x = rect.left() + 2.0;
    let mid = rect.center().y;
    painter.text(
        egui::pos2(x, mid),
        egui::Align2::LEFT_CENTER,
        caret,
        egui::FontId::proportional(11.0),
        theme::TEXT_TERTIARY,
    );
    x += 16.0;
    let name_rect = painter.text(
        egui::pos2(x, mid),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        theme::TEXT_SECONDARY,
    );
    painter.text(
        egui::pos2(name_rect.right() + 8.0, mid),
        egui::Align2::LEFT_CENTER,
        count.to_string(),
        egui::FontId::proportional(12.0),
        theme::TEXT_TERTIARY,
    );
    response
}

/// One library entry as a selectable browser row (the styled treatment shared by the grouped and
/// searched views). A left-click selects it (filling the inspector), a double-click inserts it (the
/// common action), and a right-click opens its context menu; each records a [`LibraryCommand`] for
/// the caller to apply once the listing borrow has ended.
fn library_item(
    ui: &mut egui::Ui,
    entry: &library::LibraryEntry,
    selection: Option<&std::path::Path>,
    command: &mut Option<LibraryCommand>,
) {
    let selected = selection == Some(entry.path.as_path());
    let response = library_item_row(ui, &entry.file.name, selected)
        .on_hover_ui(|ui| library_entry_tooltip(ui, &entry.file));
    if response.double_clicked() {
        *command = Some(LibraryCommand::Insert(entry.path.clone()));
    } else if response.clicked() {
        *command = Some(LibraryCommand::Select(entry.path.clone()));
    }
    response.context_menu(|ui| library_row_menu(ui, &entry.path, command));
}

/// Draws one browser row and returns its response. Three states, per the Frost handoff: the selected
/// row gets an accent wash with a 2px accent left border and near-white text; a hovered unselected
/// row gets a raised fill and near-white text; a resting row is bare with muted text.
fn library_item_row(ui: &mut egui::Ui, name: &str, selected: bool) -> egui::Response {
    let height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    let text_color = if selected {
        // The accent wash with a square-left, round-right radius, then a 2px accent bar hugging the
        // left edge, so the selected row reads as a tab pulled from the panel edge.
        painter.rect_filled(
            rect,
            egui::CornerRadius {
                nw: 0,
                ne: 3,
                sw: 0,
                se: 3,
            },
            theme::ACCENT_PRIMARY.gamma_multiply(0.16),
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                rect.left_top(),
                egui::pos2(rect.left() + 2.0, rect.bottom()),
            ),
            0.0,
            theme::ACCENT_PRIMARY,
        );
        theme::TEXT_PRIMARY
    } else if response.hovered() {
        painter.rect_filled(rect, 3.0, theme::BG_RAISED);
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    ui.painter().text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        text_color,
    );
    response
}

/// The right-click menu for a library browser row: the full action set, since a row has no
/// visible buttons of its own. "Edit Graph…" is intentionally absent until it is implemented (a
/// menu item that did nothing would be a dead stub); it joins here with its own step.
fn library_row_menu(
    ui: &mut egui::Ui,
    path: &std::path::Path,
    command: &mut Option<LibraryCommand>,
) {
    canvas::style_context_menu(ui);
    if ui.button("Insert").clicked() {
        *command = Some(LibraryCommand::Insert(path.to_path_buf()));
        ui.close();
    }
    if ui.button("Edit Details…").clicked() {
        *command = Some(LibraryCommand::EditDetails(path.to_path_buf()));
        ui.close();
    }
    ui.separator();
    if ui.button("Delete").clicked() {
        // Route through the inspector's armed confirm rather than deleting from the menu, so a
        // destructive action always gets a second look.
        *command = Some(LibraryCommand::ArmDelete(path.to_path_buf()));
        ui.close();
    }
}

/// A command produced by the library UI (a browser row's click or context menu, or the
/// inspector's buttons and kebab menu), applied by [`apply_library_command`] after the read-only
/// listing borrow ends so it can mutate [`AppState`].
enum LibraryCommand {
    /// Select this entry (or clear it if it is already selected), showing it in the inspector.
    Select(std::path::PathBuf),
    /// Drop a copy of the entry into the active canvas.
    Insert(std::path::PathBuf),
    /// Open the entry's documentation card for an in-place edit.
    EditDetails(std::path::PathBuf),
    /// Select the entry and arm its delete confirmation (surfaced in the inspector).
    ArmDelete(std::path::PathBuf),
    /// Confirm and carry out the delete.
    ConfirmDelete(std::path::PathBuf),
    /// Dismiss a pending delete confirmation.
    CancelDelete,
}

/// Applies a [`LibraryCommand`] to the app state once the read-only listing borrow has ended.
fn apply_library_command(state: &mut AppState, command: LibraryCommand) {
    match command {
        LibraryCommand::Select(path) => {
            // Toggle: re-selecting the current entry clears it, so the inspector can be dismissed.
            let same = state.library_selection.as_deref() == Some(path.as_path());
            state.library_selection = (!same).then_some(path);
            // Changing the selection disarms any pending delete so a confirm never lands on the
            // wrong entry.
            state.library_pending_delete = None;
        }
        LibraryCommand::Insert(path) => {
            if let Some(file) = state
                .library
                .entries
                .iter()
                .find(|e| e.path == path)
                .map(|e| e.file.clone())
            {
                state.insert_subgraph_from_library(&file);
            }
        }
        LibraryCommand::EditDetails(path) => state.open_library_edit(&path),
        LibraryCommand::ArmDelete(path) => {
            state.library_selection = Some(path.clone());
            state.library_pending_delete = Some(path);
        }
        LibraryCommand::ConfirmDelete(path) => state.delete_library_entry(&path),
        LibraryCommand::CancelDelete => state.library_pending_delete = None,
    }
}

/// The inspector pane below the browser: for the selected library entry, a reserved thumbnail
/// slot, its formatted documentation card, and its actions. The primary action is a prominent
/// Insert button; the rest (Edit Details…, Delete) live in a kebab menu beside it, mirroring the
/// browser row's right-click menu. Delete is guarded by an armed confirm. Shows a placeholder hint
/// when nothing is selected, and scrolls when the documentation is long.
fn library_inspector(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(path) = state.library_selection.clone() else {
        ui.add_space(8.0);
        ui.weak("Select a subgraph to see its details.");
        return;
    };
    let armed = state.library_pending_delete.as_deref() == Some(path.as_path());
    let mut command: Option<LibraryCommand> = None;
    {
        // Borrow only the listing (not all of `state`) so the command can mutate state afterward.
        let listing = &state.library;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // The pane clears a stale selection before drawing, so this normally resolves; a
                // one-frame miss just draws nothing rather than panicking.
                let Some(entry) = listing.entries.iter().find(|e| e.path == path) else {
                    return;
                };
                // The inspector opens with its own heading block (SUBGRAPH eyebrow + name + stat
                // line), then the preview. The actions sit directly under the image: seeing the
                // thumbnail is what tells the user this is the subgraph they want to act on, so the
                // buttons must be in view without scrolling. The descriptive detail (description,
                // ports, footer) follows below the actions.
                library_inspector_heading(ui, &entry.file);
                ui.add_space(11.0);
                library_thumbnail_slot(ui);
                ui.add_space(8.0);
                if armed {
                    // Deleting removes the file from disk, so confirm before doing it.
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Delete \"{}\"? This removes its file.", entry.file.name),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            command = Some(LibraryCommand::ConfirmDelete(path.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            command = Some(LibraryCommand::CancelDelete);
                        }
                    });
                } else {
                    library_command_bar(ui, &path, &mut command);
                }
                ui.add_space(8.0);
                library_port_sections(ui, &entry.file);
            });
    }
    if let Some(command) = command {
        apply_library_command(state, command);
    }
}

/// The inspector's heading block: a `SUBGRAPH` eyebrow, the subgraph name as a 17px semibold
/// heading, and a one-line stat (input/output counts and category) in muted monospace. Replaces the
/// old `INSPECTOR: <name>` pane heading, so the name reads as the subject of the card.
fn library_inspector_heading(ui: &mut egui::Ui, file: &library::SubgraphFile) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("SUBGRAPH")
            .size(10.5)
            .color(theme::TEXT_TERTIARY),
    );
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(file.name.as_str())
            .size(17.0)
            .family(egui::FontFamily::Name("plex-semibold".into()))
            .color(theme::TEXT_PRIMARY),
    );
    ui.add_space(3.0);
    let category = if file.category.trim().is_empty() {
        "Uncategorized"
    } else {
        file.category.trim()
    };
    let stat = format!(
        "{} · {} · {}",
        count_phrase(file.inputs.len(), "input"),
        count_phrase(file.outputs.len(), "output"),
        category,
    );
    ui.label(
        egui::RichText::new(stat)
            .family(egui::FontFamily::Monospace)
            .size(11.0)
            .color(theme::TEXT_TERTIARY),
    );
}

/// A count with its noun, pluralised: "1 input", "3 inputs".
fn count_phrase(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The inspector's action command bar: a primary accent-filled Insert that grows to fill the row,
/// then compact Edit-details and Delete icon buttons. Replaces the old single Insert button plus a
/// `⋮` kebab, so the actions are all visible. Delete arms the inspector's confirm rather than
/// deleting outright.
fn library_command_bar(
    ui: &mut egui::Ui,
    path: &std::path::Path,
    command: &mut Option<LibraryCommand>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // Insert takes the row minus the two fixed 34px icon buttons and their two 8px gaps.
        let insert_w = (ui.available_width() - 2.0 * (34.0 + 8.0)).max(72.0);
        if library_insert_button(ui, insert_w).clicked() {
            *command = Some(LibraryCommand::Insert(path.to_path_buf()));
        }
        if library_icon_button(
            ui,
            egui_phosphor::regular::PENCIL_SIMPLE,
            "Edit details",
            false,
        )
        .clicked()
        {
            *command = Some(LibraryCommand::EditDetails(path.to_path_buf()));
        }
        if library_icon_button(ui, egui_phosphor::regular::TRASH, "Delete subgraph", true).clicked()
        {
            *command = Some(LibraryCommand::ArmDelete(path.to_path_buf()));
        }
    });
}

/// The primary Insert button: an accent-filled pill of the given width with a dark semibold label
/// and a leading plus, brightening slightly on hover.
fn library_insert_button(ui: &mut egui::Ui, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
    let fill = if resp.hovered() {
        brighten(theme::ACCENT_PRIMARY, 1.08)
    } else {
        theme::ACCENT_PRIMARY
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "+  Insert",
        egui::FontId::new(13.5, egui::FontFamily::Name("plex-semibold".into())),
        theme::BG_ABYSS,
    );
    resp
}

/// A 34x32 command-bar icon button. Resting: raised fill, hairline border, muted glyph. Hover: a
/// brighter fill and glyph; or, for a destructive button, a danger-tinted fill, border, and glyph.
fn library_icon_button(
    ui: &mut egui::Ui,
    glyph: &str,
    tooltip: &str,
    danger: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 32.0), egui::Sense::click());
    let (fill, border, fg) = match (resp.hovered(), danger) {
        (false, _) => (theme::BG_RAISED, theme::LINE, theme::TEXT_SECONDARY),
        (true, false) => (theme::BG_HOVER, theme::LINE_STRONG, theme::TEXT_PRIMARY),
        (true, true) => (
            mix(theme::DANGER, theme::BG_RAISED, 0.20),
            theme::DANGER,
            theme::DANGER,
        ),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(15.0),
        fg,
    );
    resp.on_hover_text(tooltip)
}

/// Opaque sRGB blend: `t` of `a` over `1 - t` of `b`.
fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let c = |x: u8, y: u8| (f32::from(x) * t + f32::from(y) * (1.0 - t)).round() as u8;
    egui::Color32::from_rgb(c(a.r(), b.r()), c(a.g(), b.g()), c(a.b(), b.b()))
}

/// Scales a colour's channels by `f` (>1 lightens), clamped, keeping it opaque.
fn brighten(c: egui::Color32, f: f32) -> egui::Color32 {
    let s = |x: u8| (f32::from(x) * f).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

/// Draws the inspector's reserved thumbnail slot: a framed, centered square (matching the square
/// terrain render), spanning the pane width at [`LIBRARY_THUMB_HEIGHT`]. The offline subgraph render
/// is a later step, so today the box shows an empty state: a graph glyph and "No preview yet" over a
/// faint 45-degree hatch, on the deepest chrome fill. Reserving it now keeps the inspector from
/// reflowing when the render lands.
fn library_thumbnail_slot(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LIBRARY_THUMB_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 5.0, theme::BG_ABYSS);
    // A faint 45-degree hatch, clipped to the box, so the empty inset reads as an intentional "no
    // image" surface rather than a flat void. Drawn between the fill and the border.
    let hatched = painter.with_clip_rect(rect);
    let hatch = theme::LINE.gamma_multiply(0.35);
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        hatched.line_segment(
            [
                egui::pos2(x, rect.bottom()),
                egui::pos2(x + rect.height(), rect.top()),
            ],
            egui::Stroke::new(1.0, hatch),
        );
        x += 9.0;
    }
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center() - egui::vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::GRAPH,
        egui::FontId::proportional(28.0),
        theme::TEXT_TERTIARY,
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        "No preview yet",
        egui::FontId::proportional(11.0),
        theme::TEXT_TERTIARY,
    );
}

/// The inspector's port sections below the command bar: a divider, then the input and output ports
/// as role lists. Per the Frost handoff, the ports (name + role description) carry what the subgraph
/// does; the standalone description and the category/author/license footer are omitted here (they
/// remain in the browser hover card and are editable via Edit details). Draws nothing when the
/// subgraph is portless.
fn library_port_sections(ui: &mut egui::Ui, file: &library::SubgraphFile) {
    if file.inputs.is_empty() && file.outputs.is_empty() {
        return;
    }
    ui.separator();
    library_port_list(ui, "Inputs", &file.inputs);
    library_port_list(ui, "Outputs", &file.outputs);
}

/// A subgraph's ports on one side, as a role list under the section label: one row per port, each a
/// neutral accent dot, the port name in monospace, and a wrapped plain-language description of the
/// port's role. Draws nothing for a portless side.
fn library_port_list(ui: &mut egui::Ui, heading: &str, ports: &[library::PortDoc]) {
    if ports.is_empty() {
        return;
    }
    section_heading(ui, heading);
    for port in ports {
        let name = if port.name.trim().is_empty() {
            format!("#{}", port.index)
        } else {
            port.name.clone()
        };
        library_port_row(ui, &name, port.description.trim());
        ui.add_space(8.0);
    }
}

/// One port row: a neutral accent dot aligned to the name line, the port name in monospace, and its
/// role description wrapped beneath in muted ink. The dot is the single accent colour for every port
/// (Ymir has one data type, so ports are distinguished by role, not by a type colour).
fn library_port_row(ui: &mut egui::Ui, name: &str, description: &str) {
    let line_h = 18.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // A fixed dot column, so the names and descriptions align down the list. The dot sits on the
        // name's line, top-aligned to it.
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(18.0, line_h), egui::Sense::hover());
        ui.painter().circle_filled(
            egui::pos2(dot_rect.left() + 6.5, dot_rect.top() + line_h * 0.5),
            4.5,
            theme::ACCENT_PRIMARY,
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                egui::RichText::new(name)
                    .family(egui::FontFamily::Name("plex-mono-medium".into()))
                    .size(12.0)
                    .color(theme::TEXT_PRIMARY),
            );
            if !description.is_empty() {
                ui.label(
                    egui::RichText::new(description)
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                );
            }
        });
    });
}

/// The hover card for a library entry: its name, description, port counts, and (when present)
/// author and license.
fn library_entry_tooltip(ui: &mut egui::Ui, file: &library::SubgraphFile) {
    ui.strong(&file.name);
    if !file.description.trim().is_empty() {
        ui.label(&file.description);
    }
    ui.weak(format!(
        "{} in, {} out",
        file.inputs.len(),
        file.outputs.len()
    ));
    if !file.author.is_empty() {
        let who = if file.author.name.trim().is_empty() {
            "(unnamed author)".to_string()
        } else {
            file.author.name.clone()
        };
        ui.weak(format!("by {who}"));
    }
    if !file.license.trim().is_empty() {
        ui.weak(format!("License: {}", file.license));
    }
}

/// A per-node brush-UI label resolved by convention from the node's `type_id`
/// (`paint-<suffix>-<type_id>`), falling back to `default` when the node declares none. Mirrors how
/// node names resolve through `tr`, so a new paint node relabels itself by adding string entries.
fn paint_label(type_id: &str, suffix: &str, default: &str) -> String {
    let key = format!("paint-{suffix}-{type_id}");
    let value = tr(&key);
    // `tr` echoes an unknown key; fall back to the default rather than show the raw key.
    if value == key {
        default.to_string()
    } else {
        value.to_string()
    }
}

/// The selected node's inspector: its display-name override and parameter widgets.
/// The inspector controls for a paint node's stroke param: the brush, the enable toggle, the stroke
/// count, and undo/clear. The strokes themselves are authored by brushing on the 2D map or 3D surface;
/// these set the brush and the paint target, and undo/clear rewrite the node's strokes.
fn paint_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    handle: Handle,
    current: &ParamValue,
    params: &mut Params,
    changed: &mut bool,
) {
    let count = match current {
        ParamValue::Strokes(s) => s.len(),
        _ => 0,
    };
    let active = state.paint_target == Some(handle);

    // Per-node verb for the enable button (Sculpt vs Paint), resolved by convention from the node's
    // type_id, defaulting to Paint for any paint node without its own label.
    let type_id = state
        .graph
        .node_id_of(handle)
        .and_then(|id| state.graph.spec(id))
        .map_or("", |spec| spec.type_id);
    let verb = paint_label(type_id, "verb", "Paint");

    // Enable toggle: turning it on pins the preview to this node so the viewport keeps showing what
    // you paint or sculpt; the highlight and the "click to stop" copy show the on/off state.
    let label = if active {
        format!("{verb} — click to stop")
    } else {
        verb.clone()
    };
    if ui.selectable_label(active, label).clicked() {
        if active {
            state.paint_target = None;
            // Release the pin that was set when painting began, so stopping returns the preview to
            // following the selection. A manual pin the user placed on another node is left alone.
            if state.preview_pin == Some(handle) {
                state.preview_pin = None;
            }
        } else {
            // Pin the preview to this node so the viewport keeps showing what you paint even if the
            // selection changes mid-session; released above when painting stops.
            state.paint_target = Some(handle);
            state.preview_pin = Some(handle);
        }
    }

    ui.add_space(4.0);
    // Size and Strength are log-scaled so the low end (small brushes, subtle strokes) gets real
    // slider travel instead of being crammed into the first few pixels. Hardness stays linear over
    // its full [0, 1]. A log slider needs a positive floor, so Strength starts just above 0.
    brush_slider(ui, "Size", &mut state.paint_brush.radius, 0.005, 0.5, true);
    brush_slider(
        ui,
        "Strength",
        &mut state.paint_brush.strength,
        0.005,
        1.0,
        true,
    );
    brush_slider(
        ui,
        "Hardness",
        &mut state.paint_brush.hardness,
        0.0,
        1.0,
        false,
    );

    // Mode names are per-node: Raise/Lower for a sculpt, Paint/Erase for a mask. The positive mode
    // (index 0) maps to StrokeMode::Paint, the negative to Erase, whatever they are called.
    let pos = paint_label(type_id, "mode-pos", "Paint");
    let neg = paint_label(type_id, "mode-neg", "Erase");
    let mode_i = usize::from(matches!(state.paint_brush.mode, StrokeMode::Erase));
    if let Some(i) = segmented(ui, &[pos.as_str(), neg.as_str()], mode_i) {
        state.paint_brush.mode = if i == 0 {
            StrokeMode::Paint
        } else {
            StrokeMode::Erase
        };
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{count} stroke{}",
                if count == 1 { "" } else { "s" }
            ))
            .color(theme::TEXT_TERTIARY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(count > 0, egui::Button::new("Clear"))
                .clicked()
            {
                let mut strokes = params.get_strokes("strokes", &Strokes::new()).clone();
                strokes.clear();
                params.insert("strokes", ParamValue::Strokes(strokes));
                *changed = true;
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("Undo"))
                .clicked()
            {
                let mut strokes = params.get_strokes("strokes", &Strokes::new()).clone();
                strokes.pop();
                params.insert("strokes", ParamValue::Strokes(strokes));
                *changed = true;
            }
        });
    });
}

/// A labelled brush slider bound to an `f32`, over the shared styled slider.
fn brush_slider(ui: &mut egui::Ui, label: &str, value: &mut f32, min: f64, max: f64, log: bool) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [60.0, ui.spacing().interact_size.y],
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_TERTIARY)),
        );
        let mut v = f64::from(*value);
        if param_ui::slider(ui, &mut v, min, max, log).changed() {
            *value = v as f32;
        }
    });
}

/// Applies a paint sample from the 2D map to the active Paint node: begin a new stroke with the
/// current brush, or extend the last one, then write the strokes back so the mask updates live.
fn apply_paint_sample(state: &mut AppState, sample: viewport2d::PaintSample, mode: StrokeMode) {
    let Some(target) = state.paint_target else {
        return;
    };
    let Some(id) = state.graph.node_id_of(target) else {
        return;
    };
    let mut params = state.graph.params(id).cloned().unwrap_or_default();
    let mut strokes = params.get_strokes("strokes", &Strokes::new()).clone();
    let point = StrokePoint::new(sample.x, sample.y);
    if sample.begin || strokes.is_empty() {
        strokes.push(Stroke {
            radius: state.paint_brush.radius,
            strength: state.paint_brush.strength,
            hardness: state.paint_brush.hardness,
            mode,
            shape: BrushShape::Round,
            path: vec![point],
        });
    } else if let Some(mut last) = strokes.pop() {
        last.path.push(point);
        strokes.push(last);
    }
    params.insert("strokes", ParamValue::Strokes(strokes));
    if state.graph.set_params(id, params).is_err() {
        // The node vanished mid-frame; stop painting it rather than error every frame.
        state.paint_target = None;
    }
}

fn node_inspector(ui: &mut egui::Ui, state: &mut AppState) {
    // The SETTINGS half edits the selected node (distinct from the PREVIEW half above, which
    // follows the pinned node). Nothing selected shows the eyebrow and a hint.
    let Some(handle) = state.primary else {
        section_heading(ui, "Settings");
        ui.weak("Select a node to edit its parameters.");
        return;
    };
    let Some(id) = state.graph.node_id_of(handle) else {
        section_heading(ui, "Settings");
        ui.weak("Select a node to edit its parameters.");
        return;
    };
    let Some(spec) = state.graph.spec(id) else {
        section_heading(ui, "Settings");
        ui.weak("Select a node to edit its parameters.");
        return;
    };

    // SETTINGS eyebrow with a Reset-all action at the right (reverts every parameter to its
    // default).
    let mut reset = false;
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("SETTINGS")
                .size(10.5)
                .color(theme::TEXT_TERTIARY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            reset = ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!(
                            "{}  Reset",
                            egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE
                        ))
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                    )
                    .frame(false),
                )
                .on_hover_text("Reset all parameters to their defaults")
                .clicked();
        });
    });
    ui.add_space(4.0);

    // Name heading: the display name as a semibold heading.
    ui.label(
        egui::RichText::new(node_display_name(&state.graph, id))
            .family(egui::FontFamily::Name("plex-semibold".into()))
            .size(16.0)
            .color(theme::TEXT_PRIMARY),
    );

    // Decoupling cue: shown only when the pinned preview is a different node, so it is clear that
    // editing this node updates that downstream preview.
    if let Some(pinned) = state.preview_pin
        && pinned != handle
        && let Some(pinned_id) = state.graph.node_id_of(pinned)
    {
        ui.label(
            egui::RichText::new(format!(
                "↑ changes update the pinned {} preview",
                node_display_name(&state.graph, pinned_id)
            ))
            .size(11.0)
            .color(mix(theme::ACCENT_PRIMARY, theme::TEXT_SECONDARY, 0.78)),
        );
    }

    // Name field: a dense row — a fixed-width label and a deep mono input. The type name is the
    // hint, so an empty field shows the fallback.
    let type_name = tr(&format!("node-{}", spec.type_id)).to_string();
    let mut name = state.graph.name(id).unwrap_or("").to_string();
    ui.horizontal(|ui| {
        ui.add_sized(
            [34.0, ui.spacing().interact_size.y],
            egui::Label::new(egui::RichText::new("Name").color(theme::TEXT_TERTIARY)),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text(type_name.as_str())
                .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                .text_color(theme::TEXT_PRIMARY)
                .background_color(theme::BG_ABYSS)
                .desired_width(f32::INFINITY),
        );
        if resp.changed()
            && let Err(err) = state.graph.set_name(id, name_override(&name))
        {
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    });
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
    // Reset all: overwrite every parameter with its spec default before the rows read them, so the
    // controls show the defaults and the single write-back below persists them.
    if reset {
        for pspec in &spec.params {
            params.insert(pspec.name.clone(), pspec.default.clone());
        }
        changed = true;
    }
    for (index, pspec) in spec.params.iter().enumerate() {
        // A little vertical breathing room between parameters, so the panel does not read as a
        // dense stack (#90). Between rows only: none before the first or after the last.
        if index > 0 {
            ui.add_space(6.0);
        }
        let current = param_ui::current_value(&params, pspec);
        // A painted-mask param is authored by brushing on the 2D map, not by a value widget, so it
        // gets its own controls (brush + paint toggle + undo/clear) instead of `edit`.
        if matches!(pspec.kind, ParamKind::Strokes) {
            paint_controls(ui, state, handle, &current, &mut params, &mut changed);
            continue;
        }
        // The curve editor's corner pop-out icon (#70-style) reports through this flag,
        // opening the larger, draggable window for this node's curve param.
        let mut popout = false;
        if let Some(new_value) = param_ui::edit(
            ui,
            spec.type_id,
            pspec,
            &current,
            histogram.as_deref(),
            &mut popout,
        ) {
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

    // A subgraph container: its input/output ports are named by the inner boundary nodes, so let
    // them be renamed here without diving in (a container always has the `seed` param, so the
    // empty-params early return above never skips this). Placed after the params to match the
    // Name / seed / INPUTS / OUTPUTS reading order.
    if state.graph.nested(id).is_some() {
        subgraph_port_editor(ui, state, id);
    }
}

/// Editable input/output port lists for a selected subgraph container (#106 follow-up). A
/// subgraph port's name *is* the name of its inner `Input`/`Output` boundary node, so this reads
/// those names, shows the `marker_port_label` fallback ("Input 1", "Output 2", ...) for the unnamed
/// ones, and writes edits back through the container's inner graph. Because the derived ports and
/// the canvas pins both read the same boundary-node names, a rename here updates them too, and it
/// persists (the name rides `NodeDocument.name`) without invalidating caches (name overrides are
/// excluded from the content hash).
fn subgraph_port_editor(ui: &mut egui::Ui, state: &mut AppState, id: NodeId) {
    // Collect any rename requested this frame while only borrowing the inner graph immutably; apply
    // it afterwards against a mutable clone, so the display borrow never overlaps the write.
    let mut pending: Vec<(NodeId, Option<String>)> = Vec::new();
    if let Some(inner) = state.graph.nested(id) {
        pending.extend(port_section(ui, "INPUTS", inner, INPUT_TYPE_ID));
        pending.extend(port_section(ui, "OUTPUTS", inner, OUTPUT_TYPE_ID));
    }
    if pending.is_empty() {
        return;
    }
    let Some(mut inner) = state.graph.nested(id).cloned() else {
        return;
    };
    for (marker, name) in pending {
        if let Err(err) = inner.set_name(marker, name) {
            ui.colored_label(ui.visuals().error_fg_color, err.to_string());
        }
    }
    if let Err(err) = state.graph.set_nested(id, inner) {
        ui.colored_label(ui.visuals().error_fg_color, err.to_string());
    }
}

/// Renders one port section (all `marker_type` boundary nodes of `inner`, in port order) as a
/// heading over a row per port: an accent dot and a mono name field whose hint is the enumerated
/// fallback. Returns the `(marker, new name)` edits requested this frame; an empty field clears the
/// override back to the fallback. Nothing is drawn when the subgraph has no ports of this kind.
fn port_section(
    ui: &mut egui::Ui,
    heading: &str,
    inner: &Graph,
    marker_type: &str,
) -> Vec<(NodeId, Option<String>)> {
    let markers = inner.nodes_of_type(marker_type);
    let mut edits = Vec::new();
    if markers.is_empty() {
        return edits;
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(heading)
            .size(10.5)
            .color(theme::TEXT_TERTIARY),
    );
    ui.add_space(2.0);
    for (index, &marker) in markers.iter().enumerate() {
        let fallback = marker_port_label(marker_type, index);
        let mut name = inner.name(marker).unwrap_or("").to_string();
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 3.0, theme::ACCENT_PRIMARY);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut name)
                        .hint_text(fallback.as_str())
                        .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                        .text_color(theme::TEXT_PRIMARY)
                        .background_color(theme::BG_ABYSS)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                edits.push((marker, name_override(&name)));
            }
        });
    }
    edits
}

/// Quick-pick frame tints: a set spread across the hue wheel and separated in lightness, so any
/// two are easy to tell apart at a glance. General good practice for a grouping colour, offered
/// alongside the full HSV picker for an arbitrary choice.
const FRAME_SWATCHES: [[u8; 3]; 10] = [
    [0x5a, 0x61, 0x6b], // slate (the default)
    [0x1f, 0xa0, 0xc4], // cyan
    [0x4d, 0x7e, 0xf2], // blue
    [0x9b, 0x6c, 0xf2], // violet
    [0xd2, 0x4d, 0xa0], // magenta
    [0xe0, 0x5a, 0x5a], // rose
    [0xe0, 0x91, 0x3a], // amber
    [0xd9, 0xc9, 0x4d], // yellow
    [0x3c, 0xa8, 0x8a], // teal
    [0x5a, 0xb8, 0x4d], // green
];

/// Paints a colour swatch into `rect`: the colour over a neutral backing (so a translucent fill
/// reads as lighter), with a hairline rim. `alpha` false forces the swatch opaque (border/text).
fn paint_swatch(painter: &egui::Painter, rect: egui::Rect, srgba: [u8; 4], alpha: bool) {
    let a = if alpha { srgba[3] } else { 255 };
    // A mid-grey backing behind a translucent fill, so lower opacity reads as a paler swatch.
    if a < 255 {
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(120));
    }
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(srgba[0], srgba[1], srgba[2], a),
    );
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, theme::LINE_STRONG),
        egui::StrokeKind::Inside,
    );
}

/// One colour row of the frame inspector: a mono label, a swatch of the current colour, and a mono
/// hex readout. Clicking the swatch opens a popup with the quick-pick tints and the full HSV picker
/// (opacity included when `alpha` is `OnlyBlend`). Edits `hsva` in place; returns whether it moved.
fn color_row(
    ui: &mut egui::Ui,
    label: &str,
    hsva: &mut egui::ecolor::Hsva,
    alpha: egui::widgets::color_picker::Alpha,
) -> bool {
    use egui::widgets::color_picker::{Alpha, color_picker_hsva_2d};
    let show_alpha = matches!(alpha, Alpha::OnlyBlend);
    let mut changed = false;
    ui.horizontal(|ui| {
        param_ui::plain_label(ui, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let srgba = hsva.to_srgba_unmultiplied();
            let hex = if show_alpha {
                format!(
                    "#{:02x}{:02x}{:02x}{:02x}",
                    srgba[0], srgba[1], srgba[2], srgba[3]
                )
            } else {
                format!("#{:02x}{:02x}{:02x}", srgba[0], srgba[1], srgba[2])
            };
            // Reserve the width of the longest possible readout (#rrggbbaa) and right-align the hex
            // within it, so the swatch column lines up on every row whether or not alpha is shown.
            let font = egui::FontId::new(11.0, egui::FontFamily::Monospace);
            let color = theme::TEXT_TERTIARY;
            let hex_w = ui
                .painter()
                .layout_no_wrap("#000000ff".to_owned(), font.clone(), color)
                .size()
                .x;
            let (hex_rect, _) = ui.allocate_exact_size(
                egui::vec2(hex_w, ui.spacing().interact_size.y),
                egui::Sense::hover(),
            );
            ui.painter().text(
                hex_rect.right_center(),
                egui::Align2::RIGHT_CENTER,
                hex,
                font,
                color,
            );
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 16.0), egui::Sense::click());
            paint_swatch(ui.painter(), rect, srgba, show_alpha);
            let resp = resp.on_hover_text("Edit colour");
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(184.0);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.horizontal_wrapped(|ui| {
                    for sw in FRAME_SWATCHES {
                        let (r, sresp) =
                            ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                        paint_swatch(ui.painter(), r, [sw[0], sw[1], sw[2], 255], false);
                        // A bright ring marks the swatch matching the current colour.
                        if [srgba[0], srgba[1], srgba[2]] == sw {
                            ui.painter().rect_stroke(
                                r,
                                4.0,
                                egui::Stroke::new(2.0, theme::TEXT_PRIMARY),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if sresp.clicked() {
                            let keep = hsva.a;
                            *hsva = egui::ecolor::Hsva::from_srgb(sw);
                            if show_alpha {
                                hsva.a = keep;
                            }
                            changed = true;
                        }
                    }
                });
                ui.separator();
                if color_picker_hsva_2d(ui, hsva, alpha) {
                    changed = true;
                }
            });
        });
    });
    changed
}

/// The frame inspector's Delete action: a full-width button that stays quiet until hovered, when it
/// takes the danger colour so a destructive click is deliberate. Returns its response.
fn frame_delete_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let (fill, border, fg) = if resp.hovered() {
        (
            mix(theme::DANGER, theme::BG_RAISED, 0.20),
            theme::DANGER,
            theme::DANGER,
        )
    } else {
        (theme::BG_RAISED, theme::LINE, theme::TEXT_SECONDARY)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 5.0, fill);
    painter.rect_stroke(
        rect,
        5.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}  Delete frame", egui_phosphor::regular::TRASH),
        egui::FontId::proportional(13.0),
        fg,
    );
    resp
}

/// The selected frame's inspector (#94): edits its label, fill colour and opacity, border
/// colour, and label placement, plus a delete action. Shown in place of the node inspector
/// while a frame is selected.
fn frame_inspector(ui: &mut egui::Ui, state: &mut AppState, index: usize) {
    use egui::widgets::color_picker::Alpha;

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

    ui.spacing_mut().item_spacing.y = 8.0;

    ui.horizontal(|ui| {
        param_ui::plain_label(ui, "Label");
        ui.add(
            egui::TextEdit::singleline(&mut state.frames[index].label)
                .hint_text("Frame")
                .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                .text_color(theme::TEXT_PRIMARY)
                .background_color(theme::BG_ABYSS)
                .desired_width(f32::INFINITY),
        );
    });

    // OnlyBlend keeps the fill alpha a normal 0..1 opacity (no additive/HDR mode). Border and
    // label text are opaque. A dark label colour stays readable on a bright header.
    if color_row(ui, "Fill", &mut fill_hsva, Alpha::OnlyBlend) {
        state.frames[index].fill = fill_hsva.to_srgba_unmultiplied();
    }
    if color_row(ui, "Border", &mut border_hsva, Alpha::Opaque) {
        let c = border_hsva.to_srgba_unmultiplied();
        state.frames[index].border = [c[0], c[1], c[2]];
    }
    if color_row(ui, "Label text", &mut text_hsva, Alpha::Opaque) {
        let c = text_hsva.to_srgba_unmultiplied();
        state.frames[index].text = [c[0], c[1], c[2]];
    }
    // Persist the (possibly dragged) HSVA buffers so the hue carries to the next frame.
    state.frame_color_edit = Some((index, fill_hsva, border_hsva, text_hsva));

    ui.horizontal(|ui| {
        param_ui::plain_label(ui, "Label position");
        let placement = &mut state.frames[index].label_placement;
        let label = match placement {
            project_file::LabelPlacement::TopLeft => "Top left",
            project_file::LabelPlacement::TopCenter => "Top centre",
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let button = ui.button(format!(
                "{}   {}",
                label,
                egui_phosphor::regular::CARET_DOWN
            ));
            egui::Popup::menu(&button).show(|ui| {
                ui.set_min_width(button.rect.width());
                for (value, text) in [
                    (project_file::LabelPlacement::TopLeft, "Top left"),
                    (project_file::LabelPlacement::TopCenter, "Top centre"),
                ] {
                    if ui.selectable_label(*placement == value, text).clicked() {
                        *placement = value;
                        ui.close();
                    }
                }
            });
        });
    });

    ui.add_space(4.0);
    if frame_delete_button(ui).clicked() {
        state.frames.remove(index);
        state.selected_frame = None;
        state.frame_color_edit = None;
    }
}

/// A collapsible section for the World panel (the 1c handoff): a slim header of a chevron and an
/// uppercase label, optionally trailed by a muted `badge` (an active count), over a body that
/// shows only when open. With `divider`, a faint rule sits above the header so stacked sections read
/// as distinct bands (off for the first section, which has nothing above it). Open/closed state
/// persists for the session in egui memory, keyed by `id`.
fn section(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    default_open: bool,
    divider: bool,
    badge: Option<String>,
    body: impl FnOnce(&mut egui::Ui),
) {
    use egui::collapsing_header::CollapsingState;

    // A divider above the header separates this section from the block above it, bracketed by a
    // little space. The first section (no divider) skips that and sits close to the top of the pane.
    if divider {
        ui.add_space(6.0);
        let top = ui.available_rect_before_wrap().top();
        ui.painter().hline(
            ui.max_rect().x_range(),
            top,
            egui::Stroke::new(1.0, theme::LINE),
        );
        ui.add_space(6.0);
    } else {
        ui.add_space(2.0);
    }

    let sid = ui.make_persistent_id(id);
    let mut coll = CollapsingState::load_with_default_open(ui.ctx(), sid, default_open);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    if resp.clicked() {
        // `toggle` flips the flag but does not persist it; `store` writes it to session memory.
        coll.toggle(ui);
        coll.store(ui.ctx());
    }
    let openness = coll.openness(ui.ctx());
    let ink = if resp.hovered() {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    let painter = ui.painter();
    // The chevron, drawn as a small filled triangle rather than a font glyph so it never renders as
    // a missing-glyph box (IBM Plex has no geometric triangles): right-pointing when closed,
    // down-pointing when open.
    let cx = rect.left() + 10.0;
    let cy = rect.center().y;
    let tri = if openness > 0.5 {
        vec![
            egui::pos2(cx - 4.0, cy - 2.5),
            egui::pos2(cx + 4.0, cy - 2.5),
            egui::pos2(cx, cy + 3.0),
        ]
    } else {
        vec![
            egui::pos2(cx - 2.5, cy - 4.0),
            egui::pos2(cx - 2.5, cy + 4.0),
            egui::pos2(cx + 3.0, cy),
        ]
    };
    painter.add(egui::Shape::convex_polygon(tri, ink, egui::Stroke::NONE));
    painter.text(
        egui::pos2(rect.left() + 22.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(11.0),
        ink,
    );
    if let Some(badge) = badge {
        painter.text(
            egui::pos2(rect.right() - 4.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            badge,
            egui::FontId::proportional(11.0),
            theme::TEXT_TERTIARY,
        );
    }

    coll.show_body_unindented(ui, |ui| {
        ui.add_space(2.0);
        body(ui);
    });
}

/// A frost pill toggle bound to a bool: draws the switch (accent track when on) and flips the value
/// on click. Returns the response.
fn switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let resp = param_ui::toggle(ui, *on);
    if resp.clicked() {
        *on = !*on;
    }
    resp
}

/// One labelled slider row that fits the narrow panel without overflowing: a fixed label column on
/// the left, a fixed scrub/type value box pinned right, and the slider filling the gap between them
/// (its own inline value suppressed, since the box is the value). `decimals` fixes the precision.
/// A single value edited by both the box and the slider, via a local copy to avoid a double borrow.
fn slider_row<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    decimals: usize,
) {
    let mut x = *value;
    let h = ui.spacing().interact_size.y;
    let span = range.end().to_f64() - range.start().to_f64();
    ui.horizontal(|ui| {
        // Left-aligned label in a column reserved at an exact width (drawn via the painter, so the
        // column never grows or shrinks with the label's length). This keeps every slider starting
        // at the same x and coming out the same width. Dimmed when the group is disabled.
        let (label_rect, _) = ui.allocate_exact_size(egui::vec2(76.0, h), egui::Sense::hover());
        let label_colour = if ui.is_enabled() {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_SECONDARY.gamma_multiply(0.5)
        };
        ui.painter().text(
            egui::pos2(label_rect.left(), label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            label_colour,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_sized(
                [44.0, h],
                egui::DragValue::new(&mut x)
                    .range(range.clone())
                    .speed(span * 0.002)
                    .fixed_decimals(decimals),
            );
            // The app's one styled slider (visible trough, accent fill, ringed knob) fills the width
            // left between the value box and the label, so the left panel matches the node params.
            let (min, max) = (range.start().to_f64(), range.end().to_f64());
            let mut v = x.to_f64();
            if param_ui::slider(ui, &mut v, min, max, false).changed() {
                x = Num::from_f64(v);
            }
        });
    });
    *value = x;
}

/// A subtle full-width divider between water setting groups, so the groups read as distinct bands.
fn group_separator(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let top = ui.available_rect_before_wrap().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        top,
        egui::Stroke::new(1.0, theme::LINE),
    );
    ui.add_space(6.0);
}

/// A water effect group: a divider, a header row of the group name (a step larger than the param
/// labels) and an enable toggle, over the params it gates. Flat (no bordered box), so nothing paints
/// a hard right edge against the pane border. When `on` is false the params grey out and stop
/// responding (`add_enabled_ui`) while the header toggle stays live, so the group can be reenabled.
fn water_group(ui: &mut egui::Ui, title: &str, on: &mut bool, body: impl FnOnce(&mut egui::Ui)) {
    group_separator(ui);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .size(13.0)
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            switch(ui, on);
        });
    });
    ui.add_enabled_ui(*on, body);
}

/// The world/build settings: the global eval-request inputs (seed, resolutions) that
/// apply to the whole graph, laid out as collapsible World, Build, Water, and Outputs sections.
fn world_settings(ui: &mut egui::Ui, state: &mut AppState) {
    // Frost accent: fill sliders up to the handle with the bright accent (the default trailing fill
    // uses the muted selection colour). Scoped to this pane by mutating its visuals only.
    ui.visuals_mut().selection.bg_fill = theme::ACCENT_PRIMARY;
    ui.visuals_mut().slider_trailing_fill = true;

    // WORLD: the identity and most-touched settings, now a collapsible section like the rest.
    section(
        ui,
        "world_section_world",
        "World",
        true,
        false,
        None,
        |ui| {
            ui.horizontal(|ui| {
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut state.seed).speed(1.0));
            });

            ui.add_space(4.0);
            ui.label("World extent");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut state.world_extent)
                        .speed(8.0)
                        .range(1.0..=1_000_000.0)
                        .suffix(" m"),
                );
                // The meters-to-cells bridge made tangible. Cells are square, so this is the size along
                // both axes; it follows from extent / build resolution.
                let m_per_cell = state.world_extent / state.build_res as f64;
                ui.weak(format!("≈ {m_per_cell:.3} m/cell at build"));
            });

            ui.add_space(4.0);
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

            ui.add_space(4.0);
            // Show water: the master toggle for the water overlay, on its own row directly above the sea
            // level so it is easy to find (it used to hide at the right edge of the sea-level header).
            ui.horizontal(|ui| {
                ui.label("Show water");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    switch(ui, &mut state.show_water);
                });
            });
            ui.add_space(2.0);
            ui.label("Sea level");
            // Normalized height in [0, 1]; the 3D viewport draws a water plane here. Slider fills the row
            // with a scrub/type value box pinned right (a local copy avoids the double borrow); the
            // elevation in meters (sea_level × world_height) reads below it.
            let mut sl = state.sea_level;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_sized(
                        [48.0, ui.spacing().interact_size.y],
                        egui::DragValue::new(&mut sl)
                            .range(0.0..=1.0)
                            .speed(0.002)
                            .fixed_decimals(3),
                    );
                    // The styled slider (clamps to [0, 1] by construction), matching the rest.
                    param_ui::slider(ui, &mut sl, 0.0, 1.0, false);
                });
            });
            state.sea_level = sl;
            let meters = state.sea_level * state.world_height;
            ui.weak(format!("≈ {meters:.0} m elevation"));
        },
    );

    // BUILD AND PREVIEW: the resolutions a Build and the interactive preview evaluate at.
    section(
        ui,
        "world_section_build",
        "Build and Preview",
        true,
        true,
        None,
        |ui| {
            ui.label("Build resolution");
            ui.horizontal(|ui| {
                // Custom value (UE5 landscapes need specific sizes), with presets as shortcuts.
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Preview resolution");
                ui.add(
                    egui::DragValue::new(&mut state.preview_res)
                        .speed(4.0)
                        .range(32..=1024),
                );
            });
        },
    );

    // WATER: the rendering look, grouped (the 1c handoff) into Surface / Depth / Foam, each a header
    // row with an enable toggle owning the params it gates. The effect toggles are those headers.
    section(ui, "world_section_water", "Water", true, true, None, |ui| {
        ui.horizontal(|ui| {
            ui.label("Water colour");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // egui's colour button round-trips rgb -> Hsva -> rgb every frame, which drifts the
                // floats (0.1 -> 0.10000002) even with no interaction; committing that back would
                // mark the project modified on the first frame (#160). Edit a copy and only store it
                // on a real change, so the silent round-trip never dirties the persisted colour.
                let mut colour = state.water_color;
                if ui.color_edit_button_rgb(&mut colour).changed() {
                    state.water_color = colour;
                }
            });
        });
        // Depth sits directly below the colour it tints.
        water_group(ui, "Depth", &mut state.water_depth, |ui| {
            slider_row(ui, "Falloff", &mut state.water_extinction, 1.0..=30.0, 1);
        });
        // Gerstner waves (the geometric wave surface) and the reflective finish toggle separately, so
        // you can have flat mirror water or matte chop.
        water_group(ui, "Gerstner waves", &mut state.water_waves, |ui| {
            slider_row(ui, "Speed", &mut state.water_speed, 0.0..=2.0, 2);
            slider_row(ui, "Amplitude", &mut state.water_wave, 0.0..=1.0, 2);
            slider_row(ui, "Steepness", &mut state.water_steepness, 0.0..=1.0, 2);
            slider_row(ui, "Wavelength", &mut state.water_wavelength, 0.3..=3.0, 2);
        });
        water_group(ui, "Reflection", &mut state.water_reflection, |ui| {
            slider_row(
                ui,
                "Reflectivity",
                &mut state.water_reflectivity,
                0.0..=1.0,
                2,
            );
            slider_row(ui, "Specular", &mut state.water_specular, 0.0..=1.0, 2);
        });
        water_group(ui, "Foam", &mut state.water_foam_on, |ui| {
            slider_row(ui, "Amount", &mut state.water_foam, 0.0..=1.0, 2);
            slider_row(ui, "Width", &mut state.water_foam_width, 0.0..=0.05, 3);
        });
        water_group(ui, "Wet shore", &mut state.water_wet_on, |ui| {
            slider_row(ui, "Strength", &mut state.water_wet, 0.0..=1.0, 2);
            slider_row(ui, "Width", &mut state.water_wet_width, 0.0..=0.1, 3);
        });
    });

    // OUTPUTS: endpoints a Build will write, with a badge counting how many are ticked. Endpoints
    // are nodes with no outputs; collect them (releasing the snarl borrow) before the body mutates
    // params, and count the active ones for the header badge.
    let endpoints: Vec<NodeId> = state
        .snarl
        .node_ids()
        .filter_map(|(_, &handle)| state.graph.node_id_of(handle))
        .filter(|&id| state.graph.spec(id).is_some_and(|s| s.outputs.is_empty()))
        .collect();
    let active = endpoints
        .iter()
        .filter(|&&id| {
            state
                .graph
                .params(id)
                .is_none_or(|p| p.get_bool("build", true))
        })
        .count();
    let badge = (!endpoints.is_empty()).then(|| active.to_string());
    section(
        ui,
        "world_section_outputs",
        "Outputs",
        true,
        true,
        badge,
        |ui| {
            ui.weak("Endpoints a Build will write; tick to include.");
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
        },
    );
}

/// The preview's pin toggle: a 24px square. Pinned = accent fill, accent border, light pin glyph
/// (click to unpin). Unpinned but with something to pin = a raised outline with a muted glyph (click
/// to pin). Nothing to pin = a faint, inert outline. Returns its response.
fn pin_toggle(ui: &mut egui::Ui, pinned: bool, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let (fill, border, fg) = if pinned {
        (
            theme::ACCENT_PRIMARY,
            theme::ACCENT_PRIMARY,
            theme::TEXT_PRIMARY,
        )
    } else if !enabled {
        (
            egui::Color32::TRANSPARENT,
            theme::LINE,
            theme::TEXT_TERTIARY.gamma_multiply(0.6),
        )
    } else if resp.hovered() {
        (theme::BG_HOVER, theme::LINE_STRONG, theme::TEXT_PRIMARY)
    } else {
        (theme::BG_RAISED, theme::LINE, theme::TEXT_SECONDARY)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::PUSH_PIN,
        egui::FontId::proportional(14.0),
        fg,
    );
    resp.on_hover_text(if pinned {
        "Unpin preview"
    } else {
        "Pin this node's preview"
    })
}

/// The small `PINNED` pill shown beside the pinned node's name: accent text in an accent outline.
fn pinned_pill(ui: &mut egui::Ui) {
    name_pill(ui, "PINNED", theme::ACCENT_PRIMARY);
}

/// The small `EXPERIMENTAL` pill shown beside an experimental node's name: warning-amber, distinct
/// from the accent `PINNED` pill and colourblind-safe against it.
fn experimental_pill(ui: &mut egui::Ui) {
    name_pill(ui, "EXPERIMENTAL", theme::WARNING).on_hover_text(
        "Functional but rough: expect artifacts. A stylistic effect, not a settled tool.",
    );
}

/// A small outlined text pill in `color`, shared by the node-name badges. Returns its response so a
/// caller can attach a tooltip.
fn name_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let font = egui::FontId::proportional(9.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let pad = egui::vec2(6.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, color);
    resp
}

/// A segmented control: `labels` shown as one pill in a deep `c0` container, the active segment
/// filled `c3` with bright ink, the rest muted (an inactive segment lifts slightly on hover).
/// Returns the index of a newly-clicked (inactive) segment, else `None`. Sizes to its labels.
fn segmented(ui: &mut egui::Ui, labels: &[&str], active: usize) -> Option<usize> {
    let font = egui::FontId::proportional(12.0);
    let seg_pad_x = 12.0;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap((*l).to_string(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
                + seg_pad_x * 2.0
        })
        .collect();
    let inner_h = 22.0;
    let outer = 2.0;
    let total_w = widths.iter().sum::<f32>() + outer * 2.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, inner_h + outer * 2.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 5.0, theme::BG_ABYSS);
    let mut clicked = None;
    let mut x = rect.left() + outer;
    let top = rect.top() + outer;
    for (i, (label, w)) in labels.iter().zip(&widths).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(*w, inner_h));
        let resp = ui.interact(
            seg,
            ui.id().with(("segmented", i, *label)),
            egui::Sense::click(),
        );
        let is_active = i == active;
        if is_active {
            ui.painter().rect_filled(seg, 4.0, theme::BG_HOVER);
        } else if resp.hovered() {
            ui.painter().rect_filled(seg, 4.0, theme::BG_RAISED);
        }
        ui.painter().text(
            seg.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            font.clone(),
            if is_active {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_TERTIARY
            },
        );
        if resp.clicked() && !is_active {
            clicked = Some(i);
        }
        x += w;
    }
    clicked
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

    // The PREVIEW half shows the pinned node's output; the SETTINGS half below edits the selected
    // node. The eyebrow makes that pinned-vs-selected split explicit (the two are usually different
    // nodes).
    section_heading(ui, "Preview");
    // One consistent gap between every preview row (the default is too tight; the reserved dial row
    // was too loose).
    ui.spacing_mut().item_spacing.y = 6.0;

    // The preview shows the pinned node if one is set, else the selection. Only nodes with
    // an output qualify; evaluating an endpoint would run its side effect. When nothing is
    // selected the same layout is drawn with placeholder content (a neutral header and a
    // black image), so the pane never collapses or swaps to a bare box.
    let target = state.preview_target();
    let id = target.and_then(|t| state.graph.node_id_of(t));
    let is_pinned = target.is_some() && state.preview_pin == target;

    // Pinned identity row: a pin toggle (accent-filled while pinned, an outline while the preview
    // merely follows the selection), the previewed node's name, and a PINNED pill. The preview
    // freshness (up to date / evaluating / error) is a hover on the name rather than a visible dot,
    // keeping the row to the pin + name + pill of the design.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if pin_toggle(ui, is_pinned, target.is_some()).clicked() {
            state.preview_pin = if is_pinned { None } else { target };
        }
        match id {
            Some(id) => {
                ui.label(
                    egui::RichText::new(node_display_name(&state.graph, id))
                        .family(egui::FontFamily::Name("plex-semibold".into()))
                        .size(14.0)
                        .color(theme::TEXT_PRIMARY),
                )
                .on_hover_text(state.preview.status_label());
                if is_pinned {
                    pinned_pill(ui);
                }
                if state
                    .graph
                    .spec(id)
                    .is_some_and(|s| is_experimental(s.type_id))
                {
                    experimental_pill(ui);
                }
                // A bypassed node shows its input, not its own output (#105).
                if state.graph.is_bypassed(id) {
                    ui.label(
                        egui::RichText::new("bypassed")
                            .size(10.5)
                            .color(theme::TEXT_TERTIARY),
                    );
                }
            }
            None => {
                ui.label(egui::RichText::new("No node").color(theme::TEXT_TERTIARY));
            }
        }
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
            // A plain menu popup, not an egui ComboBox: the ComboBox wraps its items in a scroll
            // area whose auto-shrink makes the viewport equal the content height, so rounding
            // flickers a scrollbar in and out. This handful of outputs needs no scrolling, so a menu
            // renders them directly.
            let button = ui.button(format!(
                "{}   {}",
                output_names[selected],
                egui_phosphor::regular::CARET_DOWN
            ));
            egui::Popup::menu(&button).show(|ui| {
                ui.set_min_width(button.rect.width());
                for (index, name) in output_names.iter().enumerate() {
                    if ui.selectable_label(index == selected, name).clicked() {
                        selected = index;
                        ui.close();
                    }
                }
            });
        });
        state.preview.set_display_output(selected);
    }

    // View controls (persistent display settings, shown even with no node selected): a segmented
    // Heightfield/Relief mode, and on the right either the relief light dial or a segmented
    // Auto/Fixed scale. Heightfield is the greyscale field; Relief is the lit topography. Auto
    // stretches the field's actual range; Fixed maps a true [0, 1].
    let mut mode = state.preview.mode();
    let mut scale = state.preview.scale();
    // Stacked, not side by side: the panel is too narrow for both segmented controls on one row.
    // First row: the Heightfield/Relief mode.
    let mode_i = usize::from(mode == shade::ShadeMode::Relief);
    if let Some(i) = segmented(ui, &["Heightfield", "Relief"], mode_i) {
        mode = if i == 0 {
            shade::ShadeMode::Height
        } else {
            shade::ShadeMode::Relief
        };
    }
    // Second row: the Auto/Fixed scale in Heightfield, the light dial in Relief.
    if mode == shade::ShadeMode::Relief {
        // A fixed-height row equal to the dial, laid out centre-aligned, so the "Light" label,
        // the dial, and the angle readout all sit on the dial's midline. A plain `ui.horizontal`
        // centres each widget against the row height known when it is placed, so the label (added
        // before the taller dial) would float up while the dial and readout centre lower.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), sun::DIAL_SIZE),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("Light").color(theme::TEXT_SECONDARY),
                ));
                state.preview.light_indicator(ui);
                // Read the angles after the dial so a drag this frame is reflected.
                let (az, alt) = state.preview.light_angles();
                ui.label(
                    egui::RichText::new(format!("{az:.0}° · {alt:.0}°"))
                        .family(egui::FontFamily::Monospace)
                        .color(theme::TEXT_TERTIARY),
                );
            },
        );
    } else {
        // Sized to its content, so it does not leave a gap above the image in Heightfield mode.
        ui.horizontal(|ui| {
            let scale_i = usize::from(scale == shade::HeightScale::Fixed);
            if let Some(i) = segmented(ui, &["Auto", "Fixed"], scale_i) {
                scale = if i == 0 {
                    shade::HeightScale::Auto
                } else {
                    shade::HeightScale::Fixed
                };
            }
        });
    }
    state.preview.set_mode(mode);
    state.preview.set_scale(scale);
    state
        .preview
        .set_water(state.sea_level as f32, state.show_water);

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

/// A subtle all-caps section label (the handoff's `ink-lo` eyebrow): uppercased, small, and muted,
/// set on the pane background rather than a heavy header band. Shared by the browser and the port
/// lists so their section labels match.
fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(10.5)
            .color(theme::TEXT_TERTIARY),
    );
    ui.add_space(4.0);
}

/// Draws a pane header strip: a full-width band a touch darker than the pane body, with
/// `contents` laid out horizontally inside it. Shared by the preview and node-list panes so
/// their headers match.
fn header_strip(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::BG_ABYSS)
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
    // Captured before borrowing the menu: subgraph boundary markers are disabled (shown
    // greyed) outside a subgraph (#106), in the menu the same as in the palette.
    let inside_subgraph = !state.nav.is_empty();
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
            menu.highlight = step_highlight(&rows, menu.highlight, true);
        }
        if up {
            menu.highlight = step_highlight(&rows, menu.highlight, false);
        }
        menu.highlight = menu.highlight.min(n - 1);
        // A query change or drill-in can leave the highlight on a divider; nudge off it so
        // Enter always has a real row.
        if matches!(rows.get(menu.highlight), Some(MenuRow::Separator)) {
            menu.highlight = step_highlight(&rows, menu.highlight, true);
        }
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
                // The blue selection is the only row highlight; suppress egui's own hover box
                // (fill and stroke) so it never competes with the blue. egui styles whichever
                // row the pointer is physically over, so mid-transit the entering row would draw
                // its own hover box a frame before `menu.highlight` catches up to it, flashing a
                // second highlight next to the leaving row's blue one. Suppressing both leaves a
                // single highlight, the blue selection, which already tracks the pointer. (The
                // row height is pinned in `menu_row`, so this is only about the visible box, not
                // the layout.)
                let hovered_vis = &mut ui.visuals_mut().widgets.hovered;
                hovered_vis.weak_bg_fill = egui::Color32::TRANSPARENT;
                hovered_vis.bg_fill = egui::Color32::TRANSPARENT;
                hovered_vis.bg_stroke = egui::Stroke::NONE;
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
                    // A divider is drawn as a plain separator line and takes no interaction,
                    // so it never becomes the hovered/highlighted or clicked row.
                    if matches!(row, MenuRow::Separator) {
                        ui.separator();
                        continue;
                    }
                    let enabled = match row {
                        MenuRow::Node(type_id) => node_addable(type_id, inside_subgraph),
                        _ => true,
                    };
                    let resp = menu_row(ui, row, i == menu.highlight, enabled);
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
            // A disabled marker (top level) is a no-op: keyboard Enter on it keeps the menu
            // open rather than creating it (the mouse path is already blocked by add_enabled).
            MenuRow::Node(type_id) if !node_addable(type_id, inside_subgraph) => {}
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
            // A divider is never activated (navigation and clicks skip it).
            MenuRow::Separator => {}
        }
    }
}

/// One menu row: a full-width selectable button showing the row's [`menu_row_text`],
/// with a disclosure chevron painted at the right edge for a category, so the chevrons
/// align in a column instead of trailing each name at a different x (#93). `selected`
/// draws the keyboard/hover highlight.
fn menu_row(ui: &mut egui::Ui, row: MenuRow, selected: bool, enabled: bool) -> egui::Response {
    // Pin the row to the boxed height so a state change can never reflow the column. egui's
    // selectable button draws a plain row (inactive, unselected) with no frame stroke but a
    // boxed row (hovered or selected) with one, and `Frame::total_margin` counts the stroke
    // width, so a boxed row is 2px taller than a plain one. Normally one row is boxed (the
    // selection) and it is stable, but mid-transit the leaving row is still selected while the
    // entering row is hovered: two boxed rows at once grow the menu 2px, then it shrinks when
    // the highlight catches up, a visible jitter. A floor equal to the boxed height (text plus
    // both button-padding edges) makes every row that height, so the box only ever recolours a
    // fixed-size row.
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * ui.spacing().button_padding.y;
    let resp = ui
        .add_enabled(
            enabled,
            egui::Button::selectable(selected, menu_row_text(row)).min_size(egui::vec2(0.0, row_h)),
        )
        .on_disabled_hover_text("Only available inside a subgraph");
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
    if let MenuRow::Node(type_id) = row
        && is_experimental(type_id)
    {
        // A small amber marker at the right edge flags the node as experimental before it is added.
        let x = resp.rect.right() - ui.spacing().button_padding.x;
        ui.painter().text(
            egui::pos2(x, resp.rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "exp",
            egui::FontId::proportional(9.5),
            theme::WARNING,
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

/// Whether two canvas transforms are close enough to treat as equal, so a re-fit that reproduces
/// the applied view can stop requesting frames. Sub-pixel translation and a tiny scale epsilon.
fn transforms_close(a: egui::emath::TSTransform, b: egui::emath::TSTransform) -> bool {
    (a.scaling - b.scaling).abs() < 1e-3 && (a.translation - b.translation).length() < 0.5
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
    // Collapsed (or animating past the collapse point): draw only the labeled bar, no graph. The
    // stat is the node count of the active graph.
    if pane_collapsed(ui.available_height()) {
        let n = state.graph.node_count();
        let stat = format!("{n} node{}", if n == 1 { "" } else { "s" });
        workspace_collapsed_bar(ui, egui_phosphor::regular::GRAPH, "Graph", &stat, state);
        return;
    }
    // No vertical item spacing in the pane: the breadcrumb bar (when shown) then butts directly
    // against the snarl canvas below it, with no strip of dark pane background showing between them.
    ui.spacing_mut().item_spacing.y = 0.0;
    // Breadcrumb while inside a subgraph (#106): "Project › Mountain › …", each earlier
    // segment a link that pops back out to that level. Shown only when dived in, so the
    // top-level canvas is unchanged. Runs first so a click swaps the active context before
    // the canvas below renders it this frame.
    if !state.nav.is_empty() {
        let depth = state.nav.len();
        let mut exit_target: Option<usize> = None;
        // A chrome address bar spanning the canvas width: deepest chrome fill with a bottom hairline
        // to the light canvas, padded on the left and vertically centred. "Project" and each earlier
        // container are accent links back to that level; the current context is inert ink-hi; the
        // separators are muted.
        let bar = egui::Frame::new()
            .fill(theme::BG_ABYSS)
            .inner_margin(egui::Margin {
                left: 12,
                right: 10,
                top: 6,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ui.link("Project").clicked() {
                        exit_target = Some(0);
                    }
                    for (i, frame) in state.nav.iter().enumerate() {
                        ui.label(egui::RichText::new("›").color(theme::TEXT_TERTIARY));
                        if i + 1 == depth {
                            // The current context: same font and size as the links, distinguished by
                            // colour only (bright ink-hi vs the accent links), so nothing reads as a
                            // different size.
                            ui.label(egui::RichText::new(&frame.label).color(theme::TEXT_PRIMARY));
                        } else if ui.link(&frame.label).clicked() {
                            exit_target = Some(i + 1);
                        }
                    }
                });
            });
        ui.painter().hline(
            bar.response.rect.x_range(),
            bar.response.rect.bottom(),
            egui::Stroke::new(1.0, theme::LINE),
        );
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
    // A request to frame the whole graph, set after opening a project or on first launch. Read,
    // not taken: it is cleared only once the fit actually succeeds (below), because on the very
    // first render the snarl has not measured its nodes yet, so the fit has no rects to frame and
    // must retry next frame rather than being consumed to no effect (the startup top-left bug).
    let frame_to_graph = state.frame_to_graph_request;

    // Per-node thumbnails (#42): evaluate every output-producing node at thumbnail
    // resolution off-thread, and draw each result in its node body below.
    let thumb_request = EvalRequest::new(
        thumbnails::THUMB_RES,
        thumbnails::THUMB_RES,
        Region::UNIT,
        state.seed,
    )
    .with_world_extent(state.world_extent)
    .with_world_height(state.world_height)
    .with_sea_level(state.sea_level);
    // The working set is culled to the last-frame view (#74): off-screen nodes and a
    // zoomed-out canvas (where a thumbnail is too small to read) are skipped, so a
    // large graph evaluates only what is on screen. Disabled entirely from the View
    // menu.
    let visible: Vec<canvas::Handle> = if state.thumbnails_enabled {
        // Nodes that get a thumbnail, paired with their canvas position: output-producing
        // ones, plus subgraph Output markers (which show the field feeding them, #106).
        let candidates: Vec<(canvas::Handle, egui::Pos2)> = state
            .snarl
            .node_ids()
            .filter_map(|(snarl_id, &h)| {
                let has_thumbnail = state
                    .graph
                    .node_id_of(h)
                    .and_then(|id| state.graph.spec(id))
                    .is_some_and(|spec| !spec.outputs.is_empty() || spec.type_id == OUTPUT_TYPE_ID);
                let pos = state.snarl.get_node_info(snarl_id).map(|info| info.pos);
                has_thumbnail.then_some(()).and(pos).map(|p| (h, p))
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
    // Inside a subgraph, bind the live input fields so interior thumbnails show real data
    // rather than the Input markers' zero stand-in (#106). `None` at the top level.
    let binding = state.subgraph_inputs();
    state.thumbnails.sync(
        &state.graph,
        &visible,
        &thumb_request,
        now,
        binding.as_ref(),
    );
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
        create_subgraph_request: None,
        save_to_library_request: None,
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
    // Wires and pins in the cyan connection accent, drawn prominent: a thicker wire and larger pin
    // than snarl's small defaults, so connections read clearly on the light canvas (our own pin
    // style, just bolder, not the handoff's typed ringed ports). Width/colour become settings
    // later (#57).
    let style = egui_snarl::ui::SnarlStyle {
        wire_width: Some(3.0),
        pin_fill: Some(theme::ACCENT_FROST),
        pin_size: Some(13.0),
        // Selection reads as a filled accent header strip (painted in `show_header`) plus a bold
        // accent frame and an outer glow (painted in `final_node_rect`) — a lit title bar that stands
        // out in a dense graph. No selection fill over the body: snarl's default grey fill dimmed the
        // card and read like a bypassed node, and even a faint cyan wash muddied the preview thumbnail
        // and text, so the body stays fully legible and the header carries the signal.
        // Selection is drawn as an accent header plus an accent card border (the `node_frame` viewer
        // override), which sits tight on the card edge and covers snarl's sub-pixel rounding seam.
        // Snarl's own selection rect is disabled (no fill, no stroke) so it does not add a second,
        // looser border outside the card.
        select_stoke: Some(egui::Stroke::NONE),
        select_fill: Some(egui::Color32::TRANSPARENT),
        // The Frost canvas is a frosted icy-light surface (the one light region in the dark chrome),
        // with no grid for now (the grid draw is suppressed in `draw_background`).
        bg_frame: Some(egui::Frame::new().fill(theme::CANVAS_BASE)),
        // Node cards: a light frosted body with a 1px hairline border, 5px corners, and a soft drop
        // shadow, and a slightly darker header strip. Node text is dark on light via the scoped
        // `canvas_visuals` applied to the widget below.
        node_frame: Some(
            egui::Frame::new()
                .fill(theme::NODE_BG)
                .stroke(egui::Stroke::new(1.0, theme::NODE_LINE))
                .corner_radius(5)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 3],
                    blur: 9,
                    spread: 0,
                    color: egui::Color32::from_rgba_unmultiplied(15, 30, 50, 115),
                })
                // Top margin matches the header frame's top margin (4) so the header sits flush with
                // the card's top edge: any larger top margin leaves a band of the light card body
                // above the header, which is jarring once the header is filled with the accent. The
                // sides and bottom keep 6px so the body pins and preview have room.
                .inner_margin(egui::Margin {
                    left: 6,
                    right: 6,
                    top: 4,
                    bottom: 6,
                }),
        ),
        header_frame: Some(
            egui::Frame::new()
                .fill(theme::NODE_HEAD)
                .corner_radius(egui::CornerRadius {
                    nw: 5,
                    ne: 5,
                    sw: 0,
                    se: 0,
                })
                .inner_margin(egui::Margin::symmetric(6, 4)),
        ),
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

    // The node graph is the one LIGHT region in the dark chrome. Node text is coloured dark
    // explicitly (in the viewer's header/pin hooks), rather than via a light visuals override:
    // egui-snarl renders through egui's Scene, which leaks a ui-scoped visuals change to the whole
    // context, so overriding visuals here would flip the dark chrome light.
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
    // Nodes the viewer asks to wrap into a new subgraph (context-menu "Create subgraph",
    // #106). Applied at the end of the pane.
    let create_subgraph_request = std::mem::take(&mut viewer.create_subgraph_request);
    // A container the viewer asks to save to the library (context-menu "Save to library",
    // #106). Opens the save dialog at the end of the pane.
    let save_to_library_request = viewer.save_to_library_request;
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

    // Double-click a node body to dive into it if it is a subgraph (#106): the mouse
    // counterpart to the context-menu "Edit subgraph". Hit-tested like a select click (node
    // body, excluding the collapse chevron); applied at the end, where dive_in no-ops for a
    // non-container.
    let double_click_dive = ui
        .ctx()
        .input(|i| {
            i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
                .then(|| i.pointer.interact_pos())
        })
        .flatten()
        .filter(|_| !menu_open)
        .filter(|p| canvas_rect.contains(*p))
        .filter(|p| over_canvas_surface(ui, *p))
        .and_then(|screen_pos| {
            let pos = to_global.inverse() * screen_pos;
            node_rects
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .and_then(|(handle, rect)| {
                    let chevron = egui::Rect::from_min_size(
                        rect.min,
                        egui::Vec2::splat(ui.spacing().icon_width + 12.0),
                    );
                    (!chevron.contains(pos)).then_some(*handle)
                })
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

    // The pending view was consumed by this frame's render; it is one-shot, so replace it with a
    // freshly computed fit (or clear it) rather than let it fight later pan/zoom (#65). A fit only
    // lands once the snarl has measured its nodes, so clear the frame request only when the fit
    // succeeds; a request that found no rects yet (the very first render) stays set and retries
    // next frame instead of being silently lost (the startup top-left bug).
    match frame_all_fit.flatten() {
        Some(fit) => {
            state.pending_view = Some(fit);
            // Clear the frame request only once the fit is a no-op against the view already
            // applied (`to_global`): the first renders report a not-yet-settled canvas rect (a
            // small window still sizing up), and accepting that early fit would frame the graph to
            // the wrong rect and never re-fit. Re-fit until it converges, then stop.
            if transforms_close(to_global, fit) {
                state.frame_to_graph_request = false;
            } else {
                ui.ctx().request_repaint();
            }
        }
        None => {
            // One-shot: clear any applied camera/subgraph-pop view so it does not fight later
            // pan/zoom. If a fit was requested but found no rects yet (the snarl has not measured
            // its nodes), keep the request and repaint so it retries.
            state.pending_view = None;
            if state.frame_to_graph_request {
                ui.ctx().request_repaint();
            }
        }
    }

    // Apply the click to the selection: a plain click selects one node (or clears on
    // empty canvas), Ctrl/Cmd-click toggles a node in or out of the set (#84).
    if let Some(hit) = click_hit {
        let additive = ui.input(|i| i.modifiers.command);
        match hit {
            ClickHit::Node(handle) if additive => state.toggle_selection(handle),
            ClickHit::Node(handle) => state.select_only(handle),
            ClickHit::Empty if !additive => {
                // A plain click on empty canvas dismisses the preview: go blank rather than fall
                // back to the graph's result node.
                state.clear_selection();
                state.preview_dismissed = true;
            }
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

    // Wrap a selection into a new subgraph the context menu asked to create (#106), then
    // dive into one the menu asked to edit. Both run last, after this frame's other edits;
    // only one fires per frame (distinct menu items).
    if let Some(nodes) = create_subgraph_request {
        state.create_subgraph_from(&nodes);
    }
    // Open the "Save to library" dialog for a container the context menu asked to save (#106).
    if let Some(handle) = save_to_library_request {
        state.open_library_save(handle);
    }
    // Dive in from the context menu, or from a double-click on a container (dive_in is a
    // no-op for a non-container, so a double-click on an ordinary node does nothing).
    if let Some(handle) = dive_request.or(double_click_dive) {
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

/// The "Save to library" dialog (#106): a documentation form for a subgraph, saved to the
/// user library on confirm. A no-op when the dialog is closed.
fn library_save_dialog(ctx: &egui::Context, state: &mut AppState) {
    if state.library_save.is_none() {
        return;
    }
    // Category quick-picks are the categories already used in the library; collected before the
    // dialog is borrowed, since both live on `state`. Licenses are a fixed common set. Both fields
    // stay free text (the dropdown only suggests).
    let mut categories: Vec<String> = state
        .library
        .entries
        .iter()
        .map(|e| e.file.category.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort();
    categories.dedup();
    let licenses: Vec<String> = LICENSE_SUGGESTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let editing = matches!(
        state.library_save.as_ref().map(|d| &d.source),
        Some(SubgraphSource::Existing { .. })
    );
    let mut tab = state.library_save_tab;
    let mut save = false;
    let mut cancel = false;
    let style = ctx.global_style();
    egui::Window::new("library-save-dialog")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .frame(
            egui::Frame::window(&style)
                .fill(theme::BG_SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::LINE_STRONG))
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ctx, |ui| {
            ui.set_width(452.0);
            let Some(dialog) = state.library_save.as_mut() else {
                return;
            };

            // Header: an eyebrow over the subgraph name, with a close button at the right.
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(if editing {
                            "EDIT LIBRARY SUBGRAPH"
                        } else {
                            "SAVE SUBGRAPH TO LIBRARY"
                        })
                        .size(10.0)
                        .color(theme::TEXT_TERTIARY),
                    );
                    let name = if dialog.name.trim().is_empty() {
                        "Untitled subgraph"
                    } else {
                        dialog.name.trim()
                    };
                    ui.label(
                        egui::RichText::new(name)
                            .family(egui::FontFamily::Name("plex-semibold".into()))
                            .size(16.0)
                            .color(theme::TEXT_PRIMARY),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(egui_phosphor::regular::X)
                                    .color(theme::TEXT_TERTIARY),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Close")
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });

            // Tabs, over a divider.
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                dialog_tab(ui, &mut tab, LibraryTab::Details, "Details");
                let ports = format!(
                    "Ports  {}\u{b7}{}",
                    dialog.inputs.len(),
                    dialog.outputs.len()
                );
                dialog_tab(ui, &mut tab, LibraryTab::Ports, &ports);
                dialog_tab(ui, &mut tab, LibraryTab::Attribution, "Attribution");
            });
            ui.add_space(5.0);
            ui.separator();
            ui.add_space(8.0);

            match tab {
                LibraryTab::Details => {
                    dialog_label(ui, "Name");
                    let name_resp = ui.add(
                        egui::TextEdit::singleline(&mut dialog.name)
                            .background_color(theme::BG_ABYSS)
                            .desired_width(f32::INFINITY),
                    );
                    // Editing the name re-arms the overwrite guard and clears any stale message.
                    if name_resp.changed() {
                        dialog.confirm_overwrite = false;
                        dialog.error = None;
                    }
                    ui.add_space(8.0);
                    dialog_label(ui, "Category");
                    editable_combo(
                        ui,
                        "lib-cat",
                        &mut dialog.category,
                        "Uncategorized",
                        &categories,
                    );
                    ui.add_space(8.0);
                    dialog_label(ui, "License");
                    editable_combo(
                        ui,
                        "lib-lic",
                        &mut dialog.license,
                        "optional, e.g. CC0-1.0",
                        &licenses,
                    );
                    ui.add_space(8.0);
                    dialog_label(ui, "Description");
                    ui.add(
                        egui::TextEdit::multiline(&mut dialog.description)
                            .hint_text("what this subgraph produces")
                            .background_color(theme::BG_ABYSS)
                            .desired_width(f32::INFINITY)
                            .desired_rows(2),
                    );
                }
                LibraryTab::Ports => {
                    ui.label(
                        egui::RichText::new(
                            "These ports come from the subgraph \u{2014} edit each name and \
                             description. To change the set itself, edit the graph and re-save it \
                             to the library.",
                        )
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                    );
                    library_port_fields(ui, "INPUTS", INPUT_TYPE_ID, &mut dialog.inputs);
                    library_port_fields(ui, "OUTPUTS", OUTPUT_TYPE_ID, &mut dialog.outputs);
                }
                LibraryTab::Attribution => {
                    ui.label(
                        egui::RichText::new(
                            "Optional. Attached to the subgraph so others know who made it. \
                             Prefilled from your Settings profile.",
                        )
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_space(6.0);
                    author_fields_ui(ui, "library-save-author", &mut dialog.author);
                }
            }

            // Footer: any error, then the ghost Cancel and primary Save at the right.
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            if let Some(err) = &dialog.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
                ui.add_space(6.0);
            }
            // The button relabels once an overwrite is armed, so the second click reads as the
            // deliberate action it is.
            let save_label = if dialog.confirm_overwrite {
                "Overwrite"
            } else {
                "Save changes"
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if dialog_primary_button(ui, save_label).clicked() {
                    save = true;
                }
                ui.add_space(6.0);
                if dialog_ghost_button(ui, "Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    state.library_save_tab = tab;
    if save {
        state.save_subgraph_to_library();
    } else if cancel {
        state.library_save = None;
    }
}

/// A small muted field label for the Save-to-library dialog.
fn dialog_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(11.0)
            .color(theme::TEXT_TERTIARY),
    );
}

/// One tab of the Save-to-library dialog: a frameless text button that sets the active tab and, when
/// active, carries an accent underline.
fn dialog_tab(ui: &mut egui::Ui, active: &mut LibraryTab, this: LibraryTab, text: &str) {
    let is_active = *active == this;
    let color = if is_active {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    let family = if is_active {
        egui::FontFamily::Name("plex-medium".into())
    } else {
        egui::FontFamily::Proportional
    };
    let resp = ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(13.0)
                .color(color)
                .family(family),
        )
        .frame(false),
    );
    if resp.clicked() {
        *active = this;
    }
    if is_active {
        ui.painter().hline(
            resp.rect.x_range(),
            resp.rect.bottom() + 3.0,
            egui::Stroke::new(2.0, theme::ACCENT_PRIMARY),
        );
    }
}

/// The primary (accent-filled, dark-text) dialog action button.
fn dialog_primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(13.0)
                .color(theme::BG_ABYSS)
                .family(egui::FontFamily::Name("plex-medium".into())),
        )
        .fill(theme::ACCENT_PRIMARY)
        .corner_radius(5.0)
        .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// A ghost (outlined, transparent) dialog action button.
fn dialog_ghost_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(13.0)
                .color(theme::TEXT_SECONDARY),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(5.0)
        .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// An editable combobox: a free-text field plus a dropdown of suggestions that only *fill* it. The
/// user can always type a value not in the list; the suggestions just shortcut the common ones.
fn editable_combo(
    ui: &mut egui::Ui,
    id: &str,
    text: &mut String,
    hint: &str,
    suggestions: &[String],
) {
    let btn_w = 26.0;
    let gap = 4.0;
    let field_w = (ui.available_width() - btn_w - gap).max(60.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.add(
            egui::TextEdit::singleline(text)
                .id_salt(id)
                .hint_text(hint)
                .background_color(theme::BG_ABYSS)
                .desired_width(field_w),
        );
        let btn = ui.add_sized(
            [btn_w, ui.spacing().interact_size.y],
            egui::Button::new(egui_phosphor::regular::CARET_DOWN),
        );
        if !suggestions.is_empty() {
            let mut picked: Option<String> = None;
            egui::Popup::menu(&btn).show(|ui| {
                ui.set_min_width(field_w);
                for s in suggestions {
                    if ui
                        .selectable_label(text.as_str() == s.as_str(), s)
                        .clicked()
                    {
                        picked = Some(s.clone());
                        ui.close();
                    }
                }
            });
            if let Some(p) = picked {
                *text = p;
            }
        }
    });
}

/// Writes the dialog's edited port names onto the subgraph document's boundary-marker nodes of
/// `marker_type`, and returns the normalized [`library::PortDoc`]s. A copy dropped from the library
/// derives its port names from these markers, so applying the edits here is what keeps a renamed
/// port consistent between the library card and an instance.
///
/// The document keeps its nodes in ascending `stable_id` order, the same order ports derive in, so
/// the k-th marker of `marker_type` is port k (sorted defensively rather than relying on the
/// invariant). A blank name, or one left at the positional default ("Input 2"), clears the marker
/// override so the port falls back to that label; any other name is stored on both the marker and
/// the returned doc, whose `name` is therefore always the resolved display name.
fn reconcile_port_names(
    graph: &mut ProjectDocument,
    marker_type: &str,
    docs: &[library::PortDoc],
) -> Vec<library::PortDoc> {
    let mut markers: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.type_id == marker_type)
        .map(|(i, _)| i)
        .collect();
    markers.sort_by_key(|&i| graph.nodes[i].stable_id);
    docs.iter()
        .enumerate()
        .map(|(index, doc)| {
            let fallback = marker_port_label(marker_type, index);
            let typed = doc.name.trim();
            let (override_name, display) = if typed.is_empty() || typed == fallback {
                (None, fallback)
            } else {
                (Some(typed.to_string()), typed.to_string())
            };
            if let Some(&node) = markers.get(index) {
                graph.nodes[node].name = override_name;
            }
            library::PortDoc {
                index,
                name: display,
                description: doc.description.clone(),
            }
        })
        .collect()
}

/// A titled block of per-port rows for the Save-to-library dialog: an eyebrow (INPUTS / OUTPUTS)
/// over one row per port, each an editable mono name field and an editable description. The name's
/// hint is the positional fallback ("Input 1"), so clearing it reverts the port to that label.
/// Renders nothing for a portless side. No port dot: it carries no information here.
fn library_port_fields(
    ui: &mut egui::Ui,
    title: &str,
    marker_type: &str,
    ports: &mut [library::PortDoc],
) {
    if ports.is_empty() {
        return;
    }
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(title)
            .size(10.5)
            .color(theme::TEXT_TERTIARY),
    );
    ui.add_space(3.0);
    for (index, port) in ports.iter_mut().enumerate() {
        let fallback = marker_port_label(marker_type, index);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(
                egui::TextEdit::singleline(&mut port.name)
                    .id_salt(("port-name", marker_type, index))
                    .hint_text(fallback.as_str())
                    .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                    .text_color(theme::TEXT_PRIMARY)
                    .background_color(theme::BG_ABYSS)
                    .desired_width(120.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut port.description)
                    .id_salt(("port-desc", marker_type, index))
                    .hint_text("description")
                    .background_color(theme::BG_ABYSS)
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(4.0);
    }
}

/// The Settings dialog (#106): edits the app-global preferences draft. Today it holds the author
/// profile, the optional identity attached to a shared subgraph. Save commits the draft and
/// writes it to config; Cancel (or closing the window) discards it.
fn settings_dialog(ctx: &egui::Context, state: &mut AppState) {
    if state.settings_edit.is_none() {
        return;
    }
    let mut open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            let Some(draft) = state.settings_edit.as_mut() else {
                return;
            };
            ui.strong("Author");
            ui.label(
                egui::RichText::new(
                    "Optional. Attached to subgraphs you save, so others know who made them and \
                     how to reach you. Blank fields are left out.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            author_fields_ui(ui, "settings-author", &mut draft.author);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

    if save {
        state.commit_settings();
    } else if cancel || !open {
        state.settings_edit = None;
    }
}

/// The About window (Help -> About Ymir): the app name, the build-stamped version, the
/// license, and a link to the repository, so a bug report can name the exact build. Opened
/// from the Help menu, closed by its own close control.
fn about_dialog(ctx: &egui::Context, state: &mut AppState) {
    if !state.about_open {
        return;
    }
    let mut open = true;
    egui::Window::new("About Ymir")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            ui.heading("Ymir");
            ui.label(
                egui::RichText::new("Node-based procedural terrain generator.")
                    .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(8.0);
            egui::Grid::new("about-facts")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Version").color(theme::TEXT_TERTIARY));
                    ui.label(
                        egui::RichText::new(ymir_build_info::version_string())
                            .family(egui::FontFamily::Monospace),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("License").color(theme::TEXT_TERTIARY));
                    ui.label(env!("CARGO_PKG_LICENSE"));
                    ui.end_row();
                    ui.label(egui::RichText::new("Repository").color(theme::TEXT_TERTIARY));
                    ui.hyperlink(env!("CARGO_PKG_REPOSITORY"));
                    ui.end_row();
                });
        });
    if !open {
        state.about_open = false;
    }
}

/// The author-identity fields (name, email, website, documentation) as a two-column grid,
/// shared by the Settings dialog and the Save-to-library dialog. `id` names the grid so two
/// instances never collide.
fn author_fields_ui(ui: &mut egui::Ui, id: &str, author: &mut preferences::AuthorProfile) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for (label, hint, field) in [
                ("Name", "your name or handle", &mut author.name),
                ("Email", "you@example.com", &mut author.email),
                ("Website", "https://example.com", &mut author.website),
                (
                    "Documentation",
                    "https://example.com/docs",
                    &mut author.docs,
                ),
            ] {
                ui.label(label);
                ui.add(
                    egui::TextEdit::singleline(field)
                        .hint_text(hint)
                        .desired_width(300.0),
                );
                ui.end_row();
            }
        });
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
            .with_world_height(state.world_height)
            .with_sea_level(state.sea_level);
        state.graph.output_key(id, &request).ok()
    })();
    // Diagnostic (logged, so it shows headless too): report the lookup once per distinct
    // (key, hit) transition, so a single Build reveals whether the viewport actually finds the
    // build-quality fields in the cache or falls back to the preview, and why.
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_PROBE: AtomicU64 = AtomicU64::new(u64::MAX);
    let Some(key) = key else {
        if LAST_PROBE.swap(1, Ordering::Relaxed) != 1 {
            log::debug!(
                "viewport build lookup: no previewed node to show (store_open={})",
                state.field_store.is_some(),
            );
        }
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
    let loaded = state.field_store.as_ref().and_then(|store| store.load(key));
    let probe = (key << 1) | u64::from(loaded.is_some());
    if LAST_PROBE.swap(probe, Ordering::Relaxed) != probe {
        log::debug!(
            "viewport build lookup: key={key:016x} store_open={} -> {}",
            state.field_store.is_some(),
            if loaded.is_some() {
                "HIT (showing build)"
            } else {
                "miss (showing preview)"
            },
        );
    }
    state.viewport_build = loaded.map(|fields| (key, fields));
}

fn viewport_pane(ui: &mut egui::Ui, state: &mut AppState) {
    // The pane rect, captured before `show` consumes it, anchors the floating control HUD.
    let rect = ui.available_rect_before_wrap();
    // Collapsed (or animating past the collapse point): draw only the labeled bar, no render. The
    // stat names the current projection.
    if pane_collapsed(rect.height()) {
        let stat = match state.viewport_mode {
            viewport2d::Mode::ThreeD => "3D",
            viewport2d::Mode::TwoD => "2D",
        };
        workspace_collapsed_bar(
            ui,
            egui_phosphor::regular::MOUNTAINS,
            "Preview",
            stat,
            state,
        );
        return;
    }

    // Prefer build-quality fields for the shown node when the disk cache has them (after a
    // Build of the unchanged graph), else the live preview field. The same field feeds both
    // the 3D mesh and the 2D map, so the two projections show identical data.
    refresh_viewport_build(state);
    let display = state.preview.display_output();
    let build_field = state
        .viewport_build
        .as_ref()
        .and_then(|(_, fields)| fields.get(display.min(fields.len().saturating_sub(1))));
    let showing_build = build_field.is_some();
    let field = build_field.or_else(|| state.preview.field());

    // Sea level and the Show water toggle drive the same overlay across projections: the 3D water
    // plane and the 2D map's water tint. Presentation only, so no re-evaluation on change (#96).
    let sea_level = state.sea_level as f32;
    let show_water = state.show_water;

    // Paint mode is on when a paint node is the target and is the previewed node.
    let paint_active = state.paint_target.is_some() && state.preview_target() == state.paint_target;

    // Ctrl + wheel resizes the brush, Shift + wheel changes its hardness. Read from the raw wheel
    // events rather than `smooth_scroll_delta`: egui reroutes a modified scroll before it lands there
    // (Ctrl becomes a zoom delta, Shift moves to the x axis), so the smoothed value reads zero under
    // either modifier. Normalize the raw delta to points so the step is consistent across mice. Size is
    // exponential (matching its log slider), hardness linear. Only while the pointer is over the pane.
    if paint_active {
        const SIZE_WHEEL: f32 = 0.0015;
        const HARDNESS_WHEEL: f32 = 0.002;
        let pane = ui.available_rect_before_wrap();
        let layer = ui.layer_id();
        let over = ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|p| pane.contains(p) && ui.ctx().layer_id_at(p) == Some(layer));
        let (wheel, ctrl, shift) = ui.input(|i| {
            let mut wheel = 0.0_f32;
            let mut modifiers = egui::Modifiers::default();
            for event in &i.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: mods,
                    ..
                } = event
                {
                    let to_points = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 50.0,
                        egui::MouseWheelUnit::Page => 400.0,
                    };
                    wheel += delta.y * to_points;
                    modifiers = *mods;
                }
            }
            (wheel, modifiers.ctrl, modifiers.shift)
        });
        if over && wheel != 0.0 && (ctrl || shift) {
            if ctrl {
                state.paint_brush.radius =
                    (state.paint_brush.radius * (wheel * SIZE_WHEEL).exp()).clamp(0.005, 0.5);
            } else {
                state.paint_brush.hardness =
                    (state.paint_brush.hardness + wheel * HARDNESS_WHEEL).clamp(0.0, 1.0);
            }
        }
    }

    // Ctrl held inverts Raise <-> Lower for the stroke and the cursor's mark, without touching the
    // stored mode, so you can carve a quick pit without leaving the brush.
    let effective_mode = if paint_active && ui.input(|i| i.modifiers.ctrl) {
        match state.paint_brush.mode {
            StrokeMode::Paint => StrokeMode::Erase,
            StrokeMode::Erase => StrokeMode::Paint,
        }
    } else {
        state.paint_brush.mode
    };

    // The brush cursor, shown in whichever projection is active; its mark reflects the effective mode.
    let brush = paint_active.then_some(viewport2d::BrushCursor {
        radius: state.paint_brush.radius,
        hardness: state.paint_brush.hardness,
        raise: matches!(effective_mode, StrokeMode::Paint),
    });

    match state.viewport_mode {
        viewport2d::Mode::ThreeD => {
            // True world proportion: a height of 1.0 rises to (world_height / world_extent)
            // over the unit footprint. The exaggeration multiplies it, so 1x is real-world
            // proportion.
            let true_proportion =
                (state.world_height / state.world_extent.max(f64::EPSILON)) as f32;
            // Advance the animation phase by the real frame delta scaled by the speed control, but
            // only while an animated layer is on, so the speed slider changes future motion without
            // rescaling the elapsed phase (which would jump the waves) and dropped frames do not
            // slow it (the delta is real). Frozen or hidden water leaves the phase untouched.
            if state.show_water && (state.water_waves || state.water_foam_on) {
                let dt = ui.input(|i| i.stable_dt);
                state.water_phase += dt * state.water_speed;
            }
            let settings = viewport::ViewSettings {
                fixed_range: state.viewport_scale == shade::HeightScale::Fixed,
                vertical_scale: true_proportion * state.viewport_exaggeration,
                fly_speed: state.viewport_fly_speed,
                sea_level,
                show_water,
                water_depth: state.water_depth,
                water_waves: state.water_waves,
                water_reflection: state.water_reflection,
                water_foam_on: state.water_foam_on,
                water_extinction: state.water_extinction,
                water_color: state.water_color,
                water_wave: state.water_wave,
                water_reflectivity: state.water_reflectivity,
                water_specular: state.water_specular,
                water_steepness: state.water_steepness,
                water_wavelength: state.water_wavelength,
                water_foam: state.water_foam,
                water_foam_width: state.water_foam_width,
                // The wet-shore toggle gates the effect by passing zero strength when off.
                water_wet: if state.water_wet_on {
                    state.water_wet
                } else {
                    0.0
                },
                water_wet_width: state.water_wet_width,
                water_time: state.water_phase,
                water_speed: state.water_speed,
            };
            // Paint mode: with a Paint node targeted and previewed, a plain drag on the 3D surface
            // brushes onto it (ray-cast pick). Orbit stays on Alt-drag, fly on right-drag.
            let sample = viewport::show(
                ui,
                &mut state.viewport_camera,
                field,
                settings,
                state.viewport_lighting,
                &mut state.viewport_mesh,
                brush,
            );
            if let Some(sample) = sample {
                apply_paint_sample(state, sample, effective_mode);
            }
        }
        viewport2d::Mode::TwoD => {
            // Paint mode is on when a Paint node is the target and it is the node the map previews,
            // so brushing lands on the mask you are looking at.
            let sample = state.viewport_2d.show(
                ui,
                state.render_state.as_ref(),
                field,
                viewport2d::MapDisplay {
                    output: display,
                    scale: state.viewport_scale,
                    sea_level,
                    show_water,
                },
                brush,
            );
            if let Some(sample) = sample {
                apply_paint_sample(state, sample, effective_mode);
            }
        }
    }

    // The on-render control cluster overlaid at the top-left of the viewport (#164): a compact row
    // of the projection toggle, the fidelity readout, the fly speed (3D only), and a Display button
    // that opens a flyout holding the rest (height scale, exaggeration, light).
    let mut mode = state.viewport_mode;
    let mut scale = state.viewport_scale;
    let mut exaggeration = state.viewport_exaggeration;
    let mut light = state.viewport_lighting;
    let mut shade_mode = state.viewport_2d.shade_mode();
    let mut light2d = state.viewport_2d.relief_light();
    let mut fly_speed = state.viewport_fly_speed;
    // The tapped outputs of the previewed node and the index the viewport shows (#165). The flyout's
    // Output picker and the inspector thumbnail's picker both read and write this one index, so the
    // two stay in sync. Names come from the node's spec, matching the inspector.
    let output_names: Vec<String> = state
        .preview_target()
        .and_then(|t| state.graph.node_id_of(t))
        .and_then(|id| state.graph.spec(id))
        .map(|spec| spec.outputs.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    let mut display_output = state.preview.display_output();
    let display_output_before = display_output;
    // Draw the cluster only when the viewport is tall enough to hold it, so a short viewport shows
    // only the render rather than chrome overflowing it.
    if rect.height() >= WORKSPACE_HUD_MIN {
        egui::Area::new(ui.id().with("viewport-cluster"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top() + egui::vec2(8.0, 8.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE.show(ui, |ui| {
                    // The cluster row: each control floats on the render in its own subtle chip
                    // (an integrated-overlay look, not one raised popup box). The Display button's
                    // response is returned so the flyout can anchor beneath it and toggle on its click.
                    let display_btn = ui
                        .horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            // The projection: the 3D relief or the flat 2D map (#134). The 2D view
                            // gives data maps (flow, masks) the room the small preview pane can't.
                            overlay_chip(ui, |ui| {
                                let mode_i = usize::from(mode == viewport2d::Mode::TwoD);
                                if let Some(i) = segmented(ui, &["3D", "2D"], mode_i) {
                                    mode = if i == 0 {
                                        viewport2d::Mode::ThreeD
                                    } else {
                                        viewport2d::Mode::TwoD
                                    };
                                }
                            });
                            // 2D: the Height/Relief shading toggle rides the cluster (its scale and
                            // 2D sun live in the flyout). Height is the greyscale field, best for data
                            // maps; Relief is the hillshade, best for reading shape.
                            if mode == viewport2d::Mode::TwoD {
                                overlay_chip(ui, |ui| {
                                    let shade_i =
                                        usize::from(shade_mode == shade::ShadeMode::Relief);
                                    if let Some(i) = segmented(ui, &["Height", "Relief"], shade_i) {
                                        shade_mode = if i == 0 {
                                            shade::ShadeMode::Height
                                        } else {
                                            shade::ShadeMode::Relief
                                        };
                                    }
                                });
                            }
                            // Whether the viewport shows the full build result or the coarse
                            // preview, so it is clear which fidelity is on screen while tuning.
                            overlay_chip(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(if showing_build {
                                        "Showing: build"
                                    } else {
                                        "Showing: preview"
                                    })
                                    .family(egui::FontFamily::Monospace)
                                    .size(11.5)
                                    .color(theme::TEXT_TERTIARY),
                                )
                                .on_hover_text(
                                    "Build quality appears after a Build, until the graph changes; otherwise the live preview",
                                );
                            });
                            // Fly speed rides the cluster (always accessible), only in 3D where the
                            // fly camera exists. Shift boosts 4x over this.
                            if mode == viewport2d::Mode::ThreeD {
                                overlay_chip(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("Fly speed")
                                            .size(11.5)
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .on_hover_text(
                                        "Speed of the right-mouse + WASD fly-through (Shift boosts 4x)",
                                    );
                                    // The styled slider (visible trough + accent fill), matching the
                                    // flyout sliders. It fills available width, so pin it to a fixed
                                    // region, and show the value beside it since the styled slider has
                                    // no inline readout.
                                    let mut v = f64::from(fly_speed);
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(90.0, 18.0),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            if param_ui::slider(ui, &mut v, 0.05, 1.5, true).changed()
                                            {
                                                fly_speed = v as f32;
                                            }
                                        },
                                    );
                                    ui.label(
                                        egui::RichText::new(format!("{fly_speed:.2}"))
                                            .family(egui::FontFamily::Monospace)
                                            .size(11.5)
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                });
                            }
                            display_button(ui)
                        })
                        .inner;
                    // The Display flyout, its open state managed by hand so the Output list can float
                    // as its own overlay (below) without the list's clicks tripping a close: a nested
                    // menu popup renders in its own layer and would dismiss a click-outside flyout on
                    // selection. So the flyout ignores clicks (dragging its sliders never dismisses
                    // it); the Display button toggles it, and a click outside it and the list, or
                    // Escape, closes it.
                    let flyout_open_id = ui.make_persistent_id("viewport-flyout-open");
                    let outputs_open_id = ui.make_persistent_id("viewport-outputs-open");
                    let mut flyout_open = ui.data(|d| d.get_temp(flyout_open_id).unwrap_or(false));
                    let mut outputs_open = ui.data(|d| d.get_temp(outputs_open_id).unwrap_or(false));
                    let has_outputs = output_names.len() > 1;
                    let cur = display_output.min(output_names.len().saturating_sub(1));
                    if display_btn.clicked() {
                        flyout_open = !flyout_open;
                        outputs_open = false;
                    }
                    let mut flyout_rect = None;
                    let mut output_btn_rect = None;
                    if flyout_open {
                        let inner = egui::Popup::from_response(&display_btn)
                            .open(true)
                            .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
                            .gap(6.0)
                            .frame(egui::Frame::popup(ui.style()))
                            .show(|ui| {
                                ui.set_max_width(250.0);
                                // The Output header block tops the flyout: the picked output on a faint
                                // accent-tinted block (the "what", not the "how"), with a note that it
                                // mirrors the inspector thumbnail's picker. Always shown; a single-output
                                // node has no choice, so its name is static rather than a dropdown.
                                if !output_names.is_empty() {
                                    egui::Frame::new()
                                        .fill(theme::ACCENT_PRIMARY.gamma_multiply(0.12))
                                        .corner_radius(6)
                                        .inner_margin(egui::Margin::symmetric(8, 6))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                flyout_label(ui, "Output");
                                                if has_outputs {
                                                    let field = output_field(
                                                        ui,
                                                        &output_names[cur],
                                                        outputs_open,
                                                    );
                                                    output_btn_rect = Some(field.rect);
                                                    if field.clicked() {
                                                        outputs_open = !outputs_open;
                                                    }
                                                } else {
                                                    ui.label(
                                                        egui::RichText::new(&output_names[cur])
                                                            .color(theme::TEXT_PRIMARY),
                                                    );
                                                }
                                            });
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{}  synced with inspector thumbnail",
                                                        egui_phosphor::regular::ARROWS_CLOCKWISE
                                                    ))
                                                    .size(10.5)
                                                    .color(theme::TEXT_TERTIARY),
                                                );
                                            });
                                        });
                                    group_separator(ui);
                                }
                                match mode {
                                    viewport2d::Mode::ThreeD => {
                                        viewport_3d_controls(ui, &mut scale, &mut exaggeration, &mut light);
                                    }
                                    viewport2d::Mode::TwoD => {
                                        viewport_2d_controls(ui, shade_mode, &mut scale, &mut light2d);
                                    }
                                }
                            });
                        flyout_rect = inner.map(|r| r.response.rect);
                    }
                    // The Output list: its own foreground overlay, so revealing it never grows the
                    // flyout. Anchored under the Output button and raised above the flyout; picking one
                    // sets the shared index and closes the list (the click lands inside the list, so it
                    // never closes the flyout).
                    let mut list_rect = None;
                    if flyout_open
                        && outputs_open
                        && let Some(anchor) = output_btn_rect
                    {
                        let list = egui::Area::new(ui.id().with("viewport-outputs-list"))
                            .order(egui::Order::Foreground)
                            .fixed_pos(anchor.left_bottom() + egui::vec2(0.0, 4.0))
                            .show(ui.ctx(), |ui| {
                                egui::Frame::popup(ui.style()).show(ui, |ui| {
                                    // Size to the widest option (plus the label padding), not the
                                    // anchor button, whose width tracks the *selected* name; otherwise
                                    // a short selection shrinks the list and wraps the longer options.
                                    let font = egui::TextStyle::Button.resolve(ui.style());
                                    let widest = output_names
                                        .iter()
                                        .map(|n| {
                                            ui.painter()
                                                .layout_no_wrap(
                                                    n.clone(),
                                                    font.clone(),
                                                    theme::TEXT_PRIMARY,
                                                )
                                                .size()
                                                .x
                                        })
                                        .fold(0.0_f32, f32::max);
                                    let pad = ui.spacing().button_padding.x * 2.0 + 6.0;
                                    ui.set_min_width((widest + pad).max(anchor.width()));
                                    for (i, name) in output_names.iter().enumerate() {
                                        if ui.selectable_label(i == cur, name).clicked() {
                                            display_output = i;
                                            outputs_open = false;
                                        }
                                    }
                                });
                            });
                        // Raise above the flyout (both Foreground) so the list is never occluded.
                        ui.ctx().move_to_top(list.response.layer_id);
                        list_rect = Some(list.response.rect);
                    }
                    // Manual dismissal: Escape backs out (list first, then flyout); a click outside the
                    // Display button, the flyout, and the list closes everything.
                    if flyout_open {
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            if outputs_open {
                                outputs_open = false;
                            } else {
                                flyout_open = false;
                            }
                        } else if ui.input(|i| i.pointer.any_click()) {
                            let pos = ui.input(|i| i.pointer.interact_pos());
                            let inside_flyout =
                                flyout_rect.zip(pos).is_some_and(|(r, p)| r.contains(p));
                            let inside_list = list_rect.zip(pos).is_some_and(|(r, p)| r.contains(p));
                            let on_btn = pos.is_some_and(|p| display_btn.rect.contains(p));
                            if !on_btn && !inside_flyout && !inside_list {
                                flyout_open = false;
                                outputs_open = false;
                            }
                        }
                    }
                    ui.data_mut(|d| {
                        d.insert_temp(flyout_open_id, flyout_open);
                        d.insert_temp(outputs_open_id, outputs_open);
                    });
                });
            });
    }
    // Write the picked output back only when the flyout changed it, so this never clobbers a change
    // the inspector's picker made the same frame.
    if display_output != display_output_before {
        state.preview.set_display_output(display_output);
    }
    state.viewport_mode = mode;
    state.viewport_scale = scale;
    state.viewport_exaggeration = exaggeration;
    state.viewport_lighting = light;
    state.viewport_2d.set_shade_mode(shade_mode);
    state.viewport_2d.set_relief_light(light2d);
    state.viewport_fly_speed = fly_speed;
}

/// A left label for a flyout row that reserves its own measured width plus a small gap, so the
/// control to its right never overlaps it however long the label is (a fixed column was narrower than
/// "Exaggeration" renders). Painted rather than a widget so it sits flush with the row's baseline.
fn flyout_label(ui: &mut egui::Ui, text: &str) {
    let font = egui::FontId::proportional(11.5);
    let text_w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_SECONDARY)
        .size()
        .x;
    let h = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(text_w + 10.0, h), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(rect.left(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        theme::TEXT_SECONDARY,
    );
}

/// The cluster's Display button: accent-outlined with a sliders glyph, the only cluster control that
/// opens a panel. Returned so the flyout can anchor to it and toggle on its click.
fn display_button(ui: &mut egui::Ui) -> egui::Response {
    let label = format!("{}  Display", egui_phosphor::regular::SLIDERS_HORIZONTAL);
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(13.0)
                .color(theme::ACCENT_PRIMARY),
        )
        .fill(chip_fill())
        .stroke(egui::Stroke::new(1.0, theme::ACCENT_PRIMARY))
        .corner_radius(6)
        // Match the chip row height (the segmented control's 26px content + the chips' 4px top and
        // bottom margins), so the button's top and bottom borders line up with the other controls.
        .min_size(egui::vec2(0.0, 34.0)),
    )
    .on_hover_text("Height scale, exaggeration, and light")
}

/// Wraps a cluster control in subtle translucent-dark chrome, so it floats directly on the render and
/// stays legible over both the dark 3D relief and the light 2D map while reading as embedded, not as
/// a raised popup box. Each cluster control gets its own chip rather than one wrapping frame.
fn overlay_chip<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(chip_fill())
        .stroke(egui::Stroke::new(1.0, chip_border()))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, add)
        .inner
}

/// Frosted chrome for the on-render cluster chips (shared by the Display button, for consistency): a
/// translucent slate at low opacity, so the render shows through and the chip reads as a subtle,
/// unobtrusive frost rather than a solid dark box, while light text stays readable over both the dark
/// 3D relief and the lighter 2D map. egui cannot blur, so this approximates frosted glass with alpha.
fn chip_fill() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(34, 39, 47, 105)
}

/// The chip's hairline edge: a faint light stroke that gives the frost a defined border without
/// weight.
fn chip_border() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(150, 160, 175, 55)
}

/// The flyout's Output dropdown field: a dark rounded field spanning the available width, the current
/// output name on the left and a caret on the right. Returns its click response, so the caller anchors
/// the floating option list under it.
fn output_field(ui: &mut egui::Ui, name: &str, open: bool) -> egui::Response {
    let w = ui.available_width().max(48.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, theme::BG_ABYSS);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.5),
        theme::TEXT_PRIMARY,
    );
    let caret = if open {
        egui_phosphor::regular::CARET_UP
    } else {
        egui_phosphor::regular::CARET_DOWN
    };
    painter.text(
        egui::pos2(rect.right() - 8.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        caret,
        egui::FontId::proportional(12.0),
        theme::TEXT_SECONDARY,
    );
    resp
}

/// Fixed width of one light-grid cell, so the two columns line up and their sliders match width.
const LIGHT_CELL_W: f32 = 100.0;

/// One cell of the flyout's 2-column light grid: a `label ........ value` header over a full-cell
/// slider (its inline value suppressed, since the header carries it). `suffix` (e.g. "°") is shown on
/// the readout only.
fn light_cell(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    decimals: usize,
    suffix: &str,
) {
    ui.vertical(|ui| {
        ui.set_width(LIGHT_CELL_W);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(11.5)
                    .color(theme::TEXT_SECONDARY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{value:.decimals$}{suffix}"))
                        .family(egui::FontFamily::Monospace)
                        .size(11.5)
                        .color(theme::TEXT_PRIMARY),
                );
            });
        });
        // The app's styled slider (visible dark trough, accent fill, ringed knob), so the track reads
        // on the dark flyout where egui's default rail blends in — and it matches the mockup.
        let mut v = f64::from(*value);
        if param_ui::slider(
            ui,
            &mut v,
            f64::from(*range.start()),
            f64::from(*range.end()),
            false,
        )
        .changed()
        {
            *value = v as f32;
        }
    });
}

/// The 3D display-flyout controls: the Auto/Fixed height scale, the vertical exaggeration, and the
/// sun under the collapsing LIGHT section. Fly speed is not here: it rides the always-visible
/// cluster, since it is adjusted often enough that burying it in the flyout would be a nuisance.
fn viewport_3d_controls(
    ui: &mut egui::Ui,
    scale: &mut shade::HeightScale,
    exaggeration: &mut f32,
    light: &mut viewport::Lighting,
) {
    ui.spacing_mut().item_spacing.y = 6.0;
    // Height scale: Fixed shows true amplitude; Auto normalizes to fill the relief (and so hides
    // amplitude). Mirrors the 2D preview's Auto/Fixed toggle.
    ui.horizontal(|ui| {
        flyout_label(ui, "Height scale");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active = usize::from(*scale == shade::HeightScale::Auto);
            if let Some(i) = segmented(ui, &["Fixed", "Auto"], active) {
                *scale = if i == 0 {
                    shade::HeightScale::Fixed
                } else {
                    shade::HeightScale::Auto
                };
            }
        })
        .response
        .on_hover_text(
            "Fixed shows true height (clips out of range); Auto stretches to fill the relief",
        );
    });
    // Exaggeration: a right-pinned "1.00x" scrub/type box, the log slider filling the gap with its
    // own inline value suppressed (the box is the value), mirroring the panel slider rows.
    ui.horizontal(|ui| {
        flyout_label(ui, "Exaggeration");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let h = ui.spacing().interact_size.y;
            ui.add_sized(
                [48.0, h],
                egui::DragValue::new(exaggeration)
                    .range(0.25..=8.0)
                    .speed(0.01)
                    .fixed_decimals(2)
                    .suffix("x"),
            )
            .on_hover_text("1x is real-world proportion (set by World height)");
            // The styled slider (visible trough, accent fill), so the track reads on the dark flyout.
            let mut v = f64::from(*exaggeration);
            if param_ui::slider(ui, &mut v, 0.25, 8.0, true).changed() {
                *exaggeration = v as f32;
            }
        });
    });
    // The sun, under a LIGHT section tagged with the projection it edits, collapsed by default to
    // keep the flyout compact. A 2-column grid: azimuth/elevation, then intensity/ambient.
    section(
        ui,
        "viewport-light-3d",
        "Light",
        false,
        true,
        Some("3D sun".to_string()),
        |ui| {
            egui::Grid::new("viewport-light-3d-grid")
                .num_columns(2)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    light_cell(ui, "Azimuth", &mut light.azimuth_deg, 0.0..=360.0, 0, "°");
                    light_cell(
                        ui,
                        "Elevation",
                        &mut light.elevation_deg,
                        0.0..=90.0,
                        0,
                        "°",
                    );
                    ui.end_row();
                    light_cell(ui, "Intensity", &mut light.intensity, 0.0..=2.0, 2, "");
                    light_cell(ui, "Ambient", &mut light.ambient, 0.0..=1.0, 2, "");
                    ui.end_row();
                });
        },
    );
}

/// The 2D map's flyout controls. The Height/Relief toggle now rides the cluster, so this shows the
/// set for the active shading: in Height, the Auto/Fixed scale; in Relief, the 2D sun dial (a single
/// drag sets azimuth and altitude together), which the map steers from here rather than an on-map
/// overlay dial.
fn viewport_2d_controls(
    ui: &mut egui::Ui,
    shade_mode: shade::ShadeMode,
    scale: &mut shade::HeightScale,
    light: &mut [f32; 3],
) {
    ui.spacing_mut().item_spacing.y = 6.0;
    match shade_mode {
        // Height shading: the Auto/Fixed scale (which does not apply to the hillshade).
        shade::ShadeMode::Height => {
            ui.horizontal(|ui| {
                flyout_label(ui, "Height scale");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let active = usize::from(*scale == shade::HeightScale::Auto);
                    if let Some(i) = segmented(ui, &["Fixed", "Auto"], active) {
                        *scale = if i == 0 {
                            shade::HeightScale::Fixed
                        } else {
                            shade::HeightScale::Auto
                        };
                    }
                })
                .response
                .on_hover_text(
                    "Fixed maps a true [0, 1] (clips out of range); Auto stretches the actual range",
                );
            });
        }
        // Relief shading: the 2D sun, its own light independent of the 3D sun. The dial is a single
        // draggable disk (azimuth from angle, altitude from distance), far more direct than two
        // sliders; it only writes on a drag, so idle frames never rebuild the CPU-shaded map (#167).
        shade::ShadeMode::Relief => {
            section(
                ui,
                "viewport-light-2d",
                "Light",
                false,
                true,
                Some("2D sun".to_string()),
                |ui| {
                    ui.horizontal(|ui| {
                        crate::sun::dial(ui, light);
                        // Read the angles after the dial so a drag this frame shows immediately.
                        let (az, alt) = crate::sun::light_angles(*light);
                        ui.label(
                            egui::RichText::new(format!("{az:>3.0}° · {alt:>2.0}°"))
                                .family(egui::FontFamily::Monospace)
                                .color(theme::TEXT_TERTIARY),
                        );
                    });
                },
            );
        }
    }
}
inventory::submit! { PaneKind { id: "viewport-3d", draw: viewport_pane } }

// ---- layout description + fixed-panel backend -------------------------------

/// The footer (Section 5): status, hints, and context. A placeholder for now.
fn footer_pane(ui: &mut egui::Ui, _state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.add_space(MENU_VPAD);
        // A status dot then a mono label, per the Frost status bar. "Ready" is an ok state (the
        // green here is a status indicator, not a red-vs-green discrimination).
        let d = 7.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), d * 0.5, theme::SUCCESS);
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Ready")
                .monospace()
                .color(theme::TEXT_SECONDARY),
        );
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

/// The collapsed dock's width: a narrow icon rail, wide enough for one Phosphor glyph plus its
/// button padding and the rail's own margin.
const DOCK_RAIL_WIDTH: f32 = 36.0;

/// The width of the flanking side panels (the left dock and the right inspector), kept equal so
/// the workspace is symmetric. Sized to the right panel's square preview plus a small margin.
const SIDE_PANEL_WIDTH: f32 = 260.0;

/// Height of a workspace pane collapsed to a labeled bar (the design's edge tab). A collapsed pane
/// draws only this bar, never its content.
const WORKSPACE_BAR_H: f32 = 32.0;

/// The minimum height of a *shown* pane in Split, and the point at which dragging the divider
/// collapses the shrinking pane to its labeled bar.
const WORKSPACE_COLLAPSE_SNAP: f32 = 100.0;

/// Clearance from the snap thresholds that a *restore* reopens Split with, so the divider never
/// lands right at the edge of the snap zone (where the next click on the handle would re-collapse
/// it). Only affects where restore/startup opens; free dragging still reaches the snap point.
const WORKSPACE_RESTORE_CLEARANCE: f32 = 40.0;

/// Below this height a pane draws its labeled bar instead of its content; above it, its content.
/// Between this and [`WORKSPACE_COLLAPSE_SNAP`] a pane shows content but not its floating controls.
/// Sits between the bar height and the snap, so it is only crossed while a collapse/expand animates.
const WORKSPACE_PANE_CONTENT_MIN: f32 = 64.0;

/// The viewport must be at least this tall to draw its floating HUD, so a short viewport shows only
/// the render rather than a HUD overflowing it.
const WORKSPACE_HUD_MIN: f32 = 150.0;

/// Duration (seconds) of the collapse/expand height animation when the layout mode changes.
const WORKSPACE_ANIM_SECS: f64 = 0.22;

/// Whether a workspace pane of this height draws its labeled bar rather than its content.
fn pane_collapsed(height: f32) -> bool {
    height <= WORKSPACE_PANE_CONTENT_MIN
}

/// The workspace layout mode: which of the two stacked panes is emphasized. `Split` shares the
/// height over a draggable divider; `Graph` maximizes the node graph (the viewport collapses to a
/// bottom bar); `Preview` maximizes the 2D/3D viewport (the graph collapses to a top bar).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceMode {
    Split,
    Graph,
    Preview,
}

/// Draws a collapsed pane as a labeled bar filling the pane, drawing no pane content (the design's
/// core fix: a minimized pane is an identity bar, not a tiny render). Shows the pane icon, name, and
/// a live stat; the whole bar is clickable to restore the Split layout (the quick "click the tab to
/// bring it back"). Deep chrome fill, brightening on hover.
fn workspace_collapsed_bar(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    stat: &str,
    state: &mut AppState,
) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ui.available_height()),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    let fill = if resp.hovered() {
        theme::BG_RAISED
    } else {
        theme::BG_ABYSS
    };
    painter.rect_filled(rect, 0.0, fill);
    let cy = rect.center().y;
    let icon_rect = painter.text(
        egui::pos2(rect.left() + 12.0, cy),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(15.0),
        theme::TEXT_SECONDARY,
    );
    let name_rect = painter.text(
        egui::pos2(icon_rect.right() + 8.0, cy),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::new(12.5, egui::FontFamily::Name("plex-medium".into())),
        theme::TEXT_PRIMARY,
    );
    if !stat.is_empty() {
        painter.text(
            egui::pos2(name_rect.right() + 10.0, cy),
            egui::Align2::LEFT_CENTER,
            stat,
            egui::FontId::new(11.0, egui::FontFamily::Monospace),
            theme::TEXT_TERTIARY,
        );
    }
    if resp
        .on_hover_text("Click to restore the split view")
        .clicked()
    {
        state.workspace_mode = WorkspaceMode::Split;
    }
}

/// Draws one segment of the layout switcher: a mini ratio icon (two stacked bars sized to the mode's
/// split) over a segment background that fills when active or hovered. The emphasized pane's bar is
/// the accent when active. Returns the segment's response.
fn layout_mode_segment(ui: &mut egui::Ui, mode: WorkspaceMode, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 22.0), egui::Sense::click());
    let painter = ui.painter();
    let bg = if active {
        theme::BG_HOVER
    } else if resp.hovered() {
        theme::BG_RAISED
    } else {
        theme::BG_ABYSS
    };
    painter.rect_filled(rect, 4.0, bg);
    let inner = egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 13.0));
    let gap = 1.5;
    // The top bar's share of the icon; the bottom takes the rest. Graph is top-heavy (the graph
    // sits on top), Preview bottom-heavy.
    let top_frac = match mode {
        WorkspaceMode::Split => 0.5,
        WorkspaceMode::Graph => 0.68,
        WorkspaceMode::Preview => 0.32,
    };
    let bars_h = inner.height() - gap;
    let top_h = bars_h * top_frac;
    let top = egui::Rect::from_min_size(inner.left_top(), egui::vec2(inner.width(), top_h));
    let bot = egui::Rect::from_min_size(
        egui::pos2(inner.left(), inner.top() + top_h + gap),
        egui::vec2(inner.width(), bars_h - top_h),
    );
    // Emphasize the focused (larger) pane in accent when active; both panes neutral otherwise.
    let accent = theme::ACCENT_PRIMARY;
    let dim = theme::TEXT_TERTIARY;
    let (top_col, bot_col) = match (mode, active) {
        (WorkspaceMode::Split, true) => (accent, accent),
        (WorkspaceMode::Graph, true) => (accent, dim),
        (WorkspaceMode::Preview, true) => (dim, accent),
        _ => (theme::TEXT_SECONDARY, theme::TEXT_SECONDARY),
    };
    painter.rect_filled(top, 1.5, top_col);
    painter.rect_filled(bot, 1.5, bot_col);
    resp
}

/// The workspace layout switcher (design 1c): a compact three-segment control — Split, Graph,
/// Preview — floated at the top-right of the workspace, matching the app's segmented toggles. A
/// click picks that mode; clicking the active Graph or Preview segment toggles back to Split (a
/// quick restore, alongside clicking the collapsed bar itself).
fn layout_mode_switcher(ui: &egui::Ui, body_rect: egui::Rect, state: &mut AppState) {
    let modes = [
        (WorkspaceMode::Split, "Split view"),
        (WorkspaceMode::Graph, "Maximize the graph"),
        (WorkspaceMode::Preview, "Maximize the preview"),
    ];
    egui::Area::new(ui.id().with("layout-switcher"))
        .order(egui::Order::Foreground)
        .fixed_pos(body_rect.right_top() + egui::vec2(-8.0, 8.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (mode, tip) in modes {
                            let active = state.workspace_mode == mode;
                            if layout_mode_segment(ui, mode, active)
                                .on_hover_text(tip)
                                .clicked()
                            {
                                state.workspace_mode = if active && mode != WorkspaceMode::Split {
                                    WorkspaceMode::Split
                                } else {
                                    mode
                                };
                            }
                        }
                    });
                });
        });
}

/// Draws a resize divider's grab handle: a short centered pill on the divider line at screen `y`
/// over `x_range`, brightening when the pointer is near (where the owning resizable panel accepts
/// the drag). Purely a visual affordance; the panel handles the drag itself. Shared by the
/// workspace split and the left dock's browser/inspector split so both read the same.
fn divider_handle(ui: &egui::Ui, y: f32, x_range: egui::Rangef) {
    let near = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|p| (p.y - y).abs() <= 6.0 && x_range.contains(p.x));
    let handle = egui::Rect::from_center_size(
        egui::pos2((x_range.min + x_range.max) * 0.5, y),
        egui::vec2(44.0, 5.0),
    );
    // Paint on a clip expanded past the pane edge, so the lower half of the pill (which sits over the
    // pane below the divider) is not cut off.
    let painter = ui.painter().with_clip_rect(ui.clip_rect().expand(8.0));
    // A light fill with a dark rim reads against both the light canvas above and the dark viewport
    // below; it brightens to near-white when the pointer is near, where the drag is accepted.
    let fill = if near {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    painter.rect_filled(handle, 2.5, fill);
    painter.rect_stroke(
        handle,
        2.5,
        egui::Stroke::new(1.0, theme::BG_ABYSS),
        egui::StrokeKind::Outside,
    );
}

/// Mounts the left dock (#106): archetype 2's project/global column, mirroring the right pane
/// below the full-width ribbon. Collapsed, it is a narrow icon rail (one button per registered
/// dock pane); open, it is a full pane with a switcher header (the pane icons plus a collapse
/// button) over the active pane's body. Returns the panel response so the caller can draw its
/// right border with the other section borders.
fn mount_dock(ui: &mut egui::Ui, state: &mut AppState) -> egui::InnerResponse<()> {
    let panes = dock::dock_panes();
    // Resolve the active pane: an empty or stale id falls back to the first pane in rail order
    // (the lowest `order`), so the dock always has something to show once opened.
    let active_id: String = panes
        .iter()
        .find(|pane| pane.id == state.dock.active)
        .or_else(|| panes.first())
        .map_or_else(String::new, |pane| pane.id.to_string());

    if state.dock.open {
        egui::Panel::left("dock-panel")
            // Fixed width, matching the right inspector so the two side panels are symmetric.
            .resizable(false)
            .exact_size(SIDE_PANEL_WIDTH)
            .show_separator_line(false)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0))
            .show_inside(ui, |ui| {
                // Switcher header: the pane icons on the left, a collapse button on the right.
                header_strip(ui, |ui| {
                    for pane in &panes {
                        let selected = pane.id == active_id;
                        if ui
                            .selectable_label(selected, pane.icon)
                            .on_hover_text(pane.title)
                            .clicked()
                        {
                            state.dock.active = pane.id.to_string();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(egui_phosphor::regular::CARET_LEFT)
                            .on_hover_text("Collapse")
                            .clicked()
                        {
                            state.dock.open = false;
                        }
                    });
                });
                // The active pane's body. No inner margin: the pane spans the dock's full width so
                // its own panels and dividers can reach both borders. Each pane pads its own
                // content (see `library_pane`).
                egui::CentralPanel::default()
                    .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(0))
                    .show_inside(ui, |ui| {
                        if let Some(pane) = dock::dock_pane(&active_id) {
                            (pane.draw)(ui, state);
                        }
                    });
            })
    } else {
        // Collapsed: a narrow icon rail. Clicking an icon opens the dock to that pane.
        egui::Panel::left("dock-panel")
            .resizable(false)
            .exact_size(DOCK_RAIL_WIDTH)
            .show_separator_line(false)
            .frame(
                egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(4, 6)),
            )
            .show_inside(ui, |ui| {
                ui.vertical_centered(|ui| {
                    for pane in &panes {
                        if ui.button(pane.icon).on_hover_text(pane.title).clicked() {
                            state.dock.open = true;
                            state.dock.active = pane.id.to_string();
                        }
                    }
                });
            })
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

    // The ribbon (node category tabs + selection + node grid) is full-width top chrome under the
    // menu (archetype 2): a global palette like a toolbar, so it spans the whole width and never
    // moves when a side panel toggles. Its bottom border is drawn with the section borders below.
    let palette = egui::Panel::top("palette-panel")
        .show_separator_line(false)
        // No inner margin (the ribbon draws its own full-width bands), filled to match the
        // category band so the spacing between the two bands does not show the darker default
        // fill through the gap.
        .frame(
            egui::Frame::side_top_panel(ui.style())
                .fill(theme::BG_SURFACE)
                .inner_margin(0),
        )
        .show_inside(ui, |ui| draw_pane(layout.palette, ui, state));

    // Section 5: the footer (its top border is drawn below), a bit darker than the body.
    let footer = egui::Panel::bottom("footer-panel")
        .show_separator_line(false)
        .frame(egui::Frame::side_top_panel(ui.style()).fill(theme::BG_SURFACE))
        .show_inside(ui, |ui| draw_pane(layout.footer, ui, state));

    // Section 4: the right column — the node inspector and preview. It sits BELOW the ribbon
    // (archetype 2), flanking the canvas, so toggling it never shifts the ribbon. Fixed width
    // (not resizable): sized to the square preview image plus a small margin, since the contents
    // do not reflow nicely at other widths. Halved horizontal padding (4 here plus the preview's
    // 4) so the image sits close to the edges.
    let section_4 = egui::Panel::right("section-4")
        // Not resizable: exact_size alone leaves the panel resizable, so the edge still shows
        // a resize cursor even though dragging does nothing.
        .resizable(false)
        .exact_size(SIDE_PANEL_WIDTH)
        .show_separator_line(false)
        .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(4, 2)))
        .show_inside(ui, |ui| draw_pane(layout.right_panel, ui, state));

    // Section 3-left: the dock (archetype 2, left = project/global sources and tools). Like the
    // right column it sits below the ribbon and flanks the canvas; its right border is drawn with
    // the section borders below. Created after the right column so both bound the workspace.
    let dock = mount_dock(ui, state);

    // Section 3: the workspace body — the canvas with the 3D viewport stacked beneath it,
    // flanked by the side column. No frame margin, so the canvas hugs the section borders.
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            // The viewport panel's height is driven by the layout mode: a remembered fraction in
            // Split (with the divider draggable), a single bar in Graph (the viewport collapses),
            // or all but a bar in Preview (the graph collapses). Each collapsed pane draws only its
            // labeled bar, never its content.
            let body_rect = ui.max_rect();
            let avail = ui.available_height();
            let bar = WORKSPACE_BAR_H;
            // Split keeps each shown pane at least the snap height; the collapsed modes shrink the
            // off pane all the way to the bar.
            let split_min = WORKSPACE_COLLAPSE_SNAP;
            let split_max = (avail - WORKSPACE_COLLAPSE_SNAP).max(split_min);
            let goal = match state.workspace_mode {
                // Open Split with clearance from the snap thresholds, so a restore never reopens the
                // pane sitting on the edge of the snap zone. Free dragging (below) still reaches the
                // real min/max.
                WorkspaceMode::Split => {
                    let lo = split_min + WORKSPACE_RESTORE_CLEARANCE;
                    let hi = (split_max - WORKSPACE_RESTORE_CLEARANCE).max(lo);
                    (state.viewport_frac * avail).clamp(lo, hi)
                }
                WorkspaceMode::Graph => bar,
                WorkspaceMode::Preview => (avail - bar).max(bar),
            };
            let split = state.workspace_mode == WorkspaceMode::Split;
            // Animate the viewport height toward the mode's goal. On a mode change, ease from the
            // height rendered last frame; the first frame snaps (no animation) and forces the size,
            // so a stale persisted size never shows as a sliver viewport at startup.
            let now = ui.input(|i| i.time);
            let first = state.workspace_mode_prev.is_none();
            if state.workspace_mode_prev != Some(state.workspace_mode) {
                state.viewport_anim_from = if first { goal } else { state.viewport_last_h };
                // Start the first frame already finished (no startup animation); a real mode change
                // starts now.
                state.viewport_anim_start = if first {
                    now - WORKSPACE_ANIM_SECS
                } else {
                    now
                };
                state.workspace_mode_prev = Some(state.workspace_mode);
            }
            let t = if first {
                1.0
            } else {
                ((now - state.viewport_anim_start) / WORKSPACE_ANIM_SECS).clamp(0.0, 1.0) as f32
            };
            // Ease-out quart: fast to start, decelerating into a soft settle at the target.
            let eased = 1.0 - (1.0 - t).powi(4);
            let animated = state.viewport_anim_from + (goal - state.viewport_anim_from) * eased;
            let animating = t < 1.0;
            // Steady Split is the only freely-resizable state; a collapse/expand animation or a
            // maximized mode drives the height directly.
            let steady_split = split && !animating && !first;
            let panel = egui::Panel::bottom("viewport-panel")
                .show_separator_line(false)
                // The 3D stage's own background, not the dark chrome panel fill: a mid cool slate
                // that keeps the neutral grey terrain legible in both highlight and shadow. No inner
                // margin, so the viewport hugs the panel edges.
                .frame(egui::Frame::new().fill(theme::VIEWPORT_BG));
            let panel = if steady_split {
                panel
                    .resizable(true)
                    .min_size(split_min)
                    .max_size(split_max)
                    .default_size(animated)
            } else {
                panel
                    .resizable(false)
                    .exact_size(animated.clamp(bar, (avail - bar).max(bar)))
            };
            let viewport = panel.show_inside(ui, |ui| draw_pane(layout.viewport, ui, state));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| draw_pane(layout.canvas, ui, state));

            let vp_h = viewport.response.rect.height();
            state.viewport_last_h = vp_h;
            if animating {
                ui.ctx().request_repaint();
            }
            // In steady Split, dragging a pane down to the snap point collapses it (the shrinking
            // pane is replaced by its labeled bar); otherwise remember the divider position so a
            // restore returns to it. Skipped while animating, so restoring never re-collapses.
            if steady_split && avail > 0.0 {
                if vp_h <= split_min + 1.0 {
                    state.workspace_mode = WorkspaceMode::Graph;
                } else if vp_h >= split_max - 1.0 {
                    state.workspace_mode = WorkspaceMode::Preview;
                } else {
                    state.viewport_frac = (vp_h / avail).clamp(0.05, 0.95);
                }
            }

            // A light border between the canvas and the viewport; in steady Split, a centered grab
            // handle makes the resize affordance obvious.
            let divider_y = viewport.response.rect.top();
            let x_range = viewport.response.rect.x_range();
            ui.painter().hline(x_range, divider_y, line);
            if steady_split {
                divider_handle(ui, divider_y, x_range);
            }

            // The single layout switcher, floated at the workspace top-right regardless of mode.
            layout_mode_switcher(ui, body_rect, state);
        });

    // Section borders, drawn last so they sit on top: the full-width lines under the menu and
    // the ribbon, the footer's top edge, and the heavier boundary left of the side column.
    let painter = ui.painter();
    painter.hline(
        menu.response.rect.x_range(),
        menu.response.rect.bottom(),
        line,
    );
    painter.hline(
        palette.response.rect.x_range(),
        palette.response.rect.bottom(),
        heavy,
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
    painter.vline(
        dock.response.rect.right(),
        dock.response.rect.y_range(),
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
        // The Frost theme's typefaces: IBM Plex Sans (UI) and IBM Plex Mono (values, labels), both
        // OFL, embedded so they ship in the binary (see assets/fonts/). egui has no synthetic bold,
        // so each weight is a real file registered as its own named family; the built-in
        // proportional/monospace families lead with the Regular weights and keep egui's own fonts as
        // fallbacks after them. Phosphor is then appended as the icon fallback (below).
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            (
                "plex-sans",
                include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf").as_slice(),
            ),
            (
                "plex-sans-medium",
                include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf").as_slice(),
            ),
            (
                "plex-sans-semibold",
                include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf").as_slice(),
            ),
            (
                "plex-mono",
                include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf").as_slice(),
            ),
            (
                "plex-mono-medium",
                include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf").as_slice(),
            ),
        ] {
            fonts.font_data.insert(
                name.to_owned(),
                std::sync::Arc::new(egui::FontData::from_static(bytes)),
            );
        }
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "plex-sans".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "plex-mono".to_owned());
        // The heavier weights, addressable via `FontFamily::Name` for titles and emphasis.
        fonts.families.insert(
            egui::FontFamily::Name("plex-medium".into()),
            vec!["plex-sans-medium".to_owned()],
        );
        fonts.families.insert(
            egui::FontFamily::Name("plex-semibold".into()),
            vec!["plex-sans-semibold".to_owned()],
        );
        fonts.families.insert(
            egui::FontFamily::Name("plex-mono-medium".into()),
            vec!["plex-mono-medium".to_owned()],
        );

        // Install the Phosphor icon font as a fallback in the proportional family, so an
        // icon const (a disclosure caret, etc.) renders anywhere text does.
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
        // Overlay the user's saved preferences (the author profile, #106) from config, here in
        // the app shell so the test-constructed state never touches the filesystem.
        apply_preferences(&mut state);
        // Load the subgraph library listing for the left dock (#106), again in the app shell so
        // the test-constructed state never touches the filesystem.
        state.reload_library();
        // Load the recent-projects list from config (empty on first run), here in the app
        // shell so the test-constructed state never touches the filesystem.
        state.recent = load_recent();
        // Open the build cache's disk store (read view) for the viewport, in the app shell so
        // the test-constructed state never touches the filesystem.
        state.field_store = build::open_store();
        // Set up the 3D viewport's wgpu pipeline once, now that the wgpu device exists, and stash
        // the render state so the 2D map can shade on the GPU too (#167).
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            viewport::init(render_state);
            state.render_state = Some(render_state.clone());
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
        // The "Save to library" dialog floats over the panes when open (#106).
        library_save_dialog(ui.ctx(), &mut self.state);
        settings_dialog(ui.ctx(), &mut self.state);
        about_dialog(ui.ctx(), &mut self.state);
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

    /// A minimal library file wrapping the identity inner graph, with a chosen seed and a
    /// one-node interior layout, for the insert tests.
    #[cfg(test)]
    fn sample_library_file(seed: i64) -> library::SubgraphFile {
        let mut view = project_file::ViewState::default();
        view.nodes.insert(0, [12.0, 34.0]);
        library::SubgraphFile {
            format_version: library::SUBGRAPH_FORMAT_VERSION,
            name: "Passthrough".to_string(),
            category: "Utility".to_string(),
            description: "Feeds input to output.".to_string(),
            inputs: vec![library::PortDoc {
                index: 0,
                name: "In".to_string(),
                description: String::new(),
            }],
            outputs: vec![library::PortDoc {
                index: 0,
                name: "Out".to_string(),
                description: String::new(),
            }],
            author: preferences::AuthorProfile::default(),
            license: String::new(),
            seed,
            graph: identity_inner().to_document(),
            view,
        }
    }

    #[test]
    fn inserting_a_library_subgraph_adds_a_named_seeded_container() {
        let mut state = AppState::new();
        state.new_project();
        let before = state.graph.node_count();

        state.insert_subgraph_from_library(&sample_library_file(7));

        assert_eq!(
            state.graph.node_count(),
            before + 1,
            "exactly one container added"
        );
        let handle = state.primary.expect("the new container is selected");
        let id = state.graph.node_id_of(handle).expect("container id");

        // The saved inner graph is nested, and the container's arity reflects its boundary
        // markers (one Input, one Output).
        let inner = state.graph.nested(id).expect("inner graph nested");
        assert_eq!(inner.node_count(), 2, "Input + Output preserved");
        let spec = state.graph.spec(id).expect("spec");
        assert_eq!(spec.inputs.len(), 1, "one input port from the Input marker");
        assert_eq!(
            spec.outputs.len(),
            1,
            "one output port from the Output marker"
        );

        // The saved seed and the library name are applied.
        assert_eq!(
            state.graph.params(id).expect("params").get_i64("seed", -1),
            7,
            "the saved seed is applied"
        );
        assert_eq!(
            state.graph.name(id),
            Some("Passthrough"),
            "the library name becomes the display name"
        );

        // The saved interior layout is registered under the new container's path.
        assert_eq!(
            state.subgraph_layouts.get(&vec![handle]).map(BTreeMap::len),
            Some(1),
            "the saved interior layout is registered"
        );
    }

    #[test]
    fn deleting_a_library_entry_removes_its_file_and_clears_the_selection() {
        // A real temp file so the delete has something to remove.
        let path =
            std::env::temp_dir().join(format!("ymir-del-test-{}.ymirsub", std::process::id()));
        let file = sample_library_file(1);
        library::write_subgraph(&path, &file).expect("write temp file");
        assert!(path.exists(), "the temp entry exists before delete");

        let mut state = AppState::new();
        state.library = library::LibraryListing {
            entries: vec![library::LibraryEntry {
                path: path.clone(),
                file,
            }],
            errors: Vec::new(),
        };
        state.library_selection = Some(path.clone());
        state.library_pending_delete = Some(path.clone());

        state.delete_library_entry(&path);

        assert!(!path.exists(), "the file is removed from disk");
        assert_eq!(state.library_selection, None, "the selection is cleared");
        assert_eq!(
            state.library_pending_delete, None,
            "the armed delete is cleared"
        );
    }

    #[test]
    fn library_search_matches_name_category_and_description() {
        let file = sample_library_file(1); // name "Passthrough", category "Utility", desc "Feeds..."
        // An empty query matches everything.
        assert!(library_matches(&file, ""));
        // Case-insensitive substring on the name.
        assert!(library_matches(&file, "pass"));
        // Matches on the category and the description too.
        assert!(library_matches(&file, "utility"));
        assert!(library_matches(&file, "feeds"));
        // A query in none of the three fields does not match.
        assert!(!library_matches(&file, "erosion"));
    }

    #[test]
    fn inserting_a_corrupt_library_subgraph_adds_nothing_and_reports_it() {
        let mut state = AppState::new();
        state.new_project();
        let before = state.graph.node_count();

        // An unloadable document (an unsupported format version) must add nothing.
        let mut file = sample_library_file(1);
        file.graph.format_version = file.graph.format_version.wrapping_add(1);
        state.insert_subgraph_from_library(&file);

        assert_eq!(
            state.graph.node_count(),
            before,
            "a bad document leaves the canvas untouched"
        );
        assert!(
            state.status.is_some(),
            "the failure is reported to the user"
        );
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
    fn subgraph_interior_layout_persists_through_save_and_restore() {
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

        // Dive in, move an inner node, exit (so the layout lands in subgraph_layouts).
        state.dive_in(handle);
        let (snarl_id, inner_handle) = state
            .snarl
            .node_ids()
            .map(|(id, &h)| (id, h))
            .next()
            .expect("an inner node");
        let moved = egui::Pos2::new(250.0, 175.0);
        if let Some(info) = state.snarl.get_node_info_mut(snarl_id) {
            info.pos = moved;
        }
        state.exit_subgraph();

        // Snapshot (what Save writes) then restore (what Open reads): the interior layout
        // survives the round-trip rather than re-cascading.
        let restored = state.snapshot().restore().expect("restore");
        let positions = restored
            .subgraph_layouts
            .get(&vec![handle])
            .expect("inner layout restored");
        let pos = positions.get(&inner_handle).expect("inner node position");
        assert!(
            (pos[0] - moved.x).abs() < 1e-3 && (pos[1] - moved.y).abs() < 1e-3,
            "the inner node's saved position survived save + restore"
        );
    }

    #[test]
    fn create_subgraph_from_wraps_a_selection_into_a_container() {
        let mut state = AppState::new();
        state.new_project();
        let f = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "generator.fbm",
            egui::Pos2::ZERO,
        )
        .expect("f");
        let a = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "modifier.null",
            egui::Pos2::new(50.0, 0.0),
        )
        .expect("a");
        let b = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "modifier.null",
            egui::Pos2::new(100.0, 0.0),
        )
        .expect("b");
        let sink = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "endpoint.export",
            egui::Pos2::new(150.0, 0.0),
        )
        .expect("sink");
        state.graph.connect(f, 0, a, 0).expect("f->a");
        state.graph.connect(a, 0, b, 0).expect("a->b");
        state.graph.connect(b, 0, sink, 0).expect("b->sink");
        let (ha, hb) = (
            state.graph.stable_id(a).unwrap(),
            state.graph.stable_id(b).unwrap(),
        );

        state.create_subgraph_from(&[ha, hb]);

        // a and b are wrapped: feeder, container, sink remain, and the canvas matches.
        assert_eq!(state.graph.node_count(), 3);
        assert_eq!(
            state.snarl.node_ids().count(),
            3,
            "canvas matches the graph"
        );
        // The container is the new selection, a subgraph with one derived input and output.
        let container = state.primary.expect("container selected");
        let container_id = state.graph.node_id_of(container).expect("container id");
        let inner = state.graph.nested(container_id).expect("is a container");
        assert_eq!(
            inner.node_count(),
            4,
            "a, b, plus one input and one output marker"
        );
        let spec = state.graph.spec(container_id).expect("spec");
        assert_eq!(spec.inputs.len(), 1);
        assert_eq!(spec.outputs.len(), 1);

        // The interior layout is stored untangled, not cascaded: the two wrapped nodes keep
        // their positions (a at x=50, b at x=100) and the markers sit to either side.
        let layout = state
            .subgraph_layouts
            .get(&vec![container])
            .expect("interior layout stored");
        assert_eq!(
            layout.len(),
            4,
            "two wrapped nodes plus two markers, placed"
        );
        assert!(
            layout.values().any(|p| p[0] < 50.0),
            "an Input marker sits left of the wrapped cluster"
        );
        assert!(
            layout.values().any(|p| p[0] > 100.0),
            "an Output marker sits right of the wrapped cluster"
        );
    }

    #[test]
    fn subgraph_inputs_bind_the_real_upstream_field() {
        let mut state = AppState::new();
        state.new_project();
        // fbm -> null -> export; wrap {null} into a subgraph.
        let f = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "generator.fbm",
            egui::Pos2::ZERO,
        )
        .expect("fbm");
        let n = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "modifier.null",
            egui::Pos2::new(50.0, 0.0),
        )
        .expect("null");
        let sink = canvas::add_node(
            &mut state.graph,
            &mut state.snarl,
            "endpoint.export",
            egui::Pos2::new(100.0, 0.0),
        )
        .expect("export");
        state.graph.connect(f, 0, n, 0).expect("f->n");
        state.graph.connect(n, 0, sink, 0).expect("n->sink");
        let hn = state.graph.stable_id(n).unwrap();

        state.create_subgraph_from(&[hn]);
        let container = state.primary.expect("container selected");
        state.dive_in(container);

        // Inside, the lone Input marker binds to the fbm feeding the container.
        let inputs = state.subgraph_inputs().expect("bound inputs exist");
        let request = EvalRequest::new(16, 16, Region::UNIT, state.seed);
        let bound = inputs.bound_fields(&state.graph, &request);
        assert_eq!(bound.len(), 1, "one input marker is bound");
        let (_, field) = &bound[0];
        // It is the real fbm field (has variation), not the markers' flat zero stand-in.
        let height = field
            .layer(ymir_core::layers::HEIGHT)
            .expect("height layer");
        let first = height.as_slice()[0];
        assert!(
            height.as_slice().iter().any(|&v| (v - first).abs() > 1e-6),
            "the bound input is the real upstream field, not a flat zero"
        );
    }

    #[test]
    fn exiting_a_subgraph_restores_the_parent_view() {
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

        // A distinctive parent pan/zoom; dive in, then pop out.
        let parent_view = egui::emath::TSTransform::new(egui::vec2(123.0, 45.0), 1.7);
        state.canvas_view = Some(CanvasView {
            to_global: parent_view,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
        });
        state.dive_in(handle);
        state.exit_subgraph();

        // Popping out requests the parent's view again, rather than keeping the interior's.
        assert_eq!(state.pending_view, Some(parent_view));
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
    fn generators_are_grouped_in_order_with_separators() {
        let entries = node_entries();
        let rows = menu_rows(&entries, "", Some("generator"));
        assert_eq!(rows.first(), Some(&MenuRow::Back));
        // Nodes appear in palette-group order: noise (Flow included), then cellular, then
        // shapes, then gradient/falloff.
        let node_seq: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                MenuRow::Node(t) => Some(*t),
                _ => None,
            })
            .collect();
        let pos = |t: &str| node_seq.iter().position(|&x| x == t).expect("node present");
        assert!(
            pos("generator.fbm") < pos("generator.flow"),
            "fbm before flow"
        );
        assert!(
            pos("generator.flow") < pos("generator.cellular_bumps"),
            "flow (noise) before cellular"
        );
        assert!(
            pos("generator.cellular_bumps") < pos("generator.radial"),
            "cellular before shapes"
        );
        assert!(
            pos("generator.radial") < pos("generator.gradient"),
            "shapes before gradient/falloff"
        );
        // Separators sit between groups: at least one, never at an end, never adjacent
        // (which would mean an empty group).
        assert!(rows.iter().any(|r| matches!(r, MenuRow::Separator)));
        assert_ne!(rows.first(), Some(&MenuRow::Separator));
        assert_ne!(rows.last(), Some(&MenuRow::Separator));
        assert!(
            !rows
                .windows(2)
                .any(|w| w[0] == MenuRow::Separator && w[1] == MenuRow::Separator),
            "no two separators are adjacent"
        );
    }

    #[test]
    fn menu_navigation_skips_separators() {
        let rows = vec![
            MenuRow::Back,
            MenuRow::Node("a"),
            MenuRow::Separator,
            MenuRow::Node("b"),
        ];
        // Down from the node before the divider lands on the node after it, not the divider.
        assert_eq!(step_highlight(&rows, 1, true), 3);
        // Up from that node steps back over the divider.
        assert_eq!(step_highlight(&rows, 3, false), 1);
        // Wrapping from the last selectable row reaches Back, skipping nothing spurious.
        assert_eq!(step_highlight(&rows, 3, true), 0);
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
    fn boundary_markers_are_addable_only_inside_a_subgraph() {
        // Outside a subgraph the markers are disabled (not addable); inside, addable.
        assert!(!node_addable("subgraph.input", false));
        assert!(!node_addable("subgraph.output", false));
        assert!(node_addable("subgraph.input", true));
        assert!(node_addable("subgraph.output", true));
        // Ordinary nodes are addable at any level.
        assert!(node_addable("generator.fbm", false));
        assert!(node_addable("modifier.null", true));
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

        // A previewable selection is the target. Selecting the endpoint (no output of its own)
        // instead previews what it exports: the generator wired into its input (#133).
        state.select_only(generator);
        assert_eq!(state.preview_target(), Some(generator));
        state.select_only(endpoint);
        assert_eq!(state.preview_target(), Some(generator));

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
    fn dismissing_the_preview_blanks_it_instead_of_the_sink_fallback() {
        let mut state = AppState::new();
        state.graph = Graph::new();
        state.snarl = Snarl::new();
        let pos = egui::Pos2::ZERO;
        let gen_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        let mod_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "modifier.invert", pos).unwrap();
        state.graph.connect(gen_id, 0, mod_id, 0).unwrap();
        let modifier = state.graph.stable_id(mod_id).unwrap();

        // Nothing dismissed: the sink is previewed, as before.
        assert_eq!(state.preview_target(), Some(modifier));

        // The background-click deselect sets this: the preview goes blank rather than to the sink.
        state.preview_dismissed = true;
        assert_eq!(state.preview_target(), None);

        // A fresh open or load clears the selection, which restores the result-node fallback.
        state.clear_selection();
        assert_eq!(state.preview_target(), Some(modifier));
    }

    #[test]
    fn selecting_an_export_endpoint_previews_its_input_not_an_unrelated_sink() {
        let mut state = AppState::new();
        state.graph = Graph::new();
        state.snarl = Snarl::new();
        let pos = egui::Pos2::ZERO;
        // A source feeding an export endpoint, plus a separate dangling generator added last (so
        // it has the highest stable id) that the sink fallback would otherwise pick.
        let src_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        let export_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "endpoint.export", pos).unwrap();
        let dangling_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "generator.fbm", pos).unwrap();
        state.graph.connect(src_id, 0, export_id, 0).unwrap();
        let src = state.graph.stable_id(src_id).unwrap();
        let export = state.graph.stable_id(export_id).unwrap();
        let dangling = state.graph.stable_id(dangling_id).unwrap();

        // The dangling generator is the highest-id previewable sink, so the sink fallback alone
        // would pick it.
        assert_eq!(state.preview_sink(), Some(dangling));

        // But selecting the export previews what it exports: the node wired into its input, not
        // the unrelated dangling branch.
        state.select_only(export);
        assert_eq!(state.preview_target(), Some(src));

        // An export with nothing wired has no input to show, so it still falls back to the sink.
        let empty_id =
            canvas::add_node(&mut state.graph, &mut state.snarl, "endpoint.export", pos).unwrap();
        let empty = state.graph.stable_id(empty_id).unwrap();
        state.select_only(empty);
        assert_eq!(state.preview_target(), Some(dangling));
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
    fn data_path_prefers_xdg_data_then_home_share_then_none() {
        use std::ffi::OsString;
        use std::path::PathBuf;
        // XDG_DATA_HOME wins when set and non-empty.
        assert_eq!(
            data_path(
                Some(OsString::from("/xdg-data")),
                Some(OsString::from("/home/u")),
                "subgraphs"
            ),
            Some(PathBuf::from("/xdg-data/ymir/subgraphs"))
        );
        // An empty XDG value falls through to HOME/.local/share (the XDG data default).
        assert_eq!(
            data_path(
                Some(OsString::new()),
                Some(OsString::from("/home/u")),
                "subgraphs"
            ),
            Some(PathBuf::from("/home/u/.local/share/ymir/subgraphs"))
        );
        // No XDG: HOME/.local/share.
        assert_eq!(
            data_path(None, Some(OsString::from("/home/u")), "subgraphs"),
            Some(PathBuf::from("/home/u/.local/share/ymir/subgraphs"))
        );
        // Neither set: unavailable.
        assert_eq!(data_path(None, None, "subgraphs"), None);
    }

    #[test]
    fn sanitize_filename_keeps_safe_chars_and_falls_back() {
        // Alphanumerics, dash, and underscore survive untouched.
        assert_eq!(sanitize_filename("Mount_Fuji-2"), "Mount_Fuji-2");
        // Spaces and path separators become dashes, so the stem is a single safe segment.
        assert_eq!(sanitize_filename("rocky ridge/v2"), "rocky-ridge-v2");
        // Surrounding whitespace is trimmed before mapping.
        assert_eq!(sanitize_filename("  crater  "), "crater");
        // A name of only punctuation would collapse to empty, so it falls back.
        assert_eq!(sanitize_filename("///"), "subgraph");
        assert_eq!(sanitize_filename(""), "subgraph");
    }

    #[test]
    fn save_decision_guards_a_blank_name_then_an_overwrite() {
        // A blank (or whitespace) name is rejected before anything else.
        assert_eq!(
            save_decision("   ", false, false),
            SaveDecision::NameRequired
        );
        // A blank name is rejected even when it would otherwise overwrite: an empty name never
        // arms an overwrite.
        assert_eq!(save_decision("", true, false), SaveDecision::NameRequired);
        // A new name (no existing file) writes straight away.
        assert_eq!(save_decision("Ridge", false, false), SaveDecision::Write);
        // An existing name warns first, arming the confirmation.
        assert_eq!(
            save_decision("Ridge", true, false),
            SaveDecision::ConfirmOverwrite
        );
        // Once confirmed, the same existing name writes (overwrites).
        assert_eq!(save_decision("Ridge", true, true), SaveDecision::Write);
    }

    #[test]
    fn foreign_overwrite_spares_an_in_place_edit() {
        let a = std::path::Path::new("/lib/a.ymirsub");
        let b = std::path::Path::new("/lib/b.ymirsub");
        // A fresh save (no original) onto an existing name is a foreign overwrite.
        assert!(is_foreign_overwrite(true, Some(a), None));
        // Editing an entry in place (target equals its original) is never a conflict.
        assert!(!is_foreign_overwrite(true, Some(a), Some(a)));
        // Renaming onto a different existing entry is a conflict.
        assert!(is_foreign_overwrite(true, Some(b), Some(a)));
        // No file at the target: never a conflict, whatever the original.
        assert!(!is_foreign_overwrite(false, Some(a), None));
    }

    #[test]
    fn editing_a_library_entry_applies_metadata_and_reconciles_port_names() {
        // build_subgraph_file for an Existing source uses the stored graph, so no canvas is needed.
        let state = AppState::new();
        let original = sample_library_file(9);
        // The sample's graph markers are unnamed while its port docs read "In"/"Out": exactly the
        // divergence the reconcile fixes. Rename the input to prove an edit propagates too.
        let mut inputs = original.inputs.clone();
        inputs[0].name = "base terrain".to_string();
        let dialog = LibrarySave {
            source: SubgraphSource::Existing {
                original_path: std::path::PathBuf::from("/lib/passthrough.ymirsub"),
                graph: original.graph.clone(),
                seed: original.seed,
                view: original.view.clone(),
            },
            name: "Renamed".to_string(),
            category: "Terrain".to_string(),
            description: "Edited description.".to_string(),
            inputs,
            outputs: original.outputs.clone(),
            author: original.author.clone(),
            license: original.license.clone(),
            error: None,
            confirm_overwrite: false,
        };

        let built = state
            .build_subgraph_file(&dialog)
            .expect("build the edited file");

        // The seed and the edited metadata are applied.
        assert_eq!(built.seed, original.seed, "the seed is preserved");
        assert_eq!(built.name, "Renamed");
        assert_eq!(built.description, "Edited description.");
        assert_eq!(built.category, "Terrain");
        // The port names are reconciled onto the graph's boundary markers, so an instance dropped
        // from the library derives the same names the card shows (marker name -> derived port name
        // is covered by ymir-core's own subgraph tests).
        assert_eq!(built.inputs[0].name, "base terrain");
        assert_eq!(built.outputs[0].name, "Out");
        let input_marker = built
            .graph
            .nodes
            .iter()
            .find(|n| n.type_id == INPUT_TYPE_ID)
            .expect("an input marker");
        let output_marker = built
            .graph
            .nodes
            .iter()
            .find(|n| n.type_id == OUTPUT_TYPE_ID)
            .expect("an output marker");
        assert_eq!(input_marker.name.as_deref(), Some("base terrain"));
        assert_eq!(output_marker.name.as_deref(), Some("Out"));
    }

    #[test]
    fn reconcile_port_names_writes_names_and_keeps_defaults_unnamed() {
        // Two input markers on a document; name the first, leave the second at its default.
        let mut inner = Graph::new();
        // The first input marker stays wired-free; only its presence and order matter here.
        let _a = inner.add_op(
            ymir_core::registry::make("subgraph.input").expect("input a"),
            Params::default(),
        );
        let b = inner.add_op(
            ymir_core::registry::make("subgraph.input").expect("input b"),
            Params::default(),
        );
        let out = inner.add_op(
            ymir_core::registry::make("subgraph.output").expect("output"),
            Params::default(),
        );
        inner.connect(b, 0, out, 0).expect("wire");
        let mut doc = inner.to_document();

        let docs = vec![
            library::PortDoc {
                index: 0,
                name: "height".to_string(),
                description: String::new(),
            },
            // Left at the positional default: should clear the override, not store a literal.
            library::PortDoc {
                index: 1,
                name: "Input 2".to_string(),
                description: String::new(),
            },
        ];
        let out_docs = reconcile_port_names(&mut doc, INPUT_TYPE_ID, &docs);

        // The returned docs carry the resolved display names.
        assert_eq!(out_docs[0].name, "height");
        assert_eq!(out_docs[1].name, "Input 2");
        // The markers (in stable-id order, so port order): the first named, the second left a
        // genuine `None` so it falls back to its positional label rather than storing a literal.
        let markers: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| n.type_id == INPUT_TYPE_ID)
            .collect();
        assert_eq!(markers[0].name.as_deref(), Some("height"));
        assert_eq!(markers[1].name, None, "a default port stays unnamed");
    }

    #[test]
    fn write_then_read_project_round_trips_through_a_file() {
        // Exercises the real file I/O wrappers (the in-memory serde path is covered in
        // project_file): write a session to disk, read it back, confirm it matches.
        let (graph, snarl) = starter::starter_graph();
        let file = project_file::ProjectFile::capture(
            &graph,
            &snarl,
            project_file::WorldSettings {
                seed: 7,
                world_extent: 2048.0,
                world_height: 640.0,
                build_res: 4096,
                preview_res: 384,
                sea_level: 0.55,
                show_water: true,
                water: project_file::WaterSettings {
                    depth: false,
                    waves: true,
                    reflection: false,
                    foam_on: false,
                    extinction: 12.5,
                    color: [0.2, 0.3, 0.4],
                    wave: 0.25,
                    reflectivity: 0.75,
                    specular: 0.1,
                    steepness: 0.4,
                    wavelength: 1.5,
                    foam: 0.8,
                    foam_width: 0.02,
                    wet_on: false,
                    wet: 0.5,
                    wet_width: 0.04,
                    speed: 0.9,
                },
            },
            &[],
        );
        let path =
            std::env::temp_dir().join(format!("ymir-default-test-{}.ymir", std::process::id()));

        write_project(&path, &file).expect("write project");
        let restored = read_project(&path).expect("read project");
        std::fs::remove_file(&path).expect("remove temp file");

        assert_eq!(restored.graph.to_document(), graph.to_document());
        assert_eq!(restored.seed, 7);
        assert_eq!(restored.world_extent, 2048.0);
        assert_eq!(restored.world_height, 640.0);
        assert_eq!(restored.build_res, 4096);
        assert_eq!(restored.preview_res, 384);
        assert_eq!(restored.sea_level, 0.55);
        assert!(restored.show_water);
        // The water look and effect layers travel with the project (#157).
        assert!(!restored.water.depth);
        assert!(restored.water.waves);
        assert!(!restored.water.reflection);
        assert!(!restored.water.foam_on);
        assert_eq!(restored.water.extinction, 12.5);
        assert_eq!(restored.water.color, [0.2, 0.3, 0.4]);
        assert_eq!(restored.water.speed, 0.9);
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
