//! Mask generator: derives a `[0, 1]` `mask` layer from the input, so mask-aware
//! nodes (erosion, combine) have a real mask to read.
//!
//! Two modes, selectable by param: `slope` masks by the height field's gradient
//! magnitude (select steep areas), `height` masks by elevation (select high
//! areas). Both map their value through a smoothstep between `low` and `high`;
//! setting `low` above `high` inverts the selection. The `height` layer passes
//! through untouched.
//!
//! The slope is the gradient in world units (scaled by the region span and
//! resolution), so it approximates the continuous slope and reads consistently as
//! the preview resolution changes, rather than drifting with cell size. It is then
//! compressed into `[0, 1)` so the `[0, 1]` thresholds span the full slope range.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.mask";

/// Mask source modes, in dropdown order. These are the stored param values.
const MODE_SLOPE: &str = "slope";
const MODE_HEIGHT: &str = "height";
const MODES: &[&str] = &[MODE_SLOPE, MODE_HEIGHT];

/// Mask generator: one input, one output. Writes [`layers::MASK`].
#[derive(Clone)]
pub struct Mask;

impl Operator for Mask {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            tags: &["mask", "slope", "select", "height", "modifier"],
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "mode",
                    ParamKind::Enum { options: MODES },
                    ParamValue::Text(MODE_SLOPE.to_string()),
                ),
                ParamSpec::new(
                    "low",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.0),
                ),
                ParamSpec::new(
                    "high",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let low = params.get_f64("low", 0.0) as f32;
        let high = params.get_f64("high", 0.5) as f32;
        let slope_mode = params.get_str("mode", MODE_SLOPE) != MODE_HEIGHT;

        // Cells per world unit, for a resolution-stable world-space gradient.
        let region = input.region();
        let wx = (width as f64 / region.width().max(f64::EPSILON)) as f32;
        let wy = (height as f64 / region.height().max(f64::EPSILON)) as f32;

        let mask = Layer::from_fn(width, height, |x, y| {
            let value = if slope_mode {
                // Central difference over clamped neighbours (one-sided at edges),
                // scaled to world units so the magnitude is resolution-stable.
                let xm = x.saturating_sub(1);
                let xp = (x + 1).min(width - 1);
                let ym = y.saturating_sub(1);
                let yp = (y + 1).min(height - 1);
                let gx = (h.get(xp, y).unwrap_or(0.0) - h.get(xm, y).unwrap_or(0.0)) * wx
                    / (xp - xm) as f32;
                let gy = (h.get(x, yp).unwrap_or(0.0) - h.get(x, ym).unwrap_or(0.0)) * wy
                    / (yp - ym) as f32;
                let slope = (gx * gx + gy * gy).sqrt();
                // Compress the unbounded world slope into [0, 1) so the low/high
                // thresholds span the full range; a 45-degree slope (magnitude 1)
                // maps to 0.5.
                slope / (1.0 + slope)
            } else {
                h.get(x, y).unwrap_or(0.0)
            };
            smoothstep(low, high, value)
        });

        let mut out = input.clone();
        out.set_layer(layers::MASK, Arc::new(mask));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to
/// `[0, 1]`. `low` above `high` inverts (selects values below `low`); `low` equal
/// to `high` is a hard step at that threshold.
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let d = high - low;
    let t = if d.abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / d).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Mask) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
    }

    /// A field whose height is a left-to-right ramp from 0 to 1.
    fn ramp_field(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                x as f32 / (size - 1) as f32
            })),
        )
    }

    fn mask(input: &Field, mode: &str, low: f32, high: f32) -> Field {
        let params = Params::new()
            .with("mode", ParamValue::Text(mode.to_string()))
            .with("low", ParamValue::Float(f64::from(low)))
            .with("high", ParamValue::Float(f64::from(high)));
        Mask.eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    #[test]
    fn slope_mask_is_high_on_a_ramp_and_zero_on_a_flat() {
        // A full 0->1 ramp over the unit region has world slope ~1, well above the
        // default high of 0.5, so the interior masks near 1.
        let ramped = mask(&ramp_field(16), MODE_SLOPE, 0.0, 0.5);
        let m = ramped.layer(layers::MASK).unwrap();
        assert!(m.get(8, 8).unwrap() > 0.99, "steep interior should mask ~1");

        // A flat field has zero slope, so the mask is zero.
        let flat = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.5)));
        let flat_mask = mask(&flat, MODE_SLOPE, 0.0, 0.5);
        assert_eq!(
            flat_mask.layer(layers::MASK).unwrap().get(8, 8).unwrap(),
            0.0
        );
    }

    #[test]
    fn height_mask_selects_by_elevation() {
        // Height-band [0.4, 0.6] over a ramp: low x (low height) masks 0, high x
        // masks 1, and the mask is monotonically non-decreasing across the ramp.
        let masked = mask(&ramp_field(32), MODE_HEIGHT, 0.4, 0.6);
        let m = masked.layer(layers::MASK).unwrap();
        assert_eq!(m.get(0, 0).unwrap(), 0.0);
        assert_eq!(m.get(31, 0).unwrap(), 1.0);
        let row: Vec<f32> = (0..32).map(|x| m.get(x, 0).unwrap()).collect();
        assert!(
            row.windows(2).all(|w| w[1] >= w[0]),
            "mask must rise with height"
        );
    }

    #[test]
    fn mask_stays_in_unit_range() {
        let masked = mask(&ramp_field(16), MODE_SLOPE, 0.0, 0.5);
        assert!(
            masked
                .layer(layers::MASK)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn height_passes_through() {
        let input = ramp_field(16);
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let masked = mask(&input, MODE_SLOPE, 0.0, 0.5);
        let after = masked.layer(layers::HEIGHT).unwrap().content_hash();
        assert_eq!(
            before, after,
            "the height layer must pass through unchanged"
        );
    }

    #[test]
    fn is_deterministic() {
        let input = ramp_field(16);
        assert_eq!(
            mask(&input, MODE_SLOPE, 0.0, 0.5).content_hash(),
            mask(&input, MODE_SLOPE, 0.0, 0.5).content_hash()
        );
    }

    #[test]
    fn erosion_reads_the_generated_mask() {
        use crate::ThermalErosion;

        // A height-band mask varies across the ramp, so thermal scaled by it differs
        // from unmasked thermal — proving the generated mask drives selective erosion.
        let input = ramp_field(16);
        let masked = mask(&input, MODE_HEIGHT, 0.0, 1.0);

        let erosion_params = Params::new()
            .with("talus", ParamValue::Float(0.0))
            .with("strength", ParamValue::Float(1.0))
            .with("iterations", ParamValue::Int(5));
        let selective = ThermalErosion
            .eval(Inputs::required_only(&[&masked]), &erosion_params, &ctx())
            .unwrap();
        let uniform = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &erosion_params, &ctx())
            .unwrap();
        assert_ne!(
            selective[0].layer(layers::HEIGHT).unwrap().content_hash(),
            uniform[0].layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }

    #[test]
    fn masked_out_regions_are_preserved_by_erosion() {
        use crate::ThermalErosion;

        // A height-band mask protects the low half of a ramp. After thermal erosion
        // the masked-out cells (mask 0) keep their original height exactly, while the
        // masked-in cells change — the protect, not just a scaled shed.
        let input = ramp_field(16);
        let masked = mask(&input, MODE_HEIGHT, 0.45, 0.55);
        let m = masked.layer(layers::MASK).unwrap();

        let tp = Params::new()
            .with("talus", ParamValue::Float(0.0))
            .with("strength", ParamValue::Float(1.0))
            .with("iterations", ParamValue::Int(20));
        let eroded = ThermalErosion
            .eval(Inputs::required_only(&[&masked]), &tp, &ctx())
            .unwrap()
            .remove(0);

        let before = masked.layer(layers::HEIGHT).unwrap();
        let after = eroded.layer(layers::HEIGHT).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                if m.get(x, y).unwrap() == 0.0 {
                    assert_eq!(
                        before.get(x, y).unwrap(),
                        after.get(x, y).unwrap(),
                        "masked-out cell ({x},{y}) must be untouched"
                    );
                }
            }
        }
    }

    #[test]
    fn output_matches_golden_value() {
        let out = mask(&ramp_field(16), MODE_SLOPE, 0.0, 0.5);
        assert_eq!(out.content_hash().to_u64(), 0x6fc3_e906_1fff_aa6a);
    }
}
