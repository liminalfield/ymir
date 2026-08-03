//! Levels: rescales the `height` layer's range, the precise companion to Curve.
//!
//! Where Curve bends the elevation *profile*, Levels rescales its *range*: it stretches
//! an input window `[in_low, in_high]` to full, applies a gamma midtone bias, and maps
//! the result into an output window `[out_low, out_high]`. This is the right tool for the
//! jobs a Curve does badly: normalizing out-of-range height back into `[0, 1]` before a
//! Curve, controlling amplitude (a gentle low plain via a narrow output window), or
//! clipping a window (it doubles as a Clamp). Both windows may sit outside `[0, 1]`: an
//! input past the range pulls drifted height back, and a signed output window (e.g.
//! `-0.01..0.01`) recentres a `0..1` field on zero, turning noise into signed detail to
//! Add onto a base without shifting it up.
//!
//! Mask-aware per the convention: the leveled height is composited over the original
//! through the `mask` layer, so `mask = 1` is fully applied and `mask = 0` is the
//! original. Other layers pass through.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, LevelsTransfer, NodeSpec, Operator, ParamGroup, ParamKind,
    ParamSpec, ParamValue, Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.levels";

/// Default top of the output window, in metres: the default world height, so an unset Levels maps
/// its input window onto the full vertical range exactly as it did when this read `1.0` (#377).
const DEFAULT_OUT_HIGH: f64 = 256.0;

/// Levels modifier: one input, one output.
#[derive(Clone)]
pub struct Levels;

impl Operator for Levels {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            // All five declare one composite control (#369). They are a window, a bend and a
            // window, and read as five unrelated sliders only because nothing said otherwise;
            // the relationship between them is the tool. They stay five separate named
            // parameters, so each is still settable and addressable on its own.
            params: vec![
                // Input window stretched to full. Allowed far past [0, 1], because the window has to
                // reach whatever the incoming field actually carries, and not every field is a
                // height: Distance emits metres from a contour, which on a wide world runs to
                // thousands. A window that stopped at 4 could not normalize the very fields this node
                // exists to normalize. The output window below stays narrow on purpose, since what
                // comes *out* of Levels is a height and heights work in [0, 1].
                ParamSpec::new(
                    "in_low",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(0.0),
                )
                .in_group(ParamGroup::Levels),
                ParamSpec::new(
                    "in_high",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(1.0),
                )
                .in_group(ParamGroup::Levels),
                // Midtone bias: > 1 lifts the mids, < 1 lowers them. Logarithmically distributed,
                // because 1.0 is neutral and is the geometric midpoint of the range: on a linear
                // control every value below neutral is crushed into a tenth of the travel, so
                // halving and doubling the bias would cost wildly different drags.
                ParamSpec::new(
                    "gamma",
                    ParamKind::Float {
                        min: 0.1,
                        max: 10.0,
                    },
                    ParamValue::Float(1.0),
                )
                .logarithmic()
                .in_group(ParamGroup::Levels),
                // Output window mapped into, in metres of elevation (#377): what leaves Levels
                // is a height, and a height is a thing a person can picture. A narrow window
                // scales amplitude down (a gentle plain). Allowed negative and past the world's
                // height, so Levels can still produce signed or over-range output: a small signed
                // window centres a field on zero to add as detail without shifting the base up,
                // matching the no-hard-clamp height model.
                //
                // The *input* window above stays unitless, and the asymmetry is real rather than
                // an oversight: it windows whatever arrives, which may be metres from Distance, a
                // selection, or a height, so it has no one unit to declare.
                ParamSpec::new(
                    "out_low",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Meters)
                .in_group(ParamGroup::Levels),
                ParamSpec::new(
                    "out_high",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_OUT_HIGH),
                )
                .with_unit(Unit::Meters)
                .in_group(ParamGroup::Levels),
            ],
            emitted_layers: Vec::new(),
            mask_aware: true,
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    /// The output window is metres, converted against the world's vertical scale, so a change to
    /// world height changes what it means (#377).
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_HEIGHT
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        // The transfer lives in `ymir-core` beside the `ParamGroup::Levels` declaration, so the
        // inspector draws the same curve this applies rather than a second copy of it. The output
        // window is declared in metres and the layer is normalized, so it converts here (#377).
        let mut levels = LevelsTransfer::from_params(params);
        levels.out_low = ctx.height_from_meters(params.get_f64("out_low", 0.0));
        levels.out_high = ctx.height_from_meters(params.get_f64("out_high", DEFAULT_OUT_HIGH));

        let shaped = Layer::from_fn(width, height, |x, y| {
            let original = h.get(x, y).unwrap_or(0.0);
            let leveled = levels.apply(original);
            let m = mask.get(x, y).unwrap_or(1.0);
            original + (leveled - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Levels) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::{param_runs, registry};

    #[test]
    fn all_five_params_form_one_composite_control() {
        // The five are one control, and `param_runs` only merges a *consecutive* run, so
        // inserting an ungrouped parameter between them would silently split the editor in two.
        // Asserted here rather than left to review.
        let spec = Levels.spec();
        let runs = param_runs(&spec.params);
        assert_eq!(runs.len(), 1, "expected one run, got {runs:?}");
        assert_eq!(runs[0], (Some(ParamGroup::Levels), 0..5));
        let names: Vec<&str> = spec.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["in_low", "in_high", "gamma", "out_low", "out_high"],
            "the composite editor reads its members by position, so their order is contractual"
        );
    }

    #[test]
    fn an_unset_node_is_the_neutral_transfer_once_converted() {
        // The schema defaults are metres and `LevelsTransfer::NEUTRAL` is normalized, so they are
        // no longer the same numbers (#377). What must still hold is that an unset node, once its
        // output window is converted, *is* the neutral transfer: input unchanged in, unchanged
        // out. Asserted through the node rather than by comparing the two constants.
        for height in [1.0, 0.5, 0.0] {
            let out = at(&run(&field_with(height, None), &Params::new()), 0, 0);
            assert!(
                (out - height).abs() < 1e-6,
                "an unset Levels should pass {height} through, got {out}"
            );
        }

        // And the declared defaults still agree with each other: `out_low` at the bottom of the
        // range, `out_high` at the world's height, so the window is the whole vertical extent.
        let spec = Levels.spec();
        let declared_out_high = spec
            .params
            .iter()
            .find(|p| p.name == "out_high")
            .and_then(|p| match p.default {
                ParamValue::Float(v) => Some(v),
                _ => None,
            })
            .expect("out_high is declared");
        assert!((declared_out_high - DEFAULT_OUT_HIGH).abs() < f64::EPSILON);
    }

    #[test]
    fn the_input_window_fallbacks_match_the_declared_defaults() {
        // The input window is still unitless, so its fallbacks and its declaration must agree.
        let from_empty = LevelsTransfer::from_params(&Params::new());
        let spec = Levels.spec();
        let declared = |name: &str| {
            let ParamValue::Float(v) = spec
                .params
                .iter()
                .find(|p| p.name == name)
                .expect("declared")
                .default
            else {
                panic!("{name} defaults to a float");
            };
            v as f32
        };
        assert_eq!(from_empty.in_low, declared("in_low"));
        assert_eq!(from_empty.in_high, declared("in_high"));
        assert_eq!(from_empty.gamma, declared("gamma"));
    }

    #[test]
    fn gamma_is_logarithmic_so_neutral_sits_mid_travel() {
        let spec = Levels.spec();
        let gamma = spec
            .params
            .iter()
            .find(|p| p.name == "gamma")
            .expect("gamma is declared");
        assert_eq!(gamma.scale, ymir_core::Scale::Logarithmic);
        // Neutral is the geometric midpoint of the range, which is what makes a logarithmic
        // control put it at the centre of the track.
        let ParamKind::Float { min, max } = gamma.kind else {
            panic!("gamma is a float");
        };
        assert!(
            ((min * max).sqrt() - 1.0).abs() < 1e-9,
            "neutral 1.0 should be the geometric midpoint of [{min}, {max}]"
        );
    }

    /// A world as tall as the default output window, so an unset Levels is the identity and the
    /// `[0, 1]` fields these tests build read directly as metres of that world (#377).
    fn ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 0).with_world_height(DEFAULT_OUT_HIGH)
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
        // A window a quarter of the world tall quarters the range: 1.0 -> 0.25, 0.5 -> 0.125.
        // Stated in metres now, against a world `DEFAULT_OUT_HIGH` tall (#377).
        let p = params(&[("out_low", 0.0), ("out_high", DEFAULT_OUT_HIGH * 0.25)]);
        assert!((at(&run(&field_with(1.0, None), &p), 0, 0) - 0.25).abs() < 1e-6);
        assert!((at(&run(&field_with(0.5, None), &p), 0, 0) - 0.125).abs() < 1e-6);
    }

    #[test]
    fn a_signed_output_window_centres_the_field_on_zero() {
        // A signed window maps 0..1 to -0.01..0.01, so a mid-grey field lands on zero and the
        // extremes straddle it: the recipe for turning a 0..1 noise into signed detail to Add
        // without shifting the base up. The output is not clamped back into [0, 1].
        let p = params(&[
            ("out_low", DEFAULT_OUT_HIGH * -0.01),
            ("out_high", DEFAULT_OUT_HIGH * 0.01),
        ]);
        assert!((at(&run(&field_with(0.5, None), &p), 0, 0) - 0.0).abs() < 1e-6);
        assert!((at(&run(&field_with(0.0, None), &p), 0, 0) - -0.01).abs() < 1e-6);
        assert!((at(&run(&field_with(1.0, None), &p), 0, 0) - 0.01).abs() < 1e-6);
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
        // The output window is stated in metres now (#377). Naming the whole vertical range
        // rather than the number 1.0 is the same transfer as before, so the golden is unmoved:
        // this change is units, not maths.
        let p = params(&[
            ("in_low", 0.1),
            ("in_high", 0.9),
            ("gamma", 1.5),
            ("out_low", 0.0),
            ("out_high", DEFAULT_OUT_HIGH),
        ]);
        let out = run(&input, &p);
        assert_eq!(out.content_hash().to_u64(), 0x524f_0b6f_4c94_0b91);
    }
}
