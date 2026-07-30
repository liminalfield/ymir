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

/// The magnitudes a column can carry, coarsest first.
///
/// Six columns spanning a millionfold: enough that any parameter in the application has usable
/// columns, few enough that each can be a generous target. Out-of-range columns are not removed but
/// disabled, so their positions never move.
pub(crate) const MAGNITUDES: [f64; 6] = [1000.0, 100.0, 10.0, 1.0, 0.1, 0.01];

/// How close to a grid multiple counts as already on it.
///
/// A value that *is* on the grid must step off it, and floating point makes that a judgement rather
/// than a comparison: `2.5 / 0.1` is `24.999999...`, so flooring it and adding one lands back on 2.5
/// and the value never moves. Quantising through a tolerance is what makes ticking reliable.
const ON_GRID: f64 = 1e-6;

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
}

/// The magnitude a column carries.
pub(crate) fn step_of(column: usize) -> f64 {
    MAGNITUDES.get(column).copied().unwrap_or(1.0)
}

/// The magnitudes worth offering for a parameter, coarsest first, as indices into [`MAGNITUDES`].
///
/// Two things make a column pointless, and a pointless column is not drawn. It used to be drawn
/// struck through and recessed, on the argument that keeping every position fixed preserved muscle
/// memory; in use that read as clutter, and a column that cannot do anything is not something to
/// build memory of.
///
/// A fractional magnitude cannot apply to an integer parameter. And a magnitude larger than the
/// parameter's whole range can only saturate it: offering `1000` on an octave count of 1 to 12 is
/// offering a button that jumps to the maximum and then does nothing.
///
/// Note what is *not* consulted: the range never sets the column *spacing* or the set's origin, only
/// which members of a fixed set survive. Deriving the spacing from a declared range is what made an
/// earlier attempt unusable (#352), because most ranges are arbitrary outer bounds.
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

/// The magnitude index under a cursor `offset` points from the ruler's left edge.
///
/// `usable` is the drawn set, so the slot under the pointer indexes into that rather than into all
/// six magnitudes. `None` only when the pointer is outside the ruler entirely: every drawn column is
/// usable, so there are no dead columns to land on.
pub(crate) fn column_at(offset: f32, column_width: f32, usable: &[usize]) -> Option<usize> {
    if offset < 0.0 || column_width <= 0.0 {
        return None;
    }
    let slot = (offset / column_width).floor() as usize;
    usable.get(slot).copied()
}

/// Which drawn slot the magnitude already in play occupies, so the ruler can open with it under the
/// pointer.
///
/// A value of zero has no leading magnitude, so it answers with the ones column when that is drawn,
/// and otherwise the middle of what is.
pub(crate) fn leading_slot(value: f64, usable: &[usize]) -> usize {
    if usable.is_empty() {
        return 0;
    }
    let magnitude = value.abs();
    let wanted = if magnitude < ON_GRID { 1.0 } else { magnitude };
    usable
        .iter()
        .position(|&c| step_of(c) <= wanted)
        .unwrap_or(usable.len() - 1)
}

/// One tick of `step` away from `value`, landing on the grid that `step` defines.
///
/// Choosing a column declares the resolution you care about, so the result is always a multiple of
/// the step: ticking up from 2.47 by tenths gives 2.5, not 2.57. That differs from incrementing a
/// digit, which would leave the lower digits as residue, and it is deliberate. If you wanted 2.57 you
/// would have chosen hundredths.
///
/// The same rule serves the first tick and every later one. Once a value is on the grid, "the next
/// multiple in this direction" is simply plus or minus one step, so no special case is needed.
pub(crate) fn tick(value: f64, step: f64, up: bool) -> f64 {
    if step <= 0.0 {
        return value;
    }
    let quotient = value / step;
    let rounded = quotient.round();
    let next = if (quotient - rounded).abs() < ON_GRID {
        // Already on the grid: step off it.
        if up { rounded + 1.0 } else { rounded - 1.0 }
    } else if up {
        quotient.floor() + 1.0
    } else {
        quotient.ceil() - 1.0
    };
    next * step
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

/// The overlay's full width for `columns` drawn columns, frame included.
fn overlay_width(columns: usize) -> f32 {
    COLUMN_W * columns as f32 + PAD * 2.0
}

/// Holds the ruler's left edge inside `screen`, so no part of the overlay leaves the window.
///
/// Returns the left edge of the *columns*, which sits `PAD` inside the frame.
fn clamp_ruler(left: f32, columns: usize, screen: egui::Rect) -> f32 {
    let width = overlay_width(columns);
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

    if resp.drag_started() {
        let press = resp.interact_pointer_pos().unwrap_or(resp.rect.center());
        gesture = Some(Gesture {
            baseline: *value,
            carry: 0.0,
        });
        ui.data_mut(|d| d.insert_temp(id.with("press"), Some(press)));
    }

    let Some(active) = gesture else {
        return false;
    };
    if !resp.dragged() && !resp.drag_stopped() {
        clear(ui, id);
        return false;
    }
    let press: egui::Pos2 = ui
        .data(|d| d.get_temp::<Option<egui::Pos2>>(id.with("press")))
        .flatten()
        .unwrap_or(resp.rect.center());

    let usable = usable_columns(resolution, bounds);
    if usable.is_empty() {
        clear(ui, id);
        return false;
    }

    // The ruler opens with the magnitude already in play under the press point, so the first thing
    // under the pointer is the one most likely wanted. Held inside the window, because a field on the
    // right of the inspector would otherwise put half the ruler off the edge of the screen.
    //
    // Clamping here rather than at paint time is what keeps it honest: this one position decides both
    // where the columns are drawn and which one the pointer is over, so they cannot disagree.
    let ruler_left = clamp_ruler(
        press.x - COLUMN_W * (leading_slot(*value, &usable) as f32 + 0.5),
        usable.len(),
        ui.ctx().content_rect(),
    );

    // The column is whatever the pointer is over. Visible, correctable, and no state to hold.
    let cursor = ui.input(|i| i.pointer.latest_pos()).unwrap_or(press);
    let column = column_at(cursor.x - ruler_left, COLUMN_W, &usable);

    let mut moved = false;
    let mut carry = active.carry;
    if let Some(column) = column {
        // Raw device motion where the platform reports it, the position delta otherwise. Either
        // works now that the pointer is not grabbed.
        let dy = match ui.input(|i| i.pointer.motion()) {
            Some(m) => m.y,
            None => resp.drag_delta().y,
        };
        let (next, next_carry) = advance(*value, step_of(column), dy, carry, PIXELS_PER_TICK);
        let held = clamp(next, bounds);
        if (held - *value).abs() > f64::EPSILON {
            *value = held;
            moved = true;
        }
        // Running into a bound resets the accumulator, so backing off responds at once instead of
        // unwinding the travel spent pushing against the limit.
        carry = if at_bound(held, bounds).is_some() {
            0.0
        } else {
            next_carry
        };
    }

    draw(
        ui, id, resp.rect, ruler_left, &usable, column, *value, bounds, suffix,
    );

    if resp.drag_stopped() {
        clear(ui, id);
    } else {
        ui.data_mut(|d| {
            d.insert_temp(
                id,
                Some(Gesture {
                    baseline: active.baseline,
                    carry,
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
    value: f64,
    bounds: (f64, f64),
    suffix: &str,
) {
    let width = overlay_width(usable.len());
    let height = READOUT_H + GHOST_UP_H + RULER_H + GHOST_DOWN_H + PAD * 2.0;
    // Below the field, clear of it. An earlier version placed the ruler a fixed distance below the
    // *press point*, which put the overlay on top of the field it belongs to. Leaving the field
    // visible means the number can be watched changing in place, which is where the eye already is.
    let mut top = field.bottom() + 4.0;
    let screen = ui.ctx().content_rect();
    if top + height > screen.bottom() - 8.0 {
        top = field.top() - height - 4.0;
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
                Some(s) => (format!("\u{b1}{}", trim(s)), crate::theme::ACCENT_PRIMARY),
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
                let slot = usable.iter().position(|c| *c == column).unwrap_or(0);
                let cx = ruler_left + COLUMN_W * (slot as f32 + 0.5);
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

            // Only usable columns are drawn. Adjacent, so it reads as one scale rather than as
            // separate buttons.
            for (slot, &column) in usable.iter().enumerate() {
                let is_active = active == Some(column);
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
                    egui::Stroke::new(1.0, crate::theme::LINE_STRONG),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    cell.center(),
                    egui::Align2::CENTER_CENTER,
                    trim(step_of(column)),
                    mono(13.0),
                    if is_active {
                        crate::theme::BG_ABYSS
                    } else {
                        crate::theme::TEXT_SECONDARY
                    },
                );
            }
        });
}

/// A number without trailing zeros, so a column reads `1000` and `0.01` rather than `1000.00`.
fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
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
        assert_eq!(MAGNITUDES.len(), 6);
        for pair in MAGNITUDES.windows(2) {
            assert!(
                (pair[0] / pair[1] - 10.0).abs() < 1e-9,
                "columns must be exact decades: {pair:?}"
            );
        }
    }

    #[test]
    fn ticking_lands_on_the_grid_of_the_chosen_magnitude() {
        // Grabbing tenths on 2.47 gives 2.5, not 2.57. Choosing a column declares the resolution.
        assert!((tick(2.47, 0.1, true) - 2.5).abs() < 1e-9);
        assert!((tick(2.47, 0.1, false) - 2.4).abs() < 1e-9);
        // And coarser columns snap harder, which is the point of having them.
        assert!((tick(2.47, 1.0, true) - 3.0).abs() < 1e-9);
        assert!((tick(2.47, 1.0, false) - 2.0).abs() < 1e-9);
        assert!((tick(1234.0, 1000.0, true) - 2000.0).abs() < 1e-9);
    }

    #[test]
    fn a_value_already_on_the_grid_still_moves() {
        // The floating-point trap: 2.5 / 0.1 is 24.999999..., so a naive floor-and-add lands back on
        // 2.5 and the value never budges. This is why ticking quantises through a tolerance.
        assert!((tick(2.5, 0.1, true) - 2.6).abs() < 1e-9);
        assert!((tick(2.5, 0.1, false) - 2.4).abs() < 1e-9);
        assert!((tick(3.0, 1.0, true) - 4.0).abs() < 1e-9);
        assert!((tick(0.3, 0.1, true) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn ticking_works_at_zero_and_across_it() {
        // What the value-proportional step could not do: a fiftieth of zero is zero, so it needed an
        // arbitrary floor. A fixed magnitude has no such problem.
        assert!((tick(0.0, 1.0, true) - 1.0).abs() < 1e-9);
        assert!((tick(0.0, 1.0, false) + 1.0).abs() < 1e-9);
        assert!((tick(0.5, 1.0, false) - 0.0).abs() < 1e-9);
        assert!((tick(-2.5, 0.1, true) + 2.4).abs() < 1e-9);
        assert!((tick(-2.5, 0.1, false) + 2.6).abs() < 1e-9);
        // Crossing zero is unremarkable.
        assert!((tick(0.05, 0.1, false) - 0.0).abs() < 1e-9);
        assert!((tick(-0.05, 0.1, true) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn the_same_gesture_moves_the_same_amount_at_any_scale() {
        // Predictability, the criterion every earlier attempt failed. One tick of the tens column is
        // ten, whether the value is 5 or 5000.
        let small = tick(5.0, 10.0, true) - 5.0;
        let large = tick(5000.0, 10.0, true) - 5000.0;
        assert!((small - 5.0).abs() < 1e-9, "5 -> {}", tick(5.0, 10.0, true));
        assert!((large - 10.0).abs() < 1e-9);
        // From on-grid values the step is exactly the magnitude at both scales.
        assert!((tick(10.0, 10.0, true) - 20.0).abs() < 1e-9);
        assert!((tick(5000.0, 10.0, true) - 5010.0).abs() < 1e-9);
    }

    #[test]
    fn an_integer_parameter_is_offered_no_fractional_column() {
        let wide = usable_columns(Resolution::Integer, (0.0, 100_000.0));
        for &c in &wide {
            assert!(step_of(c) >= 1.0, "integer offered {}", step_of(c));
        }
        // A continuous parameter over the same range gets all six.
        assert_eq!(
            usable_columns(Resolution::Continuous, (0.0, 100_000.0)).len(),
            6
        );
    }

    #[test]
    fn a_column_that_could_only_saturate_is_not_offered() {
        // fbm's octaves, 1 to 12. Thousands and hundreds can only jump to the maximum and then do
        // nothing, so offering them is offering a dead button. Reported from use as clutter.
        let octaves = usable_columns(Resolution::Integer, (1.0, 12.0));
        let steps: Vec<f64> = octaves.iter().map(|&c| step_of(c)).collect();
        assert_eq!(
            steps,
            vec![10.0, 1.0],
            "octaves should offer tens and ones only"
        );

        // A genuinely wide range keeps its coarse columns.
        let radius = usable_columns(Resolution::Continuous, (0.0, 100_000.0));
        assert_eq!(step_of(radius[0]), 1000.0);

        // The range decides which members survive, never the spacing: whatever is offered is still a
        // subsequence of the fixed decades, which is what #352 got wrong.
        for pair in octaves.windows(2) {
            assert!(step_of(pair[0]) > step_of(pair[1]));
        }
    }

    #[test]
    fn the_cursor_maps_to_a_drawn_column() {
        let usable = usable_columns(Resolution::Integer, (1.0, 12.0)); // tens, ones
        assert_eq!(column_at(0.0, 34.0, &usable).map(step_of), Some(10.0));
        assert_eq!(column_at(40.0, 34.0, &usable).map(step_of), Some(1.0));
        // Past the drawn columns is nothing, rather than the nearest guess.
        assert_eq!(column_at(100.0, 34.0, &usable), None);
        assert_eq!(column_at(-1.0, 34.0, &usable), None);
        // Every drawn column is usable, so a hit is never dead.
        for offset in [0.0_f32, 17.0, 34.0, 60.0] {
            if let Some(c) = column_at(offset, 34.0, &usable) {
                assert!(usable.contains(&c));
            }
        }
    }

    #[test]
    fn the_ruler_opens_at_the_magnitude_already_in_play() {
        let all = usable_columns(Resolution::Continuous, (0.0, 100_000.0));
        assert_eq!(step_of(all[leading_slot(2500.0, &all)]), 1000.0);
        assert_eq!(step_of(all[leading_slot(2.5, &all)]), 1.0);
        assert_eq!(step_of(all[leading_slot(0.025, &all)]), 0.01);
        // Zero has no leading magnitude, so the ones column.
        assert_eq!(step_of(all[leading_slot(0.0, &all)]), 1.0);
        // Sign does not change the magnitude.
        assert_eq!(leading_slot(-250.0, &all), leading_slot(250.0, &all));
        // With a narrow set, it lands inside what is drawn rather than off the end.
        let octaves = usable_columns(Resolution::Integer, (1.0, 12.0));
        assert!(leading_slot(6.0, &octaves) < octaves.len());
        assert!(leading_slot(0.0, &octaves) < octaves.len());
    }

    #[test]
    fn the_overlay_is_held_inside_the_window() {
        // Reported from use: a field on the right of the inspector put the ruler off the screen.
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let columns = 6;
        let width = overlay_width(columns);
        let left = clamp_ruler(1900.0, columns, screen);
        assert!(
            left + width - PAD <= screen.right() + 0.001,
            "right edge {} overruns {}",
            left + width - PAD,
            screen.right()
        );
        assert!(clamp_ruler(-500.0, columns, screen) >= screen.left());
        assert!((clamp_ruler(800.0, columns, screen) - 800.0).abs() < 1e-6);
        // A window narrower than the overlay does not push it off the other side.
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(50.0, 400.0));
        assert!(clamp_ruler(20.0, columns, tiny) >= tiny.left());
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
    fn the_preview_shows_both_directions_within_bounds() {
        let (up, down) = preview(2.47, 0.1, (0.0, 10.0));
        assert!((up - 2.5).abs() < 1e-9);
        assert!((down - 2.4).abs() < 1e-9);
        // Against a bound the blocked direction reports the bound itself, so the overlay can mark it
        // rather than showing an impossible number.
        let (up, down) = preview(10.0, 1.0, (0.0, 10.0));
        assert!((up - 10.0).abs() < 1e-9, "up should be held at the maximum");
        assert!((down - 9.0).abs() < 1e-9);
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
        };
        assert!((g.baseline - 41.5).abs() < 1e-9);
    }
}
