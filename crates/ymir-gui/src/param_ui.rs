//! The parameter inspector (GUI step 5, issue #6): maps a node's `ParamSpec`
//! schema to editor widgets with no per-node code.
//!
//! The schema-to-widget mapping ([`widget_for`]) and value resolution
//! ([`current_value`]) are pure and unit-tested; only [`edit`] touches egui. Edits
//! are written back to the canonical graph by the caller via `Graph::set_params`.

use eframe::egui;
use ymir_core::{ParamKind, ParamSpec, ParamValue, Params, Unit};

use crate::preview::Histogram;

/// The editor widget a parameter kind maps to. Derived purely from the schema, so
/// the mapping is unit-testable without egui.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Widget {
    /// A value field over `[min, max]` for a float: typed exactly, or scrubbed with the magnitude
    /// ruler. Shown with the unit as a suffix when it has one.
    Quantity {
        min: f64,
        max: f64,
        unit: Option<Unit>,
    },
    /// A drag value over `[min, max]` for an integer.
    IntDrag { min: i64, max: i64 },
    /// A checkbox for a boolean.
    Checkbox,
    /// A single-line text field.
    Text,
    /// A filesystem-path text field with a Browse button (a native file picker).
    Path,
    /// A dropdown over a fixed set of option ids.
    Dropdown { options: &'static [&'static str] },
    /// A visual transfer-curve editor.
    CurveEditor,
    /// A colour swatch that opens a picker.
    ColorPicker,
    /// A kind this build cannot edit yet. `ParamKind` is `#[non_exhaustive]`, so a
    /// future kind degrades to a read-only display rather than risk corrupting a
    /// value it does not understand.
    ReadOnly,
}

/// Maps a parameter schema to its editor widget. Takes the whole spec, since a
/// float's widget depends on whether it carries a unit (an open quantity edits as a
/// value field, a bare ratio as a slider).
pub(crate) fn widget_for(spec: &ParamSpec) -> Widget {
    match &spec.kind {
        // Every float is a value box. There used to be a slider under bounded, unit-less ones,
        // and it was the weaker of the two controls at everything: it cannot show a unit, it
        // cannot reach a value finer than a pixel, and its range decides its precision, so a wide
        // one is unusable and a narrow one wastes the row. The box types an exact value and the
        // magnitude ruler scrubs one across orders of magnitude, which is what the slider was
        // reached for and did worse.
        ParamKind::Float { min, max } => Widget::Quantity {
            min: *min,
            max: *max,
            unit: spec.unit,
        },
        ParamKind::Int { min, max } => Widget::IntDrag {
            min: *min,
            max: *max,
        },
        ParamKind::Bool => Widget::Checkbox,
        ParamKind::Text => Widget::Text,
        ParamKind::Path => Widget::Path,
        ParamKind::Enum { options } => Widget::Dropdown { options },
        ParamKind::Curve => Widget::CurveEditor,
        ParamKind::Color => Widget::ColorPicker,
        // ParamKind is #[non_exhaustive]; an unknown future kind degrades, never
        // panics. This is graceful degradation, not a swallowed case.
        _ => Widget::ReadOnly,
    }
}

/// The effective value of a parameter for a node: the value the node has set, or
/// the schema default when it has not set one.
pub(crate) fn current_value(params: &Params, spec: &ParamSpec) -> ParamValue {
    params
        .get(&spec.name)
        .cloned()
        .unwrap_or_else(|| spec.default.clone())
}

/// The display suffix for a unit, including a leading space (egui draws it abutting
/// the number). Prose lives here in the GUI, never in the schema.
fn unit_suffix(unit: Unit) -> &'static str {
    match unit {
        Unit::Meters => " m",
        Unit::Degrees => "°",
    }
}

/// A short human display of a value, for the read-only fallback.
pub(crate) fn value_text(value: &ParamValue) -> String {
    match value {
        ParamValue::Float(v) => format!("{v}"),
        ParamValue::Int(v) => format!("{v}"),
        ParamValue::Bool(v) => format!("{v}"),
        ParamValue::Text(v) => v.clone(),
        ParamValue::Curve(c) => format!("curve ({} points)", c.points().len()),
        ParamValue::Strokes(s) => format!("painted ({} strokes)", s.len()),
        // The source, not the number it computes: the number is visible in the result, and what
        // a reader cannot otherwise recover is what produced it.
        ParamValue::Expr(source) => format!("= {source}"),
        // Hex, because that is how a colour is written down everywhere else a user will meet
        // this one: a picker, a manifest, an engine's material settings.
        ParamValue::Color(rgb) => {
            let [r, g, b] = srgb_bytes(*rgb);
            format!("#{r:02X}{g:02X}{b:02X}")
        }
    }
}

/// A stored colour as the 0-255 channels egui works in.
///
/// The stored value is sRGB in `[0, 1]` (see [`ParamValue::Color`]), so this is a scale and a
/// round, not a colour-space conversion. Rounding rather than truncating keeps a value that
/// round-trips through the picker from drifting down a step each time.
fn srgb_bytes(rgb: [f64; 3]) -> [u8; 3] {
    rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// The inverse of [`srgb_bytes`].
fn srgb_from_bytes(bytes: [u8; 3]) -> [f64; 3] {
    bytes.map(|c| f64::from(c) / 255.0)
}

/// Fixed width of a parameter row's value box.
const VALUE_W: f32 = 54.0;

/// What a parameter row needs beyond its own schema and value.
///
/// Grouped rather than passed loose because these arrive together, are all about the row's
/// surroundings rather than the parameter itself, and a row that wants one usually wants another.
pub(crate) struct RowContext<'a> {
    /// The node the parameter belongs to, for keying per-row state that must not follow a
    /// widget's position in the panel.
    pub node: u64,
    /// What a computed value currently works out to, or `None` when it did not resolve or is not
    /// computed at all.
    pub computed: Option<f64>,
    /// The input distribution, drawn behind a curve or levels editor.
    pub histogram: Option<&'a Histogram>,
    /// The node's committed parameters, its declared schema, and the world settings: what a
    /// half-typed expression is resolved against so the row can say what it currently means
    /// before it is committed.
    pub resolve_against: Option<&'a DraftEnv<'a>>,
}

/// What a draft expression is checked against.
///
/// The check goes through the engine's own resolver rather than a copy of the rule in the GUI.
/// The resolver builds the variable environment itself, so a second implementation here would
/// drift and start accepting names the engine rejects, or the reverse, which is worse than no
/// check at all.
pub(crate) struct DraftEnv<'a> {
    /// The node's committed parameters, with the draft substituted over the one being edited.
    pub params: &'a Params,
    /// The node's declared schema, so a sibling nobody has edited is still a name.
    pub schema: &'a [ParamSpec],
    /// What the expression may name beyond the node itself: the world settings, and the
    /// enclosing authored node's parameters when the node sits inside one.
    pub scope: ymir_core::resolve::Scope,
    /// The node's type id, for the error the resolver reports.
    pub type_id: &'static str,
}

/// What a draft expression currently means: the number it computes, or why it does not.
///
/// Pure, so the interesting part is testable without egui.
fn draft_status(draft: &str, param: &str, env: &DraftEnv<'_>) -> Result<f64, String> {
    let mut probe = env.params.clone();
    probe.insert(
        param,
        ParamValue::Expr(draft.trim().trim_start_matches('=').into()),
    );
    match ymir_core::resolve::resolve_params(&probe, env.schema, &env.scope, env.type_id) {
        Ok(Some(resolved)) => Ok(resolved.get_f64(param, f64::NAN)),
        // Unreachable: an expression was just inserted, so there is always one to resolve.
        Ok(None) => Err("nothing to resolve".to_owned()),
        Err(err) => Err(err.to_string()),
    }
}

/// Where the "focus this parameter's expression field" flag lives between the frame that opens
/// the editor and the frame that draws it.
fn expr_focus_id(node: u64, name: &str) -> egui::Id {
    egui::Id::new(("param-expr-focus", node, name))
}

/// Whether `=` was pressed while the value box had focus.
///
/// The editor opens on the keystroke rather than when the box is committed. Waiting for the
/// commit meant typing the expression blind in a seventy-point box and only then being handed the
/// full-width field and the live check, which is the wrong way round: the help arrived after the
/// part that needed it.
fn opened_expression(ui: &egui::Ui, value: &egui::Response) -> bool {
    value.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Equals))
}

/// Where the "this parameter is being written as an expression" flag lives.
///
/// Editor state, not a value. Pressing `=` opens the field and stores *nothing*: the parameter
/// keeps whatever it had until something is committed. That is what lets the field open empty.
/// Seeding it with the old number instead, which is what an immediate `Expr` would have to do to
/// avoid failing the node on an expression of zero characters, meant the number came back even
/// when you had just cleared the box to be rid of it.
fn expr_editing_id(node: u64, name: &str) -> egui::Id {
    egui::Id::new(("param-expr-editing", node, name))
}

/// Whether this parameter is currently being written as an expression.
fn editing_expression(ui: &egui::Ui, node: u64, name: &str) -> bool {
    ui.data(|d| d.get_temp::<bool>(expr_editing_id(node, name)))
        .unwrap_or(false)
}

/// Opens the expression field on this parameter, changing no value.
fn open_expression(ui: &egui::Ui, node: u64, name: &str) {
    ui.data_mut(|d| {
        d.insert_temp(expr_editing_id(node, name), true);
        d.insert_temp(expr_focus_id(node, name), true);
    });
}

/// Closes the expression field, whether it committed something or was abandoned.
fn close_expression(ui: &egui::Ui, node: u64, name: &str) {
    ui.data_mut(|d| d.remove::<bool>(expr_editing_id(node, name)));
}

/// Catches a value box's typed text when it starts an expression rather than a number.
///
/// A numeric field never needs a literal `=`, so the prefix is free to mean this, and it is the
/// convention anyone who has met a spreadsheet already has. It is a side channel because egui's
/// parser can only answer with a number, and this input is deliberately not one: the parser
/// stashes the source, declines to produce a value, and the caller turns it into an edit.
///
/// Only offered on float rows. An expression resolves to a float, so accepting one on an integer
/// parameter would leave that parameter holding a value its own typed read cannot get back.
#[derive(Default)]
struct ExprCapture(std::cell::RefCell<Option<String>>);

impl ExprCapture {
    /// Parses `text` as a number, or stashes it as an expression when it opens with `=`.
    ///
    /// The number path matches egui's own default parser rather than a plain `parse`: whitespace
    /// anywhere is ignored (thousands separators), and the typographic minus reads as a minus.
    /// Diverging from that would silently make some values untypeable in a box that used to take
    /// them.
    fn parse(&self, text: &str) -> Option<f64> {
        let trimmed = text.trim();
        if let Some(rest) = trimmed.strip_prefix('=') {
            let source = rest.trim();
            // A bare `=` is someone part-way through typing, not an empty expression.
            if !source.is_empty() {
                *self.0.borrow_mut() = Some(source.to_owned());
            }
            return None;
        }
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '−' { '-' } else { c })
            .collect();
        cleaned.parse().ok()
    }

    /// The stashed expression, if the last parse caught one.
    fn take(self) -> Option<String> {
        self.0.into_inner()
    }
}

/// A small faint revert glyph, shown when a value is off its default; clicking it resets that one
/// parameter. Returns its response.
fn reset_icon(ui: &mut egui::Ui) -> egui::Response {
    // Allocate the same height as the value field beside it (its `add_sized` uses this same
    // interact size), so the glyph appearing when a value goes off-default cannot grow the row.
    let h = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, h), egui::Sense::click());
    let color = if resp.hovered() {
        crate::theme::TEXT_SECONDARY
    } else {
        crate::theme::TEXT_TERTIARY
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
        egui::FontId::proportional(12.0),
        color,
    );
    resp.on_hover_text("Reset to default")
}

/// Clamps or wraps a scrubbed value to its bounds: `wrap` (e.g. 360 for degrees) rolls the value
/// around that period; otherwise `clamp` bounds it to `[lo, hi]`; with neither it passes through.
pub(crate) fn finalize_value(v: f64, wrap: Option<f64>, clamp: Option<(f64, f64)>) -> f64 {
    if let Some(period) = wrap {
        v.rem_euclid(period)
    } else if let Some((lo, hi)) = clamp {
        v.clamp(lo, hi)
    } else {
        v
    }
}

/// Whether a degree-valued quantity should wrap around the circle rather than clamp to its range.
/// Only a *direction* wraps: an azimuth or rotation declares the full turn (`0..360`), so scrubbing
/// past either end rolls over. A *bounded* angle (a slope grade, a beach face, a spread) declares a
/// sub-circle range, and a value past its max is not a smaller angle the other way round but simply
/// impossible (a slope cannot exceed 90 degrees), so it clamps to the author's declared max like a
/// metric quantity. Keyed on the declared span, so the schema's range is what decides.
fn angle_wraps(min: f64, max: f64) -> bool {
    max - min >= 360.0
}

/// Display precision (decimal places) for a metric quantity: always thousandths of a metre.
///
/// Fixed rather than derived. It used to come from the parameter's declared maximum, which meant
/// every length in the app was edited in whole metres, because all of them declare a 100 km ceiling
/// and none had ever narrowed it (#352). A typed 2.5 rounded to 3, and on a small world a blur
/// radius could only be 0, 1 or 2.
///
/// Deliberately not derived from the cell size or the world extent either. Precision that shifts
/// when the build resolution changes is invisible coupling: the same project would present
/// differently between a preview and a final build, with nothing on screen to explain why.
///
/// Three rather than two, to match the unitless floats and the magnitude ruler's finest column. A
/// value that can be scrubbed to a thousandth has to be able to show one; the ruler reaches as far
/// below one as above it, so the display does too.
///
/// `max_decimals` trims trailing zeros, so a whole value still reads `5 m` and only a fractional one
/// shows its decimals. The scrub speed is a separate question, answered by [`metric_scrub_speed`].
const METRIC_DECIMALS: usize = 3;

/// Scrub step for a metric quantity, from the value being dragged.
///
/// Proportional to where the value already is, not to its declared range. Dragging near 8 m moves in
/// small steps; dragging near 5 km moves in large ones. So one rule suits a blur radius of a couple
/// of metres and a coastal reach of kilometres, and the parameter's declared ceiling never enters
/// into it.
///
/// It was briefly range-proportional, which was worse than what it replaced. Every metric parameter
/// in `ymir-nodes` declares a 100 km ceiling, so a thousandth of the range came to 100 m per step,
/// and reaching 8 m by dragging was harder than reaching 100 km. A declared maximum is an outer
/// bound, not a statement about the values anyone works at, so it is the wrong thing to scale by.
///
/// Kept separate from the display precision, which stays fixed at [`METRIC_DECIMALS`]. Deriving
/// those two from one number is what forced whole-metre display before #352.
fn metric_scrub_speed(value: f64) -> f64 {
    // A fiftieth of the current value, so roughly fifty pixels of drag doubles it. Floored so a
    // value sitting at zero is not frozen there.
    (value.abs() / 50.0).max(0.05)
}

/// Layers infinite (wrapping) scrub onto a numeric value box. While `resp` is dragged, the value is
/// driven by *raw* mouse motion (immune to the screen edge, unlike egui's position-based DragValue),
/// and the cursor is locked in place and hidden so the pointer never runs off the edge and returns
/// where it started on release. `finalize` clamps or wraps the result. Returns whether the value
/// moved this frame.
///
/// The caller builds the DragValue with `speed(0.0)` so its own edge-limited drag is inert and only
/// this scrub moves the value; DragValue still supplies display, click-to-type, and formatting.
///
/// `CursorGrab::Locked` is what pointer-lock supports on Wayland, Windows, and macOS; it pins the
/// pointer natively while still delivering relative motion (winit rejects a bare cursor *warp* on
/// Wayland — "cursor position can be set only for locked cursor"). On X11 Locked is unsupported and
/// winit logs it once per drag; the scrub still runs from raw XInput2 motion, only the pointer is
/// not pinned.
pub(crate) fn scrub_drag(
    ui: &egui::Ui,
    resp: &egui::Response,
    value: &mut f64,
    speed: f64,
    finalize: impl Fn(f64) -> f64,
) -> bool {
    if resp.drag_started() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::Locked,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }
    let mut changed = false;
    if resp.dragged() {
        // Raw device motion ignores the screen edge; fall back to the position delta only if the
        // integration does not supply it (then the scrub is bounded, as it was before).
        let dx = match ui.input(|i| i.pointer.motion()) {
            Some(m) => f64::from(m.x),
            None => f64::from(resp.drag_delta().x),
        };
        if dx != 0.0 {
            *value = finalize(*value + dx * speed);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                egui::viewport::CursorGrab::None,
            ));
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    }
    changed
}

/// A custom horizontal slider filling the available width: a 4px deep track, an accent fill up to
/// the handle, and a white handle with a ring. Drag or click anywhere on it to set. Marks its
/// response changed only when the value actually moves.
pub(crate) fn slider(
    ui: &mut egui::Ui,
    value: &mut f64,
    min: f64,
    max: f64,
    log: bool,
) -> egui::Response {
    let w = ui.available_width().max(24.0);
    let (rect, mut resp) =
        ui.allocate_exact_size(egui::vec2(w, 14.0), egui::Sense::click_and_drag());
    let r = 5.5_f32;
    let usable = (w - 2.0 * r).max(1.0);
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(w, 4.0));
    let t = to_t(*value, min, max, log).clamp(0.0, 1.0) as f32;
    let hx = rect.left() + r + t * usable;
    let cy = rect.center().y;
    let painter = ui.painter();
    painter.rect_filled(track, 2.0, crate::theme::BG_ABYSS);
    painter.rect_filled(
        egui::Rect::from_min_max(track.left_top(), egui::pos2(hx, track.bottom())),
        2.0,
        crate::theme::ACCENT_PRIMARY,
    );
    painter.circle_filled(egui::pos2(hx, cy), r, crate::theme::TEXT_PRIMARY);
    painter.circle_stroke(
        egui::pos2(hx, cy),
        r,
        egui::Stroke::new(2.0, crate::theme::BG_SURFACE),
    );
    let before = *value;
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let nt = (f64::from(pos.x - rect.left() - r) / f64::from(usable)).clamp(0.0, 1.0);
        *value = from_t(nt, min, max, log);
    }
    if *value != before {
        resp.mark_changed();
    }
    resp
}

/// Normalizes a value to `0..1` across `[min, max]`, log-scaled when `log` and the range is positive.
fn to_t(x: f64, min: f64, max: f64, log: bool) -> f64 {
    if log && min > 0.0 && max > 0.0 {
        (x.ln() - min.ln()) / (max.ln() - min.ln())
    } else if (max - min).abs() < f64::EPSILON {
        0.0
    } else {
        (x - min) / (max - min)
    }
}

/// The inverse of [`to_t`]: a `0..1` position back to a value in `[min, max]`.
fn from_t(t: f64, min: f64, max: f64, log: bool) -> f64 {
    if log && min > 0.0 && max > 0.0 {
        (min.ln() + t * (max.ln() - min.ln())).exp()
    } else {
        min + t * (max - min)
    }
}

/// A node parameter's row label: the resolved display name in muted mono, with the parameter's
/// one-line description shown as a hover tooltip when the catalog has one. Label and description
/// come from [`ymir_nodes::resolve_param`], so the inspector, the tooltip, and the generated
/// reference all read the same strings.
pub(crate) fn param_label(ui: &mut egui::Ui, type_id: &str, name: &str) {
    let resolved = ymir_nodes::resolve_param(type_id, name);
    let resp = render_label(ui, &resolved.label);
    if let Some(desc) = resolved.description {
        resp.on_hover_text(desc);
    }
}

/// Renders an already-display label (the frame and colour-picker rows) in the same muted mono
/// style as a parameter label, without catalog resolution or a tooltip.
pub(crate) fn plain_label(ui: &mut egui::Ui, text: &str) {
    render_label(ui, text);
}

/// The row for a parameter whose value is computed: the same label grammar as any other, then an
/// `=` marking it computed and the number it currently works out to.
///
/// The number is what the row shows, because that is the question being asked while working; the
/// source is one hover away and is what the field edits. The marker is a glyph rather than a
/// colour, which sidesteps rather than works around the constraint that a state must not be
/// distinguished by hue alone: every tool that ships expressions signals them by colour and then
/// has to solve that problem.
///
/// `computed` is `None` when the expression did not resolve, which is a real state a user needs
/// to see rather than an impossible one: the row then shows `=!` and says why on hover.
///
/// Two lines, like a slider row: the label, marker and number above, the expression across the
/// full panel width beneath. A value box is about ten monospace characters wide, and
/// `world_height * sea_level` is twenty-four, so the expression cannot share that line with
/// anything.
///
/// Returns the committed value: an expression, or a plain number when what was typed is one, so
/// there is a way back out of being computed.
fn expression_row(
    ui: &mut egui::Ui,
    type_id: &str,
    ctx: &RowContext<'_>,
    spec: &ParamSpec,
    source: &str,
    computed: Option<f64>,
) -> Option<ParamValue> {
    let name = spec.name.as_str();
    let node = ctx.node;
    // The draft is keyed by node and parameter, never by the widget's auto-generated id. The
    // params pane does not push an id per node, so row ids are positional: a draft keyed that way
    // would follow the position and leak into whatever parameter sat there for the next node.
    let draft_id = egui::Id::new(("param-expr-draft", node, name));
    let mut text: String = ui
        .data(|d| d.get_temp::<String>(draft_id))
        .unwrap_or_else(|| source.to_owned());
    let mut result = None;

    ui.horizontal(|ui| {
        param_label(ui, type_id, name);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Back to a stored value, the same affordance every other row has. Without it the
            // only escape from a computed parameter was resetting the whole node.
            if reset_icon(ui).clicked() {
                ui.data_mut(|d| d.remove::<String>(draft_id));
                close_expression(ui, node, name);
                result = Some(spec.default.clone());
            }
            let (glyph, color) = match computed {
                Some(_) => ("=", crate::theme::ACCENT_PRIMARY),
                None => ("=!", crate::theme::ERROR),
            };
            ui.label(egui::RichText::new(glyph).monospace().color(color).strong());
            if let Some(value) = computed {
                ui.label(
                    egui::RichText::new(format!("{value:.3}"))
                        .monospace()
                        .color(crate::theme::TEXT_SECONDARY),
                );
            }
        });
    });

    let edited = ui.add(
        egui::TextEdit::singleline(&mut text)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY),
    );
    let hover = match computed {
        Some(_) => format!("= {source}"),
        None => format!("= {source}\n\nthis does not resolve, so the node cannot run"),
    };
    let edited = edited.on_hover_text(hover);
    // Opened by `=` in the value box a frame ago: hand the caret straight over, so the keystroke
    // that asks for an expression and the typing of one are a single gesture.
    let focus_id = expr_focus_id(node, name);
    if ui.data(|d| d.get_temp::<bool>(focus_id)).unwrap_or(false) {
        edited.request_focus();
        ui.data_mut(|d| d.remove::<bool>(focus_id));
    }

    // Held as a draft while typing and committed once, on Enter or when focus leaves. Writing
    // through on every keystroke commits one expression per character, all but the last of them
    // broken, and each one re-evaluates the graph and blanks the preview. The numeric rows beside
    // this one already defer for the same reason.
    if edited.changed() {
        ui.data_mut(|d| d.insert_temp(draft_id, text.clone()));
    }
    // While the field has focus, say what the draft currently means. The check is the engine's
    // own resolver, so the number shown is the number the node would run on and the message is
    // the compiler's, naming the mistake rather than reporting that there was one. Compiling is
    // pure and cheap (one node's parameters), so doing it per frame while focused is fine.
    // An empty field means "put the default back", not a broken expression, so it says nothing
    // rather than reporting that the parser ran out of input.
    if edited.has_focus()
        && !text.trim().is_empty()
        && let Some(env) = ctx.resolve_against
    {
        match draft_status(&text, name, env) {
            Ok(value) => {
                ui.label(
                    egui::RichText::new(format!("= {value:.4}"))
                        .monospace()
                        .small()
                        .color(crate::theme::TEXT_SECONDARY),
                );
            }
            Err(message) => {
                // Amber, not rose: while you are still typing this is unfinished rather than
                // broken. The text carries the meaning either way, so nothing rests on the hue.
                ui.label(
                    egui::RichText::new(message)
                        .small()
                        .color(crate::theme::WARNING),
                );
            }
        }
    }

    if edited.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(draft_id));
        // Whether it committed or was abandoned, the field is done: a parameter that ends up
        // holding an expression renders as one from its own value, and one that does not goes
        // back to its value box.
        close_expression(ui, node, name);
        // Escape abandons the edit and puts the value back, matching every other parameter row.
        if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && text != source {
            result = Some(committed_value(&text, spec));
        }
    }
    result
}

/// What committed text means: a plain number where it is one, an expression otherwise.
///
/// A parameter has to be able to stop being computed. Typing a bare number is the obvious way to
/// say so, and storing `Expr("5")` instead would leave it computed forever with nothing to
/// indicate why. Empty means the same as reset, since there is no expression left to evaluate.
fn committed_value(text: &str, spec: &ParamSpec) -> ParamValue {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return spec.default.clone();
    }
    // A leading `=` is the gesture that opens this field, not part of the expression: the grammar
    // has no `=` and would reject it. It is stripped rather than refused because someone who
    // typed it once to get here will type it again, and storing `="foo"` verbatim would leave the
    // field that exists for expressions rejecting the very syntax that creates them.
    let trimmed = trimmed.strip_prefix('=').map_or(trimmed, str::trim_start);
    if trimmed.is_empty() {
        return spec.default.clone();
    }
    match trimmed.parse::<f64>() {
        Ok(number) => ParamValue::Float(number),
        Err(_) => ParamValue::Expr(trimmed.to_owned()),
    }
}

/// Draws a row label in the shared muted-mono style and returns its response, so a caller can
/// attach a hover tooltip.
fn render_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        egui::RichText::new(text)
            .family(egui::FontFamily::Monospace)
            .size(12.0)
            .color(crate::theme::TEXT_SECONDARY),
    )
}

/// A 34x18 pill toggle: an accent track with the knob right when on, a raised track with the knob
/// left when off. Returns its response (click to flip).
pub(crate) fn toggle(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 18.0), egui::Sense::click());
    let track = if on {
        crate::theme::ACCENT_PRIMARY
    } else {
        crate::theme::BG_HOVER
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 9.0, track);
    let knob_x = if on {
        rect.right() - 9.0
    } else {
        rect.left() + 9.0
    };
    painter.circle_filled(
        egui::pos2(knob_x, rect.center().y),
        6.5,
        crate::theme::TEXT_PRIMARY,
    );
    resp.on_hover_text(if on { "On" } else { "Off" })
}

/// An integer stepper: a deep field with a minus button, the value in the centre, and a plus button.
/// The buttons step by one within `[min, max]`; the centre is the same value box the float params
/// use, so an integer can be scrubbed with an infinite cursor-locked drag or clicked and typed
/// (#246). A wide range (fbm's seed runs to `i32::MAX`) is unreachable one click at a time, and
/// scrubbing at any range-proportional speed would be either useless there or twitchy on a range
/// like octaves' 1..12, so the scrub is a flat unit per pixel and typing covers the long jumps.
/// Returns whether the value changed.
fn stepper(ui: &mut egui::Ui, value: &mut i64, min: i64, max: i64) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(104.0, 24.0), egui::Sense::hover());
    let btn_w = 26.0;
    let minus = egui::Rect::from_min_size(rect.left_top(), egui::vec2(btn_w, rect.height()));
    let plus = egui::Rect::from_min_size(
        egui::pos2(rect.right() - btn_w, rect.top()),
        egui::vec2(btn_w, rect.height()),
    );
    // Seed the button ids from this allocation's own (auto-unique) id, not `ui.id()`, which several
    // stepper rows share: otherwise every stepper's minus/plus collide (egui's red id-clash boxes).
    let minus_resp = ui.interact(minus, resp.id.with("minus"), egui::Sense::click());
    let plus_resp = ui.interact(plus, resp.id.with("plus"), egui::Sense::click());
    let mut changed = false;
    if minus_resp.clicked() && *value > min {
        *value -= 1;
        changed = true;
    }
    if plus_resp.clicked() && *value < max {
        *value += 1;
        changed = true;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, crate::theme::BG_ABYSS);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, crate::theme::LINE),
        egui::StrokeKind::Inside,
    );
    let glyph = |r: &egui::Response, active: bool| {
        if !active {
            crate::theme::TEXT_TERTIARY
        } else if r.hovered() {
            crate::theme::TEXT_PRIMARY
        } else {
            crate::theme::TEXT_SECONDARY
        }
    };
    painter.text(
        minus.center(),
        egui::Align2::CENTER_CENTER,
        "\u{2212}",
        egui::FontId::proportional(15.0),
        glyph(&minus_resp, *value > min),
    );
    painter.text(
        plus.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(15.0),
        glyph(&plus_resp, *value < max),
    );
    // The centre of the field is a value box rather than painted text: a DragValue supplies
    // click-to-type and formatting, and its own edge-limited drag is made inert (speed 0) so the
    // wrapping-free infinite scrub below is the only thing that moves it, exactly as the float
    // params do. It is drawn without a frame so the field painted above stays the visible control.
    let centre = egui::Rect::from_min_max(
        egui::pos2(minus.right(), rect.top()),
        egui::pos2(plus.left(), rect.bottom()),
    );
    // Seeded from this allocation's own (auto-unique) id for the same reason the buttons are: the
    // stepper rows share a parent ui, so an auto-generated id would clash between rows and two
    // steppers would share one text-edit state.
    let value_resp = ui
        .push_id(resp.id, |ui| {
            let v = ui.visuals_mut();
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.weak_bg_fill = egui::Color32::TRANSPARENT;
                w.bg_stroke = egui::Stroke::NONE;
                w.fg_stroke.color = crate::theme::TEXT_PRIMARY;
            }
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::new(13.0, egui::FontFamily::Monospace),
            );
            ui.put(
                centre,
                egui::DragValue::new(value)
                    .speed(0.0)
                    .range(min as f64..=max as f64),
            )
        })
        .inner;
    changed |= value_resp.changed();
    // The scrub runs on a float mirror (that is what `scrub_drag` drives) and lands back on the
    // integer, keeping the sub-step remainder across frames in this widget's own temp state. That
    // carry is what makes a slow drag work: rounding every frame and re-seeding from the stored
    // integer would throw away a third of a pixel of motion each time, so the value would sit
    // still until the pointer moved fast enough and then jump.
    // The magnitude ruler (#358) on a float mirror, landing back on the integer. The fractional
    // columns are unusable here and the ruler draws them recessed and struck through rather than
    // dropping them, so a magnitude always sits in the same place whatever the parameter's kind.
    let mut mirror = *value as f64;
    if crate::magnitude::ruler_scrub(
        ui,
        &value_resp,
        &mut mirror,
        (min as f64, max as f64),
        crate::magnitude::Resolution::Integer,
        "",
    ) {
        let settled = mirror.round().clamp(min as f64, max as f64) as i64;
        if settled != *value {
            *value = settled;
            changed = true;
        }
    }
    changed
}

/// Renders the editor for one parameter and returns the new value if the user
/// changed it this frame, or `None` otherwise. The widget choice is [`widget_for`];
/// this is the thin egui-touching layer over that pure mapping. A value whose
/// variant disagrees with its kind (or an unknown kind) falls through to a
/// read-only display, so a mismatch is shown, never edited wrongly.
pub(crate) fn edit(
    ui: &mut egui::Ui,
    type_id: &str,
    spec: &ParamSpec,
    current: &ParamValue,
    ctx: &RowContext<'_>,
    popout: &mut bool,
) -> Option<ParamValue> {
    let name = spec.name.as_str();
    let (computed, histogram) = (ctx.computed, ctx.histogram);
    // A computed parameter is edited as its expression, not through the widget its kind would
    // otherwise get: what a slider or a value field would show is a result nothing can drag.
    // Shown for a parameter that already holds an expression, and for one being written: the
    // second has nothing stored yet, which is the point.
    match current {
        ParamValue::Expr(source) => {
            return expression_row(ui, type_id, ctx, spec, source, computed);
        }
        _ if editing_expression(ui, ctx.node, name) => {
            return expression_row(ui, type_id, ctx, spec, "", None);
        }
        _ => {}
    }
    match (widget_for(spec), current) {
        (Widget::Quantity { min, max, unit }, ParamValue::Float(v)) => {
            // Same row grammar as the other params (mono label left, control right), but a
            // type/scrub value field with the unit as a suffix and no slider beneath: a wide
            // world-unit range is too coarse to slide. A direction (a full-circle angle) wraps, so
            // dragging below 0 rolls to 359.9 (a small counter-clockwise turn); a bounded angle (a
            // slope grade) and every metric quantity clamp to their declared range instead, so an
            // impossible value (a slope past 90) can never be scrubbed or typed in.
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Float(d) => *d,
                _ => x,
            };
            let degrees = unit == Some(Unit::Degrees);
            // No unit means no suffix: an open range without a nameable unit still edits as a value.
            let suffix = unit.map_or("", unit_suffix);
            let wraps = degrees && angle_wraps(min, max);
            let mut result = None;
            let capture = ExprCapture::default();
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Decimals are set explicitly (DragValue derives them from `speed`, which is 0
                    // here, so otherwise it shows full float precision). Degrees always show a fixed
                    // tenth; a metric length shows a precision that suits its declared range (whole
                    // metres for a wide reach, up to a tenth for a small length like a berm crest).
                    // Display precision and scrub speed are answered separately: a length shows
                    // hundredths whatever its range, and scrubs by a step suited to that range.
                    let speed = if degrees { 0.5 } else { metric_scrub_speed(x) };
                    // DragValue supplies display, click-to-type, and formatting; its own drag is
                    // made inert (speed 0) so the infinite scrub below is the only thing that moves
                    // the value.
                    // Parsed once, on Enter or when focus leaves, rather than on every
                    // keystroke. Typing `=beach_width * 2` has to arrive whole: parsed per
                    // keystroke, `=b` would convert the parameter to an expression the moment the
                    // second character landed and pull the field out from under the typing.
                    let mut drag = egui::DragValue::new(&mut x)
                        .suffix(suffix)
                        .speed(0.0)
                        .update_while_editing(false)
                        .custom_parser(|text| capture.parse(text));
                    drag = if degrees {
                        drag.fixed_decimals(1)
                    } else {
                        drag.max_decimals(METRIC_DECIMALS)
                    };
                    // Everything but a wrapping direction clamps click-to-type to the declared
                    // range; a wrapping angle has no meaningful bound to type against (it rolls).
                    if !wraps {
                        drag = drag.range(min..=max);
                    }
                    let value = ui.add_sized(
                        egui::vec2(VALUE_W + 16.0, ui.spacing().interact_size.y),
                        drag,
                    );
                    let scrubbed = if wraps {
                        scrub_drag(ui, &value, &mut x, speed, |v| {
                            finalize_value(v, Some(360.0), None)
                        })
                    } else {
                        crate::magnitude::ruler_scrub(
                            ui,
                            &value,
                            &mut x,
                            (min, max),
                            crate::magnitude::Resolution::Continuous,
                            suffix,
                        )
                    };
                    if value.changed() || scrubbed {
                        let stored = if wraps { x.rem_euclid(360.0) } else { x };
                        result = Some(ParamValue::Float(stored));
                    }
                    if opened_expression(ui, &value) {
                        open_expression(ui, ctx.node, name);
                    }
                    if (x - default).abs() > f64::EPSILON && reset_icon(ui).clicked() {
                        x = default;
                        result = Some(ParamValue::Float(default));
                    }
                });
            });
            // An expression wins over whatever number the box was showing: it is the newer
            // instruction, and the box's value never changed.
            if let Some(source) = capture.take() {
                result = Some(ParamValue::Expr(source));
            }
            result
        }
        (Widget::IntDrag { min, max }, ParamValue::Int(v)) => {
            let mut x = *v;
            let default = match &spec.default {
                ParamValue::Int(d) => *d,
                _ => x,
            };
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if stepper(ui, &mut x, min, max) {
                        result = Some(ParamValue::Int(x));
                    }
                    // The same revert affordance the float rows carry, so an integer is not the
                    // one parameter kind you cannot put back (#246).
                    if x != default && reset_icon(ui).clicked() {
                        result = Some(ParamValue::Int(default));
                    }
                });
            });
            result
        }
        (Widget::Checkbox, ParamValue::Bool(v)) => {
            let mut x = *v;
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if toggle(ui, x).clicked() {
                        x = !x;
                        result = Some(ParamValue::Bool(x));
                    }
                });
            });
            result
        }
        (Widget::Text, ParamValue::Text(v)) => {
            let mut x = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut x)
                            .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                            .text_color(crate::theme::TEXT_PRIMARY)
                            .background_color(crate::theme::BG_ABYSS)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    result = Some(ParamValue::Text(x.clone()));
                }
            });
            result
        }
        (Widget::Path, ParamValue::Text(v)) => {
            // A path text field plus a Browse button opening the native file picker. The
            // text stays editable (paste or type a path); Browse fills it in.
            let mut x = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                if ui.button("Browse\u{2026}").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Image", &["png"])
                        .pick_file()
                {
                    x = path.display().to_string();
                    result = Some(ParamValue::Text(x.clone()));
                }
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut x)
                            .font(egui::FontSelection::Style(egui::TextStyle::Monospace))
                            .text_color(crate::theme::TEXT_PRIMARY)
                            .background_color(crate::theme::BG_ABYSS)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                {
                    result = Some(ParamValue::Text(x.clone()));
                }
            });
            result
        }
        (Widget::Dropdown { options }, ParamValue::Text(v)) => {
            let mut selected = v.clone();
            let mut result = None;
            ui.horizontal(|ui| {
                param_label(ui, type_id, name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button = ui.button(format!(
                        "{}   {}",
                        selected,
                        egui_phosphor::regular::CARET_DOWN
                    ));
                    egui::Popup::menu(&button).show(|ui| {
                        ui.set_min_width(button.rect.width());
                        for option in options {
                            if ui
                                .selectable_label(selected.as_str() == *option, *option)
                                .clicked()
                            {
                                selected = (*option).to_string();
                                result = Some(ParamValue::Text(selected.clone()));
                                ui.close();
                            }
                        }
                    });
                });
            });
            result
        }
        (Widget::ColorPicker, ParamValue::Color(rgb)) => {
            let mut bytes = srgb_bytes(*rgb);
            let mut changed = None;
            ui.horizontal(|ui| {
                ui.label(name);
                // egui's own picker, so the interaction is the one a user already knows. It
                // works in 8-bit sRGB, which is what the stored value scales to exactly.
                if ui.color_edit_button_srgb(&mut bytes).changed() {
                    changed = Some(ParamValue::Color(srgb_from_bytes(bytes)));
                }
                ui.weak(value_text(current));
            });
            changed
        }
        (Widget::CurveEditor, ParamValue::Curve(curve)) => {
            ui.label(name);
            let result = crate::curve_edit::curve_editor(ui, curve, histogram);
            *popout = result.popout_clicked;
            result.changed.map(ParamValue::Curve)
        }
        _ => {
            ui.horizontal(|ui| {
                ui.label(name);
                ui.weak(value_text(current));
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: ParamKind, default: ParamValue) -> ParamSpec {
        ParamSpec::new("p", kind, default)
    }

    #[test]
    fn drawing_the_stepper_leaves_the_value_alone() {
        // Drawing must not rewrite the value: a param edit is what marks a project modified, so a
        // widget that nudged its own value on the first frame would report a phantom edit.
        let mut value = 65_323_i64;
        egui::__run_test_ui(|ui| {
            assert!(!stepper(ui, &mut value, 0, i64::from(i32::MAX)));
        });
        assert_eq!(value, 65_323);
    }

    #[test]
    fn finalize_value_wraps_or_clamps() {
        // Degrees wrap around the period: below 0 rolls up, at/above the period rolls down.
        assert_eq!(finalize_value(-0.1, Some(360.0), None), 359.9);
        assert!((finalize_value(370.0, Some(360.0), None) - 10.0).abs() < 1e-9);
        assert_eq!(finalize_value(45.0, Some(360.0), None), 45.0);
        // Metric clamps to its range.
        assert_eq!(finalize_value(-5.0, None, Some((0.0, 100.0))), 0.0);
        assert_eq!(finalize_value(150.0, None, Some((0.0, 100.0))), 100.0);
        assert_eq!(finalize_value(30.0, None, Some((0.0, 100.0))), 30.0);
        // With neither, the value passes through.
        assert_eq!(finalize_value(12.5, None, None), 12.5);
    }

    #[test]
    fn reset_glyph_matches_the_value_field_height() {
        // The reset glyph shares a horizontal row with the value field, so if it allocated a taller
        // box the row would grow the moment a value went off its default (#142). Both must use the
        // same interact height.
        let mut glyph_h = 0.0;
        let mut field_h = 0.0;
        egui::__run_test_ui(|ui| {
            field_h = ui.spacing().interact_size.y;
            glyph_h = reset_icon(ui).rect.height();
        });
        assert!(field_h > 0.0, "interact height should be non-zero");
        assert_eq!(
            glyph_h, field_h,
            "the reset glyph must be the same height as the value field, or the row grows when it appears"
        );
    }

    #[test]
    fn every_float_is_a_value_box_whatever_its_range_or_unit() {
        // There is one float control now. A slider could not show a unit, could not reach a value
        // finer than a pixel, and let its range decide its precision; the box plus the magnitude
        // ruler does all of that better.
        let cases = [
            spec(
                ParamKind::Float { min: 0.0, max: 1.0 },
                ParamValue::Float(0.0),
            ),
            spec(
                ParamKind::Float {
                    min: 1.0,
                    max: 64.0,
                },
                ParamValue::Float(2.0),
            )
            .logarithmic(),
            spec(
                ParamKind::Float {
                    min: 0.0,
                    max: 100.0,
                },
                ParamValue::Float(8.0),
            )
            .with_unit(Unit::Meters),
            spec(
                ParamKind::Float {
                    min: -100_000.0,
                    max: 100_000.0,
                },
                ParamValue::Float(0.0),
            ),
        ];
        for spec in cases {
            assert!(
                matches!(widget_for(&spec), Widget::Quantity { .. }),
                "{:?} should edit as a value box",
                spec.kind
            );
        }
    }

    #[test]
    fn a_float_carries_its_declared_bounds_and_unit_to_its_widget() {
        let length = spec(
            ParamKind::Float {
                min: 0.0,
                max: 100.0,
            },
            ParamValue::Float(8.0),
        )
        .with_unit(Unit::Meters);
        assert_eq!(
            widget_for(&length),
            Widget::Quantity {
                min: 0.0,
                max: 100.0,
                unit: Some(Unit::Meters)
            }
        );
    }

    #[test]
    fn each_kind_maps_to_its_widget() {
        assert_eq!(
            widget_for(&spec(
                ParamKind::Float { min: 0.0, max: 1.0 },
                ParamValue::Float(0.0)
            )),
            Widget::Quantity {
                min: 0.0,
                max: 1.0,
                unit: None,
            }
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Int { min: 1, max: 12 },
                ParamValue::Int(1)
            )),
            Widget::IntDrag { min: 1, max: 12 }
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Bool, ParamValue::Bool(false))),
            Widget::Checkbox
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Text, ParamValue::Text(String::new()))),
            Widget::Text
        );
        assert_eq!(
            widget_for(&spec(ParamKind::Path, ParamValue::Text(String::new()))),
            Widget::Path
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Enum {
                    options: &["add", "mix"]
                },
                ParamValue::Text("add".into())
            )),
            Widget::Dropdown {
                options: &["add", "mix"]
            }
        );
        assert_eq!(
            widget_for(&spec(
                ParamKind::Curve,
                ParamValue::Curve(ymir_core::Curve::identity())
            )),
            Widget::CurveEditor
        );
    }

    #[test]
    fn a_unit_bearing_float_is_a_quantity_not_a_slider() {
        // A world-unit length edits as a quantity (value field + unit), where a bare
        // ratio over the same kind would be a slider.
        let length = spec(
            ParamKind::Float {
                min: 0.0,
                max: 100.0,
            },
            ParamValue::Float(8.0),
        )
        .with_unit(Unit::Meters);
        assert_eq!(
            widget_for(&length),
            Widget::Quantity {
                min: 0.0,
                max: 100.0,
                unit: Some(Unit::Meters)
            }
        );
    }

    #[test]
    fn a_committed_bare_number_stops_the_parameter_being_computed() {
        // Without this there is no way out: clearing the field and typing 5 stored `Expr("5")`,
        // computed forever with nothing to say why.
        let spec = spec(
            ParamKind::Float {
                min: 0.0,
                max: 10.0,
            },
            ParamValue::Float(1.0),
        );
        assert_eq!(committed_value("5", &spec), ParamValue::Float(5.0));
        assert_eq!(committed_value("  2.5 ", &spec), ParamValue::Float(2.5));
    }

    #[test]
    fn a_committed_expression_stays_an_expression() {
        let spec = spec(
            ParamKind::Float {
                min: 0.0,
                max: 10.0,
            },
            ParamValue::Float(1.0),
        );
        assert_eq!(
            committed_value("beach_width * 0.15", &spec),
            ParamValue::Expr("beach_width * 0.15".into())
        );
    }

    /// A node with one stored width and one declared amplitude, for the draft checks.
    fn draft_schema() -> Vec<ParamSpec> {
        vec![
            ParamSpec::new(
                "beach_width",
                ParamKind::Float {
                    min: 0.0,
                    max: 1000.0,
                },
                ParamValue::Float(20.0),
            ),
            ParamSpec::new(
                "amplitude",
                ParamKind::Float {
                    min: 0.0,
                    max: 1000.0,
                },
                ParamValue::Float(3.0),
            ),
        ]
    }

    fn draft_env<'a>(schema: &'a [ParamSpec], params: &'a Params) -> DraftEnv<'a> {
        DraftEnv {
            params,
            schema,
            scope: ymir_core::resolve::Scope {
                world: ymir_core::resolve::WorldGlobals {
                    sea_level: 0.25,
                    world_height: 512.0,
                    world_extent: 1000.0,
                },
                ..Default::default()
            },
            type_id: "test.node",
        }
    }

    #[test]
    fn a_valid_draft_reports_the_number_it_computes() {
        let schema = draft_schema();
        let params = Params::new();
        let env = draft_env(&schema, &params);
        assert_eq!(
            draft_status("beach_width * 0.15", "amplitude", &env),
            Ok(3.0)
        );
        // Reads the world settings too, and tolerates the character that opened the field.
        assert_eq!(draft_status("=sea_level", "amplitude", &env), Ok(0.25));
    }

    #[test]
    fn a_draft_naming_something_unknown_reports_the_compilers_own_message() {
        // The point of showing it at all: it names the mistake rather than saying there was one.
        let schema = draft_schema();
        let params = Params::new();
        let env = draft_env(&schema, &params);
        let err = draft_status("beach_widht * 2", "amplitude", &env).expect_err("unknown name");
        assert!(err.contains("beach_widht"), "message was {err:?}");
    }

    #[test]
    fn a_draft_that_would_close_a_loop_reports_it_before_it_is_committed() {
        let schema = draft_schema();
        // `beach_width` already reads `amplitude`, so a draft reading back is a cycle.
        let params = Params::new().with("beach_width", ParamValue::Expr("amplitude + 1".into()));
        let env = draft_env(&schema, &params);
        let err = draft_status("beach_width + 1", "amplitude", &env).expect_err("a cycle");
        assert!(err.contains("cycle"), "message was {err:?}");
    }

    #[test]
    fn a_draft_is_judged_against_the_committed_values() {
        let schema = draft_schema();
        let params = Params::new().with("beach_width", ParamValue::Float(40.0));
        let env = draft_env(&schema, &params);
        // The stored 40 wins over the declared 20, so the draft reports what the node would run.
        assert_eq!(
            draft_status("beach_width * 0.15", "amplitude", &env),
            Ok(6.0)
        );
    }

    #[test]
    fn the_expression_field_tolerates_the_character_that_opened_it() {
        // `=` activates the field; it is not part of the grammar. Someone who typed it once to
        // get here will type it again, and keeping it would store an expression that cannot
        // compile in the one field that exists for editing expressions.
        let spec = spec(
            ParamKind::Float {
                min: 0.0,
                max: 10.0,
            },
            ParamValue::Float(1.0),
        );
        let expected = ParamValue::Expr("world_height * sea_level".into());
        assert_eq!(committed_value("world_height * sea_level", &spec), expected);
        assert_eq!(
            committed_value("=world_height * sea_level", &spec),
            expected
        );
        assert_eq!(
            committed_value("= world_height * sea_level", &spec),
            expected
        );
        // And it does not stop a bare number ending the computed state.
        assert_eq!(committed_value("=5", &spec), ParamValue::Float(5.0));
        // A lone `=` has nothing after it, so it means the same as clearing the field.
        assert_eq!(committed_value("=", &spec), ParamValue::Float(1.0));
    }

    #[test]
    fn clearing_the_field_puts_the_default_back() {
        let spec = spec(
            ParamKind::Float {
                min: 0.0,
                max: 10.0,
            },
            ParamValue::Float(1.0),
        );
        assert_eq!(committed_value("", &spec), ParamValue::Float(1.0));
        assert_eq!(committed_value("   ", &spec), ParamValue::Float(1.0));
    }

    #[test]
    fn a_plain_number_parses_as_a_number() {
        let capture = ExprCapture::default();
        assert_eq!(capture.parse("12.5"), Some(12.5));
        assert_eq!(capture.take(), None, "nothing was an expression");
    }

    #[test]
    fn the_number_path_still_accepts_what_egui_accepted() {
        // Diverging from egui's own parser would silently make some values untypeable in a box
        // that used to take them: spaced thousands, and the typographic minus.
        let capture = ExprCapture::default();
        assert_eq!(capture.parse("1 000"), Some(1000.0));
        assert_eq!(capture.parse("\u{2212}4"), Some(-4.0));
    }

    #[test]
    fn a_leading_equals_is_caught_as_an_expression_and_yields_no_number() {
        let capture = ExprCapture::default();
        assert_eq!(
            capture.parse("=beach_width * 0.15"),
            None,
            "an expression is not a number, so the box must not take a value from it"
        );
        assert_eq!(capture.take(), Some("beach_width * 0.15".to_string()));
    }

    #[test]
    fn surrounding_space_does_not_hide_the_prefix() {
        let capture = ExprCapture::default();
        assert_eq!(capture.parse("  = sea_level * world_height  "), None);
        assert_eq!(capture.take(), Some("sea_level * world_height".to_string()));
    }

    #[test]
    fn a_bare_equals_is_someone_still_typing_not_an_empty_expression() {
        let capture = ExprCapture::default();
        assert_eq!(capture.parse("="), None);
        assert_eq!(
            capture.take(),
            None,
            "committing an empty expression would break the node for a half-typed keystroke"
        );
    }

    #[test]
    fn an_unparseable_number_is_neither_a_value_nor_an_expression() {
        let capture = ExprCapture::default();
        assert_eq!(capture.parse("twelve"), None);
        assert_eq!(capture.take(), None);
    }

    #[test]
    fn only_a_full_circle_angle_wraps() {
        // A direction declares the whole turn and rolls over when scrubbed past either end.
        assert!(angle_wraps(0.0, 360.0));
        // A bounded angle (a slope grade, a beach face, a spread) declares a sub-circle range, so a
        // value past its max is impossible, not a smaller angle the other way, and it clamps.
        assert!(!angle_wraps(0.0, 80.0)); // coastal beach/bluff grade
        assert!(!angle_wraps(0.0, 90.0)); // thermal talus / slope threshold
        assert!(!angle_wraps(1.0, 180.0)); // an angular spread
    }

    #[test]
    fn a_length_shows_thousandths_whatever_its_range() {
        // The precision used to come from the declared max, and every length declares a 100 km
        // ceiling, so all of them edited in whole metres and a typed 2.5 rounded to 3 (#352). It is
        // now fixed, so the range cannot take the decimals away.
        //
        // Three rather than two, to match the unitless floats and the magnitude ruler's finest
        // column: a value that can be scrubbed to a thousandth has to be able to show one.
        assert_eq!(METRIC_DECIMALS, 3);
    }

    #[test]
    fn a_typed_fraction_of_a_metre_survives() {
        // The point of #352: a blur radius of 2.5 m has to stay 2.5 m. Nothing on the commit path
        // may round it to the scrub step or to a whole metre.
        let blur_range = Some((0.0, 100_000.0));
        assert!((finalize_value(2.5, None, blur_range) - 2.5).abs() < f64::EPSILON);
        assert!((finalize_value(0.25, None, blur_range) - 0.25).abs() < f64::EPSILON);
        // Still clamped to the declared range, which is the one thing that may change a value.
        assert!((finalize_value(-3.0, None, blur_range) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn scrub_speed_follows_the_value_not_the_declared_range() {
        // The bug this replaces: the step was a thousandth of the declared range, and every metric
        // parameter declares a 100 km ceiling, so everything scrubbed at 100 m a step and a depth
        // of 8 m was unreachable by dragging.
        //
        // Scaling by the value instead means a parameter sitting at 8 moves in small steps whatever
        // its declared maximum, and one sitting at 5000 moves in large ones.
        let small = metric_scrub_speed(8.0);
        let large = metric_scrub_speed(5000.0);
        assert!(
            small < 1.0,
            "8 m should scrub finer than a metre, got {small}"
        );
        assert!(
            large > 10.0,
            "5 km should scrub in tens of metres, got {large}"
        );
        assert!(large > small * 100.0, "the step must scale with the value");
        // Zero is floored rather than frozen: a value starting at zero must still be draggable.
        let zero = metric_scrub_speed(0.0);
        assert!(
            zero > 0.0 && zero <= 0.1,
            "zero should still move, got {zero}"
        );
        // Negative values scrub at the same rate as their positive twin.
        assert!((metric_scrub_speed(-500.0) - metric_scrub_speed(500.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn current_value_prefers_set_value_then_falls_back_to_default() {
        let spec = ParamSpec::new(
            "frequency",
            ParamKind::Float { min: 0.0, max: 8.0 },
            ParamValue::Float(2.0),
        );
        // Absent: the schema default.
        assert_eq!(current_value(&Params::new(), &spec), ParamValue::Float(2.0));
        // Present: the node's set value wins.
        let params = Params::new().with("frequency", ParamValue::Float(3.5));
        assert_eq!(current_value(&params, &spec), ParamValue::Float(3.5));
    }

    #[test]
    fn value_text_renders_each_variant() {
        assert_eq!(value_text(&ParamValue::Int(7)), "7");
        assert_eq!(value_text(&ParamValue::Bool(true)), "true");
        assert_eq!(value_text(&ParamValue::Text("ridge".into())), "ridge");
    }

    #[test]
    fn an_edit_writes_through_to_the_graph() {
        // Mirrors what params_pane does on a changed value, minus the egui widget:
        // resolve the current value, write the edit back with set_params, then
        // verify the change landed in the canonical graph.
        use ymir_core::{Graph, registry};

        let mut graph = Graph::new();
        let id = graph.add_op(registry::make("generator.fbm").expect("fbm"), Params::new());
        let spec = graph.spec(id).expect("spec");
        let pspec = spec
            .params
            .iter()
            .find(|p| matches!(p.kind, ParamKind::Float { .. }))
            .expect("fbm has a float parameter");

        // Before any edit, the effective value is the schema default.
        assert_eq!(
            current_value(&graph.params(id).cloned().unwrap_or_default(), pspec),
            pspec.default
        );

        // Apply an edit the way the pane does, then write it back.
        let mut params = graph.params(id).cloned().unwrap_or_default();
        params.insert(pspec.name.clone(), ParamValue::Float(0.123));
        graph.set_params(id, params).expect("set_params");

        // The graph now holds the edited value.
        assert_eq!(
            current_value(&graph.params(id).cloned().unwrap_or_default(), pspec),
            ParamValue::Float(0.123)
        );
    }

    #[test]
    fn a_color_spec_gets_a_picker() {
        assert_eq!(
            widget_for(&spec(ParamKind::Color, ParamValue::Color([0.0, 0.0, 0.0]))),
            Widget::ColorPicker,
            "the widget comes from the spec, so a Material node needs no per-node UI code"
        );
    }

    #[test]
    fn a_color_survives_a_trip_through_the_pickers_byte_channels() {
        // egui edits in 8-bit sRGB while the value is stored as floats, so every edit is a
        // round trip through bytes. A value that came from the picker must come back unchanged,
        // or a colour would drift a step every time the inspector redrew it.
        for bytes in [[0, 0, 0], [255, 255, 255], [1, 128, 254], [77, 33, 200]] {
            assert_eq!(srgb_bytes(srgb_from_bytes(bytes)), bytes);
        }
    }

    #[test]
    fn a_color_reads_as_hex() {
        // Hex is how a colour is written everywhere else a user will meet this one: a picker,
        // an exported manifest, an engine's material settings.
        assert_eq!(value_text(&ParamValue::Color([1.0, 0.0, 0.0])), "#FF0000");
        assert_eq!(value_text(&ParamValue::Color([0.0, 0.0, 0.0])), "#000000");
        assert_eq!(
            value_text(&ParamValue::Color([2.0, -1.0, 0.5])),
            "#FF0080",
            "an out-of-range channel clamps rather than wrapping to a nonsense colour"
        );
    }
}
