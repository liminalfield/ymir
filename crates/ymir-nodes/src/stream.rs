//! Stream erosion: iterative stream-power fluvial erosion (FastScape / Braun-Willett).
//!
//! A complete, self-contained erosion model, distinct from the pipe simulation in
//! [`crate::hydraulic`]. It evolves the terrain toward a fluvial landscape: each iteration
//! routes flow to drainage, then *incises the bed toward the cell it drains into* by the
//! stream-power law, and repeats. Two properties of that loop are what make it read as real,
//! and neither comes from a single carve:
//!
//! - **Positive feedback.** A cell that drains more cuts deeper, which draws still more flow
//!   into it. Over iterations that instability organises scattered wear into a connected,
//!   branching, dendritic network.
//! - **Base level.** Incision is *toward the receiver* (the downstream cell), solved with the
//!   implicit, unconditionally-stable update of Braun & Willett (2013). Erosion propagates up
//!   from the fixed domain boundary, so a channel cuts down to the level it drains to and no
//!   further — no runaway, and basins/lakes are just local base levels, handled for free.
//!
//! Each iteration: depression-fill for routing (shallow noise pits fill so flow connects;
//! genuine basins stay as lakes — local base levels), pick each cell's steepest-descent
//! receiver, build the drainage stack (a topological order of the flow graph), accumulate
//! catchment *area* with multiple-flow-direction routing (area spreads across every downhill
//! neighbour, so the drainage carries no grid bias — none of the diagonal "rivers" single-flow
//! D8 produces), and apply `E = K*A^m*S^n` (n = 1, so the implicit step is exact) up the stack.
//! The incision *direction* stays single-flow (toward the receiver) so the implicit solve is
//! exact, while its *magnitude* comes from the multi-flow area. Seeding area by physical cell
//! area makes it resolution-honest: where valleys form is the same at preview and build
//! resolution. The whole pass is a serial, deterministic sweep (sorted/stack ordering, never a
//! parallel reduction), which is what keeps it reproducible.
//!
//! Outputs: `heightfield` (the eroded terrain) and `flow` (the final drainage accumulation —
//! the river map). Mask-aware: the result is composited over the original through the mask.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::hydrology::{
    Grid, Receivers, build_stack, drainage_area_mfd, fill_depressions, log_normalize, receivers,
    resolve_flats,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.stream_erosion";

/// Default per-iteration incision rate (the stream-power `K`): how hard channels cut toward
/// base level each step.
const DEFAULT_STRENGTH: f64 = 0.2;
/// Default iteration count: how far the drainage network is allowed to develop. More iterations
/// deepen and connect the network (and erode more overall).
const DEFAULT_ITERATIONS: i64 = 30;
/// Default concavity (the drainage-area exponent `m`): higher concentrates incision into the
/// high-flow trunks (sharper, more defined channels); lower spreads it across the tributaries.
const DEFAULT_CONCAVITY: f64 = 0.5;
/// Default flow concentration (the MFD slope exponent): how tightly flow stays in the steepest
/// path. Low (~1) spreads flow widely across downhill neighbours, dissolving grid bias into
/// smooth dendritic drainage; high (~6) approaches single-direction routing (sharper, but
/// grid-aligned). The knob that trades artefact-free spread against channel tightness.
const DEFAULT_CONCENTRATION: f64 = 1.5;
/// Default maximum depression fill, in working height units. Pits shallower than this fill so
/// flow routes through them (removing the noise speckle); deeper basins stay as depressions and
/// become lakes (local base levels) where flow terminates.
const DEFAULT_FILL: f64 = 0.05;

/// Stream erosion modifier: one input (plus an optional mask), two outputs.
#[derive(Clone)]
pub struct StreamErosion;

impl Operator for StreamErosion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![
                PortSpec::new("in"),
                // Optional: a field whose height is the selection. When unwired, the input's
                // own mask layer is used by convention, else erode everywhere.
                PortSpec::optional("mask"),
            ],
            outputs: vec![PortSpec::new("heightfield"), PortSpec::new("flow")],
            params: vec![
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_STRENGTH),
                ),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 0, max: 500 },
                    ParamValue::Int(DEFAULT_ITERATIONS),
                ),
                ParamSpec::new(
                    "concavity",
                    ParamKind::Float { min: 0.1, max: 2.0 },
                    ParamValue::Float(DEFAULT_CONCAVITY),
                ),
                ParamSpec::new(
                    "concentration",
                    ParamKind::Float { min: 1.0, max: 6.0 },
                    ParamValue::Float(DEFAULT_CONCENTRATION),
                ),
                ParamSpec::new(
                    "fill",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FILL),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let grid = Grid { width, height };

        let strength = params.get_f64("strength", DEFAULT_STRENGTH) as f32;
        let concavity = params.get_f64("concavity", DEFAULT_CONCAVITY) as f32;
        let concentration = params.get_f64("concentration", DEFAULT_CONCENTRATION) as f32;
        let max_fill = params.get_f64("fill", DEFAULT_FILL) as f32;
        let iterations = params
            .get_i64("iterations", DEFAULT_ITERATIONS)
            .clamp(0, 100_000) as usize;
        // Physical cell area, so the accumulated catchment is in world units and the drainage
        // structure is resolution-honest.
        let cell_area = {
            let m = ctx.meters_per_cell() as f32;
            (m * m).max(1e-12)
        };

        let source = input.layer_or(layers::HEIGHT, 0.0);
        let bed = source.as_slice().to_vec();

        // Evolve a working copy of the terrain. The drainage area from the final iteration
        // becomes the flow output.
        let mut z = bed.clone();
        let mut area = vec![cell_area; bed.len()];
        for _ in 0..iterations {
            // Erosion is the slow node; poll cancellation each pass so a superseded preview
            // aborts instead of running to completion.
            if ctx.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Fill pits, then resolve the resulting flats so filled basins drain across their
            // real geometry rather than along grid-aligned spokes.
            let filled = resolve_flats(&fill_depressions(&z, &grid, max_fill), &grid);
            let receivers = receivers(&filled, &grid);
            let stack = build_stack(&receivers.to);
            // Drainage magnitude is multi-flow (spread across downhill neighbours) so it carries
            // no grid bias; the incision *direction/order* stays single-flow (the stack) so the
            // implicit step is exact.
            area = drainage_area_mfd(&filled, &grid, concentration, cell_area);
            incise(&mut z, &stack, &receivers, &area, strength, concavity);
        }

        // Flow output: the final drainage accumulation, log-normalized (it spans orders of
        // magnitude) so tributaries stay visible alongside the trunks.
        let max_area = area.iter().copied().fold(0.0_f32, f32::max);
        let flow = log_normalize(&area, max_area);

        // Composite the eroded terrain over the original through the mask (an explicit mask
        // input wins; else the input's own mask layer; else everywhere).
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };
        let result = Layer::from_fn(width, height, |x, y| {
            let idx = y * width + x;
            let m = mask.get(x, y).unwrap_or(1.0);
            bed[idx] + (z[idx] - bed[idx]) * m
        });
        let flow_layer = Layer::from_fn(width, height, |x, y| flow[y * width + x]);

        let mut heightfield = input.clone();
        heightfield.set_layer(layers::HEIGHT, Arc::new(result));
        let flow_field = Field::new(width, height, input.region())
            .with_layer(layers::HEIGHT, Arc::new(flow_layer));

        Ok(vec![heightfield, flow_field])
    }
}

/// One implicit stream-power incision pass over the stack. Traversing downstream-first (stack
/// order), each cell is drawn toward its (already-updated) receiver by the stream-power law
/// `E = strength * A^concavity * S` with the unconditionally-stable implicit solution for
/// `n = 1`, clamped so the bed only ever lowers. Channels (high drainage) cut hard toward base
/// level; hillslopes (low drainage) barely move, which is what preserves their fine detail.
fn incise(
    z: &mut [f32],
    stack: &[usize],
    receivers: &Receivers,
    area: &[f32],
    strength: f32,
    concavity: f32,
) {
    let max_area = area.iter().copied().fold(1e-12_f32, f32::max);
    for &c in stack {
        let r = receivers.to[c];
        if r == c {
            continue; // base level
        }
        // Normalized catchment (resolution-stable), sharpened by concavity, over the descent
        // distance: the implicit weight pulling this cell toward its receiver.
        let a_norm = (area[c] / max_area).clamp(0.0, 1.0);
        let f = strength * a_norm.powf(concavity) / receivers.dist[c];
        let lowered = (z[c] + f * z[r]) / (1.0 + f);
        z[c] = z[c].min(lowered);
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(StreamErosion) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{EvalContext, Region};

    fn ctx() -> EvalContext {
        // A realistic world extent so the per-cell catchment area is sane.
        EvalContext::new(32, 32, Region::UNIT, 0).with_world_extent(256.0)
    }

    /// A slope from high (top) to low (bottom), so flow accumulates downhill.
    fn ramp_field() -> Field {
        let layer = Layer::from_fn(32, 32, |_, y| 1.0 - y as f32 / 31.0);
        Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    fn run(input: &Field, params: &Params) -> Vec<Field> {
        StreamErosion
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
    }

    #[test]
    fn is_deterministic() {
        let input = ramp_field();
        let p = Params::default();
        let hash = || {
            run(&input, &p)
                .remove(0)
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash()
        };
        assert_eq!(hash(), hash());
    }

    #[test]
    fn flow_accumulates_downhill() {
        let input = ramp_field();
        let flow = run(&input, &Params::default()).remove(1);
        let layer = flow.layer(layers::HEIGHT).unwrap();
        let top = layer.get(16, 1).unwrap();
        let bottom = layer.get(16, 30).unwrap();
        assert!(
            bottom > top,
            "flow should accumulate downhill: top {top}, bottom {bottom}"
        );
    }

    #[test]
    fn erosion_only_lowers_the_bed() {
        // The implicit incision is clamped to erosion only; it never raises ground.
        let input = ramp_field();
        let out = run(&input, &Params::default()).remove(0);
        let before = input.layer(layers::HEIGHT).unwrap();
        let after = out.layer(layers::HEIGHT).unwrap();
        for (a, b) in before.as_slice().iter().zip(after.as_slice()) {
            assert!(*b <= *a + 1e-6, "erosion raised the bed: {a} -> {b}");
        }
    }

    #[test]
    fn carves_an_fbm_terrain() {
        use crate::noise::{FbmParams, fbm_field};
        let input = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 7);
        let c = EvalContext::new(64, 64, Region::UNIT, 7).with_world_extent(512.0);
        let out = StreamErosion
            .eval(Inputs::required_only(&[&input]), &Params::default(), &c)
            .unwrap();
        assert_ne!(
            input.layer(layers::HEIGHT).unwrap().content_hash(),
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "stream erosion must change the terrain"
        );
    }

    #[test]
    fn a_zero_mask_protects_the_terrain() {
        let input = ramp_field();
        let mask = Field::new(32, 32, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(32, 32, 0.0)));
        let before = input.layer(layers::HEIGHT).unwrap().content_hash();
        let required = [&input];
        let optional = [Some(&mask)];
        let out = StreamErosion
            .eval(
                Inputs::new(&required, &optional),
                &Params::default(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(
            before,
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "a zero mask must disable erosion"
        );
    }

    #[test]
    fn spec_has_heightfield_and_flow_outputs() {
        let spec = StreamErosion.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "flow"]);
        assert_eq!(spec.kind(), ymir_core::NodeKind::Modifier);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = ramp_field();
        let p = Params::default();
        let made = ymir_core::registry::make(TYPE_ID).expect("stream operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&input]), &p, &ctx())
            .unwrap();
        let direct = run(&input, &p);
        assert_eq!(
            via_registry[0]
                .layer(layers::HEIGHT)
                .unwrap()
                .content_hash(),
            direct[0].layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }
}
