//! The hybrid-multifractal generator: realistic plains-to-mountains terrain.
//!
//! A sibling of the fBm generator whose octave contributions are weighted by the terrain
//! accumulated so far (Musgrave's hybrid multifractal), so detail piles onto high ground
//! while low ground stays smooth and flat. Where fBm gives uniform roughness everywhere,
//! hybrid gives the natural mix of flat valleys and broken peaks from one node, without
//! hand-masking. The family shares the octave-layering parameters and resolution-
//! independent sampling; the terrain math lives in [`crate::noise`].
//!
//! Beyond the shared vocabulary it adds `bias` (Musgrave's offset): the altitude lift
//! applied before the weighting, which sets how much of the field reads as rough highland
//! versus smooth lowland.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

use crate::noise::{FbmParams, hybrid_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.hybrid";

/// Default altitude bias (Musgrave's offset). Around 0.7 gives a balanced mix of smooth
/// lowland and rough highland.
const DEFAULT_BIAS: f64 = 0.7;

/// Hybrid-multifractal noise generator. A generator by arity: no inputs, one output.
#[derive(Clone)]
pub struct Hybrid;

impl Operator for Hybrid {
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
                // Altitude bias (Musgrave's offset): lifts the field before the
                // multifractal weighting, so higher values push more of the terrain into
                // the rough highland range and lower values leave more smooth lowland.
                ParamSpec::new(
                    "bias",
                    ParamKind::Float { min: 0.0, max: 2.0 },
                    ParamValue::Float(DEFAULT_BIAS),
                ),
                // Per-node seed: rerolls this generator's texture without a new node or
                // touching the world seed. Mixed into the node's derived seed, so 0 is the
                // unchanged default. Mirrors the fBm and ridged generators.
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
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::NO_WORLD
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
        let bias = params.get_f64("bias", DEFAULT_BIAS) as f32;

        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = hybrid_field(ctx.width, ctx.height, ctx.region, fractal, bias, seed);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Hybrid) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "noise", sort: 13 }
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
        Hybrid
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
            "hybrid noise should vary across the field"
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
    fn the_bias_changes_the_terrain() {
        // The altitude bias must actually reshape the field.
        let ctx = default_ctx();
        let low = run(
            &Params::default().with("bias", ParamValue::Float(0.3)),
            &ctx,
        );
        let high = run(
            &Params::default().with("bias", ParamValue::Float(1.2)),
            &ctx,
        );
        assert_ne!(low.content_hash(), high.content_hash());
    }

    #[test]
    fn differs_from_fbm_at_the_same_seed() {
        // The multifractal weighting must change the output, not reproduce plain fBm.
        let ctx = default_ctx();
        let hybrid = run(&Params::default(), &ctx);
        let fbm = crate::Fbm
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap()
            .remove(0);
        assert_ne!(hybrid.content_hash(), fbm.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("hybrid operator is registered");
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
        assert_eq!(Hybrid.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Hybrid.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // Fixed fingerprint so a change to the hybrid math fails here. Updated when the
        // octave-envelope normalization changed from the infinite-octave closed form to the
        // finite geometric sum, which rescales the output (the form is unchanged).
        let out = run(&Params::default(), &default_ctx());
        assert_eq!(out.content_hash().to_u64(), 0x4fa2_4f50_6559_bf1b);
    }
}
