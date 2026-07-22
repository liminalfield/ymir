//! Histogram-Scan: window a range of input values into a crisp `[0, 1]` mask.
//!
//! The range-to-mask primitive. It reads the input's value on the `height` layer and selects a
//! window of it: fully `1` inside `[position - width/2, position + width/2]`, falling smoothly to
//! `0` over `falloff` beyond each edge. The output is a `[0, 1]` selection on the `height` layer, so
//! the rest of the toolset shapes it and an effect's `mask` input applies it, and you watch it live
//! in the 2D preview.
//!
//! The `range` mode sets what `position` and `width` are measured against, mirroring Terrace:
//! `auto` (the default) scans the input's actual `[min, max]`, so `position` slides across whatever
//! distribution arrives, unit-free. This is what lets it consume a selector's raw measure directly
//! (a Slope measure in degrees, a Curvature measure in RMS units): `position = 0.5` is the middle of
//! the incoming range whatever its absolute scale. `fixed` measures against the absolute `[0, 1]`,
//! for an input already normalized to that range.
//!
//! Where Levels and Curve reshape a value into another value, Histogram-Scan turns a value into a
//! region: the controllable "select a band of values" that those approximate awkwardly. A pure
//! per-cell transform after a deterministic range read, so it is `NO_WORLD` and byte-identical at
//! any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.histogram_scan";

/// Default window centre: the middle of the scanned range.
const DEFAULT_POSITION: f64 = 0.5;
/// Default window width: half the scanned range, a broad band out of the box.
const DEFAULT_WIDTH: f64 = 0.5;
/// Default edge softness, as a fraction of the scanned range.
const DEFAULT_FALLOFF: f64 = 0.1;

/// Range-mode ids. `auto` measures `position`/`width` against the input's actual `[min, max]`, so a
/// raw measure of any scale is scanned unit-free; `fixed` measures against the absolute `[0, 1]`, for
/// an input already normalized. Auto is the default because scanning the incoming distribution is the
/// usual intent and is what makes a selector's measure directly usable.
const RANGE_AUTO: &str = "auto";
const RANGE_FIXED: &str = "fixed";
const RANGES: &[&str] = &[RANGE_AUTO, RANGE_FIXED];

/// Histogram-Scan modifier: one input, one output. Writes the window selection to
/// [`layers::HEIGHT`].
#[derive(Clone)]
pub struct HistogramScan;

impl Operator for HistogramScan {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "position",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_POSITION),
                ),
                ParamSpec::new(
                    "width",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_WIDTH),
                ),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FALLOFF),
                ),
                ParamSpec::new(
                    "range",
                    ParamKind::Enum { options: RANGES },
                    ParamValue::Text(RANGE_AUTO.to_string()),
                ),
            ],
        }
    }

    /// A pure per-cell transform of the value: it reads no world global, so no world-setting slider
    /// invalidates it.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width_px = input.width();
        let height_px = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let position = params.get_f64("position", DEFAULT_POSITION) as f32;
        let win = params.get_f64("width", DEFAULT_WIDTH).max(0.0) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0) as f32;

        // What position/width are measured against: the input's actual range (Auto), so a raw
        // measure of any scale is scanned unit-free; or the absolute [0, 1] (Fixed). The range read
        // is a deterministic reduction, so the node stays byte-exact.
        let (lo_range, span) = if params.get_str("range", RANGE_AUTO) == RANGE_FIXED {
            (0.0f32, 1.0f32)
        } else {
            let (min, max) = h.value_range();
            (min, max - min)
        };

        let half = win * 0.5;
        let lo = position - half;
        let hi = position + half;

        // Per-cell and pure, so `from_par_fn` is byte-identical at any thread count. Each value is
        // normalized into the scanned range, then a rising lower edge times a falling upper edge
        // gives the window; `lo > hi` (never, since width >= 0) would simply yield an empty mask.
        let selection = Layer::from_par_fn(width_px, height_px, |x, y| {
            let value = h.get(x, y).unwrap_or(0.0);
            // A flat input (zero span) has no distribution to scan; treat every cell as the middle so
            // a centred window still resolves rather than dividing by zero.
            let t = if span <= f32::EPSILON {
                0.5
            } else {
                (value - lo_range) / span
            };
            let lower = smoothstep(lo - falloff, lo, t);
            let upper = 1.0 - smoothstep(hi, hi + falloff, t);
            (lower * upper).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
/// `low == high` degrades to a hard step at that threshold (a zero-width falloff).
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(HistogramScan) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx() -> EvalContext {
        EvalContext::new(16, 16, Region::UNIT, 0)
    }

    /// A field whose height ramps left-to-right from `0` to `peak`.
    fn ramp(size: usize, peak: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                peak * x as f32 / (size - 1) as f32
            })),
        )
    }

    fn scan(input: &Field, position: f32, width: f32, falloff: f32, range: &str) -> Field {
        let params = Params::new()
            .with("position", ParamValue::Float(f64::from(position)))
            .with("width", ParamValue::Float(f64::from(width)))
            .with("falloff", ParamValue::Float(f64::from(falloff)))
            .with("range", ParamValue::Text(range.to_string()));
        HistogramScan
            .eval(Inputs::required_only(&[input]), &params, &ctx())
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn a_value_in_the_window_is_selected() {
        // Centre of a 0->1 ramp sits at position 0.5, inside a width-0.5 window, so it masks ~1.
        let out = scan(&ramp(16, 1.0), 0.5, 0.5, 0.1, "auto");
        assert!(at(&out, 8, 8) > 0.9, "mid value should select ~1");
    }

    #[test]
    fn values_outside_the_window_are_excluded() {
        // The window [0.25, 0.75] excludes both ends of the ramp.
        let out = scan(&ramp(32, 1.0), 0.5, 0.5, 0.05, "auto");
        assert!(at(&out, 0, 0) < 0.01, "low end excluded");
        assert!(at(&out, 31, 0) < 0.01, "high end excluded");
    }

    #[test]
    fn falloff_softens_the_edge() {
        // A value just past the upper edge is partial with a falloff, fully out without one.
        let soft = scan(&ramp(32, 1.0), 0.3, 0.2, 0.15, "auto");
        let hard = scan(&ramp(32, 1.0), 0.3, 0.2, 0.0, "auto");
        // x = 16 on a 32 ramp is value ~0.516, past the upper edge (0.3 + 0.1 = 0.4).
        let s = at(&soft, 16, 0);
        assert!(
            s > 0.01 && s < 0.99,
            "edge should be partial under falloff: {s}"
        );
        assert_eq!(at(&hard, 16, 0), 0.0, "no falloff is a hard cutoff");
    }

    #[test]
    fn auto_range_scans_the_actual_range_unit_free() {
        // A ramp scaled to [0, 90] stands in for a Slope measure in degrees. In Auto, position 0.5
        // is the middle of that range (value ~45), so the centre selects regardless of the scale.
        let out = scan(&ramp(16, 90.0), 0.5, 0.5, 0.05, "auto");
        assert!(at(&out, 8, 8) > 0.9, "mid of the actual range selects ~1");
        assert!(at(&out, 0, 0) < 0.01, "low degrees excluded");
        assert!(at(&out, 15, 8) < 0.01, "high degrees excluded");
    }

    #[test]
    fn fixed_range_uses_absolute_values() {
        // Fixed measures against absolute [0, 1], ignoring the input's actual span. On a [0, 1] ramp
        // the centre (value ~0.5) sits in the window and selects; on a [0, 90] ramp the centre
        // (value 45) is far outside it, so the same window selects nothing there. Contrast the auto
        // test, where the [0, 90] centre selects because auto scans the real range.
        let unit = scan(&ramp(16, 1.0), 0.5, 0.5, 0.05, "fixed");
        assert!(
            at(&unit, 8, 8) > 0.9,
            "value ~0.5 sits inside the absolute window"
        );
        let scaled = scan(&ramp(16, 90.0), 0.5, 0.5, 0.05, "fixed");
        assert!(
            at(&scaled, 8, 8) < 0.01,
            "value 45 is far outside the absolute window"
        );
    }

    #[test]
    fn flat_input_resolves_without_dividing_by_zero() {
        // A constant field has zero span; every cell reads as the middle, so a centred full-width
        // window selects it and a window off to the side does not.
        let flat = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, 0.3)));
        assert!(
            at(&scan(&flat, 0.5, 1.0, 0.0, "auto"), 4, 4) > 0.9,
            "centred window selects it"
        );
        assert!(
            at(&scan(&flat, 0.1, 0.1, 0.0, "auto"), 4, 4) < 0.01,
            "off-centre window misses it"
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = scan(&ramp(16, 1.0), 0.5, 0.5, 0.1, "auto");
        assert!(
            out.layer(layers::MASK).is_none(),
            "no mask layer is written"
        );
        assert!(at(&out, 8, 8) > 0.0, "the selection is on the height layer");
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(16, 1.0);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.7)));
        let out = scan(&input, 0.5, 0.5, 0.1, "auto");
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = scan(&ramp(16, 1.0), 0.5, 0.5, 0.1, "auto");
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_byte_identical_across_runs() {
        // Pure per-cell after a deterministic range read: two runs hash identically.
        let input = ramp(16, 1.0);
        assert_eq!(
            scan(&input, 0.5, 0.5, 0.1, "auto").content_hash(),
            scan(&input, 0.5, 0.5, 0.1, "auto").content_hash()
        );
    }
}
