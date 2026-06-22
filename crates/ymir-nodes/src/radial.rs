//! The radial gradient generator: a smooth circular envelope.
//!
//! Produces a dome on the `height` layer: 1 at the center, easing to 0 at the
//! `radius` through a smoothstep falloff, and 0 beyond. It is a generator by arity
//! (no inputs, one output), and writes a plain `Field` like any other, so it is not
//! a special "mask type": it is reusable as terrain or, more often, as a control
//! envelope multiplied with detail (a Blend in Multiply mode) to shape a landform.
//! This is the first member of the Shape generator family in the control-fields
//! design; ring and directional shapes are separate nodes added later.
//!
//! The `radius` is a world-unit length (meters), converted to cells through the
//! world extent exactly as Blur does, so the dome covers the same physical reach at
//! any resolution. The `center` is a normalized position over the whole world,
//! mapped through the evaluated `region` to a cell, so a tiled build places the
//! center at the same ground as an untiled one.
//!
//! The falloff is one fixed smoothstep on purpose: a different profile is a Curve
//! node downstream, and an inverted dome (a basin) is an Invert node, rather than
//! parameters buried here. Single-purpose nodes keep the graph readable.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.radial";

/// Default radius in world units (meters). Pairs with the default world extent
/// (1024 m) to give a centered dome that fills most of the map out of the box.
const DEFAULT_RADIUS: f64 = 500.0;

/// Radial gradient generator: no inputs, one output.
#[derive(Clone)]
pub struct Radial;

impl Operator for Radial {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            tags: &[
                "radial",
                "gradient",
                "shape",
                "envelope",
                "falloff",
                "circle",
                "dome",
                "island",
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

        // World-unit radius to cells, the same bridge Blur uses, so the dome reaches
        // the same physical distance at any resolution.
        let radius_m = params.get_f64("radius", DEFAULT_RADIUS).max(0.0);
        let radius_cells = ctx.world_to_cells(radius_m);

        // The center is normalized over the whole world; map it into this region's cell
        // grid. For an untiled build (region UNIT) this is just center * resolution; for
        // a tile it lands at the same ground as the untiled build.
        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center_cell_x = (center_x - region.min_x) / region.width() * width as f64;
        let center_cell_y = (center_y - region.min_y) / region.height() * height as f64;

        let field = radial_field(
            width,
            height,
            region,
            (center_cell_x, center_cell_y),
            radius_cells,
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Radial) }
}

/// Builds a field whose `height` layer is a radial dome: 1 at `center` (in cells),
/// easing to 0 at `radius_cells` through a smoothstep, and 0 beyond. A non-positive
/// `radius_cells` yields a flat-zero field (the dome has collapsed) rather than a
/// division by zero.
fn radial_field(
    width: usize,
    height: usize,
    region: Region,
    center: (f64, f64),
    radius_cells: f64,
) -> Field {
    let (cx, cy) = center;
    let layer = Layer::from_fn(width, height, |x, y| {
        // Distance from the cell center to the dome center, in cells. Cells are square,
        // so this is isotropic and matches the world-space circle of the radius.
        let dx = (x as f64 + 0.5) - cx;
        let dy = (y as f64 + 0.5) - cy;
        let d = (dx * dx + dy * dy).sqrt();
        // Normalized distance; a collapsed radius maps every cell past the edge.
        let t = if radius_cells > 0.0 {
            (d / radius_cells) as f32
        } else {
            f32::INFINITY
        };
        // 1 at the center (t = 0), 0 at and beyond the radius (t >= 1).
        1.0 - smoothstep(0.0, 1.0, t)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
/// `low == high` degrades to a hard step at that threshold.
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
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
        Radial
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
    fn the_center_is_the_peak_and_corners_fall_off() {
        // 1 m/cell over a 64 grid, radius 20 m centered: the center cell is near 1 and
        // the far corners are 0 (well outside a 20-cell radius).
        let out = run(
            &Params::default().with("radius", ParamValue::Float(20.0)),
            &ctx(64, 64.0),
        );
        let center = at(&out, 32, 32);
        assert!(center > 0.99, "center should peak near 1, got {center}");
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
    fn a_larger_radius_makes_a_wider_dome() {
        // At a fixed off-center sample, a bigger radius means a higher value (the dome
        // reaches further). Sample halfway between center and edge.
        let small = run(
            &Params::default().with("radius", ParamValue::Float(10.0)),
            &ctx(64, 64.0),
        );
        let large = run(
            &Params::default().with("radius", ParamValue::Float(30.0)),
            &ctx(64, 64.0),
        );
        assert!(at(&large, 48, 32) > at(&small, 48, 32));
    }

    #[test]
    fn the_center_param_moves_the_peak() {
        // Push the center to the left edge: the peak is now on the left, not the middle.
        // A radius wide enough to span the grid so the falloff is measurable across it.
        let c = ctx(64, 64.0);
        let params = Params::default()
            .with("radius", ParamValue::Float(80.0))
            .with("center_x", ParamValue::Float(0.0));
        let out = run(&params, &c);
        assert!(at(&out, 0, 32) > at(&out, 32, 32));
        assert!(at(&out, 32, 32) > at(&out, 63, 32));
    }

    #[test]
    fn a_larger_world_extent_shrinks_the_dome_in_cells() {
        // Same radius in meters and same grid, but a bigger world means each cell spans
        // more meters, so the dome covers fewer cells. At a fixed off-center cell the
        // value is therefore lower in the bigger world.
        let small_world = run(
            &Params::default().with("radius", ParamValue::Float(20.0)),
            &ctx(64, 64.0),
        );
        let big_world = run(
            &Params::default().with("radius", ParamValue::Float(20.0)),
            &ctx(64, 256.0),
        );
        assert!(at(&small_world, 44, 32) > at(&big_world, 44, 32));
    }

    #[test]
    fn a_collapsed_radius_is_a_flat_zero_field() {
        // Radius 0 must not divide by zero or leak a NaN; the dome is simply gone.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(0.0)),
            &ctx(16, 16.0),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert_eq!(v, 0.0);
        }
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("radial operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Radial.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Radial.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // falloff math fails here.
        let out = run(
            &Params::default().with("radius", ParamValue::Float(400.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0x4549_e7e4_5296_94f9);
    }
}
