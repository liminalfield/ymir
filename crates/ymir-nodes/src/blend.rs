//! Blend: composites two height fields, Photoshop-style.
//!
//! Inputs are `base` (the bottom layer) and `overlay` (the top layer). A `mode`
//! chooses how the overlay combines with the base, and `opacity` (`[0, 1]`) is the
//! strength of that effect. The whole node is one expression:
//!
//! ```text
//! result = lerp(base, mode(base, overlay), opacity * mask)
//! ```
//!
//! so `opacity = 0` leaves the base untouched for every mode, `opacity = 1` applies
//! the mode fully, and `Normal` (effect = overlay) makes the slider a base<->overlay
//! crossfade. The optional `mask` input localizes the effect per cell: its height
//! layer is the selection; when it is unwired the base's own `mask` layer is used by
//! convention, and with neither the mask reads `1.0` (soft-layer contract: the node
//! never gates on a mask). The base's non-height layers pass through. The base and
//! overlay are not symmetric: the base is what survives as opacity or mask fall to
//! zero.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.blend";

/// Blend mode ids, in dropdown order. These are the stored param values. Only the
/// terrain-meaningful modes are offered: the photographic modes (screen, overlay,
/// dodge/burn, soft/hard light) assume colour in `[0, 1]` with a neutral midpoint,
/// which a heightfield does not have.
const MODE_NORMAL: &str = "normal";
const MODE_ADD: &str = "add";
const MODE_SUBTRACT: &str = "subtract";
const MODE_MULTIPLY: &str = "multiply";
const MODE_MAX: &str = "max";
const MODE_MIN: &str = "min";
const MODE_DIFFERENCE: &str = "difference";
const MODES: &[&str] = &[
    MODE_NORMAL,
    MODE_ADD,
    MODE_SUBTRACT,
    MODE_MULTIPLY,
    MODE_MAX,
    MODE_MIN,
    MODE_DIFFERENCE,
];

/// Two-input blend modifier: inputs `base` and `overlay`, one output.
#[derive(Clone)]
pub struct Blend;

impl Operator for Blend {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "combine",
            inputs: vec![
                PortSpec::new("base"),
                PortSpec::new("overlay"),
                // Optional: a field whose height is the selection. When unwired, the
                // base's own mask layer is used by convention, else apply everywhere.
                PortSpec::optional("mask"),
            ],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "mode",
                    ParamKind::Enum { options: MODES },
                    ParamValue::Text(MODE_NORMAL.to_string()),
                ),
                // Strength of the overlay's effect on the base. The default of 1.0
                // applies the mode fully; 0.0 leaves the base untouched.
                ParamSpec::new(
                    "opacity",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(1.0),
                ),
            ],
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        // The evaluator gathers every input slot before calling eval, so both are
        // present; an unwired port would have failed upstream.
        let base_field = inputs[0];
        let overlay_field = inputs[1];
        let width = base_field.width();
        let height = base_field.height();

        let base = base_field.layer_or(layers::HEIGHT, 0.0);
        let overlay = overlay_field.layer_or(layers::HEIGHT, 0.0);
        // The mask localizes the effect. An explicit mask input wins (its height
        // layer is the selection); with none, the base's own mask layer by
        // convention; with neither, a uniform 1.0 (apply everywhere). Soft-layer
        // contract either way: the node never gates on a mask.
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => base_field.layer_or(layers::MASK, 1.0),
        };

        let mode = params.get_str("mode", MODE_NORMAL);
        let opacity = params.get_f64("opacity", 1.0) as f32;

        let blended = Layer::from_fn(width, height, |x, y| {
            let bv = base.get(x, y).unwrap_or(0.0);
            let ov = overlay.get(x, y).unwrap_or(0.0);
            // The mode's full result, the "effect" we blend the base toward. Normal
            // goes straight to the overlay, so the blend below makes it a crossfade.
            let effect = match mode {
                MODE_ADD => bv + ov,
                MODE_SUBTRACT => bv - ov,
                MODE_MULTIPLY => bv * ov,
                MODE_MAX => bv.max(ov),
                MODE_MIN => bv.min(ov),
                MODE_DIFFERENCE => (bv - ov).abs(),
                // MODE_NORMAL, and any unrecognized id: the overlay (a plain over).
                _ => ov,
            };
            // Ease from the base toward the effect by opacity * mask.
            let t = opacity * mask.get(x, y).unwrap_or(1.0);
            bv + (effect - bv) * t
        });

        let mut out = base_field.clone();
        out.set_layer(layers::HEIGHT, Arc::new(blended));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Blend) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        // Blend reads its input fields' dimensions, not the context's, so the
        // context's resolution is irrelevant here.
        EvalContext::new(8, 8, Region::UNIT, 0)
    }

    fn const_field(value: f32) -> Field {
        Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, value)))
    }

    fn blend(base: &Field, overlay: &Field, mode: &str, opacity: f64) -> Field {
        let params = Params::new()
            .with("mode", ParamValue::Text(mode.to_string()))
            .with("opacity", ParamValue::Float(opacity));
        Blend
            .eval(Inputs::required_only(&[base, overlay]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn height_at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn modes_compose_base_and_overlay_at_full_opacity() {
        let base = const_field(0.6);
        let overlay = const_field(0.5);
        for (mode, expected) in [
            (MODE_NORMAL, 0.5_f32),
            (MODE_ADD, 1.1),
            (MODE_SUBTRACT, 0.1),
            (MODE_MULTIPLY, 0.30),
            (MODE_MAX, 0.6),
            (MODE_MIN, 0.5),
            (MODE_DIFFERENCE, 0.1),
        ] {
            let v = height_at(&blend(&base, &overlay, mode, 1.0), 0, 0);
            assert!(
                (v - expected).abs() < 1e-6,
                "mode {mode}: {v} != {expected}"
            );
        }
    }

    #[test]
    fn normal_at_opacity_is_a_base_to_overlay_crossfade() {
        let base = const_field(0.2);
        let overlay = const_field(0.8);
        // 0 -> base, 1 -> overlay, 0.5 -> the average.
        assert!((height_at(&blend(&base, &overlay, MODE_NORMAL, 0.0), 0, 0) - 0.2).abs() < 1e-6);
        assert!((height_at(&blend(&base, &overlay, MODE_NORMAL, 1.0), 0, 0) - 0.8).abs() < 1e-6);
        assert!((height_at(&blend(&base, &overlay, MODE_NORMAL, 0.5), 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn opacity_eases_every_mode_from_the_base() {
        let base = const_field(0.6);
        let overlay = const_field(0.5);
        // add result is 1.1; at opacity 0.5 the output is halfway from the base to it.
        assert!((height_at(&blend(&base, &overlay, MODE_ADD, 0.5), 0, 0) - 0.85).abs() < 1e-6);
        // opacity 0 leaves the base untouched, for every mode.
        for mode in MODES {
            assert!(
                (height_at(&blend(&base, &overlay, mode, 0.0), 0, 0) - 0.6).abs() < 1e-6,
                "mode {mode} at opacity 0 should leave the base unchanged"
            );
        }
    }

    #[test]
    fn mask_localizes_the_effect() {
        let mut base = const_field(0.0);
        base.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, 0.5)));
        let overlay = const_field(1.0);
        // t = opacity * mask = 0.5 * 0.5 = 0.25, so lerp(0, 1, 0.25) == 0.25 (normal).
        assert!((height_at(&blend(&base, &overlay, MODE_NORMAL, 0.5), 0, 0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_connected_mask_input_is_used_and_overrides_the_base_mask_layer() {
        // The base carries a mask layer that says "apply nowhere"; a connected mask
        // input must win, so its height (0.5) is the selection.
        let mut base = const_field(0.0);
        base.set_layer(layers::MASK, Arc::new(Layer::filled(8, 8, 0.0)));
        let overlay = const_field(1.0);
        let mask = const_field(0.5);

        let params = Params::new()
            .with("mode", ParamValue::Text(MODE_NORMAL.to_string()))
            .with("opacity", ParamValue::Float(1.0));
        let required = [&base, &overlay];
        let optional = [Some(&mask)];
        let out = Blend
            .eval(Inputs::new(&required, &optional), &params, &ctx())
            .unwrap()
            .remove(0);

        // t = opacity * mask_input = 1.0 * 0.5 = 0.5, so lerp(0, 1, 0.5) == 0.5 — not
        // 0.0, which the base's own mask layer would have produced.
        assert!((height_at(&out, 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn passes_through_the_bases_other_layers() {
        let mut base = const_field(0.3);
        base.set_layer("flow", Arc::new(Layer::filled(8, 8, 0.42)));
        let overlay = const_field(0.1);
        let out = blend(&base, &overlay, MODE_ADD, 1.0);
        // The base's non-height layers survive unchanged on the output.
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.42);
    }

    #[test]
    fn is_deterministic() {
        let base = const_field(0.3);
        let overlay = const_field(0.7);
        assert_eq!(
            blend(&base, &overlay, MODE_MAX, 0.4).content_hash(),
            blend(&base, &overlay, MODE_MAX, 0.4).content_hash()
        );
    }

    #[test]
    fn output_matches_golden_value() {
        // Two orthogonal gradients multiplied: a fixed, textured output to pin
        // behavior.
        let base = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |x, _| x as f32 / 15.0)),
        );
        let overlay = Field::new(16, 16, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |_, y| y as f32 / 15.0)),
        );
        let out = blend(&base, &overlay, MODE_MULTIPLY, 1.0);
        assert_eq!(out.content_hash().to_u64(), 0xdd85_423c_6930_f6c7);
    }
}
