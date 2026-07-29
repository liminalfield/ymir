//! The Waves generator: directional bands from a skewable ramp (dunes, ripples, corrugation).
//!
//! Sweeps a triangular ramp across the grid in a chosen direction, so the field becomes parallel
//! bands: crests spaced one `wavelength` apart, running perpendicular to `direction`. It is raw
//! material rather than a finished landform, meant to be shaped by a Curve, broken up by a Warp,
//! and masked into place like any other generator.
//!
//! # Why the profile is a plain ramp
//!
//! The output rises linearly to each crest and falls linearly away, so **the height is the position
//! within the wave**. That is what makes it shapeable: a Curve downstream maps position to height,
//! which is exactly what a wave profile is. A sine is this ramp under an S-curve, a square is it
//! under a step, and any dune profile you can draw is it under the curve you drew.
//!
//! So the node offers no waveform choice and no crest shaping. Baking a sine in would hand the user
//! a profile already bent by someone else's curve; baking a square in would throw the position away
//! entirely, since every cell would read 0 or 1 and no Curve could recover what lay between.
//!
//! # What cannot be done downstream
//!
//! `skew` moves the crest within the wavelength, making the two slopes different lengths. That is a
//! phase-domain distortion, and a value transfer function cannot produce it: a Curve maps height to
//! height, so it treats the two slopes of a symmetric wave identically however it bends them. The
//! windward slope and slip face of a dune are therefore this node's business, and the profile of
//! either one is not.
//!
//! Every parameter here is spatial for that reason. Amplitude is a Levels job, following the other
//! shape generators, which all emit `[0, 1]`.
//!
//! # A caution on full skew
//!
//! At `skew` near 1 or -1 one side of the wave collapses to a vertical face. In a heightfield that
//! face is a single cell wide and its plan outline is quantised to the grid, so it steps sideways in
//! notches and facets badly once triangulated in an engine. Spread it over a few cells with a small
//! Blur before export if you take the skew that far.
//!
//! Sampled in world coordinates, so the bands land in the same physical place at any resolution, and
//! per-cell and pure, so `from_par_fn` is byte-identical at any thread count.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec,
    ParamValue, Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.waves";

/// Default crest spacing in world units (metres). A few dozen metres reads as a dune field on a
/// kilometre-scale world rather than as ripples.
const DEFAULT_WAVELENGTH: f64 = 64.0;
/// Default band direction in degrees, matching the gradient generator's convention.
const DEFAULT_DIRECTION: f64 = 0.0;
/// Default phase: crests fall where the world origin puts them.
const DEFAULT_PHASE: f64 = 0.0;
/// Default skew: a symmetric ramp, equal slopes either side of the crest.
const DEFAULT_SKEW: f64 = 0.0;

/// How close to the ends of the wavelength the crest may sit. At the very end one slope has zero
/// length, which is a division by zero and a perfectly vertical face; this keeps it to a face one
/// thousandth of a wavelength wide, which is still vertical for any practical purpose.
const CREST_LIMIT: f32 = 0.001;

/// Waves generator: no inputs, one output.
#[derive(Clone)]
pub struct Waves;

impl Operator for Waves {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "wavelength",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_WAVELENGTH),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "direction",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(DEFAULT_DIRECTION),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "phase",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_PHASE),
                ),
                ParamSpec::new(
                    "skew",
                    ParamKind::Float {
                        min: -1.0,
                        max: 1.0,
                    },
                    ParamValue::Float(DEFAULT_SKEW),
                ),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads the world horizontal extent, to turn a wavelength in metres into region units. No sea
    /// level, world height or seed, so those settings never invalidate this node.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let (width, height, region) = (ctx.width, ctx.height, ctx.region);

        // Wavelength in region units: metres divided by the world's own extent, so the bands span
        // the same physical distance whatever the resolution or the region being evaluated.
        let wavelength_m = params.get_f64("wavelength", DEFAULT_WAVELENGTH);
        let extent = ctx.world_extent();
        let wavelength = if wavelength_m.abs() < f64::EPSILON || extent.abs() < f64::EPSILON {
            // A zero wavelength has no crests to place. Fall back to one band across the world
            // rather than dividing by zero and filling the field with NaN.
            1.0
        } else {
            wavelength_m / extent
        };

        // Direction of travel, perpendicular to the bands. 0 points along +x and rotates toward +y,
        // matching the gradient generator so the two agree when wired together.
        let angle = params.get_f64("direction", DEFAULT_DIRECTION).to_radians();
        let (dir_x, dir_y) = (angle.cos(), angle.sin());
        let phase = params.get_f64("phase", DEFAULT_PHASE);
        let skew = params.get_f64("skew", DEFAULT_SKEW).clamp(-1.0, 1.0) as f32;
        let crest = crest_position(skew);

        let layer = Layer::from_par_fn(width, height, |x, y| {
            // World position of the cell, in region units, sampled the same way the other
            // world-space generators do so the patterns register against each other.
            let u = (x as f64 + 0.5) / width as f64;
            let v = (y as f64 + 0.5) / height as f64;
            let wx = region.min_x + u * region.width();
            let wy = region.min_y + v * region.height();
            // Distance along the direction, in wavelengths. The fractional part is the position
            // within the current wave.
            let travelled = (wx * dir_x + wy * dir_y) / wavelength + phase;
            ramp(travelled.rem_euclid(1.0) as f32, crest)
        });

        Ok(vec![
            Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer)),
        ])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Waves) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "gradient", sort: 42 }
}

/// Where the crest sits within a wavelength, in `[0, 1]`, for a skew in `[-1, 1]`.
///
/// Skew 0 puts it at the midpoint, so the slopes are equal. Positive skew moves it later, so the
/// rise is long and the fall short, which is the way round a dune runs: a gentle windward slope up
/// to the crest and a slip face down from it.
fn crest_position(skew: f32) -> f32 {
    (0.5 * (1.0 + skew)).clamp(CREST_LIMIT, 1.0 - CREST_LIMIT)
}

/// The wave profile: rises linearly from 0 to 1 over `[0, crest]`, falls linearly back to 0 over
/// `[crest, 1]`.
///
/// Linear on both sides on purpose. The height is then the position within the wave, which is what
/// lets a downstream Curve define any profile at all. It also meets itself at 0 on both ends, so
/// consecutive waves join without a step.
fn ramp(phase: f32, crest: f32) -> f32 {
    if phase < crest {
        phase / crest
    } else {
        (1.0 - phase) / (1.0 - crest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, registry};

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0).with_world_extent(1024.0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Field {
        Waves
            .eval(Inputs::required_only(&[]), params, ctx)
            .expect("waves evaluates")
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer_or(layers::HEIGHT, 0.0).get(x, y).unwrap_or(0.0)
    }

    #[test]
    fn spec_is_a_generator() {
        let spec = Waves.spec();
        assert_eq!(spec.type_id, TYPE_ID);
        assert_eq!(spec.kind(), ymir_core::NodeKind::Generator);
        // Every parameter is spatial. Anything a downstream Curve or Levels could do is
        // deliberately absent: no waveform, no amplitude, no crest sharpness.
        let names: Vec<&str> = spec.inputs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.is_empty(), "a generator takes no input");
        let params: Vec<&str> = spec.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["wavelength", "direction", "phase", "skew"]);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("registered");
        assert_eq!(made.spec().type_id, Waves.spec().type_id);
    }

    #[test]
    fn the_profile_is_a_ramp_that_peaks_at_the_crest() {
        // The property the whole design rests on: height is position within the wave, so a Curve
        // downstream can map it to any profile.
        assert!((ramp(0.0, 0.5) - 0.0).abs() < 1e-6);
        assert!((ramp(0.5, 0.5) - 1.0).abs() < 1e-6);
        assert!((ramp(1.0, 0.5) - 0.0).abs() < 1e-6);
        // Linear on the way up: half way to the crest is half height.
        assert!((ramp(0.25, 0.5) - 0.5).abs() < 1e-6);
        // And linear on the way down.
        assert!((ramp(0.75, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn skew_moves_the_crest_and_nothing_else() {
        // Skew is the one thing a downstream Curve cannot reproduce, so it has to land exactly.
        assert!((crest_position(0.0) - 0.5).abs() < 1e-6);
        // Positive skew puts the crest late: a long windward rise, a short slip face.
        assert!(crest_position(0.5) > 0.5);
        assert!(crest_position(1.0) > 0.99);
        assert!(crest_position(-1.0) < 0.01);
        // The peak is still exactly 1 wherever it sits, so skew changes shape and not range.
        for skew in [-0.9_f32, -0.4, 0.0, 0.4, 0.9] {
            let c = crest_position(skew);
            assert!((ramp(c, c) - 1.0).abs() < 1e-5, "peak at skew {skew}");
        }
    }

    #[test]
    fn waves_join_without_a_step() {
        // Consecutive waves must meet at zero. A discontinuity here would alias along every trough
        // and facet once triangulated.
        for skew in [-0.8_f32, 0.0, 0.8] {
            let c = crest_position(skew);
            let end = ramp(0.999, c);
            let start = ramp(0.0, c);
            assert!(
                (end - start).abs() < 0.01,
                "skew {skew}: wave ends at {end} but the next starts at {start}"
            );
        }
    }

    #[test]
    fn output_stays_in_unit_range() {
        let field = run(
            &Params::default()
                .with("wavelength", ParamValue::Float(128.0))
                .with("skew", ParamValue::Float(0.6)),
            &ctx(64),
        );
        for &v in field.layer_or(layers::HEIGHT, 0.0).as_slice() {
            assert!((0.0..=1.0).contains(&v), "value {v} outside [0, 1]");
        }
    }

    #[test]
    fn direction_orients_the_bands() {
        // At 0 degrees the bands run along y, so a column is constant and a row varies. At 90 they
        // swap. This is what makes the node directional rather than merely periodic.
        let along_x = run(
            &Params::default()
                .with("wavelength", ParamValue::Float(256.0))
                .with("direction", ParamValue::Float(0.0)),
            &ctx(64),
        );
        assert!(
            (at(&along_x, 10, 0) - at(&along_x, 10, 63)).abs() < 1e-5,
            "at 0 degrees a column should be constant"
        );
        assert!(
            (at(&along_x, 0, 10) - at(&along_x, 40, 10)).abs() > 0.05,
            "at 0 degrees a row should vary"
        );

        let along_y = run(
            &Params::default()
                .with("wavelength", ParamValue::Float(256.0))
                .with("direction", ParamValue::Float(90.0)),
            &ctx(64),
        );
        assert!(
            (at(&along_y, 0, 10) - at(&along_y, 63, 10)).abs() < 1e-5,
            "at 90 degrees a row should be constant"
        );
    }

    #[test]
    fn wavelength_is_a_world_length() {
        // Halving the wavelength doubles the number of crests across the same world, which is what
        // "in metres" has to mean.
        let crests = |wavelength: f64| {
            let field = run(
                &Params::default().with("wavelength", ParamValue::Float(wavelength)),
                &ctx(256),
            );
            let row: Vec<f32> = (0..256).map(|x| at(&field, x, 0)).collect();
            // A crest is a local maximum along the row.
            row.windows(3)
                .filter(|w| w[1] > w[0] && w[1] >= w[2])
                .count()
        };
        assert_eq!(crests(512.0), 2, "1024 m world, 512 m waves");
        assert_eq!(crests(256.0), 4, "1024 m world, 256 m waves");
    }

    #[test]
    fn the_pattern_holds_its_place_across_resolutions() {
        // Resolution independence: the same world position must give the same height, so raising
        // the build resolution refines the bands rather than moving them.
        let params = Params::default().with("wavelength", ParamValue::Float(256.0));
        let low = run(&params, &ctx(64));
        let high = run(&params, &ctx(192));
        // Sample points that genuinely coincide. A cell centre sits at `(i + 0.5) / res`, so cell
        // `i` at 64 and cell `3i + 1` at 192 are the same world position exactly; most other
        // resolution pairs have no coinciding centres at all, and comparing near-misses would only
        // measure how steep the ramp is.
        for i in [4_usize, 8, 17, 30] {
            let j = 3 * i + 1;
            let (a, b) = (at(&low, i, i), at(&high, j, j));
            assert!(
                (a - b).abs() < 1e-5,
                "cell {i} of 64 reads {a} but the same world point at 192 reads {b}"
            );
        }
    }

    #[test]
    fn phase_slides_the_pattern() {
        let params = |p: f64| {
            Params::default()
                .with("wavelength", ParamValue::Float(256.0))
                .with("phase", ParamValue::Float(p))
        };
        let a = run(&params(0.0), &ctx(64));
        let b = run(&params(0.5), &ctx(64));
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn eval_is_deterministic() {
        let params = Params::default().with("skew", ParamValue::Float(0.3));
        assert_eq!(
            run(&params, &ctx(64)).content_hash(),
            run(&params, &ctx(64)).content_hash()
        );
    }

    #[test]
    fn a_zero_wavelength_does_not_produce_nan() {
        let field = run(
            &Params::default().with("wavelength", ParamValue::Float(0.0)),
            &ctx(32),
        );
        for &v in field.layer_or(layers::HEIGHT, 0.0).as_slice() {
            assert!(v.is_finite(), "non-finite value {v} from a zero wavelength");
        }
    }
}
