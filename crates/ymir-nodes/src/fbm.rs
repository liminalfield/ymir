//! The fBm Perlin generator: Ymir's first operator.
//!
//! Besides the noise shape (frequency, octaves, lacunarity, gain), it carries the output's
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
    Params, PortSpec, Result, layers,
};

use crate::noise::{FbmParams, fbm_field};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.fbm";

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
                // Frequency is perceptually logarithmic (octaves): a log slider spreads the
                // usable low end across the track instead of cramming it below the first tick.
                // The floor is a small positive value, since a log axis has no zero.
                ParamSpec::new(
                    "frequency",
                    ParamKind::Float {
                        min: 0.25,
                        max: 64.0,
                    },
                    ParamValue::Float(2.0),
                )
                .logarithmic(),
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
        let fbm = FbmParams {
            frequency: params.get_f64("frequency", 2.0),
            // Range is advisory until the graph/UI validate; clamp defensively so
            // an out-of-range octave count cannot misbehave.
            octaves: params.get_i64("octaves", 5).clamp(0, 32) as u32,
            lacunarity: params.get_f64("lacunarity", 2.0),
            gain: params.get_f64("gain", 0.5) as f32,
            // Integer region-width pan: a different region per step, no fractions.
            offset_x: params.get_i64("offset_x", 0) as f64,
            offset_y: params.get_i64("offset_y", 0) as f64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;
    use ymir_core::registry;

    fn default_ctx() -> EvalContext {
        EvalContext::new(8, 8, Region::UNIT, 42)
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
                &Params::new().with("offset_x", ParamValue::Int(2)),
                &ctx,
            )
            .unwrap();
        assert_ne!(base[0].content_hash(), panned[0].content_hash());
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
