//! Flow selector (droplet): a rainfall/droplet simulation of where water collects.
//!
//! The A/B counterpart to the grid [`crate::flow_select`] node. Where that conditions depressions
//! (fill/breach) and accumulates flow per cell — which fans filled basins, scars breached ones,
//! and aliases every channel border — this simulates rainfall as momentum-carried droplets that
//! trace continuous, sub-cell paths down the terrain (the approach Gaea and World Machine use).
//! Droplets pool in pits by themselves (no depression conditioning, so no fans or scars) and never
//! snap to cell edges (so no aliasing). The output is the same `[0, 1]` band selection on the
//! `height` layer, high where many droplets pass. Drainage is resolution-dependent physics: a
//! finer build resolves more channels, so a preview approximates the full build.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::hydrology::{Grid, log_normalize_span};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.flow_droplet";

/// Default band over the normalized `[0, 1]` flow (channels), matching the grid Flow selector.
const DEFAULT_MIN: f64 = 0.3;
const DEFAULT_MAX: f64 = 1.0;
const DEFAULT_FALLOFF: f64 = 0.15;
/// Default droplet momentum: 0 hugs the steepest descent (crisper, more grid-hugging), toward 1
/// carries straighter (smoother, meandering). A little momentum smooths the sub-cell path.
const DEFAULT_INERTIA: f64 = 0.3;
/// Default rainfall density, in droplets per cell. Higher is smoother but slower.
const DEFAULT_RAIN: f64 = 1.0;

/// Deterministic SplitMix64, so the droplets (and the flow map) are repeatable per seed with no
/// dependency and no hash/thread ordering.
struct SplitMix64(u64);
impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// A float in `[0, 1)` from the top 24 bits.
    fn next_unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }
}

/// Bilinear height and uphill gradient at a continuous position in `[0, w-1] x [0, h-1]`.
fn sample(bed: &[f32], w: usize, h: usize, px: f32, py: f32) -> (f32, f32, f32) {
    let cx = (px.floor() as usize).min(w - 2);
    let cy = (py.floor() as usize).min(h - 2);
    let fx = px - cx as f32;
    let fy = py - cy as f32;
    let h00 = bed[cy * w + cx];
    let h10 = bed[cy * w + cx + 1];
    let h01 = bed[(cy + 1) * w + cx];
    let h11 = bed[(cy + 1) * w + cx + 1];
    let gx = (h10 - h00) * (1.0 - fy) + (h11 - h01) * fy;
    let gy = (h01 - h00) * (1.0 - fx) + (h11 - h10) * fx;
    let height = h00 * (1.0 - fx) * (1.0 - fy)
        + h10 * fx * (1.0 - fy)
        + h01 * (1.0 - fx) * fy
        + h11 * fx * fy;
    (height, gx, gy)
}

/// Simulate `droplets` rainfall droplets over `bed`, returning a flow accumulation: each droplet
/// starts at a random cell, descends by momentum-blended steepest descent in continuous
/// coordinates, and splats a unit of flow bilinearly into the grid at each step until it pools (no
/// descent), leaves the domain, or hits the step cap. No depression conditioning; the sub-cell
/// paths keep the result smooth. Deterministic given `seed`.
fn droplet_flow(bed: &[f32], grid: &Grid, seed: u64, droplets: usize, inertia: f32) -> Vec<f32> {
    let (w, h) = (grid.width, grid.height);
    let n = w * h;
    let mut acc = vec![0.0_f32; n];
    if w < 2 || h < 2 {
        return acc;
    }
    let max_steps = w + h; // a droplet can cross the domain once
    let span_x = (w as f32 - 3.0).max(0.0);
    let span_y = (h as f32 - 3.0).max(0.0);
    let mut rng = SplitMix64(seed ^ 0x517C_C1B7_2722_0A95);

    for _ in 0..droplets {
        let mut px = 1.0 + rng.next_unit() * span_x;
        let mut py = 1.0 + rng.next_unit() * span_y;
        let (mut dx, mut dy) = (0.0_f32, 0.0_f32);
        for _ in 0..max_steps {
            let (_, gx, gy) = sample(bed, w, h, px, py);
            // Blend the previous direction (momentum) with downhill (the negative gradient). The
            // momentum is what lets a droplet coast across the countless micro-pits of raw noise
            // — at a pit floor the gradient vanishes, so the droplet keeps its heading and leaves
            // the far side — instead of pooling in the first one and never reaching a channel.
            dx = dx * inertia - gx * (1.0 - inertia);
            dy = dy * inertia - gy * (1.0 - inertia);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-6 {
                break; // truly stalled: no momentum and no gradient
            }
            dx /= len;
            dy /= len;
            px += dx;
            py += dy;
            if px < 1.0 || py < 1.0 || px > w as f32 - 2.0 || py > h as f32 - 2.0 {
                break; // left the domain
            }
            // Splat a unit of flow bilinearly at the new position (this is what keeps it smooth).
            let cx = px.floor() as usize;
            let cy = py.floor() as usize;
            let fx = px - cx as f32;
            let fy = py - cy as f32;
            acc[cy * w + cx] += (1.0 - fx) * (1.0 - fy);
            acc[cy * w + cx + 1] += fx * (1.0 - fy);
            acc[(cy + 1) * w + cx] += (1.0 - fx) * fy;
            acc[(cy + 1) * w + cx + 1] += fx * fy;
        }
    }
    acc
}

/// Droplet flow selector: one input, one output. Writes the selection to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct FlowDroplet;

impl Operator for FlowDroplet {
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
                    "inertia",
                    ParamKind::Float {
                        min: 0.0,
                        max: 0.95,
                    },
                    ParamValue::Float(DEFAULT_INERTIA),
                ),
                ParamSpec::new(
                    "rain",
                    ParamKind::Float { min: 0.1, max: 8.0 },
                    ParamValue::Float(DEFAULT_RAIN),
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
        let inertia = params.get_f64("inertia", DEFAULT_INERTIA).clamp(0.0, 0.95) as f32;
        let rain = params.get_f64("rain", DEFAULT_RAIN).max(0.0);

        let grid = Grid { width, height };
        let bed = h.as_slice().to_vec();
        let droplets = (rain * (width * height) as f64).round() as usize;
        let acc = droplet_flow(&bed, &grid, ctx.seed, droplets, inertia);
        // Stretch across the actual range so ridges read 0 and the busiest channels read 1.
        let flow = log_normalize_span(&acc);

        // Band-select the normalized flow, softening over `falloff` at each edge.
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
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(FlowDroplet) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    /// A plane whose height increases with y, so it drains toward y = 0 and droplets collect on
    /// the low (small-y) edge.
    fn ramp(size: usize) -> Field {
        let layer = Layer::from_fn(size, size, |_, y| y as f32 / (size - 1) as f32);
        Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    fn run(input: &Field, params: &Params) -> Field {
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 7);
        FlowDroplet
            .eval(Inputs::required_only(&[input]), params, &ctx)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn is_deterministic() {
        let input = ramp(48);
        assert_eq!(
            run(&input, &Params::new()).content_hash(),
            run(&input, &Params::new()).content_hash()
        );
    }

    #[test]
    fn stays_in_unit_range() {
        let out = run(&ramp(48), &Params::new());
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v))
        );
    }

    #[test]
    fn the_selection_rides_on_height_not_a_mask_layer() {
        let out = run(&ramp(48), &Params::new());
        assert!(out.layer(layers::MASK).is_none());
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = ramp(48);
        input.set_layer("debris", Arc::new(Layer::filled(48, 48, 0.7)));
        let out = run(&input, &Params::new());
        assert_eq!(out.layer("debris").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn more_flow_collects_downhill_than_on_the_ridge() {
        // On a plane draining toward y = 0, the low rows carry more droplets than the high ridge.
        let size = 64;
        let out = run(
            &ramp(size),
            &Params::new().with("rain", ParamValue::Float(4.0)),
        );
        let layer = out.layer(layers::HEIGHT).unwrap();
        let row_mean = |y: usize| -> f32 {
            (0..size).map(|x| layer.get(x, y).unwrap()).sum::<f32>() / size as f32
        };
        assert!(
            row_mean(3) > row_mean(size - 4),
            "the draining low edge should select more than the high ridge"
        );
    }

    #[test]
    fn spec_is_a_selector_modifier() {
        assert_eq!(FlowDroplet.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(FlowDroplet.spec().type_id, TYPE_ID);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = ymir_core::registry::make(TYPE_ID).expect("droplet flow selector is registered");
        let input = ramp(32);
        let ctx = EvalContext::new(32, 32, Region::UNIT, 7);
        let via = made
            .eval(Inputs::required_only(&[&input]), &Params::new(), &ctx)
            .unwrap();
        let direct = FlowDroplet
            .eval(Inputs::required_only(&[&input]), &Params::new(), &ctx)
            .unwrap();
        assert_eq!(
            via[0].layer(layers::HEIGHT).unwrap().content_hash(),
            direct[0].layer(layers::HEIGHT).unwrap().content_hash(),
        );
    }
}
