//! The ring generator: a smooth circular ridge (annulus).
//!
//! Produces a ring on the `height` layer: 1 on a circle of `radius`, easing to 0 over
//! `width` on each flank, and 0 elsewhere. It is a generator by arity (no inputs, one
//! output) and writes a plain `Field`, so like the rest of the Shape family it is a
//! reusable control envelope. The ring is the radial's companion: where radial fills
//! the disc, ring outlines it, which is the crater rim, the caldera wall, the atoll, and
//! the base of a ringed massif.
//!
//! `radius` and `width` are world-unit lengths (meters), converted to cells through the
//! world extent, so the ring keeps the same physical size at any resolution. The
//! `center` is a normalized position over the whole world, mapped through the evaluated
//! `region`, so a tiled build matches an untiled one.
//!
//! The falloff is one fixed smoothstep, like the rest of the family: a different profile
//! is a Curve node downstream, and an inverted ring (a circular moat) is an Invert node,
//! rather than parameters buried here.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

use crate::shape::{center_cell, smoothstep};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.ring";

/// Default ring radius in world units (meters). Pairs with the default world extent
/// (1024 m) to give a centered ring well inside the map.
const DEFAULT_RADIUS: f64 = 300.0;

/// Default flank width in world units (meters): the distance from the peak circle out
/// to zero on each side.
const DEFAULT_WIDTH: f64 = 100.0;

/// Ring generator: no inputs, one output.
#[derive(Clone)]
pub struct Ring;

impl Operator for Ring {
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
                    "width",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_WIDTH),
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

    /// Reads only the world horizontal extent (a world-unit param), not the world height or
    /// sea level, so those two sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let width = ctx.width;
        let height = ctx.height;
        let region = ctx.region;

        // World-unit lengths to cells, the same bridge radial and Blur use, so the ring
        // keeps the same physical size at any resolution.
        let radius_cells = ctx.world_to_cells(params.get_f64("radius", DEFAULT_RADIUS).max(0.0));
        let flank_cells = ctx.world_to_cells(params.get_f64("width", DEFAULT_WIDTH).max(0.0));

        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center = center_cell((center_x, center_y), region, width, height);

        let field = ring_field(width, height, region, center, radius_cells, flank_cells);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Ring) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "shape", sort: 31 }
}

/// Builds a field whose `height` layer is a ring: 1 on the circle of `radius_cells`
/// around `center` (in cells), easing to 0 over `flank_cells` on each side. A
/// non-positive `flank_cells` yields a flat-zero field (an infinitely thin circle has no
/// area) rather than a division by zero.
fn ring_field(
    width: usize,
    height: usize,
    region: Region,
    center: (f64, f64),
    radius_cells: f64,
    flank_cells: f64,
) -> Field {
    let (cx, cy) = center;
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Distance from the cell center to the ring center, in cells; cells are square,
        // so this is isotropic and matches the world-space circle of the radius.
        let dx = (x as f64 + 0.5) - cx;
        let dy = (y as f64 + 0.5) - cy;
        let d = (dx * dx + dy * dy).sqrt();
        // Distance from the peak circle: 0 on the ring, growing on either flank.
        let ring_dist = (d - radius_cells).abs();
        if flank_cells > 0.0 {
            // 1 on the circle, easing to 0 a flank-width out on each side.
            1.0 - smoothstep(0.0, 1.0, (ring_dist / flank_cells) as f32)
        } else {
            0.0
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
        Ring.eval(Inputs::required_only(&[]), params, ctx)
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
    fn the_peak_is_on_the_circle_not_the_center() {
        // 1 m/cell over a 64 grid, radius 20 m, a thin 5 m flank: a cell 20 cells from
        // the center sits on the ring and peaks near 1; the center is well inside the
        // hole and reads 0; the far corner is outside and reads 0.
        let out = run(
            &Params::default()
                .with("radius", ParamValue::Float(20.0))
                .with("width", ParamValue::Float(5.0)),
            &ctx(64, 64.0),
        );
        let on_ring = at(&out, 52, 32); // ~20 cells right of center
        assert!(on_ring > 0.9, "ring should peak near 1, got {on_ring}");
        assert_eq!(at(&out, 32, 32), 0.0);
        assert_eq!(at(&out, 0, 0), 0.0);
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
    fn a_wider_flank_fills_toward_the_center() {
        // Halfway between the center and the ring, a wider flank reaches further in, so
        // the value there is higher. Center at cell 32, ring at 20 cells out, sample at
        // 10 cells out (cell 42).
        let narrow = run(
            &Params::default()
                .with("radius", ParamValue::Float(20.0))
                .with("width", ParamValue::Float(5.0)),
            &ctx(64, 64.0),
        );
        let wide = run(
            &Params::default()
                .with("radius", ParamValue::Float(20.0))
                .with("width", ParamValue::Float(15.0)),
            &ctx(64, 64.0),
        );
        assert!(at(&wide, 42, 32) > at(&narrow, 42, 32));
    }

    #[test]
    fn the_center_param_moves_the_ring() {
        // Push the center to the left edge: the ring's left arc now sits at the left of
        // the grid. A cell `radius` left of the new center peaks; the old middle does not.
        let c = ctx(64, 64.0);
        let params = Params::default()
            .with("radius", ParamValue::Float(10.0))
            .with("width", ParamValue::Float(4.0))
            .with("center_x", ParamValue::Float(0.5)) // keep middle for reference below
            .with("center_y", ParamValue::Float(0.5));
        let centered = run(&params, &c);
        let shifted = run(
            &params.clone().with("center_x", ParamValue::Float(0.25)),
            &c,
        );
        // At 16 cells (0.25 * 64) the shifted ring's center sits; 10 cells right of it
        // (cell 26) is on its ring and higher than the centered ring there.
        assert!(at(&shifted, 26, 32) > at(&centered, 26, 32));
    }

    #[test]
    fn a_collapsed_flank_is_a_flat_zero_field() {
        // Width 0 must not divide by zero or leak a NaN; the ring has no area.
        let out = run(
            &Params::default().with("width", ParamValue::Float(0.0)),
            &ctx(16, 16.0),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert_eq!(v, 0.0);
        }
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("ring operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Ring.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Ring.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // ring math fails here.
        let out = run(
            &Params::default()
                .with("radius", ParamValue::Float(400.0))
                .with("width", ParamValue::Float(200.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0xd54c_f105_a000_37f9);
    }
}
