//! The rectangle generator: a flat-topped rectangular footprint with soft flanks.
//!
//! Produces a rectangle on the `height` layer: 1 across a flat core of `extent_x` by
//! `extent_y`, easing to 0 over `falloff` outside it, and 0 beyond. It is a generator by
//! arity (no inputs, one output) and writes a plain `Field`, so like the rest of the
//! Shape family it is a reusable control envelope: the plateau / mesa / table footprint,
//! and the base for a rectangular landmass. It is the box analogue of the radial dome.
//!
//! The core can be turned with `rotation`. `extent_x`, `extent_y`, and `falloff` are
//! world-unit lengths (meters), converted to cells through the world extent, so the
//! rectangle keeps the same physical size at any resolution. The `center` is a normalized
//! position over the whole world, mapped through the evaluated `region`, so a tiled build
//! matches an untiled one.
//!
//! The falloff is one fixed smoothstep, like the rest of the family, and it also rounds
//! the corners, so there is no separate corner-radius parameter. A different profile is a
//! Curve node downstream, and an inverted plateau (a rectangular pit) is an Invert node,
//! rather than parameters buried here.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

use crate::shape::{center_cell, rotate, smoothstep};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.rect";

/// Default core span along the local x axis, in world units (meters).
const DEFAULT_EXTENT_X: f64 = 500.0;
/// Default core span along the local y axis, in world units (meters).
const DEFAULT_EXTENT_Y: f64 = 300.0;
/// Default soft band outside the core, in world units (meters).
const DEFAULT_FALLOFF: f64 = 120.0;

/// Rectangle generator: no inputs, one output.
#[derive(Clone)]
pub struct Rect;

impl Operator for Rect {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            tags: &[
                "rect",
                "rectangle",
                "box",
                "shape",
                "envelope",
                "falloff",
                "plateau",
                "mesa",
                "generator",
            ],
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "extent_x",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_EXTENT_X),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "extent_y",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_EXTENT_Y),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_FALLOFF),
                )
                .with_unit(Unit::Meters),
                // Orientation of the core. 0 is axis-aligned; positive turns it.
                ParamSpec::new(
                    "rotation",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Degrees),
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

        // World-unit lengths to cells; the half-extents are what the box SDF compares
        // against, so halve here once.
        let half_x =
            ctx.world_to_cells(params.get_f64("extent_x", DEFAULT_EXTENT_X).max(0.0)) / 2.0;
        let half_y =
            ctx.world_to_cells(params.get_f64("extent_y", DEFAULT_EXTENT_Y).max(0.0)) / 2.0;
        let falloff_cells = ctx.world_to_cells(params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0));

        // Bring cell offsets into the box's local frame by undoing its rotation.
        let neg_angle = -params.get_f64("rotation", 0.0).to_radians();

        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center = center_cell((center_x, center_y), region, width, height);

        let field = rect_field(
            width,
            height,
            region,
            center,
            (half_x, half_y),
            neg_angle,
            falloff_cells,
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Rect) }
}

/// Builds a field whose `height` layer is a rectangle: 1 across the flat core of
/// half-extents `half` (in cells) around `center`, rotated by `neg_angle` (the negated
/// orientation, applied to each cell offset to reach the box's local frame), easing to 0
/// over `falloff_cells` outside it. A non-positive `falloff_cells` yields a hard-edged
/// box rather than a division by zero.
fn rect_field(
    width: usize,
    height: usize,
    region: Region,
    center: (f64, f64),
    half: (f64, f64),
    neg_angle: f64,
    falloff_cells: f64,
) -> Field {
    let (cx, cy) = center;
    let (hx, hy) = half;
    let layer = Layer::from_fn(width, height, |x, y| {
        // Cell offset from the center, rotated into the box's local frame.
        let off = ((x as f64 + 0.5) - cx, (y as f64 + 0.5) - cy);
        let (lx, ly) = rotate(off, neg_angle);
        // Signed-distance to the axis-aligned box in local coords: how far outside each
        // half-extent, clamped at the edge so the interior is a flat 0 distance.
        let qx = lx.abs() - hx;
        let qy = ly.abs() - hy;
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        if falloff_cells > 0.0 {
            1.0 - smoothstep(0.0, 1.0, (outside / falloff_cells) as f32)
        } else if outside > 0.0 {
            0.0
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
        Rect.eval(Inputs::required_only(&[]), params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A square plateau: extent and falloff in meters, on a 1 m/cell world.
    fn square(extent: f64, falloff: f64) -> Params {
        Params::default()
            .with("extent_x", ParamValue::Float(extent))
            .with("extent_y", ParamValue::Float(extent))
            .with("falloff", ParamValue::Float(falloff))
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
    fn the_core_is_flat_and_outside_is_zero() {
        // 1 m/cell over a 64 grid, a 20 m square (half-extent 10 cells) centered, thin
        // 4 m flank: the center is flat 1, and the far corners are 0.
        let out = run(&square(20.0, 4.0), &ctx(64, 64.0));
        assert!(at(&out, 32, 32) > 0.99, "core should be flat 1");
        assert_eq!(at(&out, 0, 0), 0.0);
        assert_eq!(at(&out, 63, 63), 0.0);
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
    fn a_wider_extent_reaches_further_along_its_axis() {
        // A core 40 m wide (half 20) but only 10 m tall (half 5), thin flank: 8 cells out
        // along x is still inside (high), while 8 cells out along y is outside (low).
        let out = run(
            &Params::default()
                .with("extent_x", ParamValue::Float(40.0))
                .with("extent_y", ParamValue::Float(10.0))
                .with("falloff", ParamValue::Float(2.0)),
            &ctx(64, 64.0),
        );
        assert!(at(&out, 40, 32) > 0.9, "inside along the wide axis");
        assert!(at(&out, 32, 40) < 0.1, "outside along the short axis");
    }

    #[test]
    fn rotation_turns_the_rectangle() {
        // The same 40x10 core turned 90 degrees swaps which axis reaches further: now the
        // tall direction is along y, so the point along y is inside and the one along x is
        // outside, the reverse of the unrotated case.
        let params = Params::default()
            .with("extent_x", ParamValue::Float(40.0))
            .with("extent_y", ParamValue::Float(10.0))
            .with("falloff", ParamValue::Float(2.0))
            .with("rotation", ParamValue::Float(90.0));
        let out = run(&params, &ctx(64, 64.0));
        assert!(at(&out, 32, 40) > at(&out, 40, 32));
    }

    #[test]
    fn the_center_param_moves_the_box() {
        // Push the center to the left edge: cells near the left are inside, the right edge
        // is far outside.
        let params = square(20.0, 4.0).with("center_x", ParamValue::Float(0.0));
        let out = run(&params, &ctx(64, 64.0));
        assert!(at(&out, 2, 32) > at(&out, 63, 32));
    }

    #[test]
    fn a_collapsed_falloff_is_a_hard_box() {
        // Falloff 0 must not divide by zero or leak a NaN; every cell is fully in or out.
        let out = run(&square(20.0, 0.0), &ctx(32, 32.0));
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert!(v == 0.0 || v == 1.0, "box value {v} not 0 or 1");
        }
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("rect operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Rect.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Rect.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // box math fails here.
        let out = run(
            &Params::default()
                .with("extent_x", ParamValue::Float(500.0))
                .with("extent_y", ParamValue::Float(300.0))
                .with("falloff", ParamValue::Float(200.0))
                .with("rotation", ParamValue::Float(30.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0x0956_ecd4_2757_0015);
    }
}
