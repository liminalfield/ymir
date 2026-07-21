//! Occlusion selector: an ambient-occlusion / sky-view measure of how sheltered each cell is.
//!
//! The non-local member of the selector family (Slope, Curvature, Aspect are local). Output is a
//! `[0, 1]` selection on the **`height`** layer, high in crevices and valley floors that are hemmed
//! in by higher ground, low on open peaks, ridges, and flats. It picks the sheltered terrain that
//! local slope and curvature cannot: sediment catchment, moisture, shadow.
//!
//! For each cell it casts `rays` evenly-spaced rays across the compass and marches each out to
//! `radius` (world metres), tracking the steepest upward angle to the horizon along that ray. The
//! occlusion is the mean over rays of `sin(horizon_angle)` — the fraction of each vertical slice of
//! sky blocked by terrain — so a fully open cell reads `0` and a deep pit approaches `1`.
//!
//! The horizon angle is a real terrain angle: heights are turned into a true rise-over-run through
//! the vertical:horizontal scale, and the march distance is set in world units, so the result is
//! resolution-stable. `SLOPE` context-deps (world height and extent, not the sea level).

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.occlusion";

/// Default march distance in world units (metres): a mid reach that reads local shelter without
/// scanning the whole map.
const DEFAULT_RADIUS: f64 = 100.0;
/// Default number of compass rays: enough angular coverage for a smooth measure without being slow.
const DEFAULT_RAYS: i64 = 16;

/// Occlusion selector: one input, one output. Writes the shelter measure to [`layers::HEIGHT`].
#[derive(Clone)]
pub struct Occlusion;

impl Operator for Occlusion {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![
                ParamSpec::new(
                    "radius",
                    ParamKind::Float {
                        min: 1.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_RADIUS),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "rays",
                    ParamKind::Int { min: 4, max: 64 },
                    ParamValue::Int(DEFAULT_RAYS),
                ),
            ],
        }
    }

    /// Reads the real slope scale (world height and extent) and interprets `radius` in world units;
    /// the sea level never invalidates this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::SLOPE
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let width = input.width();
        let height = input.height();
        let h = input.layer_or(layers::HEIGHT, 0.0);

        let radius_m = params.get_f64("radius", DEFAULT_RADIUS).max(0.0);
        let rays = params.get_i64("rays", DEFAULT_RAYS).clamp(4, 64) as usize;

        // A per-cell height delta becomes a true slope through this scale, so the horizon angle is a
        // real terrain angle. The march distance in cells comes from the world-unit radius, so the
        // reach is the same physical distance at any resolution. Bound the march by the grid so a huge
        // radius cannot run past the field.
        let scale = ctx.real_slope_scale() as f32;
        let max_cells = (ctx.world_to_cells(radius_m).round() as usize).clamp(1, width.max(height));

        // The ray directions (unit vectors), evenly spaced around the compass, precomputed once.
        let dirs: Vec<(f32, f32)> = (0..rays)
            .map(|k| {
                let (s, c) = (std::f32::consts::TAU * k as f32 / rays as f32).sin_cos();
                (c, s)
            })
            .collect();

        // Per cell and pure (reads only the immutable height), so `from_par_fn` is byte-identical at
        // any thread count.
        let occlusion = Layer::from_par_fn(width, height, |x, y| {
            let hc = h.get(x, y).unwrap_or(0.0);
            let mut sum = 0.0_f32;
            for &(dx, dy) in &dirs {
                // The steepest upward angle to any sample along this ray (tan form; never below flat).
                let mut horizon = 0.0_f32;
                for step in 1..=max_cells {
                    let s = step as f32;
                    let (sx, sy) = ((x as f32 + dx * s).round(), (y as f32 + dy * s).round());
                    if sx < 0.0 || sy < 0.0 || sx >= width as f32 || sy >= height as f32 {
                        break; // the ray left the grid
                    }
                    let sh = h.get(sx as usize, sy as usize).unwrap_or(0.0);
                    let slope = (sh - hc) * scale / s;
                    if slope > horizon {
                        horizon = slope;
                    }
                }
                // sin(atan(horizon)): the fraction of this direction's vertical sky slice blocked.
                sum += horizon / (1.0 + horizon * horizon).sqrt();
            }
            (sum / rays as f32).clamp(0.0, 1.0)
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(occlusion));
        Ok(vec![out])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Occlusion) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::Region;

    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0)
    }

    fn select(input: &Field, radius: f64, rays: i64) -> Field {
        let params = Params::new()
            .with("radius", ParamValue::Float(radius))
            .with("rays", ParamValue::Int(rays));
        Occlusion
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

    fn total(field: &Field) -> f32 {
        let layer = field.layer(layers::HEIGHT).unwrap();
        (0..layer.width())
            .flat_map(|x| (0..layer.height()).map(move |y| (x, y)))
            .map(|(x, y)| at(field, x, y))
            .sum()
    }

    fn filled(size: usize, v: f32) -> Field {
        Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(size, size, v)))
    }

    fn dist_from_centre(size: usize, x: usize, y: usize) -> f32 {
        let c = (size as f32 - 1.0) * 0.5;
        let (dx, dy) = (x as f32 - c, y as f32 - c);
        (dx * dx + dy * dy).sqrt() / c
    }

    /// A pit: height rises with distance from the centre, so the centre is hemmed in by higher ground.
    fn pit(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                dist_from_centre(size, x, y)
            })),
        )
    }

    /// A dome: height falls with distance, so the centre is an open peak.
    fn dome(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                1.0 - dist_from_centre(size, x, y)
            })),
        )
    }

    /// A basin: a flat floor within 40% of the radius, walls rising only beyond it. So a short reach
    /// from the centre samples only the flat floor, while a long reach sees the distant walls.
    fn basin(size: usize) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                (dist_from_centre(size, x, y) - 0.4).max(0.0)
            })),
        )
    }

    #[test]
    fn a_flat_field_is_open() {
        // Nothing rises above the horizon anywhere, so occlusion is zero everywhere.
        let out = select(&filled(32, 0.5), 1000.0, 16);
        assert!(total(&out) < 1e-3, "a flat field should read as fully open");
    }

    #[test]
    fn a_pit_centre_is_occluded_a_peak_is_open() {
        // The pit centre is ringed by higher ground (occluded); the dome centre is a peak with nothing
        // above it (open).
        let pit_c = at(&select(&pit(32), 1000.0, 16), 16, 16);
        let dome_c = at(&select(&dome(32), 1000.0, 16), 16, 16);
        assert!(
            pit_c > 0.4,
            "the pit centre should be strongly occluded: {pit_c}"
        );
        assert!(dome_c < 0.05, "the dome centre should be open: {dome_c}");
        assert!(pit_c > dome_c);
    }

    #[test]
    fn output_stays_in_range() {
        let out = select(&pit(24), 1000.0, 12);
        for x in 0..24 {
            for y in 0..24 {
                let v = at(&out, x, y);
                assert!((0.0..=1.0).contains(&v), "out of range at {x},{y}: {v}");
            }
        }
    }

    #[test]
    fn a_shorter_radius_sees_less_shelter() {
        // In a basin (flat floor, distant walls), a short reach from the centre samples only the flat
        // floor and reads open, while a long reach sees the surrounding walls and reads occluded.
        let far = at(&select(&basin(48), 1000.0, 16), 24, 24);
        let near = at(&select(&basin(48), 0.1, 16), 24, 24);
        assert!(
            far > near,
            "a longer radius should read more occluded: {far} vs {near}"
        );
        assert!(
            near < 0.05,
            "the short reach sees only the flat floor: {near}"
        );
    }

    #[test]
    fn passes_through_other_layers() {
        let mut input = pit(16);
        input.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.9)));
        let out = select(&input, 1000.0, 8);
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.9);
    }

    #[test]
    fn is_deterministic() {
        let input = pit(24);
        assert_eq!(
            select(&input, 500.0, 16).content_hash(),
            select(&input, 500.0, 16).content_hash()
        );
    }
}
