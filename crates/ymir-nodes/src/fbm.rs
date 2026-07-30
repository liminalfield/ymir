//! The fBm Perlin generator: Ymir's first operator.
//!
//! Besides the noise shape (wavelength, octaves, lacunarity, gain), it carries the output's
//! vertical scale directly: `amplitude` scales the [0, 1] shape and `bias` shifts it. This is
//! the vertical counterpart to the existing `offset_x`/`offset_y` horizontal pan, and it makes
//! the common "layer a little high-frequency detail onto a base" workflow a single control on
//! the source (amplitude 0.05, bias -0.025 → centred detail in [-0.025, 0.025]) instead of a
//! Levels node on each side of a Blend. Defaults (amplitude 1, bias 0) leave the output as the
//! plain [0, 1] shape, so existing graphs are unchanged.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::noise::{FbmParams, cycles_per_region, fbm_field, pan_in_region_widths};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.fbm";

/// Default feature size, in world units.
///
/// The old default was 2 cycles per map, and the default world is 1024 m across, so 512 m is the
/// same terrain a new graph produced before. Expressed at that world size on purpose: a default in
/// metres has to be a real size, and this is the one it already was.
const DEFAULT_WAVELENGTH: f64 = 512.0;

/// fBm Perlin noise generator. A generator by arity: no inputs, one output.
#[derive(Clone)]
pub struct Fbm;

impl Operator for Fbm {
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
                //
                // No longer logarithmic. The log axis existed to spread a cramped low end across a
                // slider track; the magnitude ruler (#358) reaches four decades directly, so the
                // control that needed the trick is gone, and this matches `waves`.
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
                // Output vertical scale: amplitude scales the [0, 1] shape, bias shifts it.
                // The pair the user reaches for to set a layer's height directly (a subtle
                // detail layer, a tall base form) without a downstream Levels.
                ParamSpec::new(
                    "amplitude",
                    ParamKind::Float { min: 0.0, max: 4.0 },
                    ParamValue::Float(1.0),
                ),
                ParamSpec::new(
                    "bias",
                    ParamKind::Float {
                        min: -1.0,
                        max: 1.0,
                    },
                    ParamValue::Float(0.0),
                ),
                // Per-node seed: rerolls this generator's texture without a new node
                // or touching the world seed. Mixed into the node's derived seed, so
                // the world seed still reshuffles everything and instances still
                // differ; 0 is the unchanged default.
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

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let fbm = FbmParams {
            frequency: cycles_per_region(
                params.get_f64("wavelength", DEFAULT_WAVELENGTH),
                ctx.world_extent(),
            ),
            // Range is advisory until the graph/UI validate; clamp defensively so
            // an out-of-range octave count cannot misbehave.
            octaves: params.get_i64("octaves", 5).clamp(0, 32) as u32,
            lacunarity: params.get_f64("lacunarity", 2.0),
            gain: params.get_f64("gain", 0.5) as f32,
            offset_x: pan_in_region_widths(params.get_f64("offset_x", 0.0), ctx.world_extent()),
            offset_y: pan_in_region_widths(params.get_f64("offset_y", 0.0), ctx.world_extent()),
        };

        // Offset the node's derived seed by the per-node seed param (0 = unchanged).
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let mut field = fbm_field(ctx.width, ctx.height, ctx.region, fbm, seed);

        // Apply the output vertical scale. The identity case (amplitude 1, bias 0) returns the
        // shape untouched, so the default path stays byte-for-byte the noise golden.
        let amplitude = params.get_f64("amplitude", 1.0) as f32;
        let bias = params.get_f64("bias", 0.0) as f32;
        if amplitude != 1.0 || bias != 0.0 {
            let scaled = {
                let shape = field.layer_or(layers::HEIGHT, 0.0);
                // Per-cell pure map: byte-identical regardless of thread count.
                Layer::from_par_fn(ctx.width, ctx.height, |x, y| {
                    shape.get(x, y).unwrap_or(0.0) * amplitude + bias
                })
            };
            field.set_layer(layers::HEIGHT, Arc::new(scaled));
        }
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Fbm) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "noise", sort: 10 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::registry;

    /// The default world, 1024 m across, which is what the editor starts a project at.
    ///
    /// Stated rather than left at the context's unit default: the wavelength is in world units now,
    /// so a context with no world describes a 1 m map, on which a 512 m feature is half a cycle.
    fn default_ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 42).with_world_extent(1024.0)
    }

    #[test]
    fn eval_is_deterministic() {
        let op = Fbm;
        let params = Params::default();
        let ctx = EvalContext::new(64, 64, Region::UNIT, 99);
        let a = op.eval(Inputs::required_only(&[]), &params, &ctx).unwrap();
        let b = op.eval(Inputs::required_only(&[]), &params, &ctx).unwrap();
        assert_eq!(a[0].content_hash(), b[0].content_hash());
    }

    #[test]
    fn operator_path_matches_noise_golden() {
        // Empty Params -> the operator falls back to the same defaults the math
        // uses, so the operator path must reproduce the noise module's golden
        // fingerprint exactly. "Same bytes," not merely "still works".
        let op = Fbm;
        let out = op
            .eval(
                Inputs::required_only(&[]),
                &Params::default(),
                &default_ctx(),
            )
            .unwrap();
        assert_eq!(out[0].content_hash().to_u64(), 0xb075_6620_1b58_4592);
    }

    #[test]
    fn the_seed_param_rerolls_just_this_node() {
        // Bumping the per-node seed changes the texture, at the same context (same
        // world seed and stable identity), with no new node.
        let op = Fbm;
        let ctx = default_ctx();
        let base = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap();
        let rerolled = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("seed", ParamValue::Int(1)),
                &ctx,
            )
            .unwrap();
        assert_ne!(base[0].content_hash(), rerolled[0].content_hash());
    }

    #[test]
    fn the_offset_param_pans_the_texture() {
        let op = Fbm;
        let ctx = default_ctx();
        let base = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap();
        let panned = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("offset_x", ParamValue::Float(2.0 * 1024.0)),
                &ctx,
            )
            .unwrap();
        assert_ne!(base[0].content_hash(), panned[0].content_hash());
    }

    #[test]
    fn widening_the_world_keeps_the_terrain_where_it_was() {
        // The reason the pan is in metres (#363). Widening the world should show more ground around
        // what you framed, not slide the frame somewhere else. The pan used to be in region widths,
        // so its real distance was offset * world_extent: widening the world panned further, and the
        // patch you had chosen moved out from under you.
        //
        // Checked at the shared corner. The 4 km world starts at the same field position as the 1 km
        // one and carries on for three times as far, so its first quarter is the whole of the small
        // world, cell for cell, at four times the resolution to put the samples in the same places.
        let op = Fbm;
        let params = Params::new()
            .with("wavelength", ParamValue::Float(256.0))
            .with("offset_x", ParamValue::Float(3000.0))
            .with("offset_y", ParamValue::Float(-1500.0));
        let small = op
            .eval(
                Inputs::required_only(&[]),
                &params,
                &EvalContext::new(32, 32, Region::UNIT, 5).with_world_extent(1024.0),
            )
            .unwrap();
        let large = op
            .eval(
                Inputs::required_only(&[]),
                &params,
                &EvalContext::new(128, 128, Region::UNIT, 5).with_world_extent(4096.0),
            )
            .unwrap();

        let small_h = small[0].layer(layers::HEIGHT).unwrap();
        let large_h = large[0].layer(layers::HEIGHT).unwrap();
        for y in 0..32 {
            for x in 0..32 {
                let a = small_h.get(x, y).unwrap_or(0.0);
                let b = large_h.get(x, y).unwrap_or(0.0);
                assert!(
                    (a - b).abs() < 1e-6,
                    "cell ({x}, {y}) moved when the world grew: {a} then {b}"
                );
            }
        }
    }

    #[test]
    fn the_pan_reaches_a_neighbouring_patch_not_only_a_whole_map_away() {
        // The offset used to be an integer in region widths, so the smallest pan available moved the
        // sampling window by the entire terrain: every step was a different view rather than a slide
        // through the field (#360). A fraction of a width has to do something.
        let op = Fbm;
        let ctx = default_ctx();
        let base = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap();
        let nudged = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("offset_x", ParamValue::Float(10.0)),
                &ctx,
            )
            .unwrap();
        assert_ne!(
            base[0].content_hash(),
            nudged[0].content_hash(),
            "a hundredth of a map must pan the noise"
        );
    }

    #[test]
    fn a_feature_keeps_its_real_size_when_the_world_grows() {
        // The whole point of the change. A 256 m feature on a 1 km world and the same 256 m feature
        // on a 4 km world are the same landform; the larger world simply holds more of them. Before,
        // the parameter was cycles per map, so growing the world made every feature four times
        // larger in metres and no graph meant anything without knowing the world size.
        let op = Fbm;
        let wavelength = Params::new().with("wavelength", ParamValue::Float(256.0));
        let small = EvalContext::new(64, 64, Region::UNIT, 3).with_world_extent(1024.0);
        let large = EvalContext::new(64, 64, Region::UNIT, 3).with_world_extent(4096.0);

        let a = op
            .eval(Inputs::required_only(&[]), &wavelength, &small)
            .unwrap();
        let b = op
            .eval(Inputs::required_only(&[]), &wavelength, &large)
            .unwrap();
        assert_ne!(
            a[0].content_hash(),
            b[0].content_hash(),
            "four times the world at a fixed feature size must sample four times as much noise"
        );

        // And the converse: the terrain that used to come out of one cycles-per-map value is still
        // reachable, by asking for the size that value described on each world.
        let quarter = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("wavelength", ParamValue::Float(1024.0)),
                &large,
            )
            .unwrap();
        assert_eq!(
            a[0].content_hash(),
            quarter[0].content_hash(),
            "a quarter of the world is a quarter of the world, whatever the world measures"
        );
    }

    #[test]
    fn a_world_with_no_extent_still_makes_noise() {
        // A context that never had a world set describes a 1 m map. Falling back to one cycle
        // across it beats dividing by zero, and beats a field of NaN.
        let op = Fbm;
        let ctx = EvalContext::new(16, 16, Region::UNIT, 1);
        let out = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap();
        let layer = out[0].layer(layers::HEIGHT).unwrap();
        assert!(
            layer.as_slice().iter().all(|v| v.is_finite()),
            "no NaN from a degenerate world"
        );
    }

    #[test]
    fn amplitude_scales_and_bias_shifts_the_output() {
        let op = Fbm;
        let ctx = default_ctx();
        let base = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap();
        let base_layer = base[0].layer(layers::HEIGHT).unwrap();
        let (base_lo, base_hi) = base_layer.value_range();

        // amplitude 0 collapses all variation; bias sets the resulting flat level.
        let flat = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new()
                    .with("amplitude", ParamValue::Float(0.0))
                    .with("bias", ParamValue::Float(0.3)),
                &ctx,
            )
            .unwrap();
        for &v in flat[0].layer(layers::HEIGHT).unwrap().as_slice() {
            assert!(
                (v - 0.3).abs() < 1e-6,
                "amplitude 0 should flatten to bias, got {v}"
            );
        }

        // Halving amplitude halves the spread; the shape is otherwise the same.
        let half = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("amplitude", ParamValue::Float(0.5)),
                &ctx,
            )
            .unwrap();
        let (half_lo, half_hi) = half[0].layer(layers::HEIGHT).unwrap().value_range();
        assert!(
            ((half_hi - half_lo) - 0.5 * (base_hi - base_lo)).abs() < 1e-6,
            "amplitude 0.5 should halve the range"
        );
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("fbm operator is registered");
        let via_registry = made
            .eval(
                Inputs::required_only(&[]),
                &Params::default(),
                &default_ctx(),
            )
            .unwrap();
        let direct = Fbm
            .eval(
                Inputs::required_only(&[]),
                &Params::default(),
                &default_ctx(),
            )
            .unwrap();
        assert_eq!(via_registry[0].content_hash(), direct[0].content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Fbm.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Fbm.spec().type_id, TYPE_ID);
    }
}
