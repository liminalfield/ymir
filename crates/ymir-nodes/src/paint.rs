//! Paint: a hand-painted `[0, 1]` mask, rasterized from brush strokes.
//!
//! A source with one optional `backdrop` input and one output. Its one param is a [`Strokes`] set
//! authored by brushing on the 2D map or the 3D surface (see the GUI); `eval` rasterizes those
//! strokes to the `height` layer at
//! the requested resolution, so the mask is **resolution-independent** — the same strokes fill the
//! same normalized region at any build resolution. The output plugs into the `mask` inputs the
//! effect nodes already honor (Directional Blur, erosion, coastal, blend), so painting scopes an
//! effect to a hand-chosen region.
//!
//! Strokes are composited in paint order per cell: paint moves the value toward 1 with opacity
//! `strength`, erase toward 0, each weighted by the brush's spatial falloff and the point's weight.
//! Radius is a fraction of the region width (canvas-relative), so the node reads no world global
//! (`NO_WORLD`). Per-cell and pure, so `from_par_fn` is byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, Stroke, StrokeMode, Strokes, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.paint";

/// Paint generator: no inputs, one output. Writes the rasterized mask to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Paint;

impl Operator for Paint {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            // An optional backdrop: the terrain to paint over. It does not affect the mask; the node
            // carries it on the `backdrop` layer so the 3D view can mesh the real surface while the
            // mask rides the height layer as a texture. Unwired, Paint is a plain source.
            inputs: vec![PortSpec::optional("backdrop")],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "strokes",
                ParamKind::Strokes,
                ParamValue::Strokes(Strokes::new()),
            )],
        }
    }

    /// Independent of every world global and, through normalized coordinates, of resolution, so no
    /// world-setting slider invalidates this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let width = ctx.width;
        let height = ctx.height;
        let empty = Strokes::new();
        let strokes = params.get_strokes("strokes", &empty);

        // Per cell, composite the strokes in paint order. Cell centres map to normalized region
        // coordinates in [0, 1], the same space the strokes are stored in.
        let layer = Layer::from_par_fn(width, height, |x, y| {
            let px = (x as f32 + 0.5) / width as f32;
            let py = (y as f32 + 0.5) / height as f32;
            let mut v = 0.0_f32;
            for stroke in strokes.strokes() {
                let alpha = stroke_coverage(stroke, px, py);
                if alpha <= 0.0 {
                    continue;
                }
                let opacity = (alpha * stroke.strength).clamp(0.0, 1.0);
                match stroke.mode {
                    // Paint toward 1, erase toward 0, at this opacity, so overlapping strokes build
                    // up and never overshoot the unit range.
                    StrokeMode::Paint => v += (1.0 - v) * opacity,
                    StrokeMode::Erase => v -= v * opacity,
                }
            }
            v.clamp(0.0, 1.0)
        });

        let mut field =
            Field::new(width, height, ctx.region).with_layer(layers::HEIGHT, Arc::new(layer));

        // Carry the backdrop terrain (display only) so the viewport can mesh the real surface under
        // the painted mask. The mask stays on the height layer, so the mask ports are unaffected.
        if let Some(backdrop) = inputs.optional(0) {
            field.set_layer(layers::BACKDROP, backdrop.layer_or(layers::HEIGHT, 0.0));
        }
        Ok(vec![field])
    }
}

/// The stroke's brush coverage at normalized point `(px, py)`: the spatial falloff at the distance
/// to the stroke's path, scaled by the interpolated per-point weight. `0` outside the brush.
fn stroke_coverage(stroke: &Stroke, px: f32, py: f32) -> f32 {
    let r = stroke.radius.max(1e-6);
    let (best_d2, best_w) = match stroke.path.as_slice() {
        [] => return 0.0,
        [p] => ((px - p.x) * (px - p.x) + (py - p.y) * (py - p.y), p.weight),
        points => {
            let mut best = (f32::INFINITY, 1.0_f32);
            for seg in points.windows(2) {
                let (d2, w) = point_segment(px, py, seg[0].x, seg[0].y, seg[1].x, seg[1].y)
                    .apply_weight(seg[0].weight, seg[1].weight);
                if d2 < best.0 {
                    best = (d2, w);
                }
            }
            best
        }
    };
    falloff(best_d2.sqrt(), r, stroke.hardness) * best_w.clamp(0.0, 1.0)
}

/// The squared distance from `(px, py)` to the segment `a-b`, and the projection parameter `t` in
/// `[0, 1]` along the segment (for interpolating the per-endpoint weight).
struct SegmentHit {
    dist2: f32,
    t: f32,
}

impl SegmentHit {
    /// Folds the endpoint weights into `(dist2, weight)` by interpolating at the projection.
    fn apply_weight(self, wa: f32, wb: f32) -> (f32, f32) {
        (self.dist2, wa + (wb - wa) * self.t)
    }
}

/// Squared distance from `(px, py)` to segment `(ax, ay)-(bx, by)` and the clamped projection.
fn point_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> SegmentHit {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (ax + dx * t, ay + dy * t);
    SegmentHit {
        dist2: (px - cx) * (px - cx) + (py - cy) * (py - cy),
        t,
    }
}

/// Brush falloff at distance `d` for radius `r` and `hardness` in `[0, 1]`: full inside a core of
/// `r * hardness`, smoothstepping to 0 at `r`, and 0 beyond. `hardness = 1` is a hard-edged disc.
fn falloff(d: f32, r: f32, hardness: f32) -> f32 {
    if d >= r {
        return 0.0;
    }
    let core = r * hardness.clamp(0.0, 1.0);
    if d <= core {
        return 1.0;
    }
    // core < d < r, so r - core > 0: no division by zero.
    let t = (r - d) / (r - core);
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Paint) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "source", sort: 52 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{BrushShape, Region, StrokePoint};

    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0)
    }

    fn eval(strokes: Strokes, size: usize) -> Field {
        let params = Params::new().with("strokes", ParamValue::Strokes(strokes));
        Paint
            .eval(Inputs::required_only(&[]), &params, &ctx(size))
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A single dot stroke centred at `(cx, cy)`.
    fn dot(
        cx: f32,
        cy: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        mode: StrokeMode,
    ) -> Stroke {
        Stroke {
            radius,
            strength,
            hardness,
            mode,
            shape: BrushShape::Round,
            path: vec![StrokePoint::new(cx, cy)],
        }
    }

    #[test]
    fn empty_strokes_give_a_zero_field() {
        let out = eval(Strokes::new(), 16);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| v == 0.0),
            "no strokes paints nothing"
        );
    }

    #[test]
    fn a_round_stroke_paints_a_disc() {
        // A hard round dot at the centre: the centre is full, a far corner is untouched.
        let out = eval(
            Strokes::from_strokes(vec![dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint)]),
            32,
        );
        assert!(at(&out, 16, 16) > 0.99, "centre is painted");
        assert_eq!(at(&out, 0, 0), 0.0, "a far corner is untouched");
    }

    #[test]
    fn erase_removes_what_was_painted() {
        // Paint a broad disc, then erase a dot in its centre.
        let painted = dot(0.5, 0.5, 0.4, 1.0, 1.0, StrokeMode::Paint);
        let erased = dot(0.5, 0.5, 0.15, 1.0, 1.0, StrokeMode::Erase);
        let out = eval(Strokes::from_strokes(vec![painted, erased]), 32);
        assert_eq!(at(&out, 16, 16), 0.0, "centre erased back to 0");
        assert!(at(&out, 16, 26) > 0.5, "the surrounding ring survives");
    }

    #[test]
    fn soft_brush_falls_off() {
        // A soft dot: full at the centre, partial partway out, zero past the radius.
        let out = eval(
            Strokes::from_strokes(vec![dot(0.5, 0.5, 0.3, 1.0, 0.0, StrokeMode::Paint)]),
            64,
        );
        let centre = at(&out, 32, 32);
        let mid = at(&out, 44, 32); // ~0.19 normalized out, inside the 0.3 radius
        assert!(centre > 0.99, "soft brush is full at the centre: {centre}");
        assert!(
            mid > 0.01 && mid < centre,
            "and falls off toward the edge: {mid}"
        );
    }

    #[test]
    fn is_resolution_independent() {
        // The same normalized stroke fills the same normalized region at any resolution: the centre
        // and a fixed normalized offset read the same at 32 and 96 cells.
        let strokes = Strokes::from_strokes(vec![dot(0.5, 0.5, 0.25, 1.0, 0.5, StrokeMode::Paint)]);
        let lo = eval(strokes.clone(), 32);
        let hi = eval(strokes, 96);
        // Centre cell of each.
        assert!(
            (at(&lo, 16, 16) - at(&hi, 48, 48)).abs() < 0.02,
            "centres match across resolution"
        );
    }

    #[test]
    fn is_byte_identical_across_runs() {
        let strokes = Strokes::from_strokes(vec![dot(0.5, 0.5, 0.25, 1.0, 0.5, StrokeMode::Paint)]);
        assert_eq!(
            eval(strokes.clone(), 32).content_hash(),
            eval(strokes, 32).content_hash()
        );
    }

    #[test]
    fn a_wired_backdrop_is_carried_for_display() {
        // The backdrop terrain rides the backdrop layer while the mask stays on height, so the mask
        // ports (which read height) are unaffected and the viewport can mesh the real surface.
        let strokes = Strokes::from_strokes(vec![dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint)]);
        let params = Params::new().with("strokes", ParamValue::Strokes(strokes));
        let terrain = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.7)));
        let required: [&Field; 0] = [];
        let optional = [Some(&terrain)];
        let out = Paint
            .eval(Inputs::new(&required, &optional), &params, &ctx(16))
            .unwrap()
            .remove(0);
        assert!(at(&out, 8, 8) > 0.99, "the mask stays on the height layer");
        assert_eq!(
            out.layer(layers::BACKDROP).unwrap().get(0, 0).unwrap(),
            0.7,
            "the backdrop terrain is carried on the backdrop layer"
        );
    }

    #[test]
    fn no_backdrop_layer_when_unwired() {
        let out = eval(Strokes::new(), 16);
        assert!(
            out.layer(layers::BACKDROP).is_none(),
            "an unwired Paint carries no backdrop layer"
        );
    }
}
