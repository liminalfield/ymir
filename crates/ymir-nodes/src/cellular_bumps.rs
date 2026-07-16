//! The Cellular Bumps generator: Worley F1 noise as cones peaking at feature points.
//!
//! Scatters one feature point per cell and renders `1 - F1` (one minus the distance to
//! the nearest point), so each point is a cone peak falling off to the cell edges: rock
//! piles, scattered bumps, scales, blisters. It is one of the three Cellular generators,
//! all sharing the Worley computation in `noise.rs`; this one returns the nearest-point
//! distance, Cracks returns the cell edges, Regions returns the cell ids.
//!
//! `frequency` sets the cell density (more cells, smaller bumps) and `jitter` how far the
//! points wander from a regular grid (0 is a grid of cones, 1 is fully organic). Sampled
//! in world coordinates, so it is resolution-independent, and seeded from the world seed
//! plus the per-node `seed`, so it is deterministic and rerollable.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

use crate::noise::{WorleyFeature, WorleyParams, worley_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.cellular_bumps";

/// Default cell density. Higher than fBm's base frequency because each cell is one
/// feature, so a usable field needs more of them than noise needs octave cycles.
const DEFAULT_FREQUENCY: f64 = 8.0;
/// Default jitter: fully organic point placement.
const DEFAULT_JITTER: f64 = 1.0;

/// Cellular Bumps generator: no inputs, one output.
#[derive(Clone)]
pub struct CellularBumps;

impl Operator for CellularBumps {
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
                    ParamValue::Float(DEFAULT_FREQUENCY),
                ),
                ParamSpec::new(
                    "jitter",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_JITTER),
                ),
                // Per-node seed: rerolls this generator's points without a new node or
                // touching the world seed. Mixed into the node's derived seed; 0 is the
                // unchanged default.
                ParamSpec::new(
                    "seed",
                    ParamKind::Int {
                        min: 0,
                        max: i64::from(i32::MAX),
                    },
                    ParamValue::Int(0),
                ),
                // Pan the sampling window (in region widths) to place the cells differently
                // without rerolling, matching the fractal-noise offset.
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
        let worley = WorleyParams {
            frequency: params.get_f64("frequency", DEFAULT_FREQUENCY),
            jitter: params.get_f64("jitter", DEFAULT_JITTER).clamp(0.0, 1.0) as f32,
            offset_x: params.get_i64("offset_x", 0) as f64,
            offset_y: params.get_i64("offset_y", 0) as f64,
        };
        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = worley_field(
            ctx.width,
            ctx.height,
            ctx.region,
            worley,
            WorleyFeature::Bumps,
            seed,
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(CellularBumps) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "cellular", sort: 20 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, layers, registry};

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        CellularBumps
            .eval(Inputs::required_only(&[]), params, ctx)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn eval_is_deterministic() {
        let c = ctx(64);
        let params = Params::default();
        assert_eq!(
            run(&params, &c).content_hash(),
            run(&params, &c).content_hash()
        );
    }

    #[test]
    fn output_stays_in_unit_range_and_varies() {
        let out = run(&Params::default(), &ctx(64));
        let layer = out.layer(layers::HEIGHT).unwrap();
        let first = layer.as_slice()[0];
        let mut varies = false;
        for &v in layer.as_slice() {
            assert!((0.0..=1.0).contains(&v), "value {v} out of [0, 1]");
            varies |= v != first;
        }
        assert!(varies, "cellular noise should vary across the field");
    }

    #[test]
    fn the_seed_param_rerolls_the_points() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(&Params::default().with("seed", ParamValue::Int(1)), &c);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn the_offset_param_pans_the_field() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(&Params::default().with("offset_x", ParamValue::Int(3)), &c);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("cellular_bumps operator is registered");
        let c = ctx(32);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(CellularBumps.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(CellularBumps.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let out = run(
            &Params::default().with("frequency", ParamValue::Float(6.0)),
            &ctx(8),
        );
        assert_eq!(out.content_hash().to_u64(), 0xa5c8_4714_ed1f_015a);
    }
}
