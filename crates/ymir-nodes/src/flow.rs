//! The flow generator: curl-warped noise with a divergence-free flow field.
//!
//! Warps a noise lookup along the curl streamlines of a noise potential, giving a swirly,
//! marbled, fluid-looking `height` layer (swirled strata, fluvial grain). Because the
//! curl is divergence-free (no sources or sinks), the warp swirls without the pinching of
//! a plain domain warp. As a byproduct the node also writes the flow vector to the
//! `flow_x` / `flow_y` layers, so a later directional-warp or erosion-grain node can read
//! the direction field off the edge (the curl math itself lives in [`crate::noise`] as
//! `curl2`, reusable by those nodes).
//!
//! Shares the octave-layering parameters and resolution-independent sampling with the
//! other noise generators; `strength` sets how far the lookup is displaced along the flow
//! (0 is plain fBm).

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

use crate::noise::{FbmParams, flow_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.flow";

/// Default flow strength: how far the noise lookup is displaced along the curl flow.
const DEFAULT_STRENGTH: f64 = 0.4;

/// Flow (curl-warped) noise generator. A generator by arity: no inputs, one output.
#[derive(Clone)]
pub struct Flow;

impl Operator for Flow {
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
                // How far the noise lookup is warped along the curl flow. 0 is plain fBm;
                // higher values swirl more.
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 4.0 },
                    ParamValue::Float(DEFAULT_STRENGTH),
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
        let strength = params.get_f64("strength", DEFAULT_STRENGTH) as f32;

        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = flow_field(ctx.width, ctx.height, ctx.region, fractal, strength, seed);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Flow) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "noise", sort: 14 }
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
        Flow.eval(Inputs::required_only(&[]), params, ctx)
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
    fn output_stays_in_unit_range_and_varies() {
        let out = run(
            &Params::default(),
            &EvalContext::new(96, 96, Region::UNIT, 7),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        let first = layer.as_slice()[0];
        let mut varies = false;
        for &v in layer.as_slice() {
            assert!((0.0..=1.0).contains(&v), "value {v} out of [0, 1]");
            varies |= v != first;
        }
        assert!(varies, "flow noise should vary across the field");
    }

    #[test]
    fn writes_the_flow_vector_layers() {
        // The divergence-free flow vector is carried on flow_x / flow_y for later
        // direction-field consumers; it must be present and not all zero.
        let out = run(
            &Params::default(),
            &EvalContext::new(32, 32, Region::UNIT, 7),
        );
        let fx = out.layer(layers::FLOW_X).expect("flow_x layer present");
        let fy = out.layer(layers::FLOW_Y).expect("flow_y layer present");
        assert!(fx.as_slice().iter().any(|&v| v != 0.0));
        assert!(fy.as_slice().iter().any(|&v| v != 0.0));
    }

    #[test]
    fn strength_changes_the_swirl() {
        // Warping along the flow must actually change the height; strength 0 is plain fBm.
        let ctx = default_ctx();
        let none = run(
            &Params::default().with("strength", ParamValue::Float(0.0)),
            &ctx,
        );
        let swirled = run(
            &Params::default().with("strength", ParamValue::Float(1.0)),
            &ctx,
        );
        assert_ne!(none.content_hash(), swirled.content_hash());
    }

    #[test]
    fn the_seed_param_rerolls_just_this_node() {
        let ctx = default_ctx();
        let base = run(&Params::default(), &ctx);
        let rerolled = run(&Params::new().with("seed", ParamValue::Int(1)), &ctx);
        assert_ne!(base.content_hash(), rerolled.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("flow operator is registered");
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
        assert_eq!(Flow.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Flow.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // Fixed fingerprint so a change to the flow math fails here.
        let out = run(&Params::default(), &default_ctx());
        assert_eq!(out.content_hash().to_u64(), 0x91bc_223e_4db1_0fe3);
    }
}
