//! Rivers: incise a drainage network into the terrain by carving flow accumulation.
//!
//! A complete, self-contained fluvial model that reads any heightfield and cuts the connected
//! channel network its drainage implies. Unlike [`StreamErosion`](crate::StreamErosion), which
//! evolves the bed by the slope-dependent stream-power law (`E = K*A^m*S^n`) and so concentrates
//! its incision near the outlets where relief is high, this carves the bed directly by drainage
//! *area*: every cell that drains toward the domain boundary is lowered in proportion to how much
//! catchment passes through it. Because the carve depends on accumulation rather than local slope,
//! channels appear along the whole network, on gentle interior slopes as well as at the trunks,
//! which is what turns smooth, low-relief terrain into visible dendritic valleys where stream power
//! would touch only the boundary fans.
//!
//! The pipeline is the shared drainage substrate (see [`crate::hydrology`]): fill shallow pits so
//! flow routes through them (deeper basins stay as lakes, local base levels), resolve the filled
//! flats so they drain across their true geometry rather than grid-aligned spokes, route steepest
//! descent, and accumulate multiple-flow catchment area (flow spreads across every downhill
//! neighbour, dissolving the grid bias single-flow routing leaves). The carve then lowers each cell
//! by `depth * flow^sharpness`, where `flow` is the log-normalized accumulation, so trunks cut
//! deepest while hillslopes barely move and keep their detail. Only cells whose drainage reaches
//! the boundary are carved: a closed basin collects water rather than shedding it, so carving there
//! would gouge a pit instead of a channel (the same reach-the-boundary rule Stream uses). The carve
//! is smoothed (`smoothing`) before it is applied, which gives the incision a cross-sectional
//! profile, a valley with banks, instead of the one-cell grid-aliased slot the raw per-cell carve
//! leaves; only the carve is blurred, so the surrounding terrain detail is untouched. The whole
//! pass is serial and deterministic, so the node is byte-reproducible.
//!
//! Outputs: `heightfield` (the incised terrain, composited over the original through the mask),
//! `flow` (the log-normalized drainage accumulation, the river map), and `wear` (how far each cell
//! was cut). The model only ever lowers the bed, so there is no deposition tap. Mask-aware: an
//! optional mask (or the input's own mask layer) scales the carve, so a protected region keeps its
//! height.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::erosion;
use crate::hydrology::{
    Grid, build_stack, drainage_area_mfd, fill_depressions, log_normalize, receivers, resolve_flats,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.rivers";

/// Default carve depth: the height (in working units) cut out of the deepest trunk channel, with
/// tributaries scaled below it by the flow. The single "how incised" knob.
const DEFAULT_DEPTH: f64 = 0.2;
/// Default carve sharpness: the power applied to the normalized flow before carving. 1 lowers the
/// whole network in proportion to flow; higher concentrates the cut into the established channels
/// (crisper valleys, flatter interfluves) and leaves faint drainage barely touched.
const DEFAULT_SHARPNESS: f64 = 2.0;
/// Default flow concentration (the MFD slope exponent): how tightly flow stays in the steepest
/// path. Low (~1) spreads flow widely into smooth dendritic drainage; high (~6) approaches
/// single-direction routing (sharper, but grid-aligned). Shared meaning with Stream.
const DEFAULT_CONCENTRATION: f64 = 1.5;
/// Default maximum depression fill, in working height units. Pits shallower than this fill so flow
/// routes through them (removing the noise speckle that would otherwise pit); deeper basins stay as
/// depressions and become lakes (local base levels) that are not carved. Shared meaning with Stream.
const DEFAULT_FILL: f64 = 0.05;
/// Default bank smoothing: how much the carve is blurred before it is applied, which turns the
/// one-cell incision slot into a valley with a cross-sectional profile. 0 is the raw per-cell carve
/// (a sharp, grid-aliased slot); higher widens and softens the banks.
const DEFAULT_SMOOTHING: f64 = 0.5;
/// The carve-smoothing radius at [`SMOOTH_REFERENCE_RES`] when `smoothing` is 1, in cells. Scaled
/// with resolution so a channel keeps the same world width at preview and build.
const SMOOTH_MAX_CELLS: f32 = 2.0;
/// The resolution the smoothing radius is expressed at, scaled with the grid from here.
const SMOOTH_REFERENCE_RES: f32 = 256.0;
/// Box passes over the carve (two passes approximate a Gaussian), so the banks are smooth rather
/// than a box-filter's flat shoulders.
const SMOOTH_PASSES: usize = 2;

/// Rivers modifier: one input (plus an optional mask), and the incised terrain, flow, and wear.
#[derive(Clone)]
pub struct Rivers;

impl Operator for Rivers {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![
                PortSpec::new("in"),
                // Optional: a field whose height is the selection. When unwired, the input's own
                // mask layer is used by convention, else carve everywhere.
                PortSpec::optional("mask"),
            ],
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("flow"),
                PortSpec::new("wear"),
            ],
            params: vec![
                ParamSpec::new(
                    "depth",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_DEPTH),
                ),
                ParamSpec::new(
                    "sharpness",
                    ParamKind::Float { min: 1.0, max: 4.0 },
                    ParamValue::Float(DEFAULT_SHARPNESS),
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
                ParamSpec::new(
                    "smoothing",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_SMOOTHING),
                ),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let grid = Grid { width, height };

        let depth = params.get_f64("depth", DEFAULT_DEPTH) as f32;
        let sharpness = params.get_f64("sharpness", DEFAULT_SHARPNESS) as f32;
        let concentration = params.get_f64("concentration", DEFAULT_CONCENTRATION) as f32;
        let max_fill = params.get_f64("fill", DEFAULT_FILL) as f32;
        let smoothing = params
            .get_f64("smoothing", DEFAULT_SMOOTHING)
            .clamp(0.0, 1.0) as f32;

        let source = input.layer_or(layers::HEIGHT, 0.0);
        let bed = source.as_slice().to_vec();
        let n = bed.len();

        // The drainage solve is the slow part and cannot be interrupted mid-call, so poll before it
        // and again after: a superseded preview then bails instead of building outputs it will
        // discard.
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }

        // Condition the terrain for routing, route it, and accumulate catchment area. Filling only
        // steers flow; the carve below is applied to the real bed, never the filled surface.
        let filled = resolve_flats(&fill_depressions(&bed, &grid, max_fill), &grid);
        let recv = receivers(&filled, &grid);
        let stack = build_stack(&recv.to);
        let area = drainage_area_mfd(&filled, &grid, concentration, 1.0);

        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }

        // The river map: accumulation log-normalized (it spans orders of magnitude) so tributaries
        // stay visible alongside the trunks.
        let max_area = area.iter().copied().fold(0.0_f32, f32::max);
        let flow = log_normalize(&area, max_area);

        // Whether each cell's drainage reaches the domain boundary (a real outlet) rather than a
        // closed interior sink (a lake). Set at the roots and inherited downstream-first: the stack
        // lists every receiver before its donors, so a cell's receiver flag is final when reached.
        let (w, h) = (width, height);
        let mut drains_out = vec![false; n];
        for &c in &stack {
            let r = recv.to[c];
            if r == c {
                let (x, y) = (c % w, c / w);
                drains_out[c] = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            } else {
                drains_out[c] = drains_out[r];
            }
        }

        // The carve amount per cell: a sharpened power of the flow along the draining network, zero
        // in the closed basins (lakes). Kept as its own field so it can be smoothed before it is
        // applied. Carving the raw per-cell flow leaves a one-cell-wide slot with near-vertical,
        // grid-aligned walls (the jagged, aliased channel); blurring the carve first gives the
        // incision a cross-sectional profile, a valley with banks, the way a real channel has, while
        // the surrounding terrain detail is left untouched (only the carve is smoothed, not the bed).
        let mut carve = vec![0.0_f32; n];
        for c in 0..n {
            if drains_out[c] {
                carve[c] = depth * flow[c].powf(sharpness);
            }
        }
        let smooth_radius =
            (smoothing * SMOOTH_MAX_CELLS * width as f32 / SMOOTH_REFERENCE_RES).round() as i32;
        if smooth_radius > 0 {
            carve = box_blur(&carve, width, height, smooth_radius, SMOOTH_PASSES);
        }

        // Apply the carve only to cells that drain out, so a lake stays exactly at its floor even
        // where the smoothing bled a little carve across its shoreline. The carve is non-negative,
        // so the bed only ever lowers.
        let mut z = bed.clone();
        for c in 0..n {
            if drains_out[c] {
                z[c] -= carve[c];
            }
        }

        // Composite the incised terrain over the original through the mask (an explicit mask input
        // wins; else the input's own mask layer; else everywhere).
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

        // Wear is how far the bed dropped, from the unmasked carve (like the other erosion nodes).
        // The model only lowers, so the change is never positive and there is no deposition tap.
        let wear: Vec<f32> = bed
            .iter()
            .zip(&z)
            .map(|(&b, &a)| (b - a).max(0.0))
            .collect();

        let region = input.region();
        let mut heightfield = input.clone();
        heightfield.set_layer(layers::HEIGHT, Arc::new(result));
        let flow_field =
            Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(flow_layer));
        Ok(vec![
            heightfield,
            flow_field,
            erosion::byproduct_field(wear, width, height, region),
        ])
    }
}

/// A separable box blur (`passes` box passes approximate a Gaussian), used to give the carve a
/// smooth cross-section before it is applied. Edge windows shrink to the in-bounds cells, so the
/// result is well-defined at the borders. Each pass reads a snapshot and writes every cell
/// independently, so it is order- and thread-count-independent (byte-identical at any core count).
fn box_blur(src: &[f32], width: usize, height: usize, radius: i32, passes: usize) -> Vec<f32> {
    use rayon::prelude::*;

    let mut buf = src.to_vec();
    if radius <= 0 || width == 0 || height == 0 {
        return buf;
    }
    let r = radius as usize;
    for _ in 0..passes {
        // Horizontal pass into `tmp`.
        let mut tmp = vec![0.0_f32; buf.len()];
        tmp.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            let base = y * width;
            for (x, out) in row.iter_mut().enumerate() {
                let lo = x.saturating_sub(r);
                let hi = (x + r).min(width - 1);
                let mut sum = 0.0_f32;
                for cell in buf.iter().take(base + hi + 1).skip(base + lo) {
                    sum += *cell;
                }
                *out = sum / (hi - lo + 1) as f32;
            }
        });
        // Vertical pass into `out`.
        let mut out = vec![0.0_f32; buf.len()];
        out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            let lo = y.saturating_sub(r);
            let hi = (y + r).min(height - 1);
            let count = (hi - lo + 1) as f32;
            for (x, cell) in row.iter_mut().enumerate() {
                let mut sum = 0.0_f32;
                for yi in lo..=hi {
                    sum += tmp[yi * width + x];
                }
                *cell = sum / count;
            }
        });
        buf = out;
    }
    buf
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Rivers) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{EvalContext, Region};

    fn ctx() -> EvalContext {
        EvalContext::new(32, 32, Region::UNIT, 0)
    }

    /// A slope from high (top) to low (bottom), so flow accumulates downhill.
    fn ramp_field() -> Field {
        let layer = Layer::from_fn(32, 32, |_, y| 1.0 - y as f32 / 31.0);
        Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    fn run(input: &Field, params: &Params) -> Vec<Field> {
        Rivers
            .eval(Inputs::required_only(&[input]), params, &ctx())
            .unwrap()
    }

    #[test]
    fn is_deterministic() {
        // The whole pipeline is serial and deterministic, so a repeat run is byte-identical.
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
    fn it_carves_and_never_raises() {
        // The model only ever lowers the bed: every cell drops or stays, so wear is where it cut
        // and there is nowhere the terrain rose.
        use crate::noise::{FbmParams, fbm_field};
        let input = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 7);
        let c = EvalContext::new(64, 64, Region::UNIT, 7);
        let out = Rivers
            .eval(Inputs::required_only(&[&input]), &Params::default(), &c)
            .unwrap();
        let before = input.layer(layers::HEIGHT).unwrap();
        let after = out[0].layer(layers::HEIGHT).unwrap();
        assert_ne!(
            before.content_hash(),
            after.content_hash(),
            "rivers must incise the terrain"
        );
        for y in 0..64 {
            for x in 0..64 {
                assert!(
                    after.get(x, y).unwrap() <= before.get(x, y).unwrap() + 1e-6,
                    "the carve must never raise the bed at ({x},{y})"
                );
            }
        }
        // Wear (output 2) records the cut.
        let wear_sum: f64 = out[2]
            .layer(layers::HEIGHT)
            .unwrap()
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum();
        assert!(wear_sum > 0.0, "wear should record the material cut away");
    }

    #[test]
    fn a_closed_basin_is_not_carved() {
        // A ramp draining to the bottom boundary, with a deep single-cell pit gouged into the
        // middle. The pit is a closed interior sink (far deeper than the fill cap), so no drainage
        // path from it reaches the boundary: it collects water as a lake and must not be carved
        // (otherwise the flow piling into it would gouge a deeper pit). The boundary-draining ramp
        // still incises.
        let mut heights: Vec<f32> = (0..32 * 32).map(|i| 1.0 - (i / 32) as f32 / 31.0).collect();
        let pit = 16 * 32 + 16;
        heights[pit] = -0.5;
        let layer = Layer::from_fn(32, 32, |x, y| heights[y * 32 + x]);
        let input = Field::new(32, 32, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer));

        let out = run(&input, &Params::default());
        let result = out[0].layer(layers::HEIGHT).unwrap();
        assert_eq!(
            result.get(16, 16).unwrap(),
            -0.5,
            "the closed-basin floor must not be carved"
        );
        assert_ne!(
            input.layer(layers::HEIGHT).unwrap().content_hash(),
            result.content_hash(),
            "the boundary-draining ramp must still incise"
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
        let out = Rivers
            .eval(
                Inputs::new(&required, &optional),
                &Params::default(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(
            before,
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "a zero mask must disable the carve"
        );
    }

    #[test]
    fn spec_outputs_are_heightfield_flow_wear() {
        let spec = Rivers.spec();
        let names: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["heightfield", "flow", "wear"]);
        assert_eq!(spec.kind(), ymir_core::NodeKind::Modifier);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let input = ramp_field();
        let p = Params::default();
        let made = ymir_core::registry::make(TYPE_ID).expect("rivers operator is registered");
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
