//! The Cellular Regions generator: Worley cell ids as flat, discrete regions.
//!
//! Gives every cell a flat random value, so the field partitions into discrete regions
//! with hard boundaries: plates, tiles, a population of zones. Its value is as a control
//! field rather than terrain directly: pick "where each region is" and shape or scatter
//! per region. It is one of the three Cellular generators, all sharing the Worley
//! computation in `noise.rs`.
//!
//! `frequency` sets how many regions there are and `jitter` how irregular their shapes
//! are (0 is a square grid of regions, 1 is fully organic cells). Sampled in world
//! coordinates, so it is resolution-independent, and seeded from the world seed plus the
//! per-node `seed`, so it is deterministic and rerollable.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

use crate::noise::{WorleyFeature, WorleyParams, worley_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.cellular_regions";

/// Default cell density (region count).
const DEFAULT_FREQUENCY: f64 = 8.0;
/// Default jitter: fully organic region shapes.
const DEFAULT_JITTER: f64 = 1.0;

/// Cellular Regions generator: no inputs, one output.
#[derive(Clone)]
pub struct CellularRegions;

impl Operator for CellularRegions {
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
                // Per-node seed: rerolls the region values and shapes without a new node
                // or touching the world seed. Mixed into the node's derived seed; 0 is
                // unchanged.
                ParamSpec::new(
                    "seed",
                    ParamKind::Int {
                        min: 0,
                        max: i64::from(i32::MAX),
                    },
                    ParamValue::Int(0),
                ),
            ],
        }
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let worley = WorleyParams {
            frequency: params.get_f64("frequency", DEFAULT_FREQUENCY),
            jitter: params.get_f64("jitter", DEFAULT_JITTER).clamp(0.0, 1.0) as f32,
        };
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = worley_field(
            ctx.width,
            ctx.height,
            ctx.region,
            worley,
            WorleyFeature::Regions,
            seed,
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(CellularRegions) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, layers, registry};

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        CellularRegions
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
        assert!(varies, "regions should differ across the field");
    }

    #[test]
    fn the_seed_param_rerolls_the_regions() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(&Params::default().with("seed", ParamValue::Int(1)), &c);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("cellular_regions operator is registered");
        let c = ctx(32);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(
            CellularRegions.spec().kind(),
            ymir_core::NodeKind::Generator
        );
        assert_eq!(CellularRegions.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let out = run(
            &Params::default().with("frequency", ParamValue::Float(6.0)),
            &ctx(8),
        );
        assert_eq!(out.content_hash().to_u64(), 0xa4a6_a8e3_1504_4743);
    }
}
