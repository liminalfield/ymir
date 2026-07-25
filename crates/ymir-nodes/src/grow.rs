//! Grow / Shrink: dilate or erode a selection by a radius in world metres.
//!
//! Selections often need widening or pulling in: a thin ridgeline from a low-strength Curvature
//! selector grown a little, or a coastal `beach` mask shrunk off the waterline. Blurring spreads a
//! selection but dims it, so it takes a second node to re-solidify; this does the grow directly.
//!
//! It treats the input as a region by its `0.5` contour and moves that boundary out (`amount > 0`,
//! grow) or in (`amount < 0`, shrink) by `amount` metres, with a `softness`-wide soft edge. The move
//! is measured with the shared eikonal distance ([`signed_distance_to_contour`](crate::distance)),
//! the same isotropic solve the coastal model and the Distance selector use, so the grown boundary
//! is a true offset with no eight-lobed star, and the result is byte-identical on every machine.
//!
//! Because it works from the `0.5` contour, an analog (soft) mask is re-rendered as a solid region
//! with a soft edge: `amount = 0` cleans a fuzzy selection to a crisp one. Other layers pass through.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::distance::signed_distance_to_contour;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.grow_shrink";

/// The contour of the input treated as the selection boundary. A selection is high (near one)
/// inside and low (near zero) outside, so its edge is the half-way crossing.
const SELECTION_LEVEL: f32 = 0.5;

/// Default grow distance in world metres. A small positive value widens a selection a little out of
/// the box; a negative value shrinks it.
const DEFAULT_AMOUNT: f64 = 10.0;
/// Default edge softness in world metres: the width of the ramp from selected to unselected across
/// the grown boundary.
const DEFAULT_SOFTNESS: f64 = 4.0;

/// Grow / Shrink modifier: one input, one output.
#[derive(Clone)]
pub struct GrowShrink;

impl Operator for GrowShrink {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "amount",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_AMOUNT),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "softness",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_SOFTNESS),
                )
                .with_unit(Unit::Meters),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads only the world horizontal extent (the distance is in world metres), not the world
    /// height or sea level, so those two sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::WORLD_EXTENT
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let selection = input.layer_or(layers::HEIGHT, 0.0);

        let amount = params.get_f64("amount", DEFAULT_AMOUNT) as f32;
        // A zero softness would divide by zero; clamp to a hair so it degrades to a hard edge.
        let softness = params.get_f64("softness", DEFAULT_SOFTNESS).max(1e-6) as f32;
        let cell_size = ctx.meters_per_cell() as f32;

        // Signed distance (world metres) from the selection's boundary: positive inside (above the
        // level), negative outside. Growing by `amount` moves the boundary out to where the distance
        // reads `-amount`, so the soft step is centred there.
        let signed = signed_distance_to_contour(&selection, SELECTION_LEVEL, cell_size);
        let grown = Layer::from_fn(width, height, |x, y| {
            let d = signed.get(x, y).unwrap_or(0.0);
            // One inside the grown boundary, zero outside, ramping over `softness` centred on it.
            soft_step((d + amount) / softness + 0.5)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(grown));
        Ok(vec![out])
    }
}

/// Cubic Hermite smoothstep of `t`, clamped to `[0, 1]`: zero at or below zero, one at or above one,
/// eased between. Used to render the grown boundary as a soft edge.
fn soft_step(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(GrowShrink) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;
    use ymir_core::{NodeKind, Region};

    /// A context where the world is a cube of side `size`, so metres-per-cell is 1 and a distance in
    /// cells equals the distance in metres.
    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0).with_world_extent(size as f64)
    }

    /// A centred solid disk selection of radius `r` cells (one inside, zero outside), so the
    /// selection boundary is a circle a known distance from the centre.
    fn disk(size: usize, r: f32) -> Field {
        let c = (size - 1) as f32 / 2.0;
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                if d <= r { 1.0 } else { 0.0 }
            })),
        )
    }

    fn run(input: &Field, amount: f64, softness: f64, ctx: &EvalContext) -> Field {
        let params = Params::new()
            .with("amount", ParamValue::Float(amount))
            .with("softness", ParamValue::Float(softness));
        GrowShrink
            .eval(Inputs::required_only(&[input]), &params, ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn spec_is_a_filter_modifier() {
        let spec = GrowShrink.spec();
        assert_eq!(spec.kind(), NodeKind::Modifier);
        assert_eq!(spec.category, "filter");
        assert_eq!(spec.type_id, TYPE_ID);
    }

    #[test]
    fn growing_widens_the_selection() {
        // A radius-8 disk on a 48 grid; a cell 11 cells from the centre is outside (0), but after
        // growing by 5 m it falls inside the grown boundary (~radius 13) and reads one.
        let field = disk(48, 8.0);
        let before = at(&field, 24 + 11, 24);
        assert!(before < 0.01, "the cell starts outside the disk: {before}");
        let grown = run(&field, 5.0, 1.0, &ctx(48));
        assert!(
            at(&grown, 24 + 11, 24) > 0.9,
            "growing by 5 m must pull the cell into the selection"
        );
    }

    #[test]
    fn shrinking_pulls_the_selection_in() {
        // The same disk; a cell 6 cells out is inside (1), but shrinking by 4 m (grown radius ~4)
        // leaves it outside and reads zero.
        let field = disk(48, 8.0);
        let before = at(&field, 24 + 6, 24);
        assert!(before > 0.99, "the cell starts inside the disk: {before}");
        let shrunk = run(&field, -4.0, 1.0, &ctx(48));
        assert!(
            at(&shrunk, 24 + 6, 24) < 0.1,
            "shrinking by 4 m must push the cell out of the selection"
        );
    }

    #[test]
    fn zero_amount_keeps_the_boundary() {
        // With no grow, the boundary stays at the original radius: a cell just inside is still
        // selected, a cell just outside is not (the region is preserved, only re-rendered crisp).
        let field = disk(64, 12.0);
        let out = run(&field, 0.0, 1.0, &ctx(64));
        assert!(at(&out, 32 + 10, 32) > 0.9, "just inside stays selected");
        assert!(at(&out, 32 + 15, 32) < 0.1, "just outside stays unselected");
    }

    #[test]
    fn softness_widens_the_edge() {
        // A wider softness spreads the ramp: sampled one cell inside the boundary, a soft edge reads
        // below one while a near-hard edge reads essentially one.
        let field = disk(64, 12.0);
        let hard = run(&field, 0.0, 0.5, &ctx(64));
        let soft = run(&field, 0.0, 8.0, &ctx(64));
        // At the boundary cell (~12 from centre) the soft edge is mid-ramp, the hard edge saturated.
        let (bx, by) = (32 + 12, 32);
        assert!(
            soft_edge_is_gentler(at(&hard, bx, by), at(&soft, bx, by)),
            "a wider softness must not produce a sharper edge"
        );
    }

    /// The soft edge's value at the boundary should sit nearer the mid-ramp (0.5) than the hard
    /// edge's, i.e. its distance from 0.5 is no greater.
    fn soft_edge_is_gentler(hard: f32, soft: f32) -> bool {
        (soft - 0.5).abs() <= (hard - 0.5).abs() + 1e-4
    }

    #[test]
    fn passes_through_other_layers() {
        let mut field = disk(32, 8.0);
        field.set_layer("flow", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = run(&field, 3.0, 1.0, &ctx(32));
        assert_eq!(
            out.layer("flow").unwrap().get(0, 0).unwrap(),
            0.7,
            "an unrelated layer must pass through"
        );
    }

    #[test]
    fn is_deterministic() {
        let field = disk(48, 10.0);
        let c = ctx(48);
        assert_eq!(
            run(&field, 6.0, 2.0, &c).content_hash(),
            run(&field, 6.0, 2.0, &c).content_hash()
        );
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let field = disk(32, 8.0);
        let made = registry::make(TYPE_ID).expect("grow/shrink is registered");
        let via_registry = made
            .eval(
                Inputs::required_only(&[&field]),
                &Params::new().with("amount", ParamValue::Float(4.0)),
                &ctx(32),
            )
            .unwrap();
        let direct = run(&field, 4.0, DEFAULT_SOFTNESS, &ctx(32));
        assert_eq!(via_registry[0].content_hash(), direct.content_hash());
    }
}
