//! The fBm Perlin generator: Ymir's first operator.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params, PortSpec,
    Result,
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
            tags: &["perlin", "fbm", "noise", "generator"],
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
            ],
        }
    }

    fn eval(&self, _inputs: &[&Field], params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let fbm = FbmParams {
            frequency: params.get_f64("frequency", 2.0),
            // Range is advisory until the graph/UI validate; clamp defensively so
            // an out-of-range octave count cannot misbehave.
            octaves: params.get_i64("octaves", 5).clamp(0, 32) as u32,
            lacunarity: params.get_f64("lacunarity", 2.0),
            gain: params.get_f64("gain", 0.5) as f32,
        };

        let field = fbm_field(ctx.width, ctx.height, ctx.region, fbm, ctx.seed);
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
        let a = op.eval(&[], &params, &ctx).unwrap();
        let b = op.eval(&[], &params, &ctx).unwrap();
        assert_eq!(a[0].content_hash(), b[0].content_hash());
    }

    #[test]
    fn operator_path_matches_noise_golden() {
        // Empty Params -> the operator falls back to the same defaults the math
        // uses, so the operator path must reproduce the noise module's golden
        // fingerprint exactly. "Same bytes," not merely "still works".
        let op = Fbm;
        let out = op.eval(&[], &Params::default(), &default_ctx()).unwrap();
        assert_eq!(out[0].content_hash().to_u64(), 0x6735_0dbf_a122_5544);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("fbm operator is registered");
        let via_registry = made.eval(&[], &Params::default(), &default_ctx()).unwrap();
        let direct = Fbm.eval(&[], &Params::default(), &default_ctx()).unwrap();
        assert_eq!(via_registry[0].content_hash(), direct[0].content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Fbm.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Fbm.spec().type_id, TYPE_ID);
    }
}
