//! The Cellular Cracks generator: Worley F2-F1 noise as a cell-edge network.
//!
//! Renders `1 - (F2 - F1)`, which is bright exactly where the nearest and second-nearest
//! feature points are equidistant (the cell boundaries) and dark in the cell interiors:
//! a network of cracks, fracture lines, dried mud, rocky cell walls. It is one of the
//! three Cellular generators, all sharing the Worley computation in `noise.rs`.
//!
//! `frequency` sets the cell density (more cells, finer crack network) and `jitter` how
//! far the points wander from a regular grid (0 is a square lattice of cracks, 1 is fully
//! organic). Sampled in world coordinates, so it is resolution-independent, and seeded
//! from the world seed plus the per-node `seed`, so it is deterministic and rerollable.

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result, Unit,
};

use crate::noise::{
    Placement, RegionOptions, WorleyFeature, WorleyParams, cycles_per_region, pan_in_region_widths,
    worley_field,
};

/// Stable type identifier and registry key.
/// Placement ids: where the feature points sit before jitter moves them. `square` is the original
/// behaviour and stays the default, so every existing project is unchanged. Named to match
/// `design/scatter.md`'s "Placement strategy", so Scatter can adopt the same vocabulary (#346).
const PLACEMENT_SQUARE: &str = "square";
const PLACEMENT_HEX: &str = "hex";
const PLACEMENTS: &[&str] = &[PLACEMENT_SQUARE, PLACEMENT_HEX];

const TYPE_ID: &str = "generator.cellular_cracks";

/// Default cell width, in world units.
///
/// The old default was 8 cells per map, and the default world is 1024 m across, so 128 m is the
/// same cells a new graph produced before.
const DEFAULT_CELL_SIZE: f64 = 128.0;
/// Default jitter: fully organic point placement.
const DEFAULT_JITTER: f64 = 1.0;

/// Cellular Cracks generator: no inputs, one output.
#[derive(Clone)]
pub struct CellularCracks;

impl Operator for CellularCracks {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                // The width of one cell, in world units. Replaces a cells-per-map count, which
                // meant a different cell size on every world size and could not go below a 64th
                // of the map.
                ParamSpec::new(
                    "cell_size",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_CELL_SIZE),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "jitter",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_JITTER),
                ),
                // Per-node seed: rerolls the network without a new node or touching the
                // world seed. Mixed into the node's derived seed; 0 is unchanged.
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

    /// Reads the world extent, which sets how many cells of the given size span the map. Sea level
    /// and world height are still nothing to do with this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let placement = if params.get_str("placement", PLACEMENT_SQUARE) == PLACEMENT_HEX {
            Placement::Hex
        } else {
            Placement::Square
        };
        let worley = WorleyParams {
            frequency: cycles_per_region(
                params.get_f64("cell_size", DEFAULT_CELL_SIZE),
                ctx.world_extent(),
            ),
            jitter: params.get_f64("jitter", DEFAULT_JITTER).clamp(0.0, 1.0) as f32,
            offset_x: pan_in_region_widths(params.get_f64("offset_x", 0.0), ctx.world_extent()),
            offset_y: pan_in_region_widths(params.get_f64("offset_y", 0.0), ctx.world_extent()),
            placement,
        };
        let seed = ctx.seed.wrapping_add(params.get_i64("seed", 0) as u64);
        let field = worley_field(
            ctx.width,
            ctx.height,
            ctx.region,
            worley,
            WorleyFeature::Cracks,
            seed,
            RegionOptions::default(),
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(CellularCracks) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "cellular", sort: 21 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, layers, registry};

    /// The default world, 1024 m across, which is what the editor starts a project at.
    ///
    /// Stated rather than left at the context's unit default: the cell size is in world units now,
    /// so a context with no world describes a 1 m map, on which a 128 m cell covers everything.
    /// The cell size that divides the test world into `n` cells, which is what these tests asked
    /// for when the parameter counted cells. The round trip through the world extent is exact in
    /// f64, so the goldens below still pin the same Worley output they always have.
    fn cells(n: f64) -> f64 {
        1024.0 / n
    }

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0).with_world_extent(1024.0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        CellularCracks
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
        assert!(varies, "the crack network should vary across the field");
    }

    #[test]
    fn the_seed_param_rerolls_the_network() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(&Params::default().with("seed", ParamValue::Int(1)), &c);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn the_offset_param_pans_the_field() {
        let c = ctx(64);
        let a = run(&Params::default(), &c);
        let b = run(
            &Params::default().with("offset_x", ParamValue::Float(3.0 * 1024.0)),
            &c,
        );
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("cellular_cracks operator is registered");
        let c = ctx(32);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(CellularCracks.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(CellularCracks.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let out = run(
            &Params::default().with("cell_size", ParamValue::Float(cells(6.0))),
            &ctx(8),
        );
        assert_eq!(out.content_hash().to_u64(), 0xf7d8_5525_df5b_f42c);
    }
}
