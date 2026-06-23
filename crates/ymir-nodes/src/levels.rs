//! Levels: rescales the `height` layer's range, the precise companion to Curve.
//!
//! Where Curve bends the elevation *profile*, Levels rescales its *range*: it stretches
//! an input window `[in_low, in_high]` to full, applies a gamma midtone bias, and maps
//! the result into an output window `[out_low, out_high]`. This is the right tool for the
//! jobs a Curve does badly: normalizing out-of-range height back into `[0, 1]` before a
//! Curve, controlling amplitude (a gentle low plain via a narrow output window), or
//! clipping a window (it doubles as a Clamp). Input points may sit outside `[0, 1]`, so
//! height that drifted out of range (after an Add) can be pulled back.
//!
//! Mask-aware per the convention: the leveled height is composited over the original
//! through the `mask` layer, so `mask = 1` is fully applied and `mask = 0` is the
//! original. Other layers pass through.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.levels";

/// Levels modifier: one input, one output.
#[derive(Clone)]
pub struct Levels;

impl Operator for Levels {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            tags: &[
                "levels",
                "range",
                "normalize",
                "clamp",
                "contrast",
                "gamma",
                "remap",
                "modifier",
            ],
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                // Input window stretched to full. Allowed past [0, 1] so out-of-range
                // height (e.g. after an Add) can be normalized back.
                ParamSpec::new(
                    "in_low",
                    ParamKind::Float {
                        min: -4.0,
                        max: 4.0,
                    },
                    ParamValue::Float(0.0),
                ),
                ParamSpec::new(
                    "in_high",
                    ParamKind::Float {
                        min: -4.0,
                        max: 4.0,
                    },
                    ParamValue::Float(1.0),
                ),
                // Midtone bias: > 1 lifts the mids, < 1 lowers them.
                ParamSpec::new(
                    "gamma",
                    ParamKind::Float {
                        min: 0.1,
                        max: 10.0,
                    },
                    ParamValue::Float(1.0),
                ),
                // Output window mapped into. A narrow window scales amplitude down (a
                // gentle plain).
                ParamSpec::new(
                    "out_low",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.0),
                ),
                ParamSpec::new(
                    "out_high",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(1.0),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        let levels = LevelParams {
            in_low: params.get_f64("in_low", 0.0) as f32,
            in_high: params.get_f64("in_high", 1.0) as f32,
            // Guard against a non-positive gamma producing a degenerate exponent.
            gamma: (params.get_f64("gamma", 1.0) as f32).max(1e-3),
            out_low: params.get_f64("out_low", 0.0) as f32,
            out_high: params.get_f64("out_high", 1.0) as f32,
        };

        let shaped = Layer::from_fn(width, height, |x, y| {
            let original = h.get(x, y).unwrap_or(0.0);
            let leveled = level_value(original, levels);
            let m = mask.get(x, y).unwrap_or(1.0);
            original + (leveled - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

/// The five Levels controls, precomputed once per evaluation.
#[derive(Clone, Copy)]
struct LevelParams {
    in_low: f32,
    in_high: f32,
    gamma: f32,
    out_low: f32,
    out_high: f32,
}

/// Maps one height value through the Levels transfer: clamp into the input window, apply
/// gamma, then map onto the output window. A zero-width input window degrades to a hard
/// step at that threshold.
fn level_value(value: f32, p: LevelParams) -> f32 {
    let span = p.in_high - p.in_low;
    let t = if span.abs() > f32::EPSILON {
        ((value - p.in_low) / span).clamp(0.0, 1.0)
    } else if value >= p.in_high {
        1.0
    } else {
        0.0
    };
    // Gamma > 1 lifts the midtones (t^(1/gamma) with 1/gamma < 1).
    let t = t.powf(1.0 / p.gamma);
    p.out_low + t * (p.out_high - p.out_low)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Levels) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::registry;

    fn ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    fn field_with(height: f32, mask: Option<f32>) -> Field {
        let mut f = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, height)));
        if let Some(m) = mask {
            f.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, m)));
        }
        f
    }

    fn run(input: &Field, params: &Params) -> Field {
        Levels
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// Builds a Params from `(name, value)` float pairs.
    fn params(pairs: &[(&str, f64)]) -> Params {
        let mut p = Params::new();
        for &(name, value) in pairs {
            p.insert(name.to_string(), ParamValue::Float(value));
        }
        p
    }

    #[test]
    fn defaults_pass_height_through() {
        assert!((at(&run(&field_with(0.37, None), &Params::default()), 0, 0) - 0.37).abs() < 1e-6);
    }

    #[test]
    fn output_window_scales_amplitude() {
        // out [0, 0.25] halves-and-quarters the full range: 1.0 -> 0.25, 0.5 -> 0.125.
        let p = params(&[("out_low", 0.0), ("out_high", 0.25)]);
        assert!((at(&run(&field_with(1.0, None), &p), 0, 0) - 0.25).abs() < 1e-6);
        assert!((at(&run(&field_with(0.5, None), &p), 0, 0) - 0.125).abs() < 1e-6);
    }

    #[test]
    fn input_window_normalizes_out_of_range_height() {
        // Height that ran to 2.0 (after an Add): mapping in [0, 2] -> [0, 1] brings it
        // back, with 2.0 -> 1.0 and 1.0 -> 0.5. This is the pre-Curve normalize.
        let p = params(&[("in_low", 0.0), ("in_high", 2.0)]);
        assert!((at(&run(&field_with(2.0, None), &p), 0, 0) - 1.0).abs() < 1e-6);
        assert!((at(&run(&field_with(1.0, None), &p), 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn input_window_clips_outside_the_range() {
        // Below in_low maps to out_low, above in_high to out_high (Levels doubles as a
        // clamp).
        let p = params(&[("in_low", 0.25), ("in_high", 0.75)]);
        assert_eq!(at(&run(&field_with(0.1, None), &p), 0, 0), 0.0);
        assert_eq!(at(&run(&field_with(0.9, None), &p), 0, 0), 1.0);
    }

    #[test]
    fn gamma_biases_the_midtones() {
        // At the input midpoint, gamma > 1 lifts above 0.5 and gamma < 1 drops below it.
        let up = at(
            &run(&field_with(0.5, None), &params(&[("gamma", 2.0)])),
            0,
            0,
        );
        let down = at(
            &run(&field_with(0.5, None), &params(&[("gamma", 0.5)])),
            0,
            0,
        );
        assert!(up > 0.5, "gamma > 1 should lift the mid: {up}");
        assert!(down < 0.5, "gamma < 1 should lower the mid: {down}");
    }

    #[test]
    fn mask_modulates_the_effect() {
        // Half mask on 1.0 with out_high 0.0 (would map to 0): halfway between original
        // (1.0) and leveled (0.0) is 0.5.
        let p = params(&[("out_high", 0.0)]);
        assert!((at(&run(&field_with(1.0, Some(0.5)), &p), 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = field_with(0.5, None);
        input.set_layer("flow", Arc::new(Layer::filled(8, 8, 0.9)));
        let out = run(&input, &Params::default());
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn is_deterministic() {
        let input = field_with(0.6, None);
        let p = params(&[("in_high", 0.8), ("gamma", 1.5), ("out_high", 0.7)]);
        assert_eq!(
            run(&input, &p).content_hash(),
            run(&input, &p).content_hash()
        );
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = field_with(0.42, None);
        let made = registry::make(TYPE_ID).expect("levels operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let direct = run(&input, &Params::default());
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_modifier() {
        assert_eq!(Levels.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(Levels.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let input = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |x, _| x as f32 / 15.0)),
        );
        let p = params(&[
            ("in_low", 0.1),
            ("in_high", 0.9),
            ("gamma", 1.5),
            ("out_low", 0.0),
            ("out_high", 1.0),
        ]);
        let out = run(&input, &p);
        assert_eq!(out.content_hash().to_u64(), 0x524f_0b6f_4c94_0b91);
    }
}
