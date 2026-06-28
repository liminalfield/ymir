//! Ymir Dark theme: the brand palette and the egui [`Visuals`] built from it (#104).
//!
//! The palette is one cold blue hue family with a small accent set. Colours are named
//! constants here, not literals scattered across the GUI, so there is one source to
//! retune. They are derived from the brand kit's canonical token source
//! (`ymir-ui-design/theme/ymir-theme.json`); if a colour changes, change the JSON and
//! mirror it here.
//!
//! [`visuals`] maps the tokens onto egui's slots once at startup. The GUI reads
//! `ui.visuals()` throughout, so this central override propagates without touching call
//! sites: the depth ramp (abyss/base/surface/raised) gives panels, menus, and the canvas
//! distinct elevations, and the line colours give them visible borders.
//!
//! Only the colours currently used are defined here; the rest of the palette
//! (`text-secondary/muted/faint`, `accent-violet/ice`) lives in the brand kit and is
//! added as later theming steps adopt it (the canvas, the text ramp).

use eframe::egui::{Color32, Stroke, Visuals};

// --- surfaces & depth (lowest to highest) ---
/// App backdrop / lowest layer.
pub const BG_ABYSS: Color32 = Color32::from_rgb(0x0b, 0x0f, 0x17);
/// Editor canvas.
pub const BG_BASE: Color32 = Color32::from_rgb(0x0f, 0x14, 0x1f);
/// Panels, sidebars, toolbars.
pub const BG_SURFACE: Color32 = Color32::from_rgb(0x16, 0x1d, 0x2b);
/// Cards, nodes, popovers, menus.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1e, 0x27, 0x38);
/// Dividers, default borders.
pub const LINE: Color32 = Color32::from_rgb(0x2b, 0x36, 0x50);
/// Active / focused borders.
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x3b, 0x49, 0x6a);

// --- text ramp ---
/// Primary copy, headings.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xd6, 0xe0, 0xf0);

// --- accents (cold core) ---
/// Interactive, focus, primary action.
pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0x6d, 0x9f, 0xef);
/// Links, active node, wire connections.
pub const ACCENT_FROST: Color32 = Color32::from_rgb(0x34, 0xc3, 0xc0);

// --- semantic (used sparingly) ---
/// Solved / valid state.
pub const SUCCESS: Color32 = Color32::from_rgb(0x5f, 0xcf, 0x9a);
/// Warnings (the one warm accent).
pub const WARNING: Color32 = Color32::from_rgb(0xe6, 0xb1, 0x5c);
/// Errors (cooled rose).
pub const ERROR: Color32 = Color32::from_rgb(0xe7, 0x6a, 0x86);
/// Text & marquee selection fill.
pub const SELECTION: Color32 = Color32::from_rgb(0x25, 0x40, 0x6b);

/// The Ymir Dark [`Visuals`], built by mapping the palette onto egui's slots.
///
/// Starts from egui's dark visuals so unmapped details (shadows, corner radii, spacing)
/// keep sensible defaults, then overrides the colour-bearing slots:
/// - **panels** read `bg-surface`, **windows/menus** `bg-raised` with a `line` border, so
///   a menu reads as raised above a panel above the canvas;
/// - **text** defaults to `text-primary`, with weaker copy deriving from it;
/// - **accents** appear only on selection, focus, and the active/hover border, keeping
///   the surfaces neutral.
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    // Surfaces.
    v.panel_fill = BG_SURFACE;
    v.window_fill = BG_RAISED;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.extreme_bg_color = BG_ABYSS; // text-edit and deepest backgrounds
    v.faint_bg_color = BG_BASE; // alternating rows, faint fills
    v.code_bg_color = BG_BASE;

    // Accents, used sparingly.
    v.hyperlink_color = ACCENT_FROST;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0, ACCENT_PRIMARY);
    v.warn_fg_color = WARNING;
    v.error_fg_color = ERROR;

    // Widget states. Non-interactive carries the default text colour and divider/border
    // strokes; interactive widgets sit on `raised` and brighten their border on hover and
    // focus rather than flooding with colour.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG_SURFACE;
    w.noninteractive.weak_bg_fill = BG_SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.inactive.bg_fill = BG_RAISED;
    w.inactive.weak_bg_fill = BG_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.hovered.bg_fill = LINE;
    w.hovered.weak_bg_fill = LINE;
    w.hovered.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.active.bg_fill = LINE_STRONG;
    w.active.weak_bg_fill = LINE_STRONG;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT_PRIMARY);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.open.bg_fill = BG_RAISED;
    w.open.weak_bg_fill = BG_RAISED;
    w.open.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    w.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    v
}
