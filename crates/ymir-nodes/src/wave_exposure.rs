//! Wave Exposure: where the coast takes a beating, as a field (#370).
//!
//! Emits normalized wave energy on the **`height`** layer (so the rest of the toolset shapes and
//! applies it) and on [`layers::WAVE_ENERGY`], high on ground the sea works hard and low where it
//! is sheltered. Wire it into a mask input and beaches fill the bays while the capes stay bare,
//! which is the variation that stops a coast reading as uniform.
//!
//! # What it measures, in this version
//!
//! Refraction. Waves bend toward shallow water, which concentrates their energy on ground that
//! juts seaward and spreads it around the inside of a bay. That is why headlands erode back while
//! bays accrete, and it is the single most visually important term in the coastal model.
//!
//! The proxy is the curvature of the shoreline in plan. Solve the signed distance from the
//! waterline, and because that field has unit gradient by construction, its Laplacian *is* the
//! curvature of the shoreline running through each point. One five-point stencil, no wave
//! simulation anywhere.
//!
//! Fetch, the other half of the model, is not here yet. Every direction is treated as equally
//! open, so a sheltered inlet facing away from the swell still reads by its shape alone. The
//! formula is written so fetch multiplies in rather than replacing any of this.
//!
//! # Why the scale is a parameter and not an implementation detail
//!
//! Curvature is a second derivative, so it magnifies whatever detail is present. On a real
//! coastline, the metre-scale wiggle of the shoreline swamps the kilometre-scale shape of the
//! headland, and the sign comes out wrong as often as right. Measured on a lobed test island with
//! fine detail added, three headlands in five read as *bays* when the distance field was
//! differentiated raw. Smoothing the field first at 120 m fixed all ten samples and separated them
//! cleanly.
//!
//! So `feature_radius` is not tidying. It is the question "how big is a headland?", which has no
//! answer the node can pick for you: a 40 m radius and a 400 m radius read different, equally real
//! features of the same coast. It is the first control to reach for when the output looks wrong.
//!
//! # Sign
//!
//! Positive convexity is ground that bulges seaward: a headland, which gains energy. Concave
//! ground is a bay, which loses it. Getting this backwards gives a coast where bays erode and
//! headlands accrete, which looks wrong without looking *obviously* wrong, so it is asserted in a
//! test against an island whose headlands and bays are known by construction rather than by eye.
//!
//! # Where it applies
//!
//! Near the water, falling back to neutral away from it, over the same `feature_radius`.
//!
//! This is not a cosmetic band. The Laplacian at a cell is the curvature of the level set through
//! that cell, which is the shoreline only at the shore; further off it is the curvature of an
//! offset curve, and midway between two shores the distance field creases into its medial axis,
//! where the value is a singularity rather than a measurement. Emitting everywhere was tried
//! first, and on an archipelago the medial axis of the *water* dominated the entire field: a real
//! property of a distance transform with nothing whatever to do with waves. The reach is the
//! extent over which the proxy proxies for anything.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::blur::gaussian_blur;
use crate::distance::sea_signed_distance;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.wave_exposure";

/// Default scale of shoreline shape the node reads, in metres.
///
/// There is no world-independent right answer: this is a length, and what counts as a headland
/// depends entirely on how big the coast is. Erring small is the better failure. Too small reads
/// as noisy but is visibly doing something and invites tuning; too large erases the coast and
/// leaves a blank field that reads as broken.
///
/// Chosen by rendering: 50 m gives a clean, legible field on a 1 km world whose islands are around
/// a hundred metres across, and 120 m on that same world had already smoothed them away.
const DEFAULT_FEATURE_RADIUS: f64 = 50.0;

/// Default refraction gain, from `design/coastal-erosion.md`. How hard headlands gain and bays
/// lose. Zero disables the term and leaves a flat field, which is the seam fetch arrives through.
const DEFAULT_REFRACTION_GAIN: f64 = 0.5;

/// A neutral coast, the value of a shoreline with no curvature either way. Headlands read above
/// it and bays below, so the midpoint is where a straight coast sits rather than an absence of
/// energy.
const NEUTRAL: f32 = 0.5;

/// Wave Exposure: one input, one output. Writes the energy to [`layers::HEIGHT`] and
/// [`layers::WAVE_ENERGY`].
#[derive(Clone)]
pub struct WaveExposure;

impl Operator for WaveExposure {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out").selection()],
            params: vec![
                ParamSpec::new(
                    "feature_radius",
                    ParamKind::Float {
                        min: 0.0,
                        max: 10_000.0,
                    },
                    ParamValue::Float(DEFAULT_FEATURE_RADIUS),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "refraction_gain",
                    ParamKind::Float { min: 0.0, max: 4.0 },
                    ParamValue::Float(DEFAULT_REFRACTION_GAIN),
                ),
            ],
            emitted_layers: vec![layers::WAVE_ENERGY],
            mask_aware: false,
        }
    }

    /// Reads the sea level (the waterline the shoreline is measured from) and the world extent
    /// (`feature_radius` is metres, converted against the cell size). Not the world height: the
    /// shoreline is a contour of the normalized height against the normalized sea level, and
    /// scaling the world vertically moves both together.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps {
            world_height: false,
            ..ymir_core::ContextDeps::ALL
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let feature_radius = params
            .get_f64("feature_radius", DEFAULT_FEATURE_RADIUS)
            .max(0.0);
        let gain = params
            .get_f64("refraction_gain", DEFAULT_REFRACTION_GAIN)
            .max(0.0) as f32;

        let cell_size = ctx.meters_per_cell() as f32;
        // Signed distance from the waterline: positive on land, negative at sea, in metres. This
        // is the shared solve the Distance node uses, so an enclosed below-sea basin is treated as
        // land and seeds no false shoreline around itself.
        let phi = sea_signed_distance(&h, ctx.sea_level() as f32, cell_size);
        // Smooth at the feature scale before differentiating (see the module docs: this is the
        // difference between the term working and the sign being wrong as often as right).
        let sigma = ctx.world_to_cells(feature_radius);
        let smoothed = gaussian_blur(phi.as_slice(), width, height, sigma);

        // Dimensionless curvature: the Laplacian of a distance field has units of 1 / length, so
        // multiplying by the feature radius gives a pure number, near 1 where the shoreline turns
        // through a radius of about `feature_radius`. Scaling by the feature size rather than by
        // the field's own statistics keeps the value a local, geometric quantity, so the reading
        // at one cape does not shift because a different part of the map changed.
        let radius = feature_radius as f32;
        let inv_cell_sq = 1.0 / (cell_size * cell_size).max(f32::MIN_POSITIVE);
        let at = |x: usize, y: usize| smoothed[y * width + x];
        // Reach: how far from the water the reading stays meaningful, and the reason it has to be
        // limited at all.
        //
        // The Laplacian at a cell is the curvature of the level set passing through it, which is
        // the shoreline only at the shore. Step away and it becomes the curvature of an offset
        // curve, and midway between two shores the distance field creases into its medial axis,
        // where that curvature is a singularity rather than a measurement. Rendered on an
        // archipelago the medial axis of the *water* dominated the whole field: a real property of
        // a distance transform, and nothing at all to do with waves.
        //
        // A reading at scale `feature_radius` cannot resolve the shoreline further than about that
        // far from it, so the value falls back to neutral over that distance. Not a cosmetic
        // cleanup: it is the extent over which the proxy is a proxy for anything.
        let inv_reach_sq = 1.0 / (radius * radius).max(f32::MIN_POSITIVE);
        let phi_at = |x: usize, y: usize| phi.get(x, y).unwrap_or(0.0);

        // Per-cell and independent, so this is byte-identical whatever the thread count.
        let energy = Layer::from_par_fn(width, height, |x, y| {
            // The border has no two-sided neighbours, so it reads neutral rather than guessing.
            if x == 0 || y == 0 || x + 1 >= width || y + 1 >= height {
                return NEUTRAL;
            }
            let c = at(x, y);
            let d2x = at(x + 1, y) - 2.0 * c + at(x - 1, y);
            let d2y = at(x, y + 1) - 2.0 * c + at(x, y - 1);
            // Negated so the sign reads as convexity: positive where the land bulges seaward.
            let convexity = -(d2x + d2y) * inv_cell_sq * radius;
            // Gaussian in the true (unsmoothed) distance from the water, so the falloff follows the
            // real shoreline rather than the blurred one.
            let d = phi_at(x, y);
            let reach = (-(d * d) * inv_reach_sq).exp();
            // Squashed rather than clamped, so the field is bounded by construction, a straight
            // coast sits at the midpoint, and `gain` reads as contrast rather than as a hard cut.
            NEUTRAL + NEUTRAL * (gain * convexity).tanh() * reach
        });

        let energy = Arc::new(energy);
        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, energy.clone());
        out.set_layer(layers::WAVE_ENERGY, energy);
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(WaveExposure) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;
    use ymir_core::{Region, registry};

    const RES: usize = 256;
    const EXTENT: f64 = 4000.0;
    const SEA: f64 = 0.5;
    /// Lobes around the test island: the tips are headlands, the troughs between them are bays.
    const LOBES: f64 = 5.0;
    /// Mean shore radius and lobe amplitude, as a fraction of the map half-width.
    const R0: f64 = 0.55;
    const AMP: f64 = 0.18;

    /// An island whose coast crosses `SEA` exactly at `r = R0 + AMP*cos(LOBES*theta)`, optionally
    /// with finer detail added so the coast is not a textbook curve.
    ///
    /// Linear in the distance from that shore, so the slope is the same all the way round: any
    /// sign read from it comes from the shoreline's plan shape and not from its steepness.
    fn island(roughness: f64) -> Field {
        let layer = Layer::from_fn(RES, RES, |x, y| {
            let u = (x as f64 + 0.5) / RES as f64 * 2.0 - 1.0;
            let v = (y as f64 + 0.5) / RES as f64 * 2.0 - 1.0;
            let r = u.hypot(v);
            let theta = v.atan2(u);
            let wiggle = roughness
                * ((17.0 * theta).sin() * 0.5
                    + (31.0 * theta).cos() * 0.3
                    + (53.0 * theta).sin() * 0.2);
            let shore = R0 + AMP * (LOBES * theta).cos() + wiggle;
            (SEA + (shore - r) * 0.5) as f32
        });
        let mut field = Field::new(RES, RES, Region::UNIT);
        field.set_layer(layers::HEIGHT, Arc::new(layer));
        field
    }

    fn ctx() -> EvalContext {
        EvalContext::new(RES, RES, Region::UNIT, 0)
            .with_world_extent(EXTENT)
            .with_sea_level(SEA)
    }

    fn run(input: &Field, params: &Params) -> Field {
        WaveExposure
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .expect("wave exposure evaluates")
            .remove(0)
    }

    /// The cell at polar `(r, theta)` in the frame the island is built in.
    fn cell(r: f64, theta: f64) -> (usize, usize) {
        let to = |t: f64| ((((t + 1.0) / 2.0) * RES as f64) as usize).min(RES - 1);
        (to(r * theta.cos()), to(r * theta.sin()))
    }

    /// Energy just inland of each lobe tip (headland) and each trough (bay), sampled at the same
    /// depth so the comparison is about plan shape and nothing else.
    fn headlands_and_bays(field: &Field) -> (Vec<f32>, Vec<f32>) {
        let layer = field.layer_or(layers::HEIGHT, 0.0);
        let inland = 0.04;
        let mut headlands = Vec::new();
        let mut bays = Vec::new();
        for i in 0..LOBES as usize {
            let tip = i as f64 * 2.0 * PI / LOBES;
            let (x, y) = cell(R0 + AMP - inland, tip);
            headlands.push(layer.get(x, y).unwrap_or(0.0));
            let (x, y) = cell(R0 - AMP - inland, tip + PI / LOBES);
            bays.push(layer.get(x, y).unwrap_or(0.0));
        }
        (headlands, bays)
    }

    #[test]
    fn headlands_take_the_energy_and_bays_are_sheltered() {
        // The sign convention, pinned. Backwards gives a coast where bays erode and headlands
        // accrete, which looks wrong without looking obviously wrong, so it is asserted against an
        // island whose headlands and bays are known by construction.
        let (headlands, bays) = headlands_and_bays(&run(&island(0.0), &Params::new()));
        for (i, &e) in headlands.iter().enumerate() {
            assert!(
                e > NEUTRAL,
                "headland {i} read {e}, expected above the neutral {NEUTRAL}"
            );
        }
        for (i, &e) in bays.iter().enumerate() {
            assert!(
                e < NEUTRAL,
                "bay {i} read {e}, expected below the neutral {NEUTRAL}"
            );
        }
    }

    #[test]
    fn a_rough_coast_still_reads_its_headlands_once_the_scale_is_set() {
        // The finding this node's `feature_radius` exists for. With fine detail on the shoreline
        // and no smoothing, curvature reads the wiggle instead of the landform and headlands come
        // out wrong; at the feature scale they come back. The failing case is asserted too, so
        // this documents the trap rather than merely avoiding it.
        let rough = island(0.04);

        let unsmoothed = Params::new().with("feature_radius", ParamValue::Float(0.0));
        let (headlands, _) = headlands_and_bays(&run(&rough, &unsmoothed));
        let wrong = headlands.iter().filter(|&&e| e <= NEUTRAL).count();
        assert!(
            wrong > 0,
            "expected the unsmoothed reading to misjudge at least one headland, \
             otherwise this test is not exercising the trap: {headlands:?}"
        );

        let smoothed = Params::new().with("feature_radius", ParamValue::Float(120.0));
        let (headlands, bays) = headlands_and_bays(&run(&rough, &smoothed));
        for (i, &e) in headlands.iter().enumerate() {
            assert!(e > NEUTRAL, "headland {i} read {e} at the feature scale");
        }
        for (i, &e) in bays.iter().enumerate() {
            assert!(e < NEUTRAL, "bay {i} read {e} at the feature scale");
        }
    }

    #[test]
    fn water_far_from_any_shore_reads_neutral() {
        // Emitting everywhere was tried first, and the medial axis of the water (the crease
        // midway between two shores, where the distance field is not differentiable) dominated
        // the whole field. It is a singularity of the distance transform, not a wave. The reading
        // is scoped to within about `feature_radius` of the water's edge, so open water is neutral.
        let out = run(&island(0.0), &Params::new());
        let layer = out.layer_or(layers::HEIGHT, 0.0);
        // The map corner: the furthest open water from this island, and where the medial axis of
        // the surrounding sea ran when the field was unscoped.
        let corner = layer.get(2, 2).unwrap_or(0.0);
        assert!(
            (corner - NEUTRAL).abs() < 1e-3,
            "open water far from the coast read {corner}, expected the neutral {NEUTRAL}"
        );
        // And the island's own middle, which is likewise nowhere near a shore at this scale.
        let middle = layer.get(RES / 2, RES / 2).unwrap_or(0.0);
        assert!(
            (middle - NEUTRAL).abs() < 1e-3,
            "the island's interior read {middle}, expected the neutral {NEUTRAL}"
        );
    }

    #[test]
    fn zero_gain_is_a_flat_field() {
        // The seam fetch arrives through: with refraction off, every cell reads neutral, so a
        // later term multiplying into this starts from an unmodulated field.
        let params = Params::new().with("refraction_gain", ParamValue::Float(0.0));
        let out = run(&island(0.04), &params);
        let layer = out.layer_or(layers::HEIGHT, 0.0);
        let (lo, hi) = layer.value_range();
        assert!(
            (lo - NEUTRAL).abs() < 1e-6 && (hi - NEUTRAL).abs() < 1e-6,
            "expected a flat {NEUTRAL}, got {lo} to {hi}"
        );
    }

    #[test]
    fn the_energy_is_emitted_as_its_own_layer_too() {
        // So the value flows downstream alongside terrain rather than only as the primary output.
        let out = run(&island(0.0), &Params::new());
        let height = out.layer_or(layers::HEIGHT, 0.0);
        let energy = out.layer_or(layers::WAVE_ENERGY, 0.0);
        assert_eq!(height.as_slice(), energy.as_slice());
    }

    #[test]
    fn the_field_stays_within_the_unit_range() {
        // Bounded by construction rather than by a clamp, at a gain well past the default.
        let params = Params::new().with("refraction_gain", ParamValue::Float(4.0));
        let out = run(&island(0.04), &params);
        let (lo, hi) = out.layer_or(layers::HEIGHT, 0.0).value_range();
        assert!(
            (0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi),
            "{lo} to {hi}"
        );
    }

    #[test]
    fn evaluation_is_byte_identical() {
        // Per-cell after a deterministic solve and blur, so repeated runs match exactly.
        let input = island(0.04);
        let a = run(&input, &Params::new());
        let b = run(&input, &Params::new());
        assert_eq!(
            a.layer_or(layers::HEIGHT, 0.0).as_slice(),
            b.layer_or(layers::HEIGHT, 0.0).as_slice()
        );
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = island(0.0);
        let made = registry::make(TYPE_ID).expect("wave exposure operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .expect("registry operator evaluates");
        let direct = run(&input, &Params::default());
        assert_eq!(
            via_registry[0].layer_or(layers::HEIGHT, 0.0).as_slice(),
            direct.layer_or(layers::HEIGHT, 0.0).as_slice()
        );
    }
}
