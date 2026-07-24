//! Domain Warp: displaces the height layer by a noise field, the organic-grain primitive.
//!
//! Warping pushes every point sideways by a smooth fBm offset, so straight features wander
//! and regular shapes turn natural: a ridge meanders, a coastline frays, a radial envelope
//! stops looking like a circle. It is the most-reached-for trick in the Substance-style
//! toolkit, and the cheapest way to take the machine look off a graph.
//!
//! A true domain warp re-evaluates the *source function* at displaced coordinates, but a
//! node operates on an arbitrary input `Field` (already a sampled grid, not a continuous
//! function), so this *bakes* the warp: it resamples the grid (bilinear, clamped at the
//! edge) at noise-offset positions. The cost of baking is that resampling softens detail a
//! little and a large displacement smears, so warp at adequate resolution and before
//! adding the very finest detail. The displacement distance (`amount`) is in world units
//! and the noise is sampled in world coordinates, so the warp pattern is
//! resolution-independent even though the resampling is a raster operation.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::noise::{FbmParams, fbm_sample};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.warp";

/// Default displacement in world units (meters). Pairs with the default world extent to
/// give a visible but not violent wander out of the box.
const DEFAULT_AMOUNT: f64 = 50.0;

/// Decorrelation salt for the y-displacement seed, so the two offset fields are
/// independent rather than the same noise on both axes (which would warp diagonally).
const WARP_Y_SALT: u64 = 0xD1B5_4A32_D192_ED03;

/// Domain Warp modifier: one input, one output.
#[derive(Clone)]
pub struct Warp;

impl Operator for Warp {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "amount",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_AMOUNT),
                )
                .with_unit(Unit::Meters),
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
                    ParamValue::Int(4),
                ),
                // Per-node seed, mixed into the derived seed; 0 is the unchanged default,
                // matching the generators.
                ParamSpec::new(
                    "seed",
                    ParamKind::Int {
                        min: 0,
                        max: i64::from(i32::MAX),
                    },
                    ParamValue::Int(0),
                ),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads only the world horizontal extent (a world-unit param), not the world height or
    /// sea level, so those two sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let region = input.region();

        // The displacement distance is a world-unit length, converted to cells like the
        // other world-unit parameters, so the same amount moves the same ground at any
        // resolution.
        let amount_m = params.get_f64("amount", DEFAULT_AMOUNT).max(0.0);
        let amount_cells = ctx.world_to_cells(amount_m) as f32;
        let frequency = params.get_f64("frequency", 2.0) as f32;

        // The two offset fields share the octave layering; only the seed differs. The base
        // frequency is applied by scaling the sample coordinates below.
        let warp = FbmParams {
            frequency: 1.0,
            octaves: params.get_i64("octaves", 4).clamp(0, 32) as u32,
            lacunarity: 2.0,
            gain: 0.5,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let seed_x = seed;
        let seed_y = seed ^ WARP_Y_SALT;

        let warped = Layer::from_fn(width, height, |x, y| {
            // World (region) coordinates of this cell, scaled by frequency for the noise.
            let u = region.min_x + (x as f64 + 0.5) / width as f64 * region.width();
            let v = region.min_y + (y as f64 + 0.5) / height as f64 * region.height();
            let nx = u as f32 * frequency;
            let ny = v as f32 * frequency;
            // Independent x and y offsets in cells.
            let dx = fbm_sample(seed_x, nx, ny, warp) * amount_cells;
            let dy = fbm_sample(seed_y, nx, ny, warp) * amount_cells;
            // Resample the input at the displaced position, clamped at the edge.
            sample_bilinear(&h, x as f32 + dx, y as f32 + dy, width, height)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(warped));
        Ok(vec![out])
    }
}

/// Bilinearly samples `layer` at cell-space position `(px, py)` (an integer coordinate is
/// a cell centre), clamping out-of-range reads to the edge so a displacement past the
/// border holds the boundary value rather than wrapping or reading zero.
fn sample_bilinear(layer: &Layer, px: f32, py: f32, width: usize, height: usize) -> f32 {
    let x0 = px.floor();
    let y0 = py.floor();
    let tx = px - x0;
    let ty = py - y0;
    let last_x = width as isize - 1;
    let last_y = height as isize - 1;
    let clamp_x = |v: isize| v.clamp(0, last_x.max(0)) as usize;
    let clamp_y = |v: isize| v.clamp(0, last_y.max(0)) as usize;
    let xa = clamp_x(x0 as isize);
    let xb = clamp_x(x0 as isize + 1);
    let ya = clamp_y(y0 as isize);
    let yb = clamp_y(y0 as isize + 1);

    let v00 = layer.get(xa, ya).unwrap_or(0.0);
    let v10 = layer.get(xb, ya).unwrap_or(0.0);
    let v01 = layer.get(xa, yb).unwrap_or(0.0);
    let v11 = layer.get(xb, yb).unwrap_or(0.0);
    let top = v00 + (v10 - v00) * tx;
    let bottom = v01 + (v11 - v01) * tx;
    top + (bottom - top) * ty
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Warp) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(res: usize, world_extent: f64) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 7).with_world_extent(world_extent)
    }

    /// A ramp left-to-right, so a horizontal displacement actually changes the values.
    fn ramp(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| x as f32 / (res - 1) as f32)),
        )
    }

    fn run(input: &Field, amount: f64, ctx: &EvalContext) -> Field {
        let params = Params::new().with("amount", ParamValue::Float(amount));
        Warp.eval(Inputs::required_only(&[input]), &params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    fn deviation(a: &Field, b: &Field) -> f32 {
        a.layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .zip(b.layer(layers::HEIGHT).unwrap().as_slice())
            .map(|(p, q)| (p - q).abs())
            .sum()
    }

    #[test]
    fn zero_amount_is_identity() {
        // No displacement samples each cell at its own centre, so the field is untouched.
        let input = ramp(32);
        let out = run(&input, 0.0, &ctx(32, 32.0));
        for y in 0..32 {
            for x in 0..32 {
                assert!((at(&out, x, y) - at(&input, x, y)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_positive_amount_warps_the_field() {
        let input = ramp(32);
        let warped = run(&input, 12.0, &ctx(32, 32.0));
        assert_ne!(input.content_hash(), warped.content_hash());
    }

    #[test]
    fn a_flat_field_stays_flat() {
        // Warp only displaces existing values; resampling a constant is the constant, so
        // it never injects new relief (proving it is a resample, not added noise).
        let flat = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.42)));
        let out = run(&flat, 30.0, &ctx(32, 32.0));
        for &v in out.layer(layers::HEIGHT).unwrap().as_slice() {
            assert!((v - 0.42).abs() < 1e-6, "flat field gained relief: {v}");
        }
    }

    #[test]
    fn a_larger_amount_warps_more() {
        let input = ramp(32);
        let c = ctx(32, 32.0);
        let small = deviation(&run(&input, 6.0, &c), &input);
        let large = deviation(&run(&input, 18.0, &c), &input);
        assert!(
            large > small,
            "more displacement should warp more: {large} vs {small}"
        );
    }

    #[test]
    fn the_seed_changes_the_warp() {
        let input = ramp(32);
        let c = ctx(32, 32.0);
        let base = run(&input, 12.0, &c);
        let rerolled = Warp
            .eval(
                Inputs::required_only(&[&input]),
                &Params::new()
                    .with("amount", ParamValue::Float(12.0))
                    .with("seed", ParamValue::Int(1)),
                &c,
            )
            .unwrap()
            .remove(0);
        assert_ne!(base.content_hash(), rerolled.content_hash());
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(32);
        input.set_layer("flow", Arc::new(Layer::filled(32, 32, 0.9)));
        let out = run(&input, 12.0, &ctx(32, 32.0));
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn resampling_stays_in_the_input_range() {
        // Bilinear interpolation of values in [0, 1] stays in [0, 1].
        let out = run(&ramp(32), 30.0, &ctx(32, 32.0));
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(32);
        let c = ctx(32, 32.0);
        assert_eq!(
            run(&input, 12.0, &c).content_hash(),
            run(&input, 12.0, &c).content_hash()
        );
    }

    #[test]
    fn spec_is_a_modifier() {
        assert_eq!(Warp.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(Warp.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let out = run(&ramp(16), 8.0, &ctx(16, 16.0));
        assert_eq!(out.content_hash().to_u64(), 0xcae5_38cd_50cd_a4d7);
    }
}
