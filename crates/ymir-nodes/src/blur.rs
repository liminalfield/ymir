//! Blur: a separable Gaussian blur of the `height` layer, by a world-unit radius.
//!
//! The radius is the blur's characteristic scale (the Gaussian sigma), given in
//! world units (meters) and converted to cells through the world extent, so the
//! same radius smooths the same physical distance at any resolution. This is the
//! scale mechanism behind the derived selectors (a curvature "at 1 km" is a Blur
//! upstream of a Curvature) and the mask-feathering tool. Mask-aware per the
//! convention: the blurred height is composited over the original through the
//! `mask` layer, and other layers pass through.
//!
//! A true Gaussian kernel is O(n*r), and a world-unit radius at build resolution
//! becomes a large cell radius, so this approximates the Gaussian with three
//! successive box blurs (central-limit theorem), each run separably with a running
//! sum: O(n) regardless of radius, deterministic, and order-independent. The passes
//! are sequential per row and column; `n` parallelism drops in unchanged once
//! benchmarks justify, exactly as the noise and erosion paths note.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.blur";

/// Default blur radius in world units (meters). Pairs with the default world extent
/// and build resolution to give a gentle, visible smoothing out of the box.
const DEFAULT_RADIUS: f64 = 8.0;

/// Gaussian blur modifier: one input, one output.
#[derive(Clone)]
pub struct Blur;

impl Operator for Blur {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in")],
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
            ],
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
        let mask = input.layer_or(layers::MASK, 1.0);

        // The radius is a world-unit length; convert to a cell-space sigma through the
        // world extent so the blur covers the same physical distance at any
        // resolution. A sub-cell sigma rounds to no blur (the grid cannot resolve it).
        let radius_m = params.get_f64("radius", DEFAULT_RADIUS).max(0.0);
        let sigma = ctx.world_to_cells(radius_m);
        let blurred = gaussian_blur(h.as_slice(), width, height, sigma);

        // Mask-aware: composite the blurred height over the original through the mask,
        // so mask = 1 is fully blurred and mask = 0 is left untouched.
        let shaped = Layer::from_fn(width, height, |x, y| {
            let original = h.get(x, y).unwrap_or(0.0);
            let b = blurred[y * width + x];
            let m = mask.get(x, y).unwrap_or(1.0);
            original + (b - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Blur) }
}

/// Approximates a Gaussian blur of standard deviation `sigma` cells with three
/// successive box blurs (central-limit theorem), run separably as rows then
/// columns. Clamp-to-edge at the boundary, so a constant field is preserved and no
/// mass leaks off the edge. Returns the blurred buffer; `src` is read-only.
fn gaussian_blur(src: &[f32], width: usize, height: usize, sigma: f64) -> Vec<f32> {
    // A box blur of radius r has variance (r^2 + r) / 3; three passes sum to
    // r^2 + r, so r = (sqrt(1 + 4 sigma^2) - 1) / 2 matches the target sigma. The
    // radius is bounded by the grid: past the extent it is, under clamp-to-edge,
    // indistinguishable from one at the edge, and the bound keeps the work finite
    // for a degenerate (or infinite) sigma.
    let max_r = width.max(height) as f64;
    let radius = (((1.0 + 4.0 * sigma * sigma).sqrt() - 1.0) / 2.0)
        .round()
        .clamp(0.0, max_r) as usize;
    if radius == 0 || src.is_empty() {
        return src.to_vec();
    }

    let mut a = src.to_vec();
    let mut b = vec![0.0_f32; src.len()];
    for _ in 0..3 {
        box_blur_h(&a, &mut b, width, height, radius);
        box_blur_v(&b, &mut a, width, height, radius);
    }
    a
}

/// One horizontal box blur of the given radius into `dst`: each cell becomes the
/// mean of the `2 * radius + 1` inputs centred on it, with indices past either end
/// clamped to the edge. A running sum keeps it O(width) per row.
fn box_blur_h(src: &[f32], dst: &mut [f32], width: usize, height: usize, radius: usize) {
    let r = radius as isize;
    let last = width as isize - 1;
    let window = (2 * radius + 1) as f64;
    for y in 0..height {
        let row = y * width;
        // Seed the window for x = 0: indices [-r, r], clamped to [0, width - 1].
        let mut sum: f64 = 0.0;
        for j in -r..=r {
            let idx = j.clamp(0, last) as usize;
            sum += f64::from(src[row + idx]);
        }
        dst[row] = (sum / window) as f32;
        for x in 1..width {
            // Slide: drop the cell leaving the left side, add the one entering on the
            // right; clamping holds the edge value once the window crosses a border.
            let leave = (x as isize - 1 - r).clamp(0, last) as usize;
            let enter = (x as isize + r).clamp(0, last) as usize;
            sum += f64::from(src[row + enter]) - f64::from(src[row + leave]);
            dst[row + x] = (sum / window) as f32;
        }
    }
}

/// One vertical box blur, the column-wise counterpart of [`box_blur_h`]: the same
/// running-sum mean over `2 * radius + 1` rows, clamped to the top and bottom edges.
fn box_blur_v(src: &[f32], dst: &mut [f32], width: usize, height: usize, radius: usize) {
    let r = radius as isize;
    let last = height as isize - 1;
    let window = (2 * radius + 1) as f64;
    for x in 0..width {
        let mut sum: f64 = 0.0;
        for j in -r..=r {
            let yy = j.clamp(0, last) as usize;
            sum += f64::from(src[yy * width + x]);
        }
        dst[x] = (sum / window) as f32;
        for y in 1..height {
            let leave = (y as isize - 1 - r).clamp(0, last) as usize;
            let enter = (y as isize + r).clamp(0, last) as usize;
            sum += f64::from(src[enter * width + x]) - f64::from(src[leave * width + x]);
            dst[y * width + x] = (sum / window) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(res: usize, world_extent: f64) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0).with_world_extent(world_extent)
    }

    fn run(input: &Field, radius_m: f64, ctx: &EvalContext) -> Field {
        let params = Params::new().with("radius", ParamValue::Float(radius_m));
        Blur.eval(Inputs::required_only(&[input]), &params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    fn constant(res: usize, v: f32) -> Field {
        Field::new(res, res, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(res, res, v)))
    }

    fn ramp(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                x as f32 / (res as f32 - 1.0)
            })),
        )
    }

    fn vertical_step(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                if x < res / 2 { 0.0 } else { 1.0 }
            })),
        )
    }

    #[test]
    fn a_constant_field_is_preserved() {
        // Clamp-to-edge means no mass leaks off the border, so a flat field is exact.
        let out = run(&constant(16, 0.42), 4.0, &ctx(16, 16.0));
        for y in 0..16 {
            for x in 0..16 {
                assert!(
                    (at(&out, x, y) - 0.42).abs() < 1e-5,
                    "constant changed at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn zero_radius_is_identity() {
        let input = ramp(16);
        let out = run(&input, 0.0, &ctx(16, 16.0));
        for y in 0..16 {
            for x in 0..16 {
                assert!((at(&out, x, y) - at(&input, x, y)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_sub_cell_radius_is_identity() {
        // World extent 16 across 16 cells is 1 m/cell, so a 0.2 m radius is well under
        // a cell: the grid cannot resolve it and the blur is a no-op.
        let input = ramp(16);
        let out = run(&input, 0.2, &ctx(16, 16.0));
        for y in 0..16 {
            for x in 0..16 {
                assert!((at(&out, x, y) - at(&input, x, y)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn it_smooths_a_step() {
        // A vertical step at 1 m/cell with a few cells of sigma: the boundary columns
        // are pulled off 0 and 1, while cells far from it are essentially untouched.
        let res = 32;
        let out = run(&vertical_step(res), 4.0, &ctx(res, res as f64));
        let left = at(&out, res / 2 - 1, res / 2);
        let right = at(&out, res / 2, res / 2);
        assert!(
            left > 0.001 && left < 0.5,
            "left boundary not softened: {left}"
        );
        assert!(
            right > 0.5 && right < 0.999,
            "right boundary not softened: {right}"
        );
        assert!(at(&out, 0, res / 2) < 0.01);
        assert!(at(&out, res - 1, res / 2) > 0.99);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = constant(16, 0.5);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.9)));
        let out = run(&input, 4.0, &ctx(16, 16.0));
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn mask_limits_the_blur() {
        // The same step, but masked off everywhere: the blur has no effect, so the
        // sharp 0/1 step is preserved exactly (mask = 0 keeps the original).
        let res = 32;
        let mut step = vertical_step(res);
        step.set_layer(layers::MASK, Arc::new(Layer::filled(res, res, 0.0)));
        let out = run(&step, 4.0, &ctx(res, res as f64));
        assert_eq!(at(&out, res / 2 - 1, res / 2), 0.0);
        assert_eq!(at(&out, res / 2, res / 2), 1.0);
    }

    #[test]
    fn a_larger_world_extent_blurs_less() {
        // Same radius in meters and same grid, but a bigger world means each cell
        // spans more meters, so the radius covers fewer cells and smooths less. A
        // linear ramp is unchanged by blur except near the clamped edges, so the
        // departure from the original measures how far the blur reaches.
        let input = ramp(32);
        let small_world = run(&input, 8.0, &ctx(32, 32.0)); // 1 m/cell, ~8-cell sigma
        let big_world = run(&input, 8.0, &ctx(32, 128.0)); // 4 m/cell, ~2-cell sigma
        let dev = |f: &Field| -> f32 {
            (0..32)
                .map(|x| (at(f, x, 16) - at(&input, x, 16)).abs())
                .sum::<f32>()
        };
        assert!(dev(&small_world) > dev(&big_world));
        assert!(dev(&big_world) > 0.0);
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(24);
        let c = ctx(24, 24.0);
        assert_eq!(
            run(&input, 5.0, &c).content_hash(),
            run(&input, 5.0, &c).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        // A centred spike, blurred with a few cells of sigma: a deterministic case
        // exercising both the interior running sum and the clamped edges.
        let res = 16;
        let input = Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, y| {
                if x == res / 2 && y == res / 2 {
                    1.0
                } else {
                    0.0
                }
            })),
        );
        let out = run(&input, 3.0, &ctx(res, res as f64));
        assert_eq!(out.content_hash().to_u64(), 0xce87_8a6a_a800_ddac);
    }
}
