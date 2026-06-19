//! Thermal (talus) erosion: a mask-aware height modifier.
//!
//! Material on slopes steeper than the talus angle slides downhill to lower
//! neighbours over several passes, forming the straight talus slopes of scree and
//! softening sharp ridges. Each pass is Jacobi (it reads the previous full state
//! and writes a fresh delta), so the result is independent of cell iteration
//! order, which determinism requires. Single-threaded for now; the per-cell
//! independence means `rayon` drops in later unchanged.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.thermal_erosion";

/// Eight-neighbour offsets with their distances. Diagonals are `sqrt(2)` away, so
/// slope is height difference over true distance and the talus threshold scales
/// with distance; without this, diagonals bias into an eight-pointed star.
const NEIGHBORS: [(i32, i32, f32); 8] = [
    (-1, 0, 1.0),
    (1, 0, 1.0),
    (0, -1, 1.0),
    (0, 1, 1.0),
    (-1, -1, core::f32::consts::SQRT_2),
    (1, -1, core::f32::consts::SQRT_2),
    (-1, 1, core::f32::consts::SQRT_2),
    (1, 1, core::f32::consts::SQRT_2),
];

/// Thermal erosion modifier: one input, one output.
#[derive(Clone)]
pub struct ThermalErosion;

impl Operator for ThermalErosion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            tags: &["thermal", "talus", "erosion", "modifier"],
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "talus",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.006),
                ),
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(0.5),
                ),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 0, max: 1000 },
                    ParamValue::Int(35),
                ),
            ],
        }
    }

    fn eval(&self, inputs: &[&Field], params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();

        let talus = params.get_f64("talus", 0.006) as f32;
        let strength = params.get_f64("strength", 0.5) as f32;
        let iterations = params.get_i64("iterations", 35).clamp(0, 100_000) as usize;

        // Soft contract: erode everywhere when no mask is present (mask 1.0).
        let source = input.layer_or(layers::HEIGHT, 0.0);
        let mask = input.layer_or(layers::MASK, 1.0);

        let mut heights = source.as_slice().to_vec();
        let mut delta = vec![0.0_f32; heights.len()];

        for _ in 0..iterations {
            // Erosion is the slow node; poll cancellation each pass so a
            // superseded preview aborts instead of running to completion.
            if ctx.is_cancelled() {
                return Err(Error::Cancelled);
            }
            erode_pass(&heights, &mut delta, width, height, talus, strength);
            for (h, d) in heights.iter_mut().zip(delta.iter()) {
                *h += *d;
            }
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
        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(blended));
        Ok(vec![out])
    }
}

/// One Jacobi pass: reads `heights`, writes per-cell movement into `delta`.
///
/// Mass-conserving by construction: what a cell sheds is exactly the sum added to
/// its lower neighbours. Out-of-bounds neighbours are skipped, so material is held
/// at the boundary rather than flowing off-grid. The mask is applied afterwards, in
/// `eval`, as a per-cell composite, so a pass here always erodes everywhere.
fn erode_pass(
    heights: &[f32],
    delta: &mut [f32],
    width: usize,
    height: usize,
    talus: f32,
    strength: f32,
) {
    for d in delta.iter_mut() {
        *d = 0.0;
    }

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let here = heights[idx];

            let mut contributions: [(usize, f32); 8] = [(0, 0.0); 8];
            let mut count = 0;
            let mut total_excess = 0.0_f32;
            let mut max_excess = 0.0_f32;

            for (dx, dy, dist) in NEIGHBORS {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue; // boundary holds material in-domain
                }
                let nidx = ny as usize * width + nx as usize;
                let diff = here - heights[nidx];
                // Lower neighbours steeper than repose only; the threshold scales
                // with distance so diagonals are not favoured.
                let threshold = talus * dist;
                if diff <= threshold {
                    continue;
                }
                let excess = diff - threshold;
                contributions[count] = (nidx, excess);
                count += 1;
                total_excess += excess;
                max_excess = max_excess.max(excess);
            }

            if total_excess > 0.0 {
                // Shed a stable fraction of the steepest excess, split among the
                // lower neighbours by their share of the excess.
                let moved = strength * max_excess * 0.5;
                delta[idx] -= moved;
                for &(nidx, excess) in &contributions[..count] {
                    delta[nidx] += moved * (excess / total_excess);
                }
            }
        }
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
            .eval(&[&input], &Params::default(), &ctx())
            .unwrap();
        let after = total_height(&out[0]);
        // Material moves but is neither created nor destroyed (boundary holds it).
        assert!(
            (before - after).abs() < 1e-4,
            "mass changed: {before} -> {after}"
        );
    }

    #[test]
    fn erosion_spreads_the_spike() {
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(&[&input], &Params::default(), &ctx())
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
    fn fully_masked_field_is_unchanged() {
        let input = spike_field(true);
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let out = ThermalErosion
            .eval(&[&input], &Params::default(), &ctx())
            .unwrap();
        let after = out[0].layer(layers::HEIGHT).unwrap().content_hash();
        assert_eq!(before, after, "mask=0 everywhere must disable erosion");
    }

    #[test]
    fn diagonal_symmetry_no_star_bias() {
        // A centred spike on a symmetric grid must erode with four-fold symmetry;
        // distance-weighted diagonals keep the four diagonal neighbours equal to
        // each other and the four orthogonal neighbours equal to each other.
        let input = spike_field(false);
        let out = ThermalErosion
            .eval(&[&input], &Params::default(), &ctx())
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
            .eval(&[&input], &Params::default(), &ctx)
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }
}
