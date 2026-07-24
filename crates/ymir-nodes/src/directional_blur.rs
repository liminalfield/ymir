//! Directional Blur: smooth the `height` layer along (or across) a guide direction field.
//!
//! Where [`Blur`](crate::Blur) is isotropic, this is anisotropic: it smooths only along a per-cell
//! direction taken from a guide, leaving the perpendicular untouched. The guide is either the
//! gradient of a scalar (`slope`: the fall line of the guide's height, or the shore normal when the
//! guide is a signed-distance field) or a vector field read directly (`flow`: the `flow_x`/`flow_y`
//! layers an erosion or the Flow node emits). The `direction` toggle blurs along the guide (streak,
//! comb, downslope smear) or across it (soften a cross-profile while keeping the guide crest crisp,
//! for example grading a beach across the shore while the coastline stays sharp).
//!
//! The guide comes from the optional `guide` input when wired, else from the input itself, so
//! `slope` with no guide is a downslope self-smear and a Distance node into `guide` steers the blur
//! by the shore. A guide direction of zero length (a flat cell, still water) has nothing to smooth
//! along, so that cell passes through unchanged.
//!
//! Mask-aware per the convention: the blurred height is composited over the original through the
//! mask, taken from the optional `mask` input (a selector's `[0, 1]` rides on its height layer) when
//! wired, else the input's own `mask` layer. So a Slope or Curvature selection can protect the
//! ridges the raw smear would otherwise bead. The direction varies per cell, so the separable O(n) box trick the isotropic Blur
//! uses does not apply; this is an oriented 1D Gaussian sampled per cell (O(n*r)), pure and
//! `from_par_fn`, so byte-identical at any thread count. It reads the world horizontal extent to
//! size the radius, and the direction is independent of the vertical scale, so it is `WORLD_EXTENT`.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.directional_blur";

/// Default blur radius in world units (meters), matching [`Blur`](crate::Blur) so the two read at a
/// comparable scale out of the box.
const DEFAULT_RADIUS: f64 = 8.0;

/// Guide-source ids. `slope` takes the gradient of the guide's height (the fall line, or a shore
/// normal from a distance field); `flow` reads the `flow_x`/`flow_y` vector layers, degrading to
/// `slope` when they are absent. Slope is the default: every field has a height gradient.
const GUIDE_SLOPE: &str = "slope";
const GUIDE_FLOW: &str = "flow";
const GUIDES: &[&str] = &[GUIDE_SLOPE, GUIDE_FLOW];

/// Direction ids. `along` smooths parallel to the guide (streak, comb, downslope smear); `across`
/// smooths perpendicular to it (soften a cross-profile, keep the guide crest crisp).
const DIR_ALONG: &str = "along";
const DIR_ACROSS: &str = "across";
const DIRECTIONS: &[&str] = &[DIR_ALONG, DIR_ACROSS];

/// A cell-space sigma below this cannot be resolved by the grid, so the blur is a no-op.
const MIN_SIGMA: f32 = 0.5;

/// Directional Blur modifier: one required input, optional `guide` and `mask` inputs, one output.
#[derive(Clone)]
pub struct DirectionalBlur;

impl Operator for DirectionalBlur {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![
                PortSpec::new("in"),
                PortSpec::optional("guide"),
                PortSpec::optional("mask"),
            ],
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
                    "guide",
                    ParamKind::Enum { options: GUIDES },
                    ParamValue::Text(GUIDE_SLOPE.to_string()),
                ),
                ParamSpec::new(
                    "direction",
                    ParamKind::Enum {
                        options: DIRECTIONS,
                    },
                    ParamValue::Text(DIR_ALONG.to_string()),
                ),
            ],
            emitted_layers: Vec::new(),
            mask_aware: true,
        }
    }

    /// Reads only the world horizontal extent (a world-unit radius); the direction is independent of
    /// the vertical scale, so the world-height and sea-level sliders never invalidate this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::WORLD_EXTENT
    }

    /// Experimental: a raw oriented smear whose artifacts (beading along curved ridges, pinching at
    /// critical points) are inherent to the single-line kernel. It yields striking flowy and
    /// mid-ocean-ridge looks, but it is a stylistic effect, not a settled clean smoother, so it is
    /// offered with a caveat. Feed a ridge-protecting mask to tame the crests.
    fn experimental(&self) -> bool {
        true
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        // The guide field: an external guide input if wired, else the input itself.
        let guide_field = inputs.optional(0).unwrap_or(input);

        // Where the blur applies: an explicit mask input (a selector's [0, 1] rides on its height
        // layer) wins, else the input's own mask layer, else everywhere. So a Slope/Curvature
        // selection can protect ridges without touching the input's carried mask.
        let mask = match inputs.optional(1) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        let radius_m = params.get_f64("radius", DEFAULT_RADIUS).max(0.0);
        let sigma = ctx.world_to_cells(radius_m) as f32;
        let along = params.get_str("direction", DIR_ALONG) != DIR_ACROSS;

        // A sub-cell sigma the grid cannot resolve: no blur, pass the field through untouched.
        if sigma < MIN_SIGMA {
            return Ok(vec![input.clone()]);
        }

        // Guide vectors: the flow layers when requested and present, else the height gradient. The
        // flow branch degrades to the gradient when either component is missing (the soft contract).
        let flow = if params.get_str("guide", GUIDE_SLOPE) == GUIDE_FLOW
            && guide_field.layer(layers::FLOW_X).is_some()
            && guide_field.layer(layers::FLOW_Y).is_some()
        {
            Some((
                guide_field.layer_or(layers::FLOW_X, 0.0),
                guide_field.layer_or(layers::FLOW_Y, 0.0),
            ))
        } else {
            None
        };
        let guide_h = guide_field.layer_or(layers::HEIGHT, 0.0);

        let src = h.as_slice();
        let taps = (3.0 * sigma).ceil().min(width.max(height) as f32) as i32;
        let inv_two_sigma2 = 1.0 / (2.0 * sigma * sigma);

        // Per-cell and pure, so `from_par_fn` is byte-identical at any thread count. Each cell takes
        // its guide direction, walks the oriented line through it sampling the scalar with Gaussian
        // weights, and composites the result over the original through the mask.
        let shaped = Layer::from_par_fn(width, height, |x, y| {
            let original = src[y * width + x];

            let (gx, gy) = match &flow {
                Some((fx, fy)) => (fx.get(x, y).unwrap_or(0.0), fy.get(x, y).unwrap_or(0.0)),
                None => {
                    // Central difference of the guide's height (one-sided at the edges): the uphill
                    // gradient. Only its direction is used, so its magnitude and sign do not matter.
                    let xm = x.saturating_sub(1);
                    let xp = (x + 1).min(width - 1);
                    let ym = y.saturating_sub(1);
                    let yp = (y + 1).min(height - 1);
                    (
                        guide_h.get(xp, y).unwrap_or(0.0) - guide_h.get(xm, y).unwrap_or(0.0),
                        guide_h.get(x, yp).unwrap_or(0.0) - guide_h.get(x, ym).unwrap_or(0.0),
                    )
                }
            };

            let len = (gx * gx + gy * gy).sqrt();
            if len < 1e-8 {
                // No direction here: nothing to smooth along, so leave the cell as it is.
                return original;
            }
            // Unit guide direction, rotated 90 degrees for the across mode.
            let (ux, uy) = (gx / len, gy / len);
            let (dx, dy) = if along { (ux, uy) } else { (-uy, ux) };

            // Oriented 1D Gaussian: a symmetric line integral, so it preserves a constant field and
            // neither brightens nor darkens overall.
            let mut sum = 0.0_f32;
            let mut wsum = 0.0_f32;
            for i in -taps..=taps {
                let t = i as f32;
                let w = (-t * t * inv_two_sigma2).exp();
                let sx = x as f32 + t * dx;
                let sy = y as f32 + t * dy;
                sum += w * sample_bilinear(src, width, height, sx, sy);
                wsum += w;
            }
            let blurred = sum / wsum;

            let m = mask.get(x, y).unwrap_or(1.0);
            original + (blurred - original) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

/// Bilinear sample of `src` at fractional `(x, y)`, clamped to the edge so the line integral neither
/// leaks off the grid nor wraps.
fn sample_bilinear(src: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let xf = x.clamp(0.0, (width - 1) as f32);
    let yf = y.clamp(0.0, (height - 1) as f32);
    let x0 = xf.floor() as usize;
    let y0 = yf.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = xf - x0 as f32;
    let ty = yf - y0 as f32;
    let top = src[y0 * width + x0] + (src[y0 * width + x1] - src[y0 * width + x0]) * tx;
    let bot = src[y1 * width + x0] + (src[y1 * width + x1] - src[y1 * width + x0]) * tx;
    top + (bot - top) * ty
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(DirectionalBlur) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        // A unit world at 32 cells: world_to_cells(m) = 32 * m, so a small radius gives a
        // few-cell sigma.
        EvalContext::new(32, 32, Region::UNIT, 0)
    }

    /// A single bright vertical line at `col`, zero elsewhere: a feature that varies only in x.
    fn vertical_line(size: usize, col: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(
                size,
                size,
                |x, _| {
                    if x == col { 1.0 } else { 0.0 }
                },
            )),
        )
    }

    /// A field whose height ramps in x, so its gradient points steadily in +x everywhere.
    fn x_ramp(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                x as f32 / (size - 1) as f32
            })),
        )
    }

    fn run(input: &Field, guide: Option<&Field>, radius: f64, gmode: &str, dir: &str) -> Field {
        let params = Params::new()
            .with("radius", ParamValue::Float(radius))
            .with("guide", ParamValue::Text(gmode.to_string()))
            .with("direction", ParamValue::Text(dir.to_string()));
        // Bind the input slices to locals so their borrows outlive the `Inputs` that stores them.
        let required = [input];
        let result = match guide {
            Some(g) => {
                let optional = [Some(g)];
                DirectionalBlur.eval(Inputs::new(&required, &optional), &params, &ctx())
            }
            None => DirectionalBlur.eval(Inputs::required_only(&required), &params, &ctx()),
        };
        result.unwrap().remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn along_spreads_the_scalar_in_the_guide_direction() {
        // Guide gradient points +x everywhere. Blurring the vertical line ALONG it spreads the line
        // sideways in x, so a neighbouring column picks up value and the line's own column dims.
        let line = vertical_line(32, 16);
        let guide = x_ramp(32);
        let out = run(&line, Some(&guide), 0.06, "slope", "along");
        assert!(
            at(&out, 15, 16) > 0.05,
            "neighbour column should pick up value"
        );
        assert!(
            at(&out, 16, 16) < 1.0,
            "the line's column should dim as it spreads"
        );
    }

    #[test]
    fn across_leaves_a_line_aligned_with_the_cross_direction_untouched() {
        // Across mode blurs perpendicular to the +x guide, i.e. along y. The line is uniform in y,
        // so smoothing along y changes nothing: the neighbour column stays empty.
        let line = vertical_line(32, 16);
        let guide = x_ramp(32);
        let out = run(&line, Some(&guide), 0.06, "slope", "across");
        assert!(
            at(&out, 15, 16) < 0.01,
            "across (along y) must not spread the line in x"
        );
        assert!(at(&out, 16, 16) > 0.99, "the line's column is preserved");
    }

    #[test]
    fn a_sub_cell_radius_is_a_no_op() {
        // radius so small sigma < 0.5 cell: the grid cannot resolve it, so the field is unchanged.
        let line = vertical_line(16, 8);
        let out = run(&line, None, 1e-6, "slope", "along");
        for x in 0..16 {
            assert_eq!(
                at(&out, x, 8),
                at(&line, x, 8),
                "sub-cell radius must pass through"
            );
        }
    }

    #[test]
    fn a_flat_field_is_preserved() {
        // No gradient anywhere means no direction to smooth along; every cell passes through.
        let flat = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.4)));
        let out = run(&flat, None, 0.1, "slope", "along");
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (v - 0.4).abs() < 1e-6)
        );
    }

    #[test]
    fn flow_guide_degrades_to_slope_when_absent() {
        // Asking for the flow guide on a field with no flow layers falls back to the height
        // gradient rather than erroring or producing a zero direction everywhere: the result
        // matches the slope guide exactly.
        let line = vertical_line(32, 16);
        let guide = x_ramp(32);
        let flow = run(&line, Some(&guide), 0.06, "flow", "along");
        let slope = run(&line, Some(&guide), 0.06, "slope", "along");
        assert_eq!(flow.content_hash(), slope.content_hash());
    }

    #[test]
    fn mask_gates_the_blur() {
        // Where mask = 0 the original is kept; where mask = 1 it blurs. A half-masked field differs
        // from the fully blurred one on the masked side and matches the original there.
        let mut line = vertical_line(32, 16);
        // Mask out the top half (y < 16), keep the bottom half.
        line.set_layer(
            layers::MASK,
            Arc::new(Layer::from_fn(
                32,
                32,
                |_, y| if y < 16 { 0.0 } else { 1.0 },
            )),
        );
        let guide = x_ramp(32);
        let out = run(&line, Some(&guide), 0.06, "slope", "along");
        assert_eq!(
            at(&out, 15, 4),
            0.0,
            "masked-out region keeps the original (empty neighbour)"
        );
        assert!(
            at(&out, 15, 20) > 0.05,
            "unmasked region blurs (neighbour picks up value)"
        );
    }

    #[test]
    fn an_explicit_mask_input_gates_the_blur() {
        // A selector-style mask field wired to the mask port (its [0, 1] on the height layer): the
        // blur applies where it is 1 and is blocked where it is 0, exactly as a Slope or Curvature
        // selection would protect ridges. This is the third port, so the guide stays optional(0).
        let line = vertical_line(32, 16);
        let guide = x_ramp(32);
        let mask = Field::new(32, 32, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(
                32,
                32,
                |_, y| if y < 16 { 0.0 } else { 1.0 },
            )),
        );
        let params = Params::new()
            .with("radius", ParamValue::Float(0.06))
            .with("guide", ParamValue::Text("slope".to_string()))
            .with("direction", ParamValue::Text("along".to_string()));
        let required = [&line];
        let optional = [Some(&guide), Some(&mask)];
        let out = DirectionalBlur
            .eval(Inputs::new(&required, &optional), &params, &ctx())
            .unwrap()
            .remove(0);
        assert_eq!(
            at(&out, 15, 4),
            0.0,
            "mask 0 keeps the original (empty neighbour)"
        );
        assert!(
            at(&out, 15, 20) > 0.05,
            "mask 1 blurs (neighbour picks up value)"
        );
    }

    #[test]
    fn is_byte_identical_across_runs() {
        let line = vertical_line(32, 16);
        let guide = x_ramp(32);
        assert_eq!(
            run(&line, Some(&guide), 0.06, "slope", "along").content_hash(),
            run(&line, Some(&guide), 0.06, "slope", "along").content_hash()
        );
    }
}
