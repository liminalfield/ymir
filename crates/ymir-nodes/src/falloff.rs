//! The radial falloff generator: a linear distance ramp, the radial profile workhorse.
//!
//! Produces the normalized radial distance on the `height` layer: 0 at `center`, rising
//! linearly to 1 at `radius`, clamped to 1 beyond. On its own it is a smooth cone that
//! carries no shape opinion, just "how far out am I, as a fraction of the radius." Its
//! point is what comes next: fed into a Curve, the curve's X axis becomes
//! distance-from-center and its Y axis becomes height, so drawing the curve draws a
//! radial landform's cross-section, swept around one center with nothing to align. A
//! dome, a crater (low floor, a rim bump, back to zero), a caldera (flat floor, then
//! rim), a ring, terraces: all are one Falloff and a different Curve.
//!
//! This is why named landforms are not nodes in Ymir (a crater node would be one
//! opinionated crater): the project ships this primitive and the Curve, plus example
//! subgraphs, rather than a node per landform. See `docs/design/shape-generators.md`.
//!
//! `radius` is a world-unit length (meters), converted to cells through the world extent,
//! so the cone keeps the same physical reach at any resolution. The `center` is a
//! normalized position over the whole world, mapped through the evaluated `region`, so a
//! tiled build matches an untiled one.
//!
//! The ramp is linear on purpose, unlike radial's smoothstep dome: a Curve downstream
//! does the shaping, and a linear ramp keeps the curve's X axis a true radial fraction,
//! so a rim drawn at x = 0.7 lands at 0.7 of the radius.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

use crate::shape::center_cell;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.falloff";

/// Default radius in world units (meters). Pairs with the default world extent (1024 m)
/// to give a centered cone that fills most of the map out of the box.
const DEFAULT_RADIUS: f64 = 500.0;

/// Radial falloff generator: no inputs, one output.
#[derive(Clone)]
pub struct Falloff;

impl Operator for Falloff {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            tags: &[
                "falloff",
                "radial",
                "distance",
                "profile",
                "cone",
                "shape",
                "envelope",
                "crater",
                "caldera",
                "generator",
            ],
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "radius",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_RADIUS),
                )
                .with_unit(Unit::Meters),
                // Center as a normalized position over the whole world (0.5 = middle),
                // so it is resolution- and extent-independent and reads as a 0..1 slider.
                ParamSpec::new(
                    "center_x",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
                ParamSpec::new(
                    "center_y",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
            ],
        }
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let width = ctx.width;
        let height = ctx.height;
        let region = ctx.region;

        // World-unit radius to cells, the same bridge radial and Blur use, so the cone
        // reaches the same physical distance at any resolution.
        let radius_cells = ctx.world_to_cells(params.get_f64("radius", DEFAULT_RADIUS).max(0.0));

        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center = center_cell((center_x, center_y), region, width, height);

        let field = falloff_field(width, height, region, center, radius_cells);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Falloff) }
}

/// Builds a field whose `height` layer is the normalized radial distance from `center`
/// (in cells): 0 at the center, rising linearly to 1 at `radius_cells`, clamped to 1
/// beyond. A non-positive `radius_cells` yields a flat field of 1 (the edge has collapsed
/// onto the center, so every cell is at or beyond it) rather than a division by zero.
fn falloff_field(
    width: usize,
    height: usize,
    region: Region,
    center: (f64, f64),
    radius_cells: f64,
) -> Field {
    let (cx, cy) = center;
    let layer = Layer::from_fn(width, height, |x, y| {
        // Distance from the cell center to the falloff center, in cells. Cells are
        // square, so this is isotropic and matches a world-space circle of the radius.
        let dx = (x as f64 + 0.5) - cx;
        let dy = (y as f64 + 0.5) - cy;
        let d = (dx * dx + dy * dy).sqrt();
        if radius_cells > 0.0 {
            (d / radius_cells).min(1.0) as f32
        } else {
            1.0
        }
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;

    /// A square context at `res` cells and `world_extent` meters, so `world_to_cells`
    /// is exercised on the real path.
    fn ctx(res: usize, world_extent: f64) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0).with_world_extent(world_extent)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        Falloff
            .eval(Inputs::required_only(&[]), params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn eval_is_deterministic() {
        let c = ctx(64, 1024.0);
        let params = Params::default();
        assert_eq!(
            run(&params, &c).content_hash(),
            run(&params, &c).content_hash()
        );
    }

    #[test]
    fn center_is_zero_and_edge_is_one() {
        // 1 m/cell over a 64 grid, radius 20 m centered: the center cell is near 0, a
        // cell at the radius reads ~1, and a cell well beyond is clamped to exactly 1.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(20.0)),
            &ctx(64, 64.0),
        );
        assert!(at(&out, 32, 32) < 0.05, "center should be ~0");
        assert!(
            (at(&out, 52, 32) - 1.0).abs() < 0.05,
            "radius edge should be ~1"
        );
        assert_eq!(at(&out, 63, 32), 1.0, "beyond radius is clamped to 1");
    }

    #[test]
    fn the_ramp_is_linear_not_smoothstepped() {
        // Radius 40 cells from center 32: a quarter of the way out (~10 cells, cell 42)
        // reads ~0.25 and three-quarters out (~30 cells, cell 62) reads ~0.75. A
        // smoothstep would bow these to ~0.16 and ~0.84, so this pins the linearity that
        // makes the Curve's X axis a true radial fraction.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(40.0)),
            &ctx(64, 64.0),
        );
        assert!(
            (at(&out, 42, 32) - 0.25).abs() < 0.05,
            "quarter should be ~0.25"
        );
        assert!(
            (at(&out, 62, 32) - 0.75).abs() < 0.05,
            "three-quarter should be ~0.75"
        );
    }

    #[test]
    fn output_stays_in_unit_range() {
        let out = run(&Params::default(), &ctx(48, 1024.0));
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert!((0.0..=1.0).contains(&v), "value {v} out of [0, 1]");
        }
    }

    #[test]
    fn the_center_param_moves_the_cone() {
        // Push the center to the left edge: the zero is now on the left, so the left
        // reads lower than the middle, which reads lower than the right.
        let c = ctx(64, 64.0);
        let params = Params::default()
            .with("radius", ParamValue::Float(80.0))
            .with("center_x", ParamValue::Float(0.0));
        let out = run(&params, &c);
        assert!(at(&out, 0, 32) < at(&out, 32, 32));
        assert!(at(&out, 32, 32) < at(&out, 63, 32));
    }

    #[test]
    fn a_collapsed_radius_is_a_flat_one_field() {
        // Radius 0 must not divide by zero or leak a NaN; the edge has collapsed onto the
        // center, so every cell is at or beyond it and reads 1.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(0.0)),
            &ctx(16, 16.0),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert_eq!(v, 1.0);
        }
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("falloff operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Falloff.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Falloff.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // distance math fails here.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(400.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0x0668_2bc5_e810_5129);
    }
}
