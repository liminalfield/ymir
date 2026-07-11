//! Thermal (talus) erosion: a mask-aware height modifier.
//!
//! Material on slopes steeper than the talus angle slides downhill to lower
//! neighbours over several passes, forming the straight talus slopes of scree and
//! softening sharp ridges. The talus angle (in degrees) and the pass count are
//! resolution-aware: the per-cell threshold scales with cell size and the passes
//! scale with resolution, so the same terrain relaxes the same way at any resolution
//! and the preview is representative of the build. An optional `mask` input localizes
//! the effect (its height layer is the selection); unwired, the input's own `mask`
//! layer is used by convention, else erosion applies everywhere. Each pass is Jacobi
//! (it reads the previous full state and writes a fresh delta), so the result is
//! independent of cell iteration order. A pass runs as two parallel gather phases (each
//! cell computes its own outputs from neighbour reads, never writing a shared cell), so it
//! is byte-identical regardless of thread count, which the determinism policy requires.

use std::sync::Arc;

use rayon::prelude::*;
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::erosion;
use crate::talus;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.thermal_erosion";

/// Default talus (repose) angle in degrees; about 35 is typical for scree.
const DEFAULT_TALUS_DEG: f64 = 35.0;
/// Default erosion passes, expressed at the reference resolution.
const DEFAULT_ITERATIONS: i64 = 35;
/// Resolution the `iterations` param is expressed at; passes scale linearly with the
/// actual resolution from here, so the world-scale amount of erosion is consistent.
const ITERATION_REFERENCE_RES: f64 = 256.0;

/// Thermal erosion modifier: one input, one output.
#[derive(Clone)]
pub struct ThermalErosion;

impl Operator for ThermalErosion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![
                PortSpec::new("in"),
                // Optional: a field whose height is the selection. When unwired, the
                // input's own mask layer is used by convention, else erode everywhere.
                PortSpec::optional("mask"),
            ],
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("wear"),
                PortSpec::new("debris"),
            ],
            params: vec![
                ParamSpec::new(
                    "talus",
                    ParamKind::Float {
                        min: 0.0,
                        max: 90.0,
                    },
                    ParamValue::Float(DEFAULT_TALUS_DEG),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 0, max: 1000 },
                    ParamValue::Int(DEFAULT_ITERATIONS),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();

        let strength = params.get_f64("strength", 0.5) as f32;

        // The talus angle as a per-cell normalized-height threshold: tan(angle) is the real
        // slope (rise over run), divided by the vertical:horizontal scale to express it as the
        // normalized height a cell may differ from a neighbour before it sheds. Resolution-aware
        // (the scale folds in cell size) and a real angle now: the world's vertical and
        // horizontal extents set real_slope_scale, so a 35 degree talus means 35 degrees on the
        // terrain. At a unit world (the default) this reduces to the prior normalized behaviour.
        let talus_deg = params.get_f64("talus", DEFAULT_TALUS_DEG) as f32;
        let talus_per_cell = talus_deg.to_radians().tan() / ctx.real_slope_scale() as f32;

        // Passes scale with resolution: material moves one cell per pass, so to relax
        // the same world distance a finer grid needs proportionally more passes, which
        // keeps the preview representative of the build. At least one pass when asked.
        let base = params
            .get_i64("iterations", DEFAULT_ITERATIONS)
            .clamp(0, 100_000);
        let iterations = if base > 0 {
            ((base as f64 * width as f64 / ITERATION_REFERENCE_RES).round() as i64)
                .clamp(1, 1_000_000) as usize
        } else {
            0
        };

        let source = input.layer_or(layers::HEIGHT, 0.0);
        // The mask localizes the erosion. An explicit mask input wins (its height
        // layer is the selection); with none, the input's own mask layer by
        // convention; with neither, a uniform 1.0 (erode everywhere). Soft-layer
        // contract either way: the node never gates on a mask.
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        let mut heights = source.as_slice().to_vec();
        let mut delta = vec![0.0_f32; heights.len()];
        // Reused per-cell scratch for the two-phase pass: how much each cell sheds and its
        // downhill excess sum (which splits the shed among lower neighbours). Allocated once
        // and overwritten each pass rather than reallocated.
        let mut moved = vec![0.0_f32; heights.len()];
        let mut total_excess = vec![0.0_f32; heights.len()];
        let pass = talus::Pass {
            width,
            height,
            talus_per_cell,
            strength,
        };

        for _ in 0..iterations {
            // Erosion is the slow node; poll cancellation each pass so a
            // superseded preview aborts instead of running to completion.
            if ctx.is_cancelled() {
                return Err(Error::Cancelled);
            }
            talus::relax_pass(&heights, &mut moved, &mut total_excess, &mut delta, &pass);
            // Apply the pass. Each cell is independent, so the parallel add is
            // byte-identical to a sequential one.
            heights
                .par_iter_mut()
                .zip(delta.par_iter())
                .for_each(|(h, d)| *h += *d);
        }

        // Composite the eroded result over the original through the mask: a fully
        // masked-out cell (mask 0) keeps its original height exactly, a fully
        // masked-in cell (mask 1) takes the eroded height, and partial masks blend.
        // This protects masked regions completely, unlike scaling each cell's
        // shedding, which lets sediment still flow into them. The erosion itself is
        // mass-conserving; the mask is a deliberate per-cell protect, so it does not
        // conserve mass across a mask gradient.
        let blended = Layer::from_fn(width, height, |x, y| {
            let idx = y * width + x;
            let original = source.get(x, y).unwrap_or(0.0);
            let m = mask.get(x, y).unwrap_or(1.0);
            original + (heights[idx] - original) * m
        });
        let mut heightfield = input.clone();
        heightfield.set_layer(layers::HEIGHT, Arc::new(blended));

        // Byproduct taps from the unmasked simulation (before the mask composite, like Stream's
        // flow tap): `wear` where the relaxation stripped a face, and the settled talus where the
        // surface ended up higher than it started. Thermal is dry mass-wasting, so its settled
        // material is scree — emitted on the `debris` port, kept distinct from the fluvial
        // `deposition` the water models produce (though it is computed the same way).
        let (wear, debris) = erosion::wear_and_deposition(source.as_slice(), &heights);
        let region = input.region();
        let wear_field = erosion::byproduct_field(wear, width, height, region);
        let debris_field = erosion::byproduct_field(debris, width, height, region);
        Ok(vec![heightfield, wear_field, debris_field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(ThermalErosion) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{EvalContext, Region};

    fn ctx() -> EvalContext {
        EvalContext::new(32, 32, Region::UNIT, 0)
    }

    /// A field with a single tall spike in the middle of a flat plain.
    fn spike_field(masked_out: bool) -> Field {
        let layer = Layer::from_fn(32, 32, |x, y| if x == 16 && y == 16 { 1.0 } else { 0.0 });
        let mut field =
            Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer));
        if masked_out {
            field.set_layer(layers::MASK, Arc::new(Layer::filled(32, 32, 0.0)));
        }
        field
    }

    fn total_height(field: &Field) -> f64 {
        field
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum()
    }

    #[test]
    fn conserves_total_height() {
        let input = spike_field(false);
        let before = total_height(&input);
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let after = total_height(&out[0]);
        // Material moves but is neither created nor destroyed (boundary holds it).
        assert!(
            (before - after).abs() < 1e-4,
            "mass changed: {before} -> {after}"
        );
    }

    #[test]
    fn spec_has_heightfield_wear_and_debris_outputs() {
        let spec = ThermalErosion.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "wear", "debris"]);
    }

    #[test]
    fn debris_output_records_accumulated_talus() {
        // A spike sheds material to its neighbours; the debris tap (output 2) is high where talus
        // piled up and zero at the peak, which only lost material.
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let debris = out[2].layer(layers::HEIGHT).unwrap();
        assert!(
            debris.get(17, 16).unwrap() > 0.0,
            "talus should accumulate beside the spike"
        );
        assert_eq!(
            debris.get(16, 16).unwrap(),
            0.0,
            "the shedding peak holds no debris"
        );
    }

    #[test]
    fn wear_output_records_stripped_material() {
        // The mirror of debris: the wear tap (output 1) is high at the shedding peak and zero on
        // the cells where talus piled up.
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let wear = out[1].layer(layers::HEIGHT).unwrap();
        assert!(
            wear.get(16, 16).unwrap() > 0.0,
            "the shedding peak should record wear"
        );
        assert_eq!(
            wear.get(17, 16).unwrap(),
            0.0,
            "a cell that only gained talus records no wear"
        );
    }

    #[test]
    fn wear_and_debris_conserve_mass() {
        // The relaxation is mass-conserving (what a cell sheds its lower neighbours receive), so
        // over the whole field the material worn away equals the material deposited. This is the
        // check that the byproduct pair is real, not a broken tap.
        use crate::noise::{FbmParams, fbm_field};
        let input = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 7);
        let out = ThermalErosion
            .eval(
                Inputs::required_only(&[&input]),
                &Params::default(),
                &EvalContext::new(64, 64, Region::UNIT, 7),
            )
            .unwrap();
        let sum = |i: usize| -> f64 {
            out[i]
                .layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .map(|&v| f64::from(v))
                .sum()
        };
        let (total_wear, total_debris) = (sum(1), sum(2));
        assert!(total_debris > 0.0, "erosion should deposit some talus");
        assert!(
            (total_wear - total_debris).abs() / total_debris < 1e-3,
            "wear ({total_wear}) and debris ({total_debris}) should balance",
        );
    }

    #[test]
    fn erosion_is_deterministic() {
        // The two-phase gather has each cell compute its own movement, so the result is
        // byte-identical run to run regardless of how rayon schedules the rows.
        use crate::noise::{FbmParams, fbm_field};
        let input = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 42);
        let c = EvalContext::new(64, 64, Region::UNIT, 42);
        let run = || {
            ThermalErosion
                .eval(Inputs::required_only(&[&input]), &Params::default(), &c)
                .unwrap()
                .remove(0)
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn erosion_spreads_the_spike() {
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let peak = out[0].layer(layers::HEIGHT).unwrap().get(16, 16).unwrap();
        // The peak sheds material to its neighbours, so it drops below 1.0.
        assert!(peak < 1.0, "peak should erode, got {peak}");
        // A neighbour gains material.
        let neighbour = out[0].layer(layers::HEIGHT).unwrap().get(17, 16).unwrap();
        assert!(
            neighbour > 0.0,
            "neighbour should receive sediment, got {neighbour}"
        );
    }

    #[test]
    fn erosion_is_resolution_consistent() {
        use crate::noise::{FbmParams, fbm_field};
        // Mean absolute height change from eroding the same fBm terrain at a given
        // resolution with the default params.
        let mean_change = |res: usize| -> f64 {
            let field = fbm_field(res, res, Region::UNIT, FbmParams::default(), 42);
            let c = EvalContext::new(res, res, Region::UNIT, 42);
            let out = ThermalErosion
                .eval(Inputs::required_only(&[&field]), &Params::default(), &c)
                .unwrap();
            let before = field.layer(layers::HEIGHT).unwrap();
            let after = out[0].layer(layers::HEIGHT).unwrap();
            before
                .as_slice()
                .iter()
                .zip(after.as_slice())
                .map(|(a, b)| f64::from((a - b).abs()))
                .sum::<f64>()
                / (res * res) as f64
        };
        let lo = mean_change(128);
        let hi = mean_change(512);
        // Erosion is visible at both (the old raw threshold did nothing at high res),
        // and the same terrain relaxes to a similar degree regardless of resolution.
        assert!(lo > 1e-4, "erosion not visible at 128: {lo}");
        assert!(hi > 1e-4, "erosion not visible at 512: {hi}");
        assert!(
            (lo / hi) < 3.0 && (hi / lo) < 3.0,
            "erosion drifted with resolution: {lo} vs {hi}"
        );
    }

    #[test]
    fn fully_masked_field_is_unchanged() {
        let input = spike_field(true);
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let after = out[0].layer(layers::HEIGHT).unwrap().content_hash();
        assert_eq!(before, after, "mask=0 everywhere must disable erosion");
    }

    #[test]
    fn an_explicit_mask_input_localizes_erosion() {
        // A selection wired into the mask input (its height layer is the selection),
        // zero everywhere, protects the spike entirely just like a mask layer would,
        // so the field is unchanged.
        let input = spike_field(false);
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let mask = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.0)));
        let required = [&input];
        let optional = [Some(&mask)];
        let out = ThermalErosion
            .eval(
                Inputs::new(&required, &optional),
                &Params::default(),
                &ctx(),
            )
            .unwrap();
        let after = out[0].layer(layers::HEIGHT).unwrap().content_hash();
        assert_eq!(before, after, "a zero mask input must disable erosion");
    }

    #[test]
    fn a_selection_drives_erosion_through_the_mask_input() {
        // End-to-end replacement for the retired Mask node's erosion-integration
        // tests: a Slope selection wired into the mask input localizes erosion, so it
        // differs from eroding everywhere.
        use crate::Slope;
        use crate::noise::{FbmParams, fbm_field};

        let input = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 42);
        let c = EvalContext::new(64, 64, Region::UNIT, 42);
        let selection = Slope
            .eval(Inputs::required_only(&[&input]), &Params::default(), &c)
            .unwrap()
            .remove(0);

        let required = [&input];
        let optional = [Some(&selection)];
        let localized = ThermalErosion
            .eval(Inputs::new(&required, &optional), &Params::default(), &c)
            .unwrap();
        let everywhere = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &c)
            .unwrap();
        assert_ne!(
            localized[0].layer(layers::HEIGHT).unwrap().content_hash(),
            everywhere[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "a wired selection must localize erosion"
        );
    }

    #[test]
    fn the_mask_input_overrides_the_mask_layer() {
        // The input carries a mask layer of 1.0 (erode), but a wired mask input of 0.0
        // (protect) wins: the field is unchanged, proving the input takes precedence.
        let mut input = spike_field(false);
        input.set_layer(layers::MASK, Arc::new(Layer::filled(32, 32, 1.0)));
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let mask = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.0)));
        let required = [&input];
        let optional = [Some(&mask)];
        let out = ThermalErosion
            .eval(
                Inputs::new(&required, &optional),
                &Params::default(),
                &ctx(),
            )
            .unwrap();
        let after = out[0].layer(layers::HEIGHT).unwrap().content_hash();
        assert_eq!(before, after, "the mask input must override the mask layer");
    }

    #[test]
    fn diagonal_symmetry_no_star_bias() {
        // A centred spike on a symmetric grid must erode with four-fold symmetry;
        // distance-weighted diagonals keep the four diagonal neighbours equal to
        // each other and the four orthogonal neighbours equal to each other.
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx())
            .unwrap();
        let h = out[0].layer(layers::HEIGHT).unwrap();
        let orth = [h.get(15, 16), h.get(17, 16), h.get(16, 15), h.get(16, 17)];
        let diag = [h.get(15, 15), h.get(17, 17), h.get(15, 17), h.get(17, 15)];
        for w in orth.windows(2) {
            assert_eq!(w[0], w[1], "orthogonal neighbours must be equal");
        }
        for w in diag.windows(2) {
            assert_eq!(w[0], w[1], "diagonal neighbours must be equal");
        }
    }

    #[test]
    fn cancelled_erosion_aborts() {
        // A pre-cancelled context makes the iteration loop bail on its first pass.
        let cancel = ymir_core::CancelToken::new();
        cancel.cancel();
        let ctx = EvalContext::new(32, 32, Region::UNIT, 0).with_cancel(cancel);
        let input = spike_field(false);
        let err = ThermalErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &ctx)
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }
}
