//! Ymir "Frost Giant" theme: the brand palette and the egui [`Visuals`] built from it.
//!
//! A hybrid light-dark scheme (see `design_handoff_frost_theme/`): a **dark chrome** shell (title
//! bar, menus, toolbar, side panels, status bar) wrapping a **frosted icy-light node canvas**. This
//! module owns the dark chrome and the accents; the light canvas and node cards are a local visuals
//! override applied in the canvas viewer, so they do not fight this global dark theme.
//!
//! Tokens are named constants here, not literals scattered across the GUI, so there is one source to
//! retune. They come from the Frost handoff (authored in OKLCH; the hex here is the sRGB
//! approximation the handoff lists). [`visuals`] maps them onto egui's slots once at startup; the
//! GUI reads `ui.visuals()` throughout, so the central override propagates without touching call
//! sites.
//!
//! The constant names are kept from the previous palette (call sites reference them) and remapped to
//! the Frost tokens: the chrome depth ramp gives panels and menus distinct elevations, the line
//! colours give visible hairline borders, and the ink ramp is a proper three-level text hierarchy.

use eframe::egui::{Color32, Stroke, Visuals};

// --- chrome: the dark UI shell, lowest to highest elevation ---
/// Title bar, status bar, deepest inputs (text edits). Frost `chrome-0`.
pub const BG_ABYSS: Color32 = Color32::from_rgb(0x2b, 0x30, 0x38);
/// Faint fills and alternating rows. A touch above the deepest inputs. Frost `chrome-0`.
pub const BG_BASE: Color32 = Color32::from_rgb(0x2b, 0x30, 0x38);
/// Default panel / toolbar background. Frost `chrome-1`.
pub const BG_SURFACE: Color32 = Color32::from_rgb(0x36, 0x3b, 0x44);
/// Raised chips, palette buttons, menus/popovers. Frost `chrome-2`.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x3f, 0x44, 0x4d);
/// Hover / stronger raised surfaces. Frost `chrome-3`.
pub const BG_HOVER: Color32 = Color32::from_rgb(0x48, 0x4e, 0x57);
/// Subtle inner dividers (1px). Frost `chrome-line-soft`.
pub const LINE: Color32 = Color32::from_rgb(0x4c, 0x52, 0x5c);
/// Primary hairline borders, active/focused borders (1px). Frost `chrome-line`.
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x5a, 0x61, 0x6b);

// --- ink: the text ramp on dark chrome (three legible levels) ---
/// Primary text, active labels, headings. Frost `ink-hi`.
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xe9, 0xed, 0xf2);
/// Secondary text, menu items, palette-chip labels. Frost `ink-mid`.
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xb2, 0xb9, 0xc2);
// `ink-lo` (tertiary text) and the aurora accents (green/violet/amber) exist in the handoff but are
// added when a later theming step first uses one, keeping this file to the colours in play.

// --- canvas: the frosted icy node-graph surface (LIGHT; the one light region in the dark chrome) ---
/// The frosted canvas fill. A solid stand-in for the handoff's `canvas-a -> canvas-b` gradient (the
/// gradient is a later polish); the midpoint of those two tokens.
pub const CANVAS_BASE: Color32 = Color32::from_rgb(0xb2, 0xbe, 0xc8);

// --- node cards (light, floating on the frosted canvas) ---
/// Node card body. The handoff frosts it with a backdrop blur, which egui cannot do, so this is a
/// near-solid light fill instead. Frost `node-bg`.
pub const NODE_BG: Color32 = Color32::from_rgb(0xe0, 0xe6, 0xec);
/// Node header strip. Frost `node-head`.
pub const NODE_HEAD: Color32 = Color32::from_rgb(0xc9, 0xd3, 0xdc);
/// Node border and internal divider (1px). Frost `node-line`.
pub const NODE_LINE: Color32 = Color32::from_rgb(0x9d, 0xae, 0xbc);
/// Node title text, dark on the light card. Frost `node-ink`.
pub const NODE_INK: Color32 = Color32::from_rgb(0x37, 0x3f, 0x4b);
/// Port labels and carets, a lighter dark ink. Frost `node-ink-mid`.
pub const NODE_INK_MID: Color32 = Color32::from_rgb(0x5d, 0x66, 0x74);

// --- accents: aurora "splashes", all share L~=0.70 C~=0.13, hue varies ---
/// Primary accent: wires, Build button, active tab, selection, focus. Frost `acc-cyan`.
pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(0x1f, 0xa0, 0xc4);
/// Wires and pin fills. The same glacial cyan as the primary accent (one data type, one colour).
pub const ACCENT_FROST: Color32 = Color32::from_rgb(0x1f, 0xa0, 0xc4);

// --- semantic (used sparingly) ---
// Bright, saturated, and separated in lightness so a red/green colour-blind reader can still tell
// them apart: success is a light spring green, error a bright rose-red (a hint of magenta, which
// carries blue and reads apart from green), warning a bright amber. Never rely on red-vs-green
// alone; these back up a shape/label cue.
/// Solved / valid state.
pub const SUCCESS: Color32 = Color32::from_rgb(0x24, 0xe0, 0xa0);
/// Warnings.
pub const WARNING: Color32 = Color32::from_rgb(0xff, 0xbf, 0x47);
/// Errors.
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x4d, 0x6d);
/// Text & marquee selection fill: the primary cyan mixed down into the chrome so selected text
/// stays legible against it.
pub const SELECTION: Color32 = Color32::from_rgb(0x2f, 0x59, 0x6a);

/// The Ymir Frost dark-chrome [`Visuals`], built by mapping the palette onto egui's slots.
///
/// Starts from egui's dark visuals so unmapped details (shadows, corner radii, spacing) keep
/// sensible defaults, then overrides the colour-bearing slots:
/// - **panels** read `chrome-1`, **menus/popovers** `chrome-2` with a `chrome-line` border, so a
///   menu reads as raised above a panel;
/// - **text** is the three-level ink ramp: primary labels `ink-hi`, resting controls `ink-mid`,
///   brightening to `ink-hi` on hover/press;
/// - **accents** (cyan) appear on selection, focus, and the pressed border, keeping surfaces neutral.
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    // Surfaces.
    v.panel_fill = BG_SURFACE;
    v.window_fill = BG_RAISED;
    v.window_stroke = Stroke::new(1.0, LINE_STRONG);
    v.extreme_bg_color = BG_ABYSS; // text-edit and deepest backgrounds
    v.faint_bg_color = BG_BASE; // alternating rows, faint fills
    v.code_bg_color = BG_BASE;

    // Accents, used sparingly.
    v.hyperlink_color = ACCENT_PRIMARY;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0, ACCENT_PRIMARY);
    v.warn_fg_color = WARNING;
    v.error_fg_color = ERROR;

    // Widget states. Non-interactive labels carry primary ink on a soft divider; a resting control
    // sits on the raised chrome with secondary ink and a soft border, then brightens its fill, text,
    // and border on hover, and takes an accent border when pressed.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG_SURFACE;
    w.noninteractive.weak_bg_fill = BG_SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.inactive.bg_fill = BG_RAISED;
    w.inactive.weak_bg_fill = BG_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);

    w.hovered.bg_fill = BG_HOVER;
    w.hovered.weak_bg_fill = BG_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.active.bg_fill = BG_HOVER;
    w.active.weak_bg_fill = BG_HOVER;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT_PRIMARY);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    w.open.bg_fill = BG_RAISED;
    w.open.weak_bg_fill = BG_RAISED;
    w.open.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    w.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    v
}
