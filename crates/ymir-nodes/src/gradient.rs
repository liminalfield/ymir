//! The directional gradient generator: a smooth linear ramp.
//!
//! Produces a trend on the `height` layer: 0 on one side of a line, 1 on the other,
//! with a smoothstep band of `width` meters between, centered on `center`. It is a
//! generator by arity (no inputs, one output) and writes a plain `Field`, so like the
//! radial dome it is reusable as terrain or, more often, as a control envelope. The
//! gradient is the canonical *non-centered* envelope: where radial gives one hero peak
//! dead center, a gradient gives a coast-to-highland trend, a dune-field direction, or
//! any regional lean, so multiplying it with noise distributes features along a
//! direction instead of around a point.
//!
//! `width` is a world-unit length (meters), converted to cells through the world
//! extent, so the ramp covers the same physical distance at any resolution. `angle` is
//! the direction of increase in degrees: 0 points along +x, and the angle rotates
//! toward +y. The `center` is a normalized position over the whole world (the point the
//! half-value line passes through), mapped through the evaluated `region`, so a tiled
//! build matches an untiled one.
//!
//! The falloff is one fixed smoothstep, like the rest of the Shape family: a different
//! profile is a Curve node downstream, and a reversed ramp is an Invert node, rather
//! than parameters buried here.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Region, Result, Unit, layers,
};

use crate::shape::{center_cell, smoothstep};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.gradient";

/// Default band width in world units (meters). Pairs with the default world extent
/// (1024 m) to give a gentle ramp across the whole map out of the box.
const DEFAULT_WIDTH: f64 = 1024.0;

/// Directional gradient generator: no inputs, one output.
#[derive(Clone)]
pub struct Gradient;

impl Operator for Gradient {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                // Direction of increase in degrees: 0 along +x, rotating toward +y.
                ParamSpec::new(
                    "angle",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "width",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_WIDTH),
                )
                .with_unit(Unit::Meters),
                // Center as a normalized position over the whole world (0.5 = middle):
                // the point the half-value line passes through.
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
            emitted_layers: Vec::new(),
            mask_aware: false,
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

        // World-unit band to cells, the same bridge radial and Blur use, so the ramp
        // spans the same physical distance at any resolution.
        let band_m = params.get_f64("width", DEFAULT_WIDTH).max(0.0);
        let band_cells = ctx.world_to_cells(band_m);

        let angle = params.get_f64("angle", 0.0).to_radians();
        let (dir_x, dir_y) = (angle.cos(), angle.sin());

        let center_x = params.get_f64("center_x", 0.5);
        let center_y = params.get_f64("center_y", 0.5);
        let center = center_cell((center_x, center_y), region, width, height);

        let field = gradient_field(width, height, region, center, (dir_x, dir_y), band_cells);
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Gradient) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "gradient", sort: 40 }
}

/// Builds a field whose `height` layer ramps from 0 to 1 across a smoothstep band of
/// `band_cells` centered at `center` (in cells), in the `dir` direction. A non-positive
/// `band_cells` degrades to a hard half-plane step at the center line rather than a
/// division by zero.
fn gradient_field(
    width: usize,
    height: usize,
    region: Region,
    center: (f64, f64),
    dir: (f64, f64),
    band_cells: f64,
) -> Field {
    let (cx, cy) = center;
    let (dx_dir, dy_dir) = dir;
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Signed distance from the center line, in cells: the cell offset projected onto
        // the unit direction. Positive on the +direction side, negative on the other.
        let ox = (x as f64 + 0.5) - cx;
        let oy = (y as f64 + 0.5) - cy;
        let s = ox * dx_dir + oy * dy_dir;
        if band_cells > 0.0 {
            // 0.5 at the center line, 0 a half-band back, 1 a half-band forward.
            smoothstep(0.0, 1.0, (0.5 + s / band_cells) as f32)
        } else if s >= 0.0 {
            1.0
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
        Gradient
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
    fn the_ramp_increases_along_the_angle() {
        // Angle 0 points along +x: the right edge is higher than the left, and the
        // value grows monotonically left to right at a fixed row.
        let out = run(&Params::default(), &ctx(64, 1024.0));
        assert!(at(&out, 0, 32) < at(&out, 32, 32));
        assert!(at(&out, 32, 32) < at(&out, 63, 32));
    }

    #[test]
    fn the_center_line_is_the_half_value() {
        // The default center (0.5, 0.5) sits on cell 32 of a 64 grid; the half-value
        // line passes through it, so the center cell reads ~0.5.
        let out = run(&Params::default(), &ctx(64, 1024.0));
        let center = at(&out, 32, 32);
        assert!(
            (center - 0.5).abs() < 0.05,
            "center should be ~0.5, got {center}"
        );
    }

    #[test]
    fn the_angle_rotates_the_ramp() {
        // At 90 degrees the direction is +y, so the ramp runs top to bottom and a fixed
        // column increases downward while a fixed row stays roughly flat.
        let out = run(
            &Params::default().with("angle", ParamValue::Float(90.0)),
            &ctx(64, 1024.0),
        );
        assert!(at(&out, 32, 0) < at(&out, 32, 63));
        assert!((at(&out, 0, 32) - at(&out, 63, 32)).abs() < 0.01);
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
    fn a_narrower_band_is_a_steeper_ramp() {
        // A narrow band concentrates the transition: just past the center the value is
        // higher than with a wide band that fades slowly. Sample one cell past center.
        let narrow = run(
            &Params::default().with("width", ParamValue::Float(64.0)),
            &ctx(64, 1024.0),
        );
        let wide = run(
            &Params::default().with("width", ParamValue::Float(4096.0)),
            &ctx(64, 1024.0),
        );
        assert!(at(&narrow, 40, 32) > at(&wide, 40, 32));
    }

    #[test]
    fn a_collapsed_band_is_a_hard_step() {
        // Width 0 must not divide by zero or leak a NaN; the ramp becomes a clean step
        // at the center line.
        let out = run(
            &Params::default().with("width", ParamValue::Float(0.0)),
            &ctx(16, 1024.0),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        for &v in layer.as_slice() {
            assert!(v == 0.0 || v == 1.0, "step value {v} not 0 or 1");
        }
        // Left of center is 0, right of center is 1.
        assert_eq!(at(&out, 0, 8), 0.0);
        assert_eq!(at(&out, 15, 8), 1.0);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("gradient operator is registered");
        let c = ctx(32, 1024.0);
        let via_registry = made
            .eval(Inputs::required_only(&[]), &Params::default(), &c)
            .unwrap();
        let direct = run(&Params::default(), &c);
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Gradient.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Gradient.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        // A fixed fingerprint over a small grid, so a refactor that silently changes the
        // ramp math fails here.
        let out = run(
            &Params::default().with("width", ParamValue::Float(512.0)),
            &ctx(8, 1024.0),
        );
        assert_eq!(out.content_hash().to_u64(), 0x180c_6e06_1324_1491);
    }
}
