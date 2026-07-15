//! The polygon generator: a flat-topped regular n-gon with soft flanks.
//!
//! Produces a regular polygon on the `height` layer: 1 across a flat core out to the
//! apothem, easing to 0 over `falloff` outside it, and 0 beyond. It is a generator by
//! arity (no inputs, one output) and writes a plain `Field`, so like the rest of the
//! Shape family it is a reusable control envelope: an angular plateau, a faceted mesa, a
//! hex or octagon base. It is the faceted sibling of the radial dome and rectangle.
//!
//! `radius` is the circumradius (center to a vertex). `sides` is the number of facets
//! (at least three). The core can be turned with `rotation`. `radius` and `falloff` are
//! world-unit lengths (meters), converted to cells through the world extent, so the
//! polygon keeps the same physical size at any resolution. The `center` is a normalized
//! position over the whole world, mapped through the evaluated `region`, so a tiled build
//! matches an untiled one.
//!
//! The polygon is the intersection of its edge half-planes: the outside distance is how
//! far the cell sits past the nearest edge line, which is flat across the interior and
//! gently rounds the vertices. The falloff is one fixed smoothstep, like the rest of the
//! family; a different profile is a Curve node downstream, and an inverted plateau is an
//! Invert node, rather than parameters buried here.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

use crate::shape::{center_cell, rotate, smoothstep};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.polygon";

/// Default circumradius in world units (meters).
const DEFAULT_RADIUS: f64 = 400.0;
/// Default number of sides (a hexagon).
const DEFAULT_SIDES: i64 = 6;
/// Default soft band outside the core, in world units (meters).
const DEFAULT_FALLOFF: f64 = 120.0;
/// The fewest sides a polygon can have; lower values clamp up to this.
const MIN_SIDES: i64 = 3;

/// Polygon generator: no inputs, one output.
#[derive(Clone)]
pub struct Polygon;

impl Operator for Polygon {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
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
                ParamSpec::new(
                    "sides",
                    ParamKind::Int {
                        min: MIN_SIDES,
                        max: 12,
                    },
                    ParamValue::Int(DEFAULT_SIDES),
                ),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_FALLOFF),
                )
                .with_unit(Unit::Meters),
                // Orientation of the polygon. 0 points a vertex up; positive turns it.
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

    /// Reads only the world horizontal extent (a world-unit param), not the world height or
    /// sea level, so those two sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let width = ctx.width;
        let height = ctx.height;
        let region = ctx.region;

        let radius_cells = ctx.world_to_cells(params.get_f64("radius", DEFAULT_RADIUS).max(0.0));
        let falloff_cells = ctx.world_to_cells(params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0));
        // A polygon needs at least three sides; the param's minimum enforces this in the
        // UI, and this guards a hand-edited project file.
        let sides = params.get_i64("sides", DEFAULT_SIDES).max(MIN_SIDES);

        // Bring cell offsets into the polygon's local frame by undoing its rotation.
        let neg_angle = -params.get_f64("rotation", 0.0).to_radians();

        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center = center_cell((center_x, center_y), region, width, height);

        let field = polygon_field(
            (width, height),
            region,
            center,
            radius_cells,
            sides,
            neg_angle,
            falloff_cells,
        );
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Polygon) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "shape", sort: 33 }
}

/// Builds a field whose `height` layer is a regular polygon: 1 across the flat core (out
/// to the apothem of `radius_cells` circumradius), easing to 0 over `falloff_cells`
/// outside it. `neg_angle` is the negated orientation applied to each cell offset to
/// reach the polygon's local frame. A non-positive `falloff_cells` yields a hard-edged
/// polygon rather than a division by zero.
fn polygon_field(
    dims: (usize, usize),
    region: Region,
    center: (f64, f64),
    radius_cells: f64,
    sides: i64,
    neg_angle: f64,
    falloff_cells: f64,
) -> Field {
    let (width, height) = dims;
    let (cx, cy) = center;
    // The polygon is the intersection of its edge half-planes. Each edge sits at the
    // apothem along its outward normal; the outward normals are evenly spaced, offset
    // half a sector from the vertices, with a vertex pointing up at rotation 0.
    let seg = std::f64::consts::TAU / sides as f64;
    let apothem = radius_cells * (std::f64::consts::PI / sides as f64).cos();
    let normal_base = -std::f64::consts::FRAC_PI_2 + 0.5 * seg;

    let layer = Layer::from_par_fn(width, height, |x, y| {
        let off = ((x as f64 + 0.5) - cx, (y as f64 + 0.5) - cy);
        let (lx, ly) = rotate(off, neg_angle);
        // Distance past the nearest edge line: the largest signed distance to any edge's
        // half-plane. Negative inside (flat core), positive outside.
        let mut max_proj = f64::NEG_INFINITY;
        for k in 0..sides {
            let (s, c) = (normal_base + k as f64 * seg).sin_cos();
            max_proj = max_proj.max(lx * c + ly * s);
        }
        let outside = (max_proj - apothem).max(0.0);
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
        Polygon
            .eval(Inputs::required_only(&[]), params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    fn ngon(radius: f64, sides: i64, falloff: f64) -> Params {
        Params::default()
            .with("radius", ParamValue::Float(radius))
            .with("sides", ParamValue::Int(sides))
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
        // A hexagon of circumradius 20 m (1 m/cell), thin 4 m flank: the center is flat 1
        // and the far corners are 0.
        let out = run(&ngon(20.0, 6, 4.0), &ctx(64, 64.0));
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
    fn more_sides_reach_further_toward_the_circumradius() {
        // Straight below the center (a flat-bottom edge for a vertex-up triangle, but a
        // vertex for a hexagon): at 0.7 of the radius the triangle's near edge (apothem
        // 0.5 r) is already outside, while the hexagon's vertex reaches the full radius,
        // so that cell is inside. The hexagon therefore reads higher there.
        let tri = run(&ngon(20.0, 3, 2.0), &ctx(64, 64.0));
        let hex = run(&ngon(20.0, 6, 2.0), &ctx(64, 64.0));
        assert!(at(&hex, 32, 46) > at(&tri, 32, 46));
    }

    #[test]
    fn rotation_turns_the_polygon() {
        // A vertex-up triangle reaches the full radius straight up (toward the vertex);
        // turned 180 degrees the vertex points down, so straight up is now a near edge and
        // that cell drops out. The up cell is therefore higher unrotated than at 180.
        let up0 = run(&ngon(20.0, 3, 2.0), &ctx(64, 64.0));
        let up180 = run(
            &ngon(20.0, 3, 2.0).with("rotation", ParamValue::Float(180.0)),
            &ctx(64, 64.0),
        );
        assert!(at(&up0, 32, 18) > at(&up180, 32, 18));
    }

    #[test]
    fn a_collapsed_falloff_is_a_hard_polygon() {
        // Falloff 0 must not divide by zero or leak a NaN; every cell is fully in or out.
        let out = run(&ngon(20.0, 5, 0.0), &ctx(32, 32.0));
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert!(v == 0.0 || v == 1.0, "polygon value {v} not 0 or 1");
        }
    }

    #[test]
    fn fewer_than_three_sides_clamps_to_a_triangle() {
        // A hand-edited 2-sided value must not divide by a degenerate sector; it clamps to
        // the 3-sided shape and produces the same field.
        let two = run(&ngon(300.0, 2, 120.0), &ctx(48, 1024.0));
        let three = run(&ngon(300.0, 3, 120.0), &ctx(48, 1024.0));
        assert_eq!(two.content_hash(), three.content_hash());
    }

    #[test]
    fn the_center_param_moves_the_polygon() {
        let params = ngon(20.0, 6, 4.0).with("center_x", ParamValue::Float(0.0));
        let out = run(&params, &ctx(64, 64.0));
        assert!(at(&out, 2, 32) > at(&out, 63, 32));
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("polygon operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Polygon.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Polygon.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // polygon math fails here.
        let out = run(
            &Params::default()
                .with("radius", ParamValue::Float(400.0))
                .with("sides", ParamValue::Int(5))
                .with("falloff", ParamValue::Float(200.0))
                .with("rotation", ParamValue::Float(30.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0x42b5_68e9_f170_db0b);
    }
}
