//! Sculpt: hand-raise and lower terrain by brushing height onto it.
//!
//! Brushes a signed height offset onto the `height` layer: a raise stroke adds height, a lower stroke
//! subtracts it, and overlapping strokes accumulate so you build form up pass by pass. Every other
//! layer passes through untouched.
//!
//! The base input is optional. With a terrain wired in, Sculpt edits that terrain (its resolution);
//! with nothing wired, it sculpts onto a flat zero field at the requested resolution, so the same node
//! builds form from scratch or adds to existing terrain. The surface it edits is also the surface you
//! paint on and mesh, so sculpting shows live without any separate display backdrop.
//!
//! Strength is the brush intensity in `[0, 1]`, and a stroke moves the surface by that much at full
//! coverage: raise adds it, lower subtracts it. The two are symmetric, the signed brush every sculpting
//! tool uses (raise, then invert to carve), so a lower stroke digs *below* the surface into a pit
//! rather than scrubbing back toward a floor. The range is not clamped either way, so a built-up peak
//! may exceed 1 and a carved pit fall below 0; the height-range convention resolves the working range
//! at display and export. The result is non-destructive by construction: a new field is produced and
//! the input is never touched. The brush is the selection, so an incoming `mask` layer passes through
//! rather than gating the stroke. Per-cell and pure, so `from_par_fn` is byte-identical at any thread
//! count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, StrokeMode, Strokes, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.sculpt";

/// Sculpt modifier: an optional base input, one output. Adds a hand-painted signed height offset to
/// the height layer.
#[derive(Clone)]
pub struct Sculpt;

impl Operator for Sculpt {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            // Pointwise, single input: the same palette cut as Invert and Levels. The input is
            // optional so the node also builds form from scratch.
            category: "adjust",
            inputs: vec![PortSpec::optional("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "strokes",
                ParamKind::Strokes,
                ParamValue::Strokes(Strokes::new()),
            )],
        }
    }

    /// The offset is authored in normalized region coordinates and added to the base in its own
    /// units, independent of every world global, so no world-setting slider invalidates this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let empty = Strokes::new();
        let strokes = params.get_strokes("strokes", &empty);

        // With a base wired, sculpt onto it at its resolution; without one, sculpt onto a flat zero
        // field at the requested resolution, so the node also builds form from scratch.
        let base = inputs.optional(0);
        let (width, height) = match base {
            Some(f) => (f.width(), f.height()),
            None => (ctx.width, ctx.height),
        };
        let base_layer = base.map(|f| f.layer_or(layers::HEIGHT, 0.0));

        // Per cell, composite the strokes in order onto the base value. Cell centres map to the
        // normalized region coordinates the strokes are stored in. A raise stroke adds its strength, a
        // lower stroke subtracts it, so overlapping strokes accumulate and a lower pass carves below the
        // surface. The range is not clamped either way (it resolves at display and export).
        let layer = Layer::from_par_fn(width, height, |x, y| {
            let mut v = base_layer
                .as_ref()
                .map_or(0.0, |b| b.get(x, y).unwrap_or(0.0));
            let px = (x as f32 + 0.5) / width as f32;
            let py = (y as f32 + 0.5) / height as f32;
            for stroke in strokes.strokes() {
                let coverage = stroke.coverage(px, py);
                if coverage <= 0.0 {
                    continue;
                }
                let amount = (coverage * stroke.strength).clamp(0.0, 1.0);
                // The shared stroke modes read as raise/lower here: Paint raises, Erase lowers.
                match stroke.mode {
                    StrokeMode::Paint => v += amount,
                    StrokeMode::Erase => v -= amount,
                }
            }
            v
        });

        let mut out = match base {
            Some(f) => f.clone(),
            None => Field::new(width, height, ctx.region),
        };
        out.set_layer(layers::HEIGHT, Arc::new(layer));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Sculpt) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{BrushShape, Region, Stroke, StrokePoint};

    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0)
    }

    /// A flat base field at `value`, on the height layer.
    fn base_field(size: usize, value: f32) -> Field {
        Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(size, size, value)))
    }

    /// Evaluate Sculpt with an optional base field wired in.
    fn eval_sculpt(base: Option<&Field>, strokes: Strokes, size: usize) -> Field {
        let params = Params::new().with("strokes", ParamValue::Strokes(strokes));
        let ctx = ctx(size);
        let out = match base {
            Some(f) => {
                let required: [&Field; 0] = [];
                let optional = [Some(f)];
                Sculpt.eval(Inputs::new(&required, &optional), &params, &ctx)
            }
            None => Sculpt.eval(Inputs::required_only(&[]), &params, &ctx),
        };
        out.unwrap().remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A single dot stroke centred at `(cx, cy)`.
    fn stroke(
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

    /// A stroke set of one dot.
    fn dot(
        cx: f32,
        cy: f32,
        radius: f32,
        strength: f32,
        hardness: f32,
        mode: StrokeMode,
    ) -> Strokes {
        Strokes::from_strokes(vec![stroke(cx, cy, radius, strength, hardness, mode)])
    }

    #[test]
    fn empty_strokes_pass_the_base_through() {
        let out = eval_sculpt(Some(&base_field(16, 0.3)), Strokes::new(), 16);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (v - 0.3).abs() < 1e-6),
            "with no strokes the base is unchanged"
        );
    }

    #[test]
    fn with_no_input_it_sculpts_from_a_zero_field() {
        // Unwired, Sculpt builds form from scratch: a full-strength raise lifts the centre to 1, the
        // rest stays flat at zero.
        let out = eval_sculpt(None, dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint), 32);
        assert!(
            (at(&out, 16, 16) - 1.0).abs() < 0.005,
            "a full-strength raise lifts the centre to 1 from zero: {}",
            at(&out, 16, 16)
        );
        assert_eq!(at(&out, 0, 0), 0.0, "a far corner stays at zero");
    }

    #[test]
    fn raise_adds_the_strength() {
        // A hard, fully-covered raise lifts the centre by its strength: 0.3 + 0.5 = 0.8.
        let out = eval_sculpt(
            Some(&base_field(32, 0.3)),
            dot(0.5, 0.5, 0.25, 0.5, 1.0, StrokeMode::Paint),
            32,
        );
        assert!(
            (at(&out, 16, 16) - 0.8).abs() < 0.005,
            "centre raised by the strength: {} vs 0.8",
            at(&out, 16, 16)
        );
        assert!(
            (at(&out, 0, 0) - 0.3).abs() < 1e-6,
            "a far corner keeps the base"
        );
    }

    #[test]
    fn lower_subtracts_the_strength() {
        // A lower stroke subtracts its strength, symmetric with raise: 0.6 - 0.4 = 0.2.
        let out = eval_sculpt(
            Some(&base_field(32, 0.6)),
            dot(0.5, 0.5, 0.25, 0.4, 1.0, StrokeMode::Erase),
            32,
        );
        assert!(
            (at(&out, 16, 16) - 0.2).abs() < 0.005,
            "centre lowered by the strength: {} vs 0.2",
            at(&out, 16, 16)
        );
    }

    #[test]
    fn overlapping_strokes_accumulate() {
        // Two full raise strokes over the same spot build to about twice one: form builds pass by pass.
        let base = base_field(32, 0.2);
        let one = eval_sculpt(
            Some(&base),
            dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint),
            32,
        );
        let two = eval_sculpt(
            Some(&base),
            Strokes::from_strokes(vec![
                stroke(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint),
                stroke(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint),
            ]),
            32,
        );
        let d1 = at(&one, 16, 16) - 0.2;
        let d2 = at(&two, 16, 16) - 0.2;
        assert!(d1 > 0.0, "one raise stroke lifts the surface: {d1}");
        assert!(
            (d2 - 2.0 * d1).abs() < 1e-4,
            "two raise strokes stack to about twice one: {d2} vs {}",
            2.0 * d1
        );
    }

    #[test]
    fn the_range_is_not_clamped_either_way() {
        // A built-up peak passes 1 and a carved pit falls below 0: honest data, not saturated, so the
        // shape survives to display and export.
        let peak = eval_sculpt(
            Some(&base_field(32, 0.95)),
            dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Paint),
            32,
        );
        assert!(
            at(&peak, 16, 16) > 1.0,
            "0.95 raised by a full stroke exceeds 1 without clamping: {}",
            at(&peak, 16, 16)
        );
        let pit = eval_sculpt(
            Some(&base_field(32, 0.05)),
            dot(0.5, 0.5, 0.25, 1.0, 1.0, StrokeMode::Erase),
            32,
        );
        assert!(
            at(&pit, 16, 16) < 0.0,
            "0.05 lowered by a full stroke drops below 0 without clamping: {}",
            at(&pit, 16, 16)
        );
    }

    #[test]
    fn other_layers_pass_through_and_the_brush_is_not_mask_gated() {
        // The base carries a mask layer at 0.7. Sculpt edits only height and leaves the mask untouched,
        // and it paints at full strength regardless of the mask (the brush is the selection).
        let mut base = base_field(16, 0.4);
        base.set_layer(layers::MASK, Arc::new(Layer::filled(16, 16, 0.7)));
        let out = eval_sculpt(
            Some(&base),
            dot(0.5, 0.5, 0.3, 1.0, 1.0, StrokeMode::Paint),
            16,
        );
        assert_eq!(
            out.layer(layers::MASK).unwrap().get(0, 0).unwrap(),
            0.7,
            "the mask layer passes through untouched"
        );
        assert!(
            (at(&out, 8, 8) - 1.4).abs() < 0.005,
            "the raise applies at full strength regardless of the mask layer: {}",
            at(&out, 8, 8)
        );
    }

    #[test]
    fn is_resolution_independent() {
        // The same normalized stroke over the same flat base reads the same at the matching normalized
        // point across resolutions.
        let lo = eval_sculpt(
            Some(&base_field(32, 0.2)),
            dot(0.5, 0.5, 0.25, 0.5, 0.5, StrokeMode::Paint),
            32,
        );
        let hi = eval_sculpt(
            Some(&base_field(96, 0.2)),
            dot(0.5, 0.5, 0.25, 0.5, 0.5, StrokeMode::Paint),
            96,
        );
        assert!(
            (at(&lo, 16, 16) - at(&hi, 48, 48)).abs() < 0.02,
            "centres match across resolution"
        );
    }

    #[test]
    fn is_byte_identical_across_runs() {
        let base = base_field(32, 0.2);
        let a = eval_sculpt(
            Some(&base),
            dot(0.5, 0.5, 0.25, 0.5, 0.5, StrokeMode::Paint),
            32,
        );
        let b = eval_sculpt(
            Some(&base),
            dot(0.5, 0.5, 0.25, 0.5, 0.5, StrokeMode::Paint),
            32,
        );
        assert_eq!(a.content_hash(), b.content_hash());
    }
}
