//! Flow selector: selects drainage channels from on-demand flow accumulation.
//!
//! Output is a `[0, 1]` selection on the **`height`** layer (so the rest of the toolset shapes
//! and applies it), high where a lot of upstream drainage passes through a cell, falling off to
//! zero outside the chosen band. It is the drainage counterpart to the Slope, Height, and
//! Curvature selectors: where Slope reads the gradient and Curvature the second derivative, this
//! reads how much water collects.
//!
//! The flow is computed on demand from the input height (depression-fill, then
//! multiple-flow-direction accumulation, then a log map that keeps tributaries visible), reusing
//! the shared [`crate::hydrology`] primitives. So it selects "where water would run" on any
//! terrain, not only on the output of an erosion node. `concentration` controls how tightly flow
//! stays in the steepest path (low spreads it widely, high keeps it channelised), matching the
//! Stream node. Like the other selectors it is not mask-aware: it derives a selection from
//! scratch.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::hydrology::{
    Grid, drainage_area_mfd, fill_depressions, log_normalize_span, resolve_flats,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.flow";

/// Default band over the normalized `[0, 1]` flow: selects the upper range (channels) out of the
/// box, with the top open so the largest rivers are included.
const DEFAULT_MIN: f64 = 0.3;
const DEFAULT_MAX: f64 = 1.0;
const DEFAULT_FALLOFF: f64 = 0.15;
/// Default flow concentration (the MFD slope exponent), matching the Stream node: low spreads
/// flow widely (smooth, dendritic), high keeps it tightly channelised.
const DEFAULT_CONCENTRATION: f64 = 1.5;

/// Flow selector: one input, one output. Writes the selection to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct FlowSelect;

impl Operator for FlowSelect {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "min",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MIN),
                ),
                ParamSpec::new(
                    "max",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_MAX),
                ),
                ParamSpec::new(
                    "falloff",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_FALLOFF),
                ),
                ParamSpec::new(
                    "concentration",
                    ParamKind::Float { min: 1.0, max: 6.0 },
                    ParamValue::Float(DEFAULT_CONCENTRATION),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let min = params.get_f64("min", DEFAULT_MIN) as f32;
        let max = params.get_f64("max", DEFAULT_MAX) as f32;
        let falloff = params.get_f64("falloff", DEFAULT_FALLOFF).max(0.0) as f32;
        let concentration = params.get_f64("concentration", DEFAULT_CONCENTRATION) as f32;

        // Compute the drainage map: fill pits fully so flow routes everywhere (a selector wants
        // the whole connected network, not lakes), resolve the resulting flats so basins drain
        // across their real geometry instead of along grid-aligned spokes, accumulate by physical
        // cell area so the pattern is resolution-honest, then log-map to keep tributaries visible.
        let grid = Grid { width, height };
        let bed = h.as_slice().to_vec();
        let cell_area = {
            let m = ctx.meters_per_cell() as f32;
            (m * m).max(1e-12)
        };
        let filled = resolve_flats(&fill_depressions(&bed, &grid, f32::INFINITY), &grid);
        let area = drainage_area_mfd(&filled, &grid, concentration, cell_area);
        // Stretch across the actual range so ridges read 0 and the largest channels read 1,
        // making the band meaningful (the cell-area seed cancels in the stretch).
        let flow = log_normalize_span(&area);

        // Band-select the normalized flow, softening over `falloff` at each edge (the same
        // trapezoid as the Slope selector).
        let selection = Layer::from_fn(width, height, |x, y| {
            let f = flow[y * width + x];
            let lower = smoothstep(min - falloff, min, f);
            let upper = 1.0 - smoothstep(max, max + falloff, f);
            (lower * upper).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));
        Ok(vec![out])
    }
}

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(FlowSelect) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    /// A ramp high at the top (y = 0), low at the bottom, so flow accumulates downhill.
    fn ramp(size: usize) -> Field {
        let layer = Layer::from_fn(size, size, |_, y| 1.0 - y as f32 / (size - 1) as f32);
        Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    fn select(input: &Field, min: f32, max: f32, falloff: f32) -> Field {
        let params = Params::new()
            .with("min", ParamValue::Float(f64::from(min)))
            .with("max", ParamValue::Float(f64::from(max)))
            .with("falloff", ParamValue::Float(f64::from(falloff)));
        // A context matching the field (as in real evaluation), with a realistic world extent
        // so the per-cell catchment area is sane.
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 0)
            .with_world_extent(256.0);
        FlowSelect
            .eval(Inputs::required_only(&[input]), &params, &ctx)
            .unwrap()
            .remove(0)
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn high_flow_channels_are_selected_and_ridges_are_not() {
        let out = select(
            &ramp(32),
            DEFAULT_MIN as f32,
            DEFAULT_MAX as f32,
            DEFAULT_FALLOFF as f32,
        );
        // Near the bottom the column's drainage has collected, so flow is high and selected;
        // the very top of the ramp is the ridge with no upstream, so it is rejected.
        assert!(at(&out, 16, 30) > 0.5, "high-flow channel should select");
        assert!(
            at(&out, 16, 0) < 0.1,
            "the no-upstream ridge top should not select"
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = select(&ramp(32), 0.3, 1.0, 0.15);
        assert!(out.layer(layers::MASK).is_none());
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(32);
        input.set_layer("debris", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = select(&input, 0.3, 1.0, 0.15);
        assert_eq!(out.layer("debris").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn stays_in_unit_range() {
        let out = select(&ramp(32), 0.3, 1.0, 0.15);
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(32);
        assert_eq!(
            select(&input, 0.3, 1.0, 0.15).content_hash(),
            select(&input, 0.3, 1.0, 0.15).content_hash()
        );
    }

    #[test]
    fn spec_is_a_selector_modifier() {
        assert_eq!(FlowSelect.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(FlowSelect.spec().type_id, TYPE_ID);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = ymir_core::registry::make(TYPE_ID).expect("flow selector is registered");
        let input = ramp(16);
        let params = Params::new();
        let ctx = EvalContext::new(16, 16, Region::UNIT, 0).with_world_extent(256.0);
        let via = made
            .eval(Inputs::required_only(&[&input]), &params, &ctx)
            .unwrap();
        let direct = FlowSelect
            .eval(Inputs::required_only(&[&input]), &params, &ctx)
            .unwrap();
        assert_eq!(
            via[0].layer(layers::HEIGHT).unwrap().content_hash(),
            direct[0].layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }
}
