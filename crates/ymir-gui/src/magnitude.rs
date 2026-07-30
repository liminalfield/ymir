//! The magnitude ruler's model: which magnitude a column means, and how a tick moves a value (#358).
//!
//! Pure arithmetic and state, no egui. The overlay that draws it is a separate concern; everything
//! here is testable without a window.
//!
//! # Why the columns are fixed
//!
//! Editing a number across orders of magnitude needs four things at once: reach, precision,
//! predictability, and no hidden modes. Four earlier attempts each traded one away, and they failed
//! for the same underlying reason: each **inferred** the step size, from the declared range (#352),
//! from the current value (#357), or from an acceleration curve. An inferred step cannot be
//! predictable, because the same gesture does different things depending on where you started.
//!
//! So the step is not inferred. It is chosen from a fixed set of magnitudes, the same set for every
//! parameter in the application. Two consequences worth stating, because both were live alternatives:
//!
//! The set is **not** derived from the parameter's declared range. That was tried and was the worst
//! of the four attempts: most length parameters declare a 100 km ceiling because it was a safe number
//! to write, not because anyone works at 100 km, so a range-derived step made 8 m harder to reach
//! than 100 km. A range-derived *column span* would repeat it, handing the widest columns to values
//! nobody wants.
//!
//! And the set does not vary between parameters. Identical columns everywhere is what lets the
//! gesture transfer from a blur radius to a wavelength without relearning, which is most of why a
//! ruler beats pointing at the digits of the number itself.
//!
//! # A tick adds, it does not snap
//!
//! Ticking used to round to the grid the chosen magnitude defines. It does not any more, and the
//! reason is worth keeping: you arrive at a column having just set the finer digits yourself, so
//! snapping discards them. Moving from hundredths to tenths on a value of 0.35 turned it into 0.4 and
//! threw away the 0.05 that had been dialled in on purpose. Plain addition also means every tick moves
//! by exactly the step, where snapping made the first one move by whatever the remainder was.

/// The magnitudes a column can carry, coarsest first.
///
/// Symmetric about one: three decades above, three below. The symmetry is the argument. A float is
/// displayed to three decimals, so stopping the ruler at hundredths showed a digit that could be read
/// and not reached, while the coarse end went to thousands. Reaching as far down as up removes the
/// asymmetry rather than explaining it.
///
/// Seven columns spanning a millionfold: enough that any parameter in the application has usable
/// ones, few enough that each is a generous target. Inert columns are faded rather than removed, so
/// their positions never move.
pub(crate) const MAGNITUDES: [f64; 7] = [1000.0, 100.0, 10.0, 1.0, 0.1, 0.01, 0.001];

/// Below this, a value counts as zero when asking which magnitude leads it.
const NEAR_ZERO: f64 = 1e-6;

/// What a value can be stepped by, given the parameter's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// Any magnitude, including the fractional columns.
    Continuous,
    /// Whole numbers only, so the fractional columns cannot apply.
    Integer,
}

/// A gesture in progress.
///
/// One state, not two. An earlier version had an aim phase and a locked scrub phase, because the
/// pointer was grabbed and hidden once scrubbing began and so could not be aimed with. That lock was
/// inherited from the older continuous scrub, which needs it: reaching a distant value there means
/// dragging a long way, and a grabbed pointer can drag past the screen edge.
///
/// The ruler does not need it, and keeping it was the mistake. Reaching a distant value here means
/// choosing a coarser column: ten ticks of the thousands column is ten thousand, in a hundred points
/// of travel. Nothing wants an unbounded drag, so nothing wants a grabbed pointer, so the pointer
/// stays visible and free and the column is always simply the one it is over. No modes, no invisible
/// state, and a mis-aim is corrected by moving the pointer rather than by starting again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Gesture {
    /// The value when the gesture began: the Escape target, and what history compares against.
    pub baseline: f64,
    /// Vertical points travelled since the last tick, carried between frames so a slow drag still
    /// moves rather than having its motion rounded away each frame.
    pub carry: f32,
    /// Whether a deliberate vertical stroke has begun, so ticking is live.
    ///
    /// Choosing a column and changing the value are one continuous gesture, so they are separated in
    /// time rather than by a mode: crossing the ruler sideways aims and moves nothing, a vertical
    /// stroke arms and ticks, and a further sideways move returns to aiming.
    pub armed: bool,
    /// Vertical travel accumulated toward arming.
    pub vertical_run: f32,
    /// Where the ruler's columns start, fixed at the press and held.
    ///
    /// Held rather than recomputed because it depends on the value's leading magnitude, and the
    /// gesture changes the value. Recomputing it made the overlay drift sideways as the number grew,
    /// putting a different column under a stationary cursor.
    pub ruler_left: f32,
}

/// The magnitude a column carries.
pub(crate) fn step_of(column: usize) -> f64 {
    MAGNITUDES.get(column).copied().unwrap_or(1.0)
}

/// Which magnitudes can actually do something for a parameter, as indices into [`MAGNITUDES`].
///
/// Two things make a column inert. A fractional magnitude cannot apply to an integer parameter. And a
/// magnitude larger than the parameter's whole range can only saturate it: `1000` on an octave count
/// of 1 to 12 jumps to the maximum and then does nothing.
///
/// Every column is still *drawn*, at its fixed position, with the inert ones faded. Removing them was
/// tried and moves the surviving columns between parameters, which costs the one property that makes
/// a fixed ruler worth having: a magnitude is always in the same place. Striking them through was also
/// tried and read as clutter. Fading says "nothing here" without moving anything.
///
/// Note what the range does and does not decide. It marks which members are inert; it never sets the
/// spacing or the origin. Deriving the spacing from a declared range is what made #352 unusable.
pub(crate) fn usable_columns(resolution: Resolution, (low, high): (f64, f64)) -> Vec<usize> {
    let span = if high > low { high - low } else { f64::MAX };
    (0..MAGNITUDES.len())
        .filter(|&c| {
            let step = step_of(c);
            let whole = !matches!(resolution, Resolution::Integer) || step >= 1.0;
            whole && step <= span
        })
        .collect()
}

/// The nearest column that can do something, for a cursor `offset` points from the ruler's left edge.
///
/// The layout is always all six magnitudes at fixed positions, so the offset maps straight to a
/// column. Two adjustments make the gesture forgiving rather than fussy: the offset is clamped to the
/// ruler, because being off the end of a ruler means nothing when the columns are the only choice;
/// and an inert column snaps to the nearest one that is not, so pointing at a faded box does
/// something sensible instead of nothing at all.
pub(crate) fn nearest_usable(offset: f32, column_width: f32, usable: &[usize]) -> Option<usize> {
    if usable.is_empty() || column_width <= 0.0 {
        return None;
    }
    let raw = (offset / column_width)
        .floor()
        .clamp(0.0, (MAGNITUDES.len() - 1) as f32) as usize;
    usable.iter().copied().min_by_key(|&c| c.abs_diff(raw))
}

/// Which column the magnitude already in play occupies, so the ruler can open with it under the
/// pointer.
///
/// A value of zero has no leading magnitude, so it answers with the ones column: the middle of the
/// ruler, and where a value is most likely to grow from.
pub(crate) fn leading_column(value: f64) -> usize {
    let magnitude = value.abs();
    if magnitude < NEAR_ZERO {
        return MAGNITUDES.iter().position(|m| *m == 1.0).unwrap_or(3);
    }
    MAGNITUDES
        .iter()
        .position(|m| *m <= magnitude)
        .unwrap_or(MAGNITUDES.len() - 1)
}

/// One tick of `step` away from `value`: plain addition.
///
/// It used to snap to the grid the step defines, so ticking up from 2.47 by tenths gave 2.5 rather
/// than 2.57. The argument was that choosing a column declares the resolution you care about. That
/// argument assumed you arrive at a column with a value someone else chose. In use you arrive having
/// just set the finer digits yourself: set 0.35 on the hundredths column, move to tenths for a coarser
/// adjustment, and snapping discarded the 0.05 you had deliberately dialled in.
///
/// Addition is also strictly more predictable, which was the design's whole purpose. Under snapping
/// the first tick moved by whatever the remainder happened to be and every later one by the step;
/// now every tick moves by the step, always.
///
/// The result is rounded to six decimals, which is finer than anything the inspector displays. That
/// is only to keep repeated addition from accumulating float noise: `0.35 + 0.1` is
/// `0.45000000000000007` in binary floating point, and a few hundred ticks of that would show.
pub(crate) fn tick(value: f64, step: f64, up: bool) -> f64 {
    if step <= 0.0 {
        return value;
    }
    let next = if up { value + step } else { value - step };
    (next * 1e6).round() / 1e6
}

/// Updates the arming state from one frame of pointer motion.
///
/// Aiming at a column and changing the value are one continuous gesture, so they are separated in
/// time rather than by a mode. A sideways frame returns to aiming and moves nothing, which is what
/// makes crossing the ruler to another column safe; sustained vertical travel arms ticking.
///
/// Two thresholds rather than one: re-aiming is deliberately harder to trip than arming, so the
/// sideways jitter of a hand holding a vertical stroke does not keep disarming it.
///
/// Returns the new armed flag and the vertical travel still accumulated toward arming.
fn aim(armed: bool, vertical_run: f32, motion: egui::Vec2) -> (bool, f32) {
    if motion.x.abs() > motion.y.abs() && motion.x.abs() > REAIM_PX {
        return (false, 0.0);
    }
    if armed {
        return (true, vertical_run);
    }
    let run = vertical_run + motion.y.abs();
    (run >= ARM_PX, run)
}

/// Turns vertical drag into ticks: the new value, and the pixels left over to carry.
///
/// `delta` is the vertical motion since the last frame, in points, positive downward as the window
/// reports it. Upward drag raises the value, which is the convention every other scrub in the
/// application already follows.
///
/// The leftover matters. Rounding the motion away each frame would make a slow drag move nothing at
/// all, because no single frame crosses the threshold. Carrying it means the gesture responds to total
/// distance travelled rather than to frame rate.
pub(crate) fn advance(
    value: f64,
    step: f64,
    delta: f32,
    carry: f32,
    pixels_per_tick: f32,
) -> (f64, f32) {
    if pixels_per_tick <= 0.0 {
        return (value, 0.0);
    }
    // Up is negative in window coordinates, and up raises the value.
    let travelled = carry - delta;
    let ticks = (travelled / pixels_per_tick).trunc();
    let mut out = value;
    let count = ticks.abs() as u32;
    for _ in 0..count {
        out = tick(out, step, ticks > 0.0);
    }
    (out, travelled - ticks * pixels_per_tick)
}

/// The value a tick would produce, for showing the user before they commit to it.
///
/// Both directions, so the overlay can preview above and below the column and make the snap rule
/// evident before anything moves.
pub(crate) fn preview(value: f64, step: f64, bounds: (f64, f64)) -> (f64, f64) {
    (
        clamp(tick(value, step, true), bounds),
        clamp(tick(value, step, false), bounds),
    )
}

/// Holds a value inside the parameter's declared bounds.
pub(crate) fn clamp(value: f64, (low, high): (f64, f64)) -> f64 {
    if low > high {
        // A reversed range is the schema's problem, not something to panic over.
        return value;
    }
    value.clamp(low, high)
}

/// Whether a value has run into either end of its range, so the overlay can say which direction is
/// still available.
pub(crate) fn at_bound(value: f64, (low, high): (f64, f64)) -> Option<Bound> {
    if low > high {
        return None;
    }
    if value <= low {
        Some(Bound::Low)
    } else if value >= high {
        Some(Bound::High)
    } else {
        None
    }
}

/// Which end of its range a value has reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// At the declared minimum.
    Low,
    /// At the declared maximum.
    High,
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

use eframe::egui;

/// Column width in points. Never below 20: a generous target is the point of the design, and it is
/// what a ruler has that pointing at the digits of a number does not.
const COLUMN_W: f32 = 34.0;
/// Ruler row height.
const RULER_H: f32 = 40.0;
/// Readout row: the value, the step, and any clamp tag.
const READOUT_H: f32 = 26.0;
/// The upward preview's band. Taller than the downward one on purpose: the live cursor sits in this
/// band during aim, so the value is top-aligned to clear it. Growing this band is the right fix if a
/// larger cursor still collides; moving the value sideways is not, because it would stop pointing at
/// its column.
const GHOST_UP_H: f32 = 24.0;
/// The downward preview's band. Nothing occludes it.
const GHOST_DOWN_H: f32 = 18.0;
/// Padding inside the overlay frame.
const PAD: f32 = 8.0;
/// Vertical travel per tick, identical for every parameter and every column.
const PIXELS_PER_TICK: f32 = 10.0;
/// Vertical travel that arms ticking, once a column has been chosen.
///
/// Nothing moves until a deliberate vertical stroke. Attributing every frame to whichever axis
/// dominated it meant a diagonal drift nudged the value while the pointer was still crossing the
/// ruler looking for a column, so aiming and setting were one gesture and interfered.
const ARM_PX: f32 = 6.0;
/// Sideways travel in one frame that returns to aiming, disarming ticking.
///
/// Larger than the jitter of a hand holding a vertical stroke, so scrubbing is not disarmed by
/// noise, and small enough that a deliberate sideways move re-aims at once.
const REAIM_PX: f32 = 3.0;

/// The overlay's full width, frame included. Constant: every magnitude is always drawn.
fn overlay_width() -> f32 {
    COLUMN_W * MAGNITUDES.len() as f32 + PAD * 2.0
}

/// Holds the ruler's left edge inside `screen`, so no part of the overlay leaves the window.
///
/// Returns the left edge of the *columns*, which sits `PAD` inside the frame.
fn clamp_ruler(left: f32, screen: egui::Rect) -> f32 {
    let width = overlay_width();
    if width >= screen.width() {
        // Nothing sensible to do with a window narrower than the overlay; keep it on the left edge
        // rather than pushing it off the other side.
        return screen.left() + PAD;
    }
    let lowest = screen.left() + PAD;
    let highest = screen.right() - width + PAD;
    left.clamp(lowest, highest)
}

/// Runs the magnitude ruler for a value field, returning whether the value moved this frame.
///
/// Shaped to drop into the same place `scrub_drag` occupies, so a row adopts it by swapping one call.
/// The cursor is locked and hidden only on the transition into scrubbing, exactly as the older scrub
/// does, and never during aim: a pinned invisible pointer cannot be aimed with.
pub(crate) fn ruler_scrub(
    ui: &mut egui::Ui,
    resp: &egui::Response,
    value: &mut f64,
    bounds: (f64, f64),
    resolution: Resolution,
    suffix: &str,
) -> bool {
    let id = resp.id.with("magnitude-ruler");
    let mut gesture: Option<Gesture> = ui.data(|d| d.get_temp::<Option<Gesture>>(id)).flatten();

    // Escape abandons the gesture and puts the value back. Checked first, so a cancelled drag cannot
    // also commit on the same frame.
    if let Some(active) = gesture
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
    {
        *value = active.baseline;
        clear(ui, id);
        return true;
    }

    // Opens on press, not on `drag_started`. egui reports a drag only once the pointer has moved past
    // its threshold, so the ruler used to appear only after the gesture had already begun to move,
    // and that first movement could tick the value before its magnitude had even been shown. Holding
    // the button opens the ruler and changes nothing; the value moves when the pointer does.
    let holding = resp.is_pointer_button_down_on() || resp.dragged();
    if holding && gesture.is_none() {
        let press = resp.interact_pointer_pos().unwrap_or(resp.rect.center());
        // The ruler's position is fixed here, once, and held for the rest of the gesture.
        //
        // Recomputing it per frame was a feedback loop with no stable state: ticking changed the
        // value, the value changed which magnitude led it, that moved the ruler under the cursor, a
        // different column came under the pointer, and the step changed. The overlay appeared to
        // wander sideways of its own accord and the value went wherever that took it.
        let left = clamp_ruler(
            press.x - COLUMN_W * (leading_column(*value) as f32 + 0.5),
            ui.ctx().content_rect(),
        );
        gesture = Some(Gesture {
            baseline: *value,
            carry: 0.0,
            armed: false,
            vertical_run: 0.0,
            ruler_left: left,
        });
    }

    let Some(active) = gesture else {
        return false;
    };
    // Alive while the button is held; released or lost, the gesture ends.
    if !holding {
        clear(ui, id);
        return false;
    }

    let usable = usable_columns(resolution, bounds);
    if usable.is_empty() {
        clear(ui, id);
        return false;
    }
    let ruler_left = active.ruler_left;

    // The column is whatever the pointer is over, clamped to the ends rather than falling off them.
    // A narrow ruler is only a couple of columns wide, so an unclamped hit test left dead ground
    // either side where the gesture silently did nothing.
    let cursor = ui
        .input(|i| i.pointer.latest_pos())
        .unwrap_or_else(|| resp.rect.center());
    let column = nearest_usable(cursor.x - ruler_left, COLUMN_W, &usable);

    let mut moved = false;
    let mut carry = active.carry;
    let mut armed = active.armed;
    let mut vertical_run = active.vertical_run;
    if let Some(column) = column {
        // Raw device motion where the platform reports it, the position delta otherwise. Either
        // works now that the pointer is not grabbed.
        let motion = ui
            .input(|i| i.pointer.motion())
            .unwrap_or_else(|| resp.drag_delta());
        (armed, vertical_run) = aim(armed, vertical_run, motion);
        if !armed {
            // Nothing is ticking, so a part-tick left over from an earlier stroke would only surface
            // later as a jump the aiming move never asked for.
            carry = 0.0;
        }
        let dy = if armed { motion.y } else { 0.0 };
        let (next, next_carry) = advance(*value, step_of(column), dy, carry, PIXELS_PER_TICK);
        let held = clamp(next, bounds);
        if (held - *value).abs() > f64::EPSILON {
            *value = held;
            moved = true;
        }
        // Travel that pushes further into a bound is discarded, so backing off responds at once
        // instead of unwinding it. Travel heading *away* is kept, which is the whole fix: clearing
        // the carry whenever the value sat on a bound trapped it there. fbm's frequency has a
        // minimum of 0.25 and a default of 0.25, so it started on its own floor and could not be
        // scrubbed up at all: the accumulator was wiped every frame before it could reach a tick.
        carry = match at_bound(held, bounds) {
            Some(Bound::Low) if next_carry < 0.0 => 0.0,
            Some(Bound::High) if next_carry > 0.0 => 0.0,
            _ => next_carry,
        };
    }

    draw(
        ui, id, resp.rect, ruler_left, &usable, column, armed, *value, bounds, suffix,
    );

    if resp.drag_stopped() && !resp.is_pointer_button_down_on() {
        clear(ui, id);
    } else {
        ui.data_mut(|d| {
            d.insert_temp(
                id,
                Some(Gesture {
                    baseline: active.baseline,
                    carry,
                    armed,
                    vertical_run,
                    ruler_left,
                }),
            );
        });
    }
    moved
}

/// Ends the gesture, clearing its stored state.
///
/// Nothing to hand back: the pointer was never grabbed.
fn clear(ui: &egui::Ui, id: egui::Id) {
    ui.data_mut(|d| {
        d.insert_temp::<Option<Gesture>>(id, None);
        d.insert_temp::<Option<egui::Pos2>>(id.with("press"), None);
    });
}

/// Paints the overlay: readout, both previews, and the ruler.
#[expect(
    clippy::too_many_arguments,
    reason = "one paint call for one overlay; splitting it would thread the same geometry through \
              several signatures without making any of them independently meaningful"
)]
fn draw(
    ui: &egui::Ui,
    id: egui::Id,
    field: egui::Rect,
    ruler_left: f32,
    usable: &[usize],
    active: Option<usize>,
    armed: bool,
    value: f64,
    bounds: (f64, f64),
    suffix: &str,
) {
    let width = overlay_width();
    let height = READOUT_H + GHOST_UP_H + RULER_H + GHOST_DOWN_H + PAD * 2.0;
    // Above the field, clear of it. An earlier version placed the ruler a fixed distance below the
    // *press point*, which put the overlay on top of the field it belongs to; leaving the field
    // visible means the number can be watched changing in place, which is where the eye already is.
    //
    // Above rather than below because the hand is below: a ruler under the field sits where the
    // pointer and the arm already are, and the value being changed is the thing that should not be
    // covered. Flips below only when there is no room above.
    let screen = ui.ctx().content_rect();
    let mut top = field.top() - height - 4.0;
    if top < screen.top() + 8.0 {
        top = field.bottom() + 4.0;
    }
    let origin = egui::pos2(ruler_left - PAD, top);
    let ruler_top = top + PAD + READOUT_H + GHOST_UP_H;

    egui::Area::new(id.with("overlay"))
        .order(egui::Order::Foreground)
        // Never takes input. Interactable by default, an area laid over the field stole the pointer
        // from it on the following frame, so `dragged()` went false and the gesture died in silence:
        // the value would sometimes move and sometimes not, depending only on where the overlay
        // happened to land.
        .interactable(false)
        .fixed_pos(origin)
        .show(ui.ctx(), |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(width, height),
                egui::Sense::focusable_noninteractive(),
            );
            let p = ui.painter();
            p.rect_filled(rect, 4.0, crate::theme::BG_RAISED);
            p.rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, crate::theme::LINE_STRONG),
                egui::StrokeKind::Inside,
            );

            let mono = |size: f32| egui::FontId::monospace(size);
            let step = active.map(step_of);

            let readout_y = rect.top() + PAD + READOUT_H * 0.5;
            p.text(
                egui::pos2(rect.left() + PAD, readout_y),
                egui::Align2::LEFT_CENTER,
                format!("{}{suffix}", trim(value)),
                mono(15.0),
                crate::theme::TEXT_PRIMARY,
            );
            let (step_text, step_ink) = match step {
                // Bright once a vertical stroke has armed ticking, dim while a column is merely
                // chosen. That is the whole difference between the two states, so it carries it.
                Some(s) => (
                    format!("\u{b1}{}", trim(s)),
                    if armed {
                        crate::theme::ACCENT_PRIMARY
                    } else {
                        crate::theme::TEXT_TERTIARY
                    },
                ),
                None => ("\u{2014}".to_string(), crate::theme::TEXT_TERTIARY),
            };
            p.text(
                egui::pos2(rect.right() - PAD, readout_y),
                egui::Align2::RIGHT_CENTER,
                step_text,
                mono(12.0),
                step_ink,
            );

            // The previews sit above and below the active column, so each number is on the side of
            // the axis that produces it and the snap rule is evident before the first tick.
            if let (Some(column), Some(s)) = (active, step) {
                let cx = ruler_left + COLUMN_W * (column as f32 + 0.5);
                let (up, down) = preview(value, s, bounds);
                let bound = at_bound(value, bounds);
                let ghost = |y: f32, v: f64, blocked: bool, align: egui::Align2| {
                    if blocked {
                        p.line_segment(
                            [egui::pos2(cx - 6.0, y), egui::pos2(cx + 6.0, y)],
                            egui::Stroke::new(1.0, crate::theme::TEXT_TERTIARY),
                        );
                    } else {
                        p.text(
                            egui::pos2(cx, y),
                            align,
                            trim(v),
                            mono(11.0),
                            crate::theme::TEXT_SECONDARY,
                        );
                    }
                };
                ghost(
                    ruler_top - GHOST_UP_H * 0.5,
                    up,
                    bound == Some(Bound::High),
                    egui::Align2::CENTER_CENTER,
                );
                ghost(
                    ruler_top + RULER_H + GHOST_DOWN_H * 0.5,
                    down,
                    bound == Some(Bound::Low),
                    egui::Align2::CENTER_CENTER,
                );
            }

            // Every magnitude is drawn, at its fixed position. The inert ones are faded rather than
            // removed or struck through: removing them moves the survivors between parameters and
            // costs the fixed positions that make the ruler worth having, and a strike read as
            // clutter. Fading says "nothing here" while nothing moves.
            for (slot, magnitude) in MAGNITUDES.iter().enumerate() {
                let live = usable.contains(&slot);
                let is_active = active == Some(slot);
                let cell = egui::Rect::from_min_size(
                    egui::pos2(ruler_left + COLUMN_W * slot as f32, ruler_top),
                    egui::vec2(COLUMN_W, RULER_H),
                );
                if is_active {
                    p.rect_filled(cell, 0.0, crate::theme::ACCENT_PRIMARY);
                }
                p.rect_stroke(
                    cell,
                    0.0,
                    egui::Stroke::new(
                        1.0,
                        if live {
                            crate::theme::LINE_STRONG
                        } else {
                            crate::theme::LINE
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                let ink = if is_active {
                    crate::theme::BG_ABYSS
                } else if live {
                    crate::theme::TEXT_SECONDARY
                } else {
                    // Faded well back: readable as a position, plainly not a control.
                    crate::theme::LINE
                };
                p.text(
                    cell.center(),
                    egui::Align2::CENTER_CENTER,
                    trim(*magnitude),
                    mono(13.0),
                    ink,
                );
            }
        });
}

/// A number without trailing zeros, so a column reads `1000` and `0.001` rather than `1000.000`.
///
/// Three decimals, matching what the inspector displays: rendering to two would have shown the
/// finest column as `0.00`.
fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_carries_its_labelled_magnitude() {
        assert_eq!(step_of(0), 1000.0);
        assert_eq!(step_of(3), 1.0);
        assert_eq!(step_of(5), 0.01);
        // Past the end falls back to ones rather than panicking; the caller should not get there, but
        // a wrong index must not take the application down.
        assert_eq!(step_of(99), 1.0);
    }

    #[test]
    fn the_columns_are_the_same_for_every_parameter() {
        // The property the whole design rests on. If this set were derived from a parameter's range,
        // the gesture would not transfer between parameters and pointing at digits would be no worse.
        assert_eq!(MAGNITUDES.len(), 7);
        // Symmetric about one: as far below as above, so no digit can be displayed without being
        // reachable.
        let ones = MAGNITUDES
            .iter()
            .position(|m| *m == 1.0)
            .expect("ones column");
        assert_eq!(
            ones,
            MAGNITUDES.len() - 1 - ones,
            "the ruler is not symmetric about one"
        );
        for pair in MAGNITUDES.windows(2) {
            assert!(
                (pair[0] / pair[1] - 10.0).abs() < 1e-9,
                "columns must be exact decades: {pair:?}"
            );
        }
    }

    #[test]
    fn an_integer_parameter_is_offered_no_fractional_column() {
        let wide = usable_columns(Resolution::Integer, (0.0, 100_000.0));
        for &c in &wide {
            assert!(step_of(c) >= 1.0, "integer offered {}", step_of(c));
        }
        // A continuous parameter over the same range gets all of them.
        assert_eq!(
            usable_columns(Resolution::Continuous, (0.0, 100_000.0)).len(),
            MAGNITUDES.len()
        );
    }

    #[test]
    fn a_tick_adds_the_step_and_keeps_the_finer_digits() {
        // Reported from use, and the reason snapping was removed: 0.35 was set deliberately on the
        // hundredths column, so moving to tenths must add a tenth rather than rounding to 0.4 and
        // discarding the work.
        assert!((tick(0.35, 0.1, true) - 0.45).abs() < 1e-9);
        assert!((tick(0.35, 0.1, false) - 0.25).abs() < 1e-9);
        // The same holds at any scale and any column.
        assert!((tick(2.47, 1.0, true) - 3.47).abs() < 1e-9);
        assert!((tick(1234.0, 1000.0, true) - 2234.0).abs() < 1e-9);
        assert!((tick(0.37, 0.01, true) - 0.38).abs() < 1e-9);
    }

    #[test]
    fn every_tick_moves_by_exactly_the_step() {
        // Predictability, the criterion every earlier attempt failed. Under snapping the first tick
        // moved by whatever the remainder happened to be; now the step is the step.
        for (start, step) in [
            (5.0, 10.0),
            (5000.0, 10.0),
            (0.35, 0.1),
            (0.0, 1.0),
            (-7.3, 1.0),
        ] {
            let up = tick(start, step, true);
            let down = tick(start, step, false);
            assert!(
                (up - (start + step)).abs() < 1e-6,
                "{start} + {step} gave {up}"
            );
            assert!(
                (down - (start - step)).abs() < 1e-6,
                "{start} - {step} gave {down}"
            );
        }
    }

    #[test]
    fn repeated_ticks_do_not_accumulate_float_noise() {
        // `0.35 + 0.1` is 0.45000000000000007 in binary floating point, and a few hundred ticks of
        // that would show in the inspector. The result is rounded finer than anything displayed.
        let mut v = 0.0;
        for _ in 0..300 {
            v = tick(v, 0.1, true);
        }
        assert!((v - 30.0).abs() < 1e-6, "300 tenths came to {v}");
        let mut v = 0.35;
        for _ in 0..10 {
            v = tick(v, 0.01, true);
        }
        assert!(
            (v - 0.45).abs() < 1e-9,
            "ten hundredths from 0.35 came to {v}"
        );
    }

    #[test]
    fn ticking_works_at_zero_and_across_it() {
        // What the value-proportional step could not do: a fiftieth of zero is zero, so it needed an
        // arbitrary floor. A fixed magnitude has no such problem.
        assert!((tick(0.0, 1.0, true) - 1.0).abs() < 1e-9);
        assert!((tick(0.0, 1.0, false) + 1.0).abs() < 1e-9);
        assert!((tick(0.5, 1.0, false) + 0.5).abs() < 1e-9);
        assert!((tick(-2.5, 0.1, true) + 2.4).abs() < 1e-9);
        assert!((tick(0.05, 0.1, false) + 0.05).abs() < 1e-9);
    }

    #[test]
    fn the_preview_shows_both_directions_within_bounds() {
        let (up, down) = preview(0.35, 0.1, (0.0, 10.0));
        assert!((up - 0.45).abs() < 1e-9);
        assert!((down - 0.25).abs() < 1e-9);
        // Against a bound the blocked direction reports the bound itself, so the overlay marks it
        // rather than showing an impossible number.
        let (up, down) = preview(10.0, 1.0, (0.0, 10.0));
        assert!((up - 10.0).abs() < 1e-9, "up should hold at the maximum");
        assert!((down - 9.0).abs() < 1e-9);
    }

    #[test]
    fn an_inert_column_is_marked_but_never_moves_out_of_place() {
        // fbm's octaves, 1 to 12: thousands and hundreds can only saturate it, and a fractional step
        // cannot apply to an integer at all. So two of six are live.
        let octaves = usable_columns(Resolution::Integer, (1.0, 12.0));
        assert_eq!(
            octaves.iter().map(|&c| step_of(c)).collect::<Vec<_>>(),
            vec![10.0, 1.0]
        );
        // But the positions are the fixed six either way: the ruler's width never changes, so a
        // magnitude is always in the same place whatever parameter is being edited.
        assert_eq!(
            overlay_width(),
            COLUMN_W * MAGNITUDES.len() as f32 + PAD * 2.0
        );
        // A wide continuous range has all of them live.
        assert_eq!(
            usable_columns(Resolution::Continuous, (0.0, 100_000.0)).len(),
            MAGNITUDES.len()
        );
    }

    #[test]
    fn pointing_at_an_inert_column_lands_on_the_nearest_live_one() {
        // Rather than doing nothing, which is what a dead hit felt like: a knack to be found.
        let octaves = usable_columns(Resolution::Integer, (1.0, 12.0)); // indices 2 and 3
        // The thousands column (slot 0) is inert; the nearest live one is tens.
        assert_eq!(nearest_usable(0.0, 34.0, &octaves).map(step_of), Some(10.0));
        // The hundredths column (slot 5) is inert; the nearest live one is ones.
        assert_eq!(
            nearest_usable(34.0 * 5.5, 34.0, &octaves).map(step_of),
            Some(1.0)
        );
        // Off either end clamps in rather than going dead.
        assert_eq!(
            nearest_usable(-500.0, 34.0, &octaves).map(step_of),
            Some(10.0)
        );
        assert_eq!(
            nearest_usable(9999.0, 34.0, &octaves).map(step_of),
            Some(1.0)
        );
        // Whatever it answers is always live.
        for offset in [-100.0_f32, 0.0, 40.0, 80.0, 120.0, 200.0, 5000.0] {
            let c = nearest_usable(offset, 34.0, &octaves).expect("always a column");
            assert!(octaves.contains(&c), "offset {offset} gave an inert column");
        }
        assert_eq!(nearest_usable(10.0, 34.0, &[]), None);
    }

    #[test]
    fn the_ruler_opens_at_the_magnitude_already_in_play() {
        assert_eq!(step_of(leading_column(2500.0)), 1000.0);
        assert_eq!(step_of(leading_column(2.5)), 1.0);
        assert_eq!(step_of(leading_column(0.025)), 0.01);
        // Zero has no leading magnitude, so the ones column.
        assert_eq!(step_of(leading_column(0.0)), 1.0);
        assert_eq!(leading_column(-250.0), leading_column(250.0));
    }

    #[test]
    fn the_overlay_is_held_inside_the_window() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let width = overlay_width();
        let left = clamp_ruler(1900.0, screen);
        assert!(left + width - PAD <= screen.right() + 0.001);
        assert!(clamp_ruler(-500.0, screen) >= screen.left());
        assert!((clamp_ruler(800.0, screen) - 800.0).abs() < 1e-6);
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(50.0, 400.0));
        assert!(clamp_ruler(20.0, tiny) >= tiny.left());
    }

    #[test]
    fn a_value_sitting_on_its_own_floor_can_still_be_scrubbed_up() {
        // fbm's frequency has a minimum of 0.25 and a default of 0.25, so it opens on its own floor.
        // Clearing the carry whenever the value sat on a bound wiped the accumulator every frame and
        // trapped it there: it could not be scrubbed at all. Only travel heading *into* the bound is
        // discarded now.
        let bounds = (0.25, 64.0);
        let (mut v, mut carry) = (0.25_f64, 0.0_f32);
        for _ in 0..4 {
            let (next, next_carry) = advance(v, 0.1, -4.0, carry, PIXELS_PER_TICK);
            v = clamp(next, bounds);
            carry = match at_bound(v, bounds) {
                Some(Bound::Low) if next_carry < 0.0 => 0.0,
                Some(Bound::High) if next_carry > 0.0 => 0.0,
                _ => next_carry,
            };
        }
        assert!(v > 0.25, "still stuck on the floor at {v}");

        // And pushing further down from the floor still banks nothing.
        let (mut v, mut carry) = (0.25_f64, 0.0_f32);
        for _ in 0..8 {
            let (next, next_carry) = advance(v, 0.1, 4.0, carry, PIXELS_PER_TICK);
            v = clamp(next, bounds);
            carry = match at_bound(v, bounds) {
                Some(Bound::Low) if next_carry < 0.0 => 0.0,
                Some(Bound::High) if next_carry > 0.0 => 0.0,
                _ => next_carry,
            };
        }
        assert!((v - 0.25).abs() < 1e-9, "floor was breached: {v}");
        assert_eq!(carry, 0.0, "travel into the bound was banked");
    }

    #[test]
    fn slow_drag_still_moves_the_value() {
        // The carry exists for this: three frames of four pixels must tick once, where rounding each
        // frame away would move nothing at all.
        let step = 1.0;
        let (mut v, mut carry) = (0.0, 0.0);
        for _ in 0..3 {
            (v, carry) = advance(v, step, -4.0, carry, 10.0);
        }
        assert!((v - 1.0).abs() < 1e-9, "three 4 px frames gave {v}");
        assert!(carry.abs() <= 10.0);
    }

    #[test]
    fn drag_direction_matches_every_other_scrub() {
        // Up raises. Window coordinates put up at negative delta.
        let (up, _) = advance(0.0, 1.0, -10.0, 0.0, 10.0);
        let (down, _) = advance(0.0, 1.0, 10.0, 0.0, 10.0);
        assert!((up - 1.0).abs() < 1e-9);
        assert!((down + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_long_drag_ticks_repeatedly() {
        let (v, carry) = advance(0.0, 10.0, -35.0, 0.0, 10.0);
        assert!((v - 30.0).abs() < 1e-9, "35 px at 10 px per tick gave {v}");
        // The remainder is carried, not discarded.
        assert!((carry - 5.0).abs() < 1e-4, "carry {carry}");
    }

    #[test]
    fn a_degenerate_sensitivity_does_not_hang_or_divide_by_zero() {
        let (v, carry) = advance(7.0, 1.0, -100.0, 0.0, 0.0);
        assert!((v - 7.0).abs() < 1e-9);
        assert_eq!(carry, 0.0);
    }

    #[test]
    fn bounds_are_reported_so_the_overlay_can_say_which_way_is_open() {
        assert_eq!(at_bound(0.0, (0.0, 1.0)), Some(Bound::Low));
        assert_eq!(at_bound(1.0, (0.0, 1.0)), Some(Bound::High));
        assert_eq!(at_bound(0.5, (0.0, 1.0)), None);
        // A reversed range from the schema is ignored rather than reported wrongly.
        assert_eq!(at_bound(0.5, (1.0, 0.0)), None);
        assert!((clamp(5.0, (1.0, 0.0)) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_gesture_carries_what_escape_and_history_both_need() {
        // The baseline is the Escape target and the value history compares against, so it is held
        // once rather than derived twice.
        let g = Gesture {
            baseline: 41.5,
            carry: 0.0,
            armed: false,
            vertical_run: 0.0,
            ruler_left: 0.0,
        };
        assert!((g.baseline - 41.5).abs() < 1e-9);
    }

    #[test]
    fn aiming_across_the_ruler_does_not_arm_ticking() {
        // Crossing sideways to reach a column, with the drift a hand makes while doing it.
        let mut armed = false;
        let mut run = 0.0;
        for _ in 0..20 {
            (armed, run) = aim(armed, run, egui::vec2(9.0, 1.5));
        }
        assert!(!armed, "sideways travel must not arm, however far it goes");
    }

    #[test]
    fn a_vertical_stroke_arms_after_a_deliberate_amount_of_travel() {
        let (armed, run) = aim(false, 0.0, egui::vec2(0.0, 2.0));
        assert!(!armed, "one small nudge is not a stroke");
        let (armed, _) = aim(armed, run, egui::vec2(0.0, 5.0));
        assert!(armed, "sustained vertical travel arms");
    }

    #[test]
    fn moving_sideways_again_returns_to_aiming() {
        let (armed, run) = aim(false, 0.0, egui::vec2(0.0, 8.0));
        assert!(armed);
        let (armed, run) = aim(armed, run, egui::vec2(-7.0, 0.5));
        assert!(!armed, "a deliberate sideways move re-aims");
        assert_eq!(run, 0.0, "and the next column starts from nothing");
    }

    #[test]
    fn jitter_while_scrubbing_does_not_disarm() {
        let (armed, run) = aim(false, 0.0, egui::vec2(0.0, 8.0));
        let (armed, _) = aim(armed, run, egui::vec2(2.0, 0.4));
        assert!(
            armed,
            "a hand holding a vertical stroke wanders a little sideways"
        );
    }
}
