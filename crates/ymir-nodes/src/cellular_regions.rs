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
    PortSpec, Result, layers,
};

use crate::noise::{
    Placement, RegionOptions, RegionValues, WorleyFeature, WorleyParams, worley_field,
};

/// Stable type identifier and registry key.
/// Placement ids: where the feature points sit before jitter moves them. `square` is the original
/// behaviour and stays the default, so every existing project is unchanged. Named to match
/// `design/scatter.md`'s "Placement strategy", so Scatter can adopt the same vocabulary (#346).
/// Whether cell boundaries are antialiased. On by default: a hard boundary stair-steps at any
/// resolution, because the pixel grid can only approximate its angle. Off gives every pixel wholly
/// to one cell, which is what a selection wants when a half-selected pixel would be wrong (#350).
const DEFAULT_ANTIALIAS: bool = true;

const PLACEMENT_SQUARE: &str = "square";
const PLACEMENT_HEX: &str = "hex";
const PLACEMENTS: &[&str] = &[PLACEMENT_SQUARE, PLACEMENT_HEX];

const TYPE_ID: &str = "generator.cellular_regions";

/// Default cell density (region count).
const DEFAULT_FREQUENCY: f64 = 8.0;
/// Default jitter: fully organic region shapes.
const DEFAULT_JITTER: f64 = 1.0;

/// Cellular Regions generator: one optional input, one output.
#[derive(Clone)]
pub struct CellularRegions;

impl Operator for CellularRegions {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            // Optional: the field each cell reads its value from. Unwired, values are a hash of
            // the cell id, which is the original behaviour and still the common case.
            inputs: vec![PortSpec::optional("values")],
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
                ParamSpec::new(
                    "antialias",
                    ParamKind::Bool,
                    ParamValue::Bool(DEFAULT_ANTIALIAS),
                ),
                ParamSpec::new(
                    "placement",
                    ParamKind::Enum {
                        options: PLACEMENTS,
                    },
                    ParamValue::Text(PLACEMENT_SQUARE.to_string()),
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

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let placement = if params.get_str("placement", PLACEMENT_SQUARE) == PLACEMENT_HEX {
            Placement::Hex
        } else {
            Placement::Square
        };
        let worley = WorleyParams {
            frequency: params.get_f64("frequency", DEFAULT_FREQUENCY),
            jitter: params.get_f64("jitter", DEFAULT_JITTER).clamp(0.0, 1.0) as f32,
            offset_x: params.get_i64("offset_x", 0) as f64,
            offset_y: params.get_i64("offset_y", 0) as f64,
            placement,
        };
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        // Each cell takes one value from the wired field, read at the cell's own feature point, so
        // the cell stays flat at whatever that field says there (#347). Unwired, the value is a
        // hash of the cell id as before. Held for the whole call so the borrow outlives the field
        // construction below.
        let source = inputs.optional(0).map(|f| f.layer_or(layers::HEIGHT, 0.0));
        // The blend width for the boundary: one output pixel, converted into the noise units `f1`
        // and `f2` are measured in. Taking the larger axis so a non-square region antialiases on
        // its coarser side rather than under-blending there.
        let antialias = params.get_bool("antialias", DEFAULT_ANTIALIAS).then(|| {
            let per_x = ctx.region.width() / ctx.width.max(1) as f64;
            let per_y = ctx.region.height() / ctx.height.max(1) as f64;
            (per_x.max(per_y) * worley.frequency) as f32
        });
        let field = worley_field(
            ctx.width,
            ctx.height,
            ctx.region,
            worley,
            WorleyFeature::Regions,
            seed,
            RegionOptions {
                values: source.as_deref().map(|layer| RegionValues { layer }),
                antialias,
            },
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(CellularRegions) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "cellular", sort: 22 }
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

    /// Runs with a field wired to the optional `values` input.
    fn run_with(values: &Field, params: &Params, ctx: &EvalContext) -> Field {
        CellularRegions
            .eval(Inputs::new(&[], &[Some(values)]), params, ctx)
            .unwrap()
            .remove(0)
    }

    /// A field whose height rises left to right across `[0, 1]`, so a cell's value says where it
    /// sits along x.
    fn ramp(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            std::sync::Arc::new(ymir_core::Layer::from_fn(res, res, |x, _| {
                f32::from(u16::try_from(x).expect("x fits")) / (res - 1) as f32
            })),
        )
    }

    /// Every distinct value present in a field's height layer, as sorted bit patterns.
    fn distinct(field: &Field) -> Vec<u32> {
        let mut v: Vec<u32> = field
            .layer_or(layers::HEIGHT, 0.0)
            .as_slice()
            .iter()
            .map(|f| f.to_bits())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn an_unwired_input_leaves_the_output_untouched() {
        // The whole point of the soft contract here: adding the port must not change a single
        // existing project. Byte-identical, not merely similar.
        let params = Params::default().with("frequency", ParamValue::Float(8.0));
        let before = run(&params, &ctx(64));
        let after = CellularRegions
            .eval(Inputs::new(&[], &[None]), &params, &ctx(64))
            .unwrap()
            .remove(0);
        assert_eq!(before.content_hash(), after.content_hash());
    }

    #[test]
    fn a_wired_field_gives_each_cell_one_value_from_it() {
        // A ramp rising left to right: cells on the left must come out darker than cells on the
        // right, and each cell must be flat, which is what the hash cannot give.
        // Antialias off: this counts distinct cell values, and the one-pixel boundary blend #350
        // adds would show up as extra values that say nothing about the assignment being tested.
        let params = Params::default()
            .with("frequency", ParamValue::Float(8.0))
            .with("jitter", ParamValue::Float(0.5))
            .with("antialias", ParamValue::Bool(false));
        let out = run_with(&ramp(128), &params, &ctx(128));
        let h = out.layer_or(layers::HEIGHT, 0.0);

        // At most one value per cell: 8x8 cells over the region, so far fewer distinct values
        // than the 16384 pixels. The hash path would give the same bound, so this alone is not
        // enough; the ordering check below is what proves the values came from the ramp.
        assert!(
            distinct(&out).len() <= 128,
            "expected one value per cell, got {}",
            distinct(&out).len()
        );

        // Left edge reads lower than right edge, because the ramp does.
        let left = h.get(2, 64).expect("left");
        let right = h.get(125, 64).expect("right");
        assert!(
            right > left,
            "ramp not followed: left {left}, right {right}"
        );
    }

    #[test]
    fn a_cell_is_flat_even_where_the_source_is_not() {
        // The reason the input exists (#347): a smooth field blended in tilts every cell top,
        // while sampling it once per cell keeps the cell flat. Walk a row and check that values
        // change only in steps, never gradually within a run.
        // Antialias off, so this measures the cell assignment rather than the one-pixel blend at
        // each boundary that #350 adds. That blend is covered separately below.
        let params = Params::default()
            .with("frequency", ParamValue::Float(6.0))
            .with("jitter", ParamValue::Float(0.0))
            .with("antialias", ParamValue::Bool(false));
        let out = run_with(&ramp(96), &params, &ctx(96));
        let h = out.layer_or(layers::HEIGHT, 0.0);
        let row: Vec<f32> = (0..96).map(|x| h.get(x, 48).unwrap_or(0.0)).collect();
        let mut runs = 0;
        for pair in row.windows(2) {
            if (pair[0] - pair[1]).abs() > f32::EPSILON {
                runs += 1;
            }
        }
        // A 6-cell frequency across the row: a handful of steps, not 95 gradual changes.
        assert!(
            runs < 12,
            "row changed {runs} times, expected a few flat runs"
        );
    }

    #[test]
    fn a_wired_input_is_deterministic() {
        let params = Params::default().with("frequency", ParamValue::Float(8.0));
        let source = ramp(64);
        let a = run_with(&source, &params, &ctx(64));
        let b = run_with(&source, &params, &ctx(64));
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn neighbouring_cells_correlate_when_the_source_is_smooth() {
        // The reported symptom (#347): with hashed values a cell that lands high has no
        // neighbours near it, so it reads as a solitary spike. Sourcing from a smooth field
        // should make adjacent cells much closer in value than hashing does.
        let params = Params::default()
            .with("frequency", ParamValue::Float(12.0))
            .with("jitter", ParamValue::Float(0.6));
        let res = 192;
        let step = |field: &Field| -> f32 {
            let h = field.layer_or(layers::HEIGHT, 0.0);
            let row: Vec<f32> = (0..res).map(|x| h.get(x, res / 2).unwrap_or(0.0)).collect();
            let jumps: Vec<f32> = row
                .windows(2)
                .map(|p| (p[0] - p[1]).abs())
                .filter(|d| *d > f32::EPSILON)
                .collect();
            if jumps.is_empty() {
                0.0
            } else {
                jumps.iter().sum::<f32>() / jumps.len() as f32
            }
        };
        let hashed = step(&run(&params, &ctx(res)));
        let sourced = step(&run_with(&ramp(res), &params, &ctx(res)));
        assert!(
            sourced < hashed * 0.5,
            "sourced steps {sourced} not much smaller than hashed {hashed}"
        );
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
    fn the_offset_param_pans_the_field() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(&Params::default().with("offset_x", ParamValue::Int(3)), &c);
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
    fn spec_is_a_source_with_one_optional_input() {
        let spec = CellularRegions.spec();
        assert_eq!(spec.type_id, TYPE_ID);
        // It reads as a source in the palette, which is the presentation question, and that is
        // unchanged by taking an input.
        assert_eq!(spec.category, "generator");
        // But the arity-derived kind is now Modifier, because the node genuinely has an input
        // socket (#347). Nothing in the engine or GUI branches on the kind, and `generator.paint`
        // set this precedent already: an optional input on a source is fine, and the soft contract
        // covers the unwired case.
        assert_eq!(spec.kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(spec.inputs.len(), 1);
        assert!(spec.inputs[0].optional, "the values input is optional");
    }

    #[test]
    fn output_matches_golden_value() {
        // Anchored with antialias off, because that is the raw cell assignment and #350 did not
        // touch it. Keeping this value proves the Worley computation itself still produces exactly
        // what it always has; the antialiased default has its own golden below.
        let out = run(
            &Params::default()
                .with("frequency", ParamValue::Float(6.0))
                .with("antialias", ParamValue::Bool(false)),
            &ctx(8),
        );
        assert_eq!(out.content_hash().to_u64(), 0xa4a6_a8e3_1504_4743);
    }

    #[test]
    fn the_antialiased_default_has_its_own_golden() {
        let out = run(
            &Params::default().with("frequency", ParamValue::Float(6.0)),
            &ctx(8),
        );
        assert_eq!(out.content_hash().to_u64(), 0x7431_c7d1_4598_14e5);
    }

    #[test]
    fn antialiasing_touches_only_the_boundaries() {
        // Interiors must be untouched: the point is to soften the joint, not the cells. Every pixel
        // that moves has to sit within two pixels of a cell boundary.
        //
        // Two rather than one because of cell vertices. `f2 - f1` grows at about twice the distance
        // from a boundary only along the bisector of two cells; where three meet, the second-nearest
        // point changes identity and the difference stays small over a slightly wider patch.
        // Measured on this case, 3922 changed pixels sit on a boundary, 194 one pixel away, 6 two
        // pixels away, and none further.
        let params = |aa: bool| {
            Params::default()
                .with("frequency", ParamValue::Float(9.0))
                .with("antialias", ParamValue::Bool(aa))
        };
        let res = 192_usize;
        let hard = run(&params(false), &ctx(res));
        let soft = run(&params(true), &ctx(res));
        let (h, sf) = (
            hard.layer_or(layers::HEIGHT, 0.0),
            soft.layer_or(layers::HEIGHT, 0.0),
        );
        let on_boundary = |x: usize, y: usize| -> bool {
            let v = h.get(x, y).unwrap_or(0.0);
            (-1..=1_isize).any(|dy| {
                (-1..=1_isize).any(|dx| {
                    let (nx, ny) = (x.wrapping_add_signed(dx), y.wrapping_add_signed(dy));
                    (h.get(nx, ny).unwrap_or(v) - v).abs() > f32::EPSILON
                })
            })
        };
        let mut moved = 0;
        for y in 2..res - 2 {
            for x in 2..res - 2 {
                if (h.get(x, y).unwrap_or(0.0) - sf.get(x, y).unwrap_or(0.0)).abs() <= f32::EPSILON
                {
                    continue;
                }
                moved += 1;
                let near_boundary = (-2..=2_isize).any(|dy| {
                    (-2..=2_isize)
                        .any(|dx| on_boundary(x.wrapping_add_signed(dx), y.wrapping_add_signed(dy)))
                });
                assert!(
                    near_boundary,
                    "pixel ({x}, {y}) changed but is more than two pixels from any boundary"
                );
            }
        }
        assert!(moved > 0, "antialiasing changed nothing");
    }

    #[test]
    fn the_softened_band_stays_one_pixel_as_resolution_rises() {
        // The reason the width is derived from the output grid: a fixed world-space width would
        // cover more pixels as the build grows, so the joint would soften instead of staying sharp.
        // Measure the share of pixels sitting on a boundary blend at two resolutions.
        let share = |res: usize| -> f32 {
            let params = Params::default()
                .with("frequency", ParamValue::Float(9.0))
                .with("antialias", ParamValue::Bool(true));
            let hard = run(
                &params.clone().with("antialias", ParamValue::Bool(false)),
                &ctx(res),
            );
            let soft = run(&params, &ctx(res));
            let (h, sf) = (
                hard.layer_or(layers::HEIGHT, 0.0),
                soft.layer_or(layers::HEIGHT, 0.0),
            );
            let mut moved = 0_usize;
            for y in 0..res {
                for x in 0..res {
                    if (h.get(x, y).unwrap_or(0.0) - sf.get(x, y).unwrap_or(0.0)).abs()
                        > f32::EPSILON
                    {
                        moved += 1;
                    }
                }
            }
            // Boundary pixels scale with perimeter (res), total with area (res^2), so the share
            // should roughly halve when the resolution doubles.
            moved as f32 / (res * res) as f32
        };
        let low = share(128);
        let high = share(256);
        assert!(
            high < low * 0.75,
            "band share {high} at 256 against {low} at 128: not shrinking with resolution"
        );
    }

    #[test]
    fn antialiasing_is_deterministic() {
        let params = Params::default().with("frequency", ParamValue::Float(9.0));
        assert_eq!(
            run(&params, &ctx(96)).content_hash(),
            run(&params, &ctx(96)).content_hash()
        );
    }
}
