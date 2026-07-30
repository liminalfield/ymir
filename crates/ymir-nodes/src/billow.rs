//! The billow generator: puffy, rounded mounds and dunes.
//!
//! A sibling of the fBm generator that folds each octave with `2|n| - 1` before summing,
//! so the noise's extremes become rounded bumps and its zero-crossings become creased
//! valleys: the rounded inverse of the ridged fold (ridged points up at crests, billow
//! bulges round). The three share the octave-layering parameters and the same
//! resolution-independent sampling; the terrain math lives in [`crate::noise`].

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result, Unit,
};

use crate::noise::{FbmParams, billow_field, cycles_per_region, pan_in_region_widths};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.billow";

/// Default feature size, in world units.
///
/// The old default was 2 cycles per map, and the default world is 1024 m across, so 512 m is the
/// same terrain a new graph produced before.
const DEFAULT_WAVELENGTH: f64 = 512.0;

/// Billow noise generator. A generator by arity: no inputs, one output.
#[derive(Clone)]
pub struct Billow;

impl Operator for Billow {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                // The base octave's period, in world units: the size of the largest features the
                // noise makes. Replaces a cycles-per-map frequency, which meant a different
                // landform on every world size and capped features at a 64th of the map.
                ParamSpec::new(
                    "wavelength",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_WAVELENGTH),
                )
                .with_unit(Unit::Meters),
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
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "offset_y",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Meters),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads the world extent, which sets how many cycles of the wavelength span the map. Sea level
    /// and world height are still nothing to do with this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    /// A window onto an unbounded coherent-noise field: `offset_x` / `offset_y` slide across it, so
    /// there is more of it to look at than the map shows.
    fn pannable_field(&self) -> bool {
        true
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let fractal = FbmParams {
            frequency: cycles_per_region(
                params.get_f64("wavelength", DEFAULT_WAVELENGTH),
                ctx.world_extent(),
            ),
            // Range is advisory until the graph/UI validate; clamp defensively.
            octaves: params.get_i64("octaves", 5).clamp(0, 32) as u32,
            lacunarity: params.get_f64("lacunarity", 2.0),
            gain: params.get_f64("gain", 0.5) as f32,
            offset_x: pan_in_region_widths(params.get_f64("offset_x", 0.0), ctx.world_extent()),
            offset_y: pan_in_region_widths(params.get_f64("offset_y", 0.0), ctx.world_extent()),
        };

        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = billow_field(ctx.width, ctx.height, ctx.region, fractal, seed);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Billow) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "noise", sort: 11 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::layers;
    use ymir_core::registry;

    /// The default world, 1024 m across, which is what the editor starts a project at.
    ///
    /// Stated rather than left at the context's unit default: the wavelength is in world units now,
    /// so a context with no world describes a 1 m map, on which a 512 m feature is half a cycle.
    fn default_ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 42).with_world_extent(1024.0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        Billow
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
            "billow noise should vary across the field"
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
        // The billow fold must actually change the output, not reproduce plain fBm.
        let ctx = default_ctx();
        let billow = run(&Params::default(), &ctx);
        let fbm = crate::Fbm
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap()
            .remove(0);
        assert_ne!(billow.content_hash(), fbm.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("billow operator is registered");
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
        assert_eq!(Billow.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Billow.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // Fixed fingerprint so a change to the billow math fails here.
        let out = run(&Params::default(), &default_ctx());
        assert_eq!(out.content_hash().to_u64(), 0xe5e6_e14d_3931_f6a5);
    }
}
