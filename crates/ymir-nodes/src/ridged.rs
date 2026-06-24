//! The ridged-multifractal generator: sharp mountain ridgelines.
//!
//! A sibling of the fBm generator that folds each octave to a ridge instead of summing
//! plainly, so the output reads as eroded mountains (sharp crests, carved valleys)
//! rather than rolling hills. The two share the octave-layering parameters and the same
//! resolution-independent sampling; the terrain math lives in [`crate::noise`]. Compose
//! it with a Shape envelope through a Blend in Multiply to place a massif.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

use crate::noise::{FbmParams, ridged_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.ridged";

/// Ridged-multifractal noise generator. A generator by arity: no inputs, one output.
#[derive(Clone)]
pub struct Ridged;

impl Operator for Ridged {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "frequency",
                    ParamKind::Float {
                        min: 0.0,
                        max: 64.0,
                    },
                    ParamValue::Float(2.0),
                ),
                ParamSpec::new(
                    "octaves",
                    ParamKind::Int { min: 1, max: 12 },
                    ParamValue::Int(5),
                ),
                ParamSpec::new(
                    "lacunarity",
                    ParamKind::Float { min: 1.0, max: 4.0 },
                    ParamValue::Float(2.0),
                ),
                ParamSpec::new(
                    "gain",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
                // Per-node seed: rerolls this generator's texture without a new node or
                // touching the world seed. Mixed into the node's derived seed, so 0 is the
                // unchanged default. Mirrors the fBm generator.
                ParamSpec::new(
                    "seed",
                    ParamKind::Int {
                        min: 0,
                        max: i64::from(i32::MAX),
                    },
                    ParamValue::Int(0),
                ),
                ParamSpec::new(
                    "offset_x",
                    ParamKind::Int {
                        min: -10_000,
                        max: 10_000,
                    },
                    ParamValue::Int(0),
                ),
                ParamSpec::new(
                    "offset_y",
                    ParamKind::Int {
                        min: -10_000,
                        max: 10_000,
                    },
                    ParamValue::Int(0),
                ),
            ],
        }
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let fractal = FbmParams {
            frequency: params.get_f64("frequency", 2.0),
            // Range is advisory until the graph/UI validate; clamp defensively.
            octaves: params.get_i64("octaves", 5).clamp(0, 32) as u32,
            lacunarity: params.get_f64("lacunarity", 2.0),
            gain: params.get_f64("gain", 0.5) as f32,
            offset_x: params.get_i64("offset_x", 0) as f64,
            offset_y: params.get_i64("offset_y", 0) as f64,
        };

        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = ridged_field(ctx.width, ctx.height, ctx.region, fractal, seed);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Ridged) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::layers;
    use ymir_core::registry;

    fn default_ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 42)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        Ridged
            .eval(Inputs::required_only(&[]), params, ctx)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn eval_is_deterministic() {
        let ctx = EvalContext::new(64, 64, Region::UNIT, 99);
        let a = run(&Params::default(), &ctx);
        let b = run(&Params::default(), &ctx);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn output_stays_in_unit_range() {
        let out = run(
            &Params::default(),
            &EvalContext::new(96, 96, Region::UNIT, 7),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &value in layer.as_slice() {
            assert!((0.0..=1.0).contains(&value), "value {value} out of [0, 1]");
        }
    }

    #[test]
    fn output_is_not_constant() {
        let out = run(
            &Params::default(),
            &EvalContext::new(64, 64, Region::UNIT, 7),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        let first = layer.as_slice()[0];
        assert!(
            layer.as_slice().iter().any(|&v| v != first),
            "ridged noise should vary across the field"
        );
    }

    #[test]
    fn the_seed_param_rerolls_just_this_node() {
        let ctx = default_ctx();
        let base = run(&Params::default(), &ctx);
        let rerolled = run(&Params::new().with("seed", ParamValue::Int(1)), &ctx);
        assert_ne!(base.content_hash(), rerolled.content_hash());
    }

    #[test]
    fn differs_from_fbm_at_the_same_seed() {
        // The ridged fold must actually change the output, not reproduce plain fBm.
        let ctx = default_ctx();
        let ridged = run(&Params::default(), &ctx);
        let fbm = crate::Fbm
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap()
            .remove(0);
        assert_ne!(ridged.content_hash(), fbm.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("ridged operator is registered");
        let via_registry = made
            .eval(
                Inputs::required_only(&[]),
                &Params::default(),
                &default_ctx(),
            )
            .unwrap();
        let direct = run(&Params::default(), &default_ctx());
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Ridged.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Ridged.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // Fixed fingerprint so a change to the ridged math fails here.
        let out = run(&Params::default(), &default_ctx());
        assert_eq!(out.content_hash().to_u64(), 0xc8d7_a548_62a5_88de);
    }
}
