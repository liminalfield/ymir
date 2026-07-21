//! Terrace: quantize the height layer into stepped bands (flat treads joined by risers).
//!
//! `bands` terraces are spread across a height interval, reshaping each band by a smoothstep whose
//! transition width is set by `sharpness`: at `0` the step is a gentle S-curve (rounded terraces), at
//! `1` a near-vertical riser between flat treads. The reshaping keeps the band endpoints fixed, so
//! terraces join continuously and the surface stays monotonic (ordering is preserved, no gaps or
//! overhangs). This is the strata / bench / mesa move.
//!
//! The interval is set by the `range` mode: `auto` (the default) spans the layer's actual
//! `[min, max]`, so `bands` is the count you see whatever the terrain's distribution; `fixed` spans
//! the absolute `[0, 1]`, so the levels sit at fixed elevations and the visible count depends on how
//! much of the range the terrain fills.
//!
//! Mask-aware per the convention: the terraced height is composited over the original through the
//! `mask` layer, so `mask = 1` is fully terraced and `mask = 0` is left untouched; other layers pass
//! through. A pure per-cell transform of the height value: resolution- and world-independent, so it
//! is `NO_WORLD` and byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.terrace";

/// Default number of terraces over the `[0, 1]` working height: enough to read as strata without
/// shredding the form.
const DEFAULT_BANDS: f64 = 8.0;
/// Default riser sharpness: halfway between rounded steps and hard treads.
const DEFAULT_SHARPNESS: f64 = 0.5;

/// Range-mode ids. `auto` spreads `bands` terraces across the layer's actual `[min, max]`, so the
/// count is what you see whatever the terrain's distribution; `fixed` places them at absolute levels
/// over `[0, 1]`, so the levels are stable but the visible count depends on the range the terrain
/// fills. Auto is the default because "N terraces" is the usual intent.
const RANGE_AUTO: &str = "auto";
const RANGE_FIXED: &str = "fixed";
const RANGES: &[&str] = &[RANGE_AUTO, RANGE_FIXED];

/// Terrace modifier: one input, one output.
#[derive(Clone)]
pub struct Terrace;

impl Operator for Terrace {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "bands",
                    ParamKind::Float {
                        min: 2.0,
                        max: 64.0,
                    },
                    ParamValue::Float(DEFAULT_BANDS),
                ),
                ParamSpec::new(
                    "sharpness",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_SHARPNESS),
                ),
                ParamSpec::new(
                    "range",
                    ParamKind::Enum { options: RANGES },
                    ParamValue::Text(RANGE_AUTO.to_string()),
                ),
            ],
        }
    }

    /// A pure per-cell transform of the height value: it reads no world global, so no world-setting
    /// slider invalidates it.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        let bands = params.get_f64("bands", DEFAULT_BANDS).max(2.0) as f32;
        let sharpness = params
            .get_f64("sharpness", DEFAULT_SHARPNESS)
            .clamp(0.0, 1.0) as f32;

        // The interval the terraces span: the layer's actual range (Auto), so `bands` is the visible
        // count whatever the distribution; or a fixed [0, 1] (Fixed), so the levels are at absolute
        // elevations. The range read is a deterministic reduction, so the node stays byte-exact.
        let (lo, span) = if params.get_str("range", RANGE_AUTO) == RANGE_FIXED {
            (0.0, 1.0)
        } else {
            let (min, max) = h.value_range();
            (min, max - min)
        };

        // Per-cell and pure, so `from_par_fn` is byte-identical at any thread count. Values are mapped
        // into [0, 1] across the band interval, terraced, and mapped back; a flat layer (zero span)
        // has nothing to terrace. Mask-aware: composite the terraced height over the original.
        let shaped = Layer::from_par_fn(width, height, |x, y| {
            let value = h.get(x, y).unwrap_or(0.0);
            let terraced = if span <= f32::EPSILON {
                value
            } else {
                lo + terrace((value - lo) / span, bands, sharpness) * span
            };
            let m = mask.get(x, y).unwrap_or(1.0);
            value + (terraced - value) * m
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(shaped));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Terrace) }
}

/// Quantizes `value` into `bands` terraces over the working `[0, 1]` height, reshaping each band by a
/// smoothstep whose riser half-width is `0.5 * (1 - sharpness)`: `sharpness = 0` gives a gentle
/// S-curve, `sharpness = 1` a near-vertical riser between flat treads. Endpoints of each band are
/// fixed (`shaped(0) = 0`, `shaped(1) = 1`), so terraces join continuously and the result is
/// monotonic. Values outside `[0, 1]` terrace on the same fixed grid (the range is never clamped).
fn terrace(value: f32, bands: f32, sharpness: f32) -> f32 {
    let f = value * bands;
    let base = f.floor();
    let t = f - base;
    let w = (0.5 * (1.0 - sharpness)).max(1e-4);
    let shaped = smoothstep(0.5 - w, 0.5 + w, t);
    (base + shaped) / bands
}

/// The classic smoothstep: `0` below `edge0`, `1` above `edge1`, a Hermite ease between. `edge0 <
/// edge1` at every call here.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
    }

    fn run(input: &Field, bands: f64, sharpness: f64) -> Field {
        let params = Params::new()
            .with("bands", ParamValue::Float(bands))
            .with("sharpness", ParamValue::Float(sharpness));
        Terrace
            .eval(
                Inputs::required_only(&[input]),
                &params,
                &ctx(input.width()),
            )
            .unwrap()
            .remove(0)
    }

    fn run_mode(input: &Field, bands: f64, sharpness: f64, range: &str) -> Field {
        let params = Params::new()
            .with("bands", ParamValue::Float(bands))
            .with("sharpness", ParamValue::Float(sharpness))
            .with("range", ParamValue::Text(range.to_string()));
        Terrace
            .eval(
                Inputs::required_only(&[input]),
                &params,
                &ctx(input.width()),
            )
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A horizontal ramp `0..1` across the row, so each column is a distinct height value.
    fn ramp(res: usize) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                x as f32 / (res as f32 - 1.0)
            })),
        )
    }

    /// A ramp from `lo` to `hi` across the row, so the terrain occupies only part of `[0, 1]`.
    fn compressed_ramp(res: usize, lo: f32, hi: f32) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                lo + (hi - lo) * (x as f32 / (res as f32 - 1.0))
            })),
        )
    }

    #[test]
    fn endpoints_are_fixed() {
        // 0 and 1 land exactly on band boundaries, so they are unchanged by any terracing.
        assert!((terrace(0.0, 8.0, 1.0)).abs() < 1e-6);
        assert!((terrace(1.0, 8.0, 1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn output_is_monotonic() {
        // Terracing preserves ordering: a rising input never produces a falling output.
        let (bands, sharp) = (6.0, 0.8);
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=1000 {
            let v = i as f32 / 1000.0;
            let out = terrace(v, bands, sharp);
            assert!(out + 1e-6 >= prev, "not monotonic at {v}: {out} < {prev}");
            prev = out;
        }
    }

    #[test]
    fn hard_terraces_share_a_flat_tread() {
        // At high sharpness, two values within the same tread quantize to the same level, and a value
        // past the riser jumps to the next band. Band width is 1/4 = 0.25.
        let sharp = 1.0;
        let a = terrace(0.03, 4.0, sharp); // f = 0.12, below the riser -> band 0
        let b = terrace(0.10, 4.0, sharp); // f = 0.40, still below the riser -> band 0
        let c = terrace(0.20, 4.0, sharp); // f = 0.80, past the riser -> band 1
        assert!(
            (a - b).abs() < 1e-4,
            "same tread should be flat: {a} vs {b}"
        );
        assert!(
            c - b > 0.2,
            "crossing the riser should step up a band: {b} -> {c}"
        );
    }

    #[test]
    fn low_sharpness_is_gentler_than_high() {
        // A rounded terrace departs from the raw value less abruptly than a hard one. Measure the
        // total deviation from the input ramp: the hard terrace deviates more.
        let res = 64;
        let input = ramp(res);
        let dev = |sharp: f64| -> f32 {
            let out = run(&input, 8.0, sharp);
            (0..res)
                .map(|x| (at(&out, x, 0) - at(&input, x, 0)).abs())
                .sum::<f32>()
        };
        assert!(
            dev(1.0) > dev(0.1),
            "hard terraces should deviate more than soft"
        );
        assert!(dev(0.1) > 0.0, "even soft terracing reshapes the ramp");
    }

    #[test]
    fn mask_limits_the_terracing() {
        // Masked off everywhere: the height is left exactly as the input.
        let res = 32;
        let mut input = ramp(res);
        input.set_layer(layers::MASK, Arc::new(Layer::filled(res, res, 0.0)));
        let out = run(&input, 8.0, 1.0);
        for x in 0..res {
            assert!((at(&out, x, 0) - (x as f32 / (res as f32 - 1.0))).abs() < 1e-6);
        }
    }

    #[test]
    fn passes_through_other_layers() {
        let res = 16;
        let mut input = ramp(res);
        input.set_layer("flow", Arc::new(Layer::filled(res, res, 0.9)));
        let out = run(&input, 8.0, 0.7);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn auto_range_tracks_the_terrain_distribution() {
        // A ramp occupying only [0.4, 0.6]. Fixed places terraces at absolute 1/N levels, so few land
        // in that narrow band; Auto spreads N terraces across the actual range, giving many more
        // visible steps for the same band count.
        let res = 128;
        let input = compressed_ramp(res, 0.4, 0.6);
        let distinct_levels = |range: &str| -> usize {
            let out = run_mode(&input, 8.0, 1.0, range);
            let mut levels: Vec<i32> = (0..res)
                .map(|x| (at(&out, x, 0) * 10_000.0).round() as i32)
                .collect();
            levels.dedup();
            levels.len()
        };
        assert!(
            distinct_levels(RANGE_AUTO) > distinct_levels(RANGE_FIXED),
            "auto should show more terraces than fixed on a compressed range",
        );

        // Auto keeps the terrain's extent: the terraced ends stay the input's min and max.
        let out = run_mode(&input, 8.0, 1.0, RANGE_AUTO);
        assert!(
            (at(&out, 0, 0) - 0.4).abs() < 1e-4,
            "auto preserves the low end"
        );
        assert!(
            (at(&out, res - 1, 0) - 0.6).abs() < 1e-4,
            "auto preserves the high end"
        );
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(24);
        assert_eq!(
            run(&input, 8.0, 0.6).content_hash(),
            run(&input, 8.0, 0.6).content_hash()
        );
    }
}
