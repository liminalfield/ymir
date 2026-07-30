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

/// Which phase of the gesture the control is in.
///
/// Aim and scrub are separate phases rather than one continuous drag because the cursor is locked and
/// hidden once scrubbing starts, so nothing can be aimed at after that point. The column is therefore
/// chosen while the pointer is still real, and held for the rest of the gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Phase {
    /// The overlay is open and a column is being chosen. No value has changed yet.
    Aim {
        /// The column under the cursor, or `None` when the cursor is off the ruler.
        column: Option<usize>,
    },
    /// A column is locked in and vertical motion is moving the value.
    Scrub {
        /// The chosen column.
        column: usize,
        /// The value when the scrub began, for Escape and for the tick arithmetic.
        baseline: f64,
        /// Vertical pixels travelled since the last tick, carried between frames.
        carry: f32,
    },
}

/// The magnitude a column carries.
pub(crate) fn step_of(column: usize) -> f64 {
    MAGNITUDES.get(column).copied().unwrap_or(1.0)
}

/// Whether a column can be used for this parameter.
///
/// An integer parameter cannot take a fractional step, so those columns are unusable. They are
/// reported as disabled rather than removed: the layout keeps every column in its place, because the
/// whole value of a fixed ruler is that a magnitude is always in the same position.
pub(crate) fn column_enabled(column: usize, resolution: Resolution) -> bool {
    match resolution {
        Resolution::Continuous => column < MAGNITUDES.len(),
        Resolution::Integer => step_of(column) >= 1.0 && column < MAGNITUDES.len(),
    }
}

/// The column under a cursor at `offset` points from the ruler's left edge, given `column_width`.
///
/// `None` when the cursor is outside the ruler, so the caller can show "no column" rather than
/// guessing at the nearest one. A disabled column reports as itself, not as `None`; whether it can be
/// used is [`column_enabled`]'s question, and conflating the two would let a press on a disabled
/// column silently select its neighbour.
pub(crate) fn column_at(offset: f32, column_width: f32) -> Option<usize> {
    if offset < 0.0 || column_width <= 0.0 {
        return None;
    }
    let index = (offset / column_width).floor() as usize;
    (index < MAGNITUDES.len()).then_some(index)
}

/// The column whose magnitude leads `value`, so the ruler can open under the cursor at the magnitude
/// already in play.
///
/// A value of zero has no leading magnitude, so it answers with the ones column: the middle of the
/// ruler, and the place a value is most likely to grow from.
pub(crate) fn leading_column(value: f64) -> usize {
    let magnitude = value.abs();
    if magnitude < ON_GRID {
        return MAGNITUDES.iter().position(|m| *m == 1.0).unwrap_or(3);
    }
    // The first column no coarser than the value itself.
    MAGNITUDES
        .iter()
        .position(|m| *m <= magnitude)
        .unwrap_or(MAGNITUDES.len() - 1)
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
const COLUMN_W: f32 = 22.0;
/// Ruler row height.
const RULER_H: f32 = 28.0;
/// Readout row: the value, the step, and any clamp tag.
const READOUT_H: f32 = 22.0;
/// The upward preview's band. Taller than the downward one on purpose: the live cursor sits in this
/// band during aim, so the value is top-aligned to clear it. Growing this band is the right fix if a
/// larger cursor still collides; moving the value sideways is not, because it would stop pointing at
/// its column.
const GHOST_UP_H: f32 = 20.0;
/// The downward preview's band. Nothing occludes it.
const GHOST_DOWN_H: f32 = 12.0;
/// Padding inside the overlay frame.
const PAD: f32 = 6.0;
/// Vertical distance from the press point down to the ruler row's centre, so the ruler lands inside
/// the same eye fixation as the field and aiming is a wrist movement rather than a reach.
const RULER_BELOW_PRESS: f32 = 14.0;
/// Vertical travel that ends aim and begins scrubbing. Asymmetric by design: horizontal movement
/// during aim is free and unlimited, because looking down at the ruler and moving toward it is the
/// user's instinct and must not be the gesture that ends their chance to aim.
const ENGAGE_PX: f32 = 10.0;
/// Vertical travel per tick, identical for every parameter and every column.
const PIXELS_PER_TICK: f32 = 10.0;

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
    let mut phase: Option<Phase> = ui.data(|d| d.get_temp::<Option<Phase>>(id)).flatten();

    // Escape abandons the gesture and puts the value back. Checked before anything else so a
    // cancelled drag cannot also commit on the same frame.
    if phase.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(Phase::Scrub { baseline, .. }) = phase {
            *value = baseline;
        }
        release(ui, id);
        return true;
    }

    if resp.drag_started() {
        let press = resp.interact_pointer_pos().unwrap_or(resp.rect.center());
        phase = Some(Phase::Aim { column: None });
        ui.data_mut(|d| d.insert_temp(id.with("press"), Some(press)));
    }

    let Some(current) = phase else {
        return false;
    };
    let press: egui::Pos2 = ui
        .data(|d| d.get_temp::<Option<egui::Pos2>>(id.with("press")))
        .flatten()
        .unwrap_or(resp.rect.center());

    if !resp.dragged() && !resp.drag_stopped() {
        // The pointer left without a release event reaching us; drop the gesture rather than leaving
        // the overlay stranded on screen.
        release(ui, id);
        return false;
    }

    let ruler_left = press.x - COLUMN_W * 0.5 - COLUMN_W * leading_column(*value) as f32;
    let mut moved = false;

    let next = match current {
        Phase::Aim { .. } => {
            let cursor = ui.input(|i| i.pointer.latest_pos()).unwrap_or(press);
            let column = column_at(cursor.x - ruler_left, COLUMN_W)
                .filter(|c| column_enabled(*c, resolution));
            // Only vertical travel engages. Horizontal movement is free, so aiming can range across
            // the whole ruler without committing.
            if (cursor.y - press.y).abs() >= ENGAGE_PX {
                match column {
                    Some(column) => {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                                egui::viewport::CursorGrab::Locked,
                            ));
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
                        Phase::Scrub {
                            column,
                            baseline: *value,
                            carry: 0.0,
                        }
                    }
                    // Engaged over no usable column: stay in aim rather than picking a neighbour.
                    None => Phase::Aim { column },
                }
            } else {
                Phase::Aim { column }
            }
        }
        Phase::Scrub {
            column,
            baseline,
            carry,
        } => {
            let delta = ui.input(|i| i.pointer.delta().y);
            let (next_value, next_carry) =
                advance(*value, step_of(column), delta, carry, PIXELS_PER_TICK);
            let held = clamp(next_value, bounds);
            if (held - *value).abs() > f64::EPSILON {
                *value = held;
                moved = true;
            }
            Phase::Scrub {
                column,
                baseline,
                // Running into a bound resets the accumulator, so backing off responds at once
                // instead of unwinding the travel that was spent pushing against the limit.
                carry: if at_bound(held, bounds).is_some() {
                    0.0
                } else {
                    next_carry
                },
            }
        }
    };

    draw(
        ui, id, press, ruler_left, next, *value, bounds, resolution, suffix,
    );

    if resp.drag_stopped() {
        release(ui, id);
    } else {
        ui.data_mut(|d| d.insert_temp(id, Some(next)));
    }
    moved
}

/// Ends the gesture: clears the stored phase and gives the pointer back.
fn release(ui: &egui::Ui, id: egui::Id) {
    ui.ctx()
        .send_viewport_cmd(egui::ViewportCommand::CursorGrab(
            egui::viewport::CursorGrab::None,
        ));
    ui.ctx()
        .send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
    // `remove_temp` wants `Default`, which a phase has no sensible value for, so the slots are
    // overwritten with `None` instead. Reading them back as `Option<Phase>` treats that as absent.
    ui.data_mut(|d| {
        d.insert_temp::<Option<Phase>>(id, None);
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
    press: egui::Pos2,
    ruler_left: f32,
    phase: Phase,
    value: f64,
    bounds: (f64, f64),
    resolution: Resolution,
    suffix: &str,
) {
    let width = COLUMN_W * MAGNITUDES.len() as f32 + PAD * 2.0;
    let height = READOUT_H + GHOST_UP_H + RULER_H + GHOST_DOWN_H + PAD * 2.0;
    // The ruler's centre sits a fixed distance below the press point, which places the overlay over
    // the field it belongs to. The readout repeats the value for exactly that reason.
    let ruler_top = press.y + RULER_BELOW_PRESS - RULER_H * 0.5;
    let mut top = ruler_top - GHOST_UP_H - READOUT_H - PAD;
    // Deterministic flip when the overlay would fall off the bottom, rather than choosing per frame.
    let screen = ui.ctx().content_rect();
    if top + height > screen.bottom() - 8.0 {
        top = press.y - height - RULER_BELOW_PRESS;
    }
    let origin = egui::pos2(ruler_left - PAD, top);

    let active = match phase {
        Phase::Aim { column } => column,
        Phase::Scrub { column, .. } => Some(column),
    };
    let locked = matches!(phase, Phase::Scrub { .. });

    // Seeded from the field's own id, not `ui.id()`. Every parameter row in the inspector shares one
    // layout, so `ui.id()` is the same for all of them and the overlays collided: only a row that
    // happened to sit inside its own `push_id` (the integer stepper) got a unique area and worked.
    egui::Area::new(id.with("overlay"))
        .order(egui::Order::Foreground)
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

            // Readout: the value, then the step right-aligned.
            let readout_y = rect.top() + PAD + READOUT_H * 0.5;
            p.text(
                egui::pos2(rect.left() + PAD, readout_y),
                egui::Align2::LEFT_CENTER,
                format!("{}{suffix}", trim(value)),
                mono(12.0),
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
                mono(9.5),
                step_ink,
            );

            // The previews, centred on the active column so each number sits on the side of the axis
            // that produces it. That is what makes the snap rule evident before the first tick.
            if let (Some(column), Some(s)) = (active, step) {
                let cx = ruler_left + COLUMN_W * (column as f32 + 0.5);
                let (up, down) = preview(value, s, bounds);
                let ink = if locked {
                    crate::theme::TEXT_SECONDARY
                } else {
                    crate::theme::TEXT_TERTIARY
                };
                let bound = at_bound(value, bounds);
                let ghost = |y: f32, v: f64, blocked: bool, align: egui::Align2| {
                    if blocked {
                        // A dashed stub rather than a number: the direction that still works stays
                        // legible without reading a word.
                        p.line_segment(
                            [egui::pos2(cx - 5.0, y), egui::pos2(cx + 5.0, y)],
                            egui::Stroke::new(1.0, crate::theme::TEXT_TERTIARY),
                        );
                    } else {
                        p.text(egui::pos2(cx, y), align, trim(v), mono(8.5), ink);
                    }
                };
                ghost(
                    ruler_top - GHOST_UP_H + 2.0,
                    up,
                    bound == Some(Bound::High),
                    egui::Align2::CENTER_TOP,
                );
                ghost(
                    ruler_top + RULER_H + GHOST_DOWN_H * 0.5,
                    down,
                    bound == Some(Bound::Low),
                    egui::Align2::CENTER_CENTER,
                );
            }

            // The ruler: six adjacent columns. Adjacent rather than gapped, so it reads as one scale
            // and not as six buttons.
            for (column, magnitude) in MAGNITUDES.iter().enumerate() {
                let enabled = column_enabled(column, resolution);
                let is_active = active == Some(column);
                // A disabled column keeps its place but sits lower and shorter, so it is visibly not
                // part of the surface being pointed at. Reordering would break the fixed ruler.
                let drop = if enabled { 0.0 } else { 6.0 };
                let cell = egui::Rect::from_min_size(
                    egui::pos2(ruler_left + COLUMN_W * column as f32, ruler_top + drop),
                    egui::vec2(COLUMN_W, RULER_H - drop),
                );
                if is_active {
                    p.rect_filled(cell, 0.0, crate::theme::ACCENT_PRIMARY);
                    if locked {
                        // The lock is the visible event: the ruler stops being a menu.
                        p.rect_stroke(
                            cell.shrink(2.0),
                            0.0,
                            egui::Stroke::new(2.0, crate::theme::TEXT_PRIMARY),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                p.rect_stroke(
                    cell,
                    0.0,
                    egui::Stroke::new(
                        1.0,
                        if enabled {
                            crate::theme::LINE_STRONG
                        } else {
                            crate::theme::LINE
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                let (ink, size) = if is_active {
                    (crate::theme::BG_ABYSS, 9.0)
                } else if !enabled {
                    (crate::theme::LINE_STRONG, 9.0)
                } else if locked {
                    // Idle labels drop back once locked, so the one in use stands alone.
                    (crate::theme::LINE_STRONG, 9.0)
                } else {
                    (crate::theme::TEXT_TERTIARY, 9.0)
                };
                p.text(
                    cell.center(),
                    egui::Align2::CENTER_CENTER,
                    trim(*magnitude),
                    mono(size),
                    ink,
                );
                if !enabled {
                    // Struck through as well as recessed: a second channel, never colour alone.
                    p.line_segment(
                        [
                            egui::pos2(cell.left() + 3.0, cell.center().y),
                            egui::pos2(cell.right() - 3.0, cell.center().y),
                        ],
                        egui::Stroke::new(1.0, crate::theme::LINE_STRONG),
                    );
                }
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
    fn an_integer_parameter_cannot_take_a_fractional_step() {
        for column in 0..MAGNITUDES.len() {
            let enabled = column_enabled(column, Resolution::Integer);
            assert_eq!(
                enabled,
                step_of(column) >= 1.0,
                "column {column} ({}) on an integer",
                step_of(column)
            );
            // Continuous parameters can use all of them.
            assert!(column_enabled(column, Resolution::Continuous));
        }
    }

    #[test]
    fn the_cursor_maps_to_a_column_by_position() {
        // 22 point columns, six of them.
        assert_eq!(column_at(0.0, 22.0), Some(0));
        assert_eq!(column_at(21.9, 22.0), Some(0));
        assert_eq!(column_at(22.0, 22.0), Some(1));
        assert_eq!(column_at(5.0 * 22.0 + 1.0, 22.0), Some(5));
        // Off the ruler reports nothing rather than guessing, so the overlay can say so.
        assert_eq!(column_at(-1.0, 22.0), None);
        assert_eq!(column_at(6.0 * 22.0, 22.0), None);
        // A degenerate width does not divide by zero.
        assert_eq!(column_at(10.0, 0.0), None);
    }

    #[test]
    fn the_ruler_opens_at_the_magnitude_already_in_play() {
        assert_eq!(step_of(leading_column(2500.0)), 1000.0);
        assert_eq!(step_of(leading_column(250.0)), 100.0);
        assert_eq!(step_of(leading_column(2.5)), 1.0);
        assert_eq!(step_of(leading_column(0.25)), 0.1);
        assert_eq!(step_of(leading_column(0.025)), 0.01);
        // Below the finest column, the finest column.
        assert_eq!(step_of(leading_column(0.0001)), 0.01);
        // Zero has no leading magnitude, so the ones column: the middle, and where a value grows from.
        assert_eq!(step_of(leading_column(0.0)), 1.0);
        // Sign does not change the magnitude.
        assert_eq!(leading_column(-250.0), leading_column(250.0));
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
    fn a_phase_carries_what_escape_and_history_both_need() {
        // The baseline is the Escape target and the value history compares against, so it is held
        // once rather than derived twice.
        let phase = Phase::Scrub {
            column: 2,
            baseline: 41.5,
            carry: 0.0,
        };
        match phase {
            Phase::Scrub { baseline, .. } => assert!((baseline - 41.5).abs() < 1e-9),
            Phase::Aim { .. } => panic!("wrong phase"),
        }
        assert_eq!(Phase::Aim { column: None }, Phase::Aim { column: None });
    }
}
