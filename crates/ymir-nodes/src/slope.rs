//! Slope selector: selects a band of steepness from the height gradient.
//!
//! Output is a `[0, 1]` selection on the **`height`** layer (so the rest of the
//! toolset shapes and applies it), high where the terrain's slope angle falls in
//! `[min, max]` degrees and falling off to zero over `falloff` degrees beyond each
//! edge. The angle is real: 0 degrees is flat, 90 is vertical, so the band is set in
//! units you can reason about, and because the selection rides on `height` you watch
//! it live in the 2D preview while you drag.
//!
//! This is the small-node successor to the old Mask's `slope` mode: it selects a
//! slope band (a range, not an arbitrary transfer function), leaving the rare
//! freeform shaping to a downstream Curve and the application to an effect's mask
//! input. The scale at which slope is measured comes from a Blur placed upstream.
//!
//! The gradient is taken as a true rise over run via the context's vertical:horizontal scale,
//! so the angle is a real terrain angle (resolution-stable, and set by the world's vertical and
//! horizontal extents). At a unit world (the default) this matches the prior normalized slope,
//! where a gradient magnitude of 1 reads as 45 degrees.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.slope";

/// Default band: a mid-slope selection that shows the band shape out of the box.
const DEFAULT_MIN: f64 = 20.0;
const DEFAULT_MAX: f64 = 50.0;
const DEFAULT_FALLOFF: f64 = 10.0;

/// Slope selector: one input, one output. Writes the band selection to
/// [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Slope;

impl Operator for Slope {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float {
                        min: 0.0,
                        max: 90.0,
                    },
                    ParamValue::Float(DEFAULT_MIN),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "max",
                    ParamKind::Float {
                        min: 0.0,
                        max: 90.0,
                    },
                    ParamValue::Float(DEFAULT_MAX),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float {
                        min: 0.0,
                        max: 90.0,
                    },
                    ParamValue::Float(DEFAULT_FALLOFF),
                )
                .with_unit(Unit::Degrees),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let min = params.get_f64("min", DEFAULT_MIN) as f32;
        let max = params.get_f64("max", DEFAULT_MAX) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0) as f32;

        // The vertical:horizontal scale turns a per-cell normalized-height delta into a true
        // slope (rise over run), so the angle is a real terrain angle and resolution-stable.
        // Cells are square, so x and y share the one scale.
        let scale = ctx.real_slope_scale() as f32;

        let selection = Layer::from_fn(width, height, |x, y| {
            // Central difference over clamped neighbours (one-sided at the edges),
            // scaled to a true slope so the magnitude is resolution-stable.
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(width - 1);
            let ym = y.saturating_sub(1);
            let yp = (y + 1).min(height - 1);
            let gx = (h.get(xp, y).unwrap_or(0.0) - h.get(xm, y).unwrap_or(0.0)) * scale
                / (xp - xm) as f32;
            let gy = (h.get(x, yp).unwrap_or(0.0) - h.get(x, ym).unwrap_or(0.0)) * scale
                / (yp - ym) as f32;
            let angle = (gx * gx + gy * gy).sqrt().atan().to_degrees();

            // Fully selected in [min, max], softening to zero over `falloff` degrees
            // beyond each edge. The product of a rising lower edge and a falling upper
            // edge gives the band; min > max simply yields an empty selection.
            let lower = smoothstep(min - falloff, min, angle);
            let upper = 1.0 - smoothstep(max, max + falloff, angle);
            (lower * upper).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
/// `low == high` degrades to a hard step at that threshold (a zero-width falloff).
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Slope) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    /// A field whose height ramps left-to-right with gradient `k` over the unit
    /// region, so its slope angle is about `atan(k)` degrees everywhere.
    fn sloped(size: usize, k: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                k * x as f32 / (size - 1) as f32
            })),
        )
    }

    /// A ramp at the given slope angle in degrees.
    fn at_angle(size: usize, degrees: f32) -> Field {
        sloped(size, degrees.to_radians().tan())
    }

    fn select(input: &Field, min: f32, max: f32, falloff: f32) -> Field {
        let params = Params::new()
            .with("min", ParamValue::Float(f64::from(min)))
            .with("max", ParamValue::Float(f64::from(max)))
            .with("falloff", ParamValue::Float(f64::from(falloff)));
        // A context matching the field, as in real evaluation (the field is produced at the
        // request resolution), so the real-slope scale uses the right cell size.
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 0);
        Slope
            .eval(Inputs::required_only(&[input]), &params, &ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn an_angle_inside_the_band_is_selected() {
        // A ~35 degree ramp sits inside [20, 50], so the interior masks ~1.
        let out = select(&at_angle(16, 35.0), 20.0, 50.0, 10.0);
        assert!(at(&out, 8, 8) > 0.9, "in-band angle should select ~1");
    }

    #[test]
    fn angles_outside_the_band_are_excluded() {
        // Below the lower falloff (5 < 20 - 10) and above the upper falloff
        // (80 > 50 + 10) both fall to zero.
        let gentle = select(&at_angle(16, 5.0), 20.0, 50.0, 10.0);
        let steep = select(&at_angle(16, 80.0), 20.0, 50.0, 10.0);
        assert!(at(&gentle, 8, 8) < 0.01, "gentle slope excluded");
        assert!(at(&steep, 8, 8) < 0.01, "over-steep slope excluded");

        // A flat field is 0 degrees, well outside the band.
        let flat = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.5)));
        assert_eq!(at(&select(&flat, 20.0, 50.0, 10.0), 8, 8), 0.0);
    }

    #[test]
    fn falloff_softens_the_edge() {
        // An angle just past the upper edge is partially selected with a falloff, but
        // fully excluded with a hard (zero) falloff.
        let soft = select(&at_angle(16, 55.0), 20.0, 50.0, 10.0);
        let hard = select(&at_angle(16, 55.0), 20.0, 50.0, 0.0);
        let s = at(&soft, 8, 8);
        assert!(
            s > 0.05 && s < 0.95,
            "edge should be partial under falloff: {s}"
        );
        assert_eq!(at(&hard, 8, 8), 0.0, "no falloff is a hard cutoff");
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = select(&at_angle(16, 35.0), 20.0, 50.0, 10.0);
        assert!(
            out.layer(layers::MASK).is_none(),
            "no mask layer is written"
        );
        assert!(at(&out, 8, 8) > 0.0, "the selection is on the height layer");
    }

    #[test]
    fn band_is_resolution_stable() {
        // The world-space gradient makes the angle resolution-independent: a ~35
        // degree ramp stays inside [20, 50] at both 16 and 64 cells.
        let lo = at(&select(&at_angle(16, 35.0), 20.0, 50.0, 10.0), 8, 8);
        let hi = at(&select(&at_angle(64, 35.0), 20.0, 50.0, 10.0), 32, 32);
        assert!(
            lo > 0.9 && hi > 0.9,
            "band drifted with resolution: {lo} vs {hi}"
        );
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = at_angle(16, 35.0);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.7)));
        let out = select(&input, 20.0, 50.0, 10.0);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = select(&at_angle(16, 35.0), 20.0, 50.0, 10.0);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_deterministic() {
        let input = at_angle(16, 35.0);
        assert_eq!(
            select(&input, 20.0, 50.0, 10.0).content_hash(),
            select(&input, 20.0, 50.0, 10.0).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        let out = select(&at_angle(16, 35.0), 20.0, 50.0, 10.0);
        assert_eq!(out.content_hash().to_u64(), 0x464f_3005_0dc8_fe11);
    }
}
