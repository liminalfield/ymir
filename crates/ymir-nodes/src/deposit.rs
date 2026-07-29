//! Deposit: rains material onto the terrain and lets it settle (snow, sand).
//!
//! Material falls from above and comes to rest by its own rules, rather than being derived from the
//! terrain's erosional history the way `wear` and `deposition` are. Snow on a range, sand over a
//! desertifying landscape.
//!
//! # A mask decides thickness; settling decides a surface
//!
//! Every rule about *where* material lands already exists as a selector: Curvature for hollows,
//! Aspect for the lee side, Slope for flats, Height for an elevation band, Occlusion for sheltered
//! ground. Any of them can be wired into `mask`. So placement is not what this node is for.
//!
//! What a mask cannot do is settle. Adding a masked constant gives an even blanket that follows
//! every bump underneath. Real snow fills a hollow because its *top* is level, thick in the middle
//! and thin at the edges, and no mask said that. It sheds a steep face because it cannot hold the
//! angle, not because a threshold cut it off. It drifts against obstacles. Things stick up out of
//! sand because the sand settled to a level and the peaks were above it.
//!
//! # `repose` spans the whole range
//!
//! Near zero the material levels out and fills hollows like a liquid: the desertification case, with
//! peaks poking through. Around 34 degrees it behaves like sand and around 38 like snow, draping the
//! terrain, holding the flats and shedding the steep faces.
//!
//! # Material sits on the terrain, it does not move it
//!
//! Settling relaxes the *surface* (terrain plus what has landed on it), then holds the cover at or
//! above zero. So material slides down and off, while the rock underneath stays exactly where it
//! was. That is also why cover can vanish from a cliff: it slid off, which is what snow does.
//!
//! Iterative, so it carries the same contract the erosion nodes do: resolution-dependent physics,
//! where a low-resolution preview approximates the build rather than matching it. Passes scale with
//! resolution so the preview stays representative.

use rayon::prelude::*;
use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    ContextDeps, Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind,
    ParamSpec, ParamValue, Params, PortSpec, Result, Unit, layers,
};

use crate::talus;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.deposit";

/// Default fall depth in world units (metres): a covering, not a burial.
const DEFAULT_DEPTH: f64 = 8.0;
/// Default angle of repose in degrees. Snow sits around here; sand a little lower.
const DEFAULT_REPOSE: f64 = 36.0;
/// Default settling passes, at the reference resolution.
const DEFAULT_ITERATIONS: i64 = 30;
/// Default elevation above which material accumulates: zero, so it falls everywhere until a snow
/// line is asked for.
const DEFAULT_ELEVATION: f64 = 0.0;
/// Default softness of that line, in world units.
const DEFAULT_ELEVATION_FALLOFF: f64 = 100.0;
/// Default wind direction in degrees, matching the other directional nodes.
const DEFAULT_WIND_DIRECTION: f64 = 0.0;
/// Default wind bias: none, so wind costs nothing until it is asked for.
const DEFAULT_WIND_BIAS: f64 = 0.0;

/// The resolution the pass count is quoted at, matching thermal so the two agree.
const ITERATION_REFERENCE_RES: f64 = 256.0;
/// Fraction of the steepest excess moved per settling pass. Fixed rather than exposed: it trades
/// against `iterations` to reach the same resting surface, so two dials for one outcome would only
/// be a way to get it wrong.
const SHED_STRENGTH: f32 = 0.5;

/// Deposit modifier: one required input, one optional mask, two outputs.
#[derive(Clone)]
pub struct Deposit;

impl Operator for Deposit {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "filter",
            inputs: vec![PortSpec::new("in"), PortSpec::optional("mask")],
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("cover").selection(),
            ],
            params: vec![
                ParamSpec::new(
                    "depth",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_DEPTH),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "repose",
                    ParamKind::Float {
                        min: 0.0,
                        max: 89.0,
                    },
                    ParamValue::Float(DEFAULT_REPOSE),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "iterations",
                    ParamKind::Int { min: 0, max: 500 },
                    ParamValue::Int(DEFAULT_ITERATIONS),
                ),
                ParamSpec::new(
                    "elevation",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_ELEVATION),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "elevation_falloff",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_ELEVATION_FALLOFF),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "wind_direction",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(DEFAULT_WIND_DIRECTION),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "wind_bias",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(DEFAULT_WIND_BIAS),
                ),
            ],
            emitted_layers: vec![layers::COVER],
            mask_aware: true,
        }
    }

    /// Reads the world's vertical extent (to place a depth and a snow line in metres) and its
    /// horizontal extent (through the repose angle, which is a real slope), but not the sea level.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::SLOPE
    }

    /// Experimental: the settling is a local relaxation, so how far material travels is bounded by
    /// the pass count rather than by the material itself. A low repose needs far more passes than
    /// the defaults suggest, and too few strand it part-way as tongues that read steeper than the
    /// ground they fell on. The look is real and useful, but the pass count is doing work the user
    /// has to know about, so it is offered with a caveat rather than as a settled tool.
    fn experimental(&self) -> bool {
        true
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let bedrock = input.layer_or(layers::HEIGHT, 0.0);

        // The mask localizes the fall. An explicit mask input wins (its height layer is the
        // selection); with none, the input's own mask layer by convention; with neither, a uniform
        // 1.0. Soft-layer contract either way: the node never gates on a mask.
        let mask = match inputs.optional(0) {
            Some(field) => field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        // Depths are in metres; the height layer is normalized against the world's vertical extent,
        // so both convert through it. A zero-height world would divide by zero, so it falls back to
        // treating the layer as already normalized.
        let world_height = ctx.world_height();
        let to_normalized = if world_height.abs() < f64::EPSILON {
            1.0
        } else {
            1.0 / world_height
        };
        let depth = (params.get_f64("depth", DEFAULT_DEPTH).max(0.0) * to_normalized) as f32;
        let elevation = (params.get_f64("elevation", DEFAULT_ELEVATION) * to_normalized) as f32;
        let elevation_falloff = (params
            .get_f64("elevation_falloff", DEFAULT_ELEVATION_FALLOFF)
            .max(0.0)
            * to_normalized) as f32;

        // The repose angle as a per-cell normalized-height threshold, exactly as thermal reads its
        // talus: tan(angle) is the real slope, divided by the vertical-to-horizontal scale. Needed
        // before the fall as well as during the settling, because ground steeper than this cannot
        // hold material at all.
        let repose_deg = params.get_f64("repose", DEFAULT_REPOSE) as f32;
        let repose_per_cell = repose_deg.to_radians().tan() / ctx.real_slope_scale() as f32;

        let wind = params
            .get_f64("wind_direction", DEFAULT_WIND_DIRECTION)
            .to_radians();
        let (wind_x, wind_y) = (wind.cos() as f32, wind.sin() as f32);
        let wind_bias = params
            .get_f64("wind_bias", DEFAULT_WIND_BIAS)
            .clamp(0.0, 1.0) as f32;

        // How much lands on each cell, before any of it settles.
        let fall = Layer::from_par_fn(width, height, |x, y| {
            let here = bedrock.get(x, y).unwrap_or(0.0);
            let mut amount = depth * mask.get(x, y).unwrap_or(1.0);
            // The snow line: nothing below it, everything above, easing across the falloff.
            if elevation > 0.0 {
                amount *= smoothstep(elevation, elevation + elevation_falloff.max(1e-6), here);
            }
            // The local slope, in the same per-cell units as the repose threshold.
            let gx = 0.5
                * (neighbour(&bedrock, x + 1, y, here)
                    - neighbour(&bedrock, x.wrapping_sub(1), y, here));
            let gy = 0.5
                * (neighbour(&bedrock, x, y + 1, here)
                    - neighbour(&bedrock, x, y.wrapping_sub(1), here));
            let slope = (gx * gx + gy * gy).sqrt();

            // Ground steeper than the repose angle holds nothing: material that lands there is
            // already moving. Gating the *fall* rather than leaving it to the settling matters,
            // because relaxation is mass-conserving, so on a long uniform slope every cell receives
            // from the one above as fast as it sheds. Draining such a slope by relaxation alone
            // would need as many passes as the slope is cells long, and at any sane iteration count
            // snow would simply sit on a cliff.
            amount *= 1.0 - smoothstep(repose_per_cell * 0.8, repose_per_cell * 1.2, slope);

            // Wind piles material on the lee. A slope faces downhill, so the direction it faces is
            // the negated gradient; where that agrees with the way the wind blows, the cell is
            // sheltered behind the ground above it and collects more.
            if wind_bias > 0.0 && slope > 1e-9 {
                let lee = ((-gx / slope) * wind_x + (-gy / slope) * wind_y).clamp(-1.0, 1.0);
                amount *= 1.0 + wind_bias * lee;
            }
            amount.max(0.0)
        });

        let iterations = settling_passes(params, width);
        if iterations == 0 {
            // Nothing settles: the material lies where it fell. Still a valid answer, and the cheap
            // way to see the fall pattern on its own.
            return Ok(outputs(input, &bedrock, fall.as_slice(), width, height));
        }

        let pass = talus::Pass {
            width,
            height,
            talus_per_cell: repose_per_cell,
            strength: SHED_STRENGTH,
        };

        let cover = settle(&bedrock, fall.as_slice(), &pass, iterations, ctx)?;
        Ok(outputs(input, &bedrock, &cover, width, height))
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Deposit) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "hillslope", sort: 63 }
}

/// Settling passes for this resolution.
///
/// Material moves one cell per pass, so a finer grid needs proportionally more passes to relax the
/// same world distance. Scaled the same way thermal scales its own, so the two agree and a preview
/// stays representative of the build.
fn settling_passes(params: &Params, width: usize) -> usize {
    let base = params
        .get_i64("iterations", DEFAULT_ITERATIONS)
        .clamp(0, 100_000);
    if base <= 0 {
        return 0;
    }
    ((base as f64 * width as f64 / ITERATION_REFERENCE_RES).round() as i64).clamp(1, 1_000_000)
        as usize
}

/// A neighbour's height, falling back to `here` outside the grid so an edge cell reads as flat
/// rather than as a cliff.
fn neighbour(layer: &Layer, x: usize, y: usize, here: f32) -> f32 {
    layer.get(x, y).unwrap_or(here)
}

/// The classic smoothstep, for easing the elevation line.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Relaxes the fallen material until it holds the repose angle, returning the settled cover depth.
///
/// The relaxation runs on the **surface**, terrain plus cover, because that is the shape material
/// actually slides down. After each pass the cover is held at or above zero, which is what keeps the
/// terrain itself still: material can slide away to nothing, but nothing can dig into the rock. It
/// is also why a cliff ends up bare.
fn settle(
    bedrock: &Layer,
    fall: &[f32],
    pass: &talus::Pass,
    iterations: usize,
    ctx: &EvalContext,
) -> Result<Vec<f32>> {
    let rock = bedrock.as_slice();
    let mut cover: Vec<f32> = fall.to_vec();
    let mut surface: Vec<f32> = rock.iter().zip(&cover).map(|(r, c)| r + c).collect();
    let mut delta = vec![0.0_f32; surface.len()];
    let mut moved = vec![0.0_f32; surface.len()];
    let mut total_excess = vec![0.0_f32; surface.len()];

    for index in 0..iterations {
        // Settling is the slow part, so poll cancellation and report progress each pass, the way
        // the erosion nodes do; a superseded preview then aborts instead of running to the end.
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.report_progress(index as f32 / iterations as f32);
        talus::shed_phase(&surface, &mut moved, &mut total_excess, pass);
        // A cell can only shed what it actually has. Without this the surface slope of the rock
        // underneath decides how much moves, so on a face steeper than repose a bare cell "sheds"
        // a great deal, the shed is clamped away when its cover cannot go below zero, and the cell
        // below still receives it. That invents material out of the bedrock's shape and piles it
        // at the foot of every steep face.
        moved
            .par_iter_mut()
            .zip(cover.par_iter())
            .for_each(|(m, c)| *m = m.min(*c));
        talus::gather_phase(&surface, &moved, &total_excess, &mut delta, pass);
        // Each cell is independent, so the parallel update is byte-identical to a sequential one.
        surface
            .par_iter_mut()
            .zip(delta.par_iter())
            .zip(cover.par_iter_mut())
            .zip(rock.par_iter())
            .for_each(|(((s, d), c), r)| {
                // Material cannot dig into the rock: what would have gone below simply is not
                // there any more, which is how a steep face ends up bare.
                *c = (*s + *d - *r).max(0.0);
                *s = *r + *c;
            });
    }
    Ok(cover)
}

/// Builds the node's two outputs: the covered terrain, and the cover depth on its own.
fn outputs(
    input: &Field,
    bedrock: &Layer,
    cover: &[f32],
    width: usize,
    height: usize,
) -> Vec<Field> {
    let rock = bedrock.as_slice();
    let surface = Layer::from_fn(width, height, |x, y| {
        let i = y * width + x;
        rock.get(i).copied().unwrap_or(0.0) + cover.get(i).copied().unwrap_or(0.0)
    });
    let depth = Layer::from_fn(width, height, |x, y| {
        cover.get(y * width + x).copied().unwrap_or(0.0)
    });

    // The heightfield carries the cover as a layer too, so a downstream Material or mask can tell
    // covered ground from bare rock without re-deriving it.
    let mut covered = input.clone();
    covered.set_layer(layers::HEIGHT, Arc::new(surface));
    covered.set_layer(layers::COVER, Arc::new(depth.clone()));

    let mut just_cover = input.clone();
    just_cover.set_layer(layers::HEIGHT, Arc::new(depth));

    vec![covered, just_cover]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, registry};

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
            .with_world_extent(1024.0)
            .with_world_height(512.0)
    }

    /// Terrain from a closure over normalized coordinates.
    fn terrain(res: usize, f: impl Fn(f32, f32) -> f32 + Sync) -> Field {
        Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_par_fn(res, res, |x, y| {
                let n = (res - 1).max(1) as f32;
                f(x as f32 / n, y as f32 / n)
            })),
        )
    }

    fn run(input: &Field, params: &Params, ctx: &EvalContext) -> Vec<Field> {
        Deposit
            .eval(Inputs::required_only(&[input]), params, ctx)
            .expect("deposit evaluates")
    }

    fn cover_of(out: &[Field]) -> &Layer {
        // The second output is the depth on its own.
        out[1].layer(layers::HEIGHT).expect("cover layer")
    }

    fn base() -> Params {
        Params::default()
            .with("depth", ParamValue::Float(10.0))
            .with("iterations", ParamValue::Int(20))
    }

    #[test]
    fn spec_is_a_mask_aware_modifier() {
        let spec = Deposit.spec();
        assert_eq!(spec.type_id, TYPE_ID);
        assert_eq!(spec.kind(), ymir_core::NodeKind::Modifier);
        assert!(spec.mask_aware);
        assert!(spec.inputs[1].optional, "the mask is optional");
        assert_eq!(spec.outputs.len(), 2);
        assert_eq!(spec.emitted_layers, vec![layers::COVER]);
        // Badged in the editor: the settling's reach is bounded by the pass count, so the node is
        // offered with a caveat rather than as a settled tool.
        assert!(Deposit.experimental());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("registered");
        assert_eq!(made.spec().type_id, Deposit.spec().type_id);
    }

    #[test]
    fn the_terrain_underneath_is_never_moved() {
        // The defining property: material sits on the rock, it does not push it around. The
        // surface must be at or above the original everywhere, whatever the settling did.
        let input = terrain(64, |u, v| 0.3 + 0.4 * (u * 6.0).sin() * (v * 5.0).cos());
        let out = run(&input, &base(), &ctx(64));
        let before = input.layer_or(layers::HEIGHT, 0.0);
        let after = out[0].layer_or(layers::HEIGHT, 0.0);
        for y in 0..64 {
            for x in 0..64 {
                let (b, a) = (
                    before.get(x, y).unwrap_or(0.0),
                    after.get(x, y).unwrap_or(0.0),
                );
                assert!(a >= b - 1e-6, "cell ({x}, {y}) sank from {b} to {a}");
            }
        }
    }

    #[test]
    fn cover_is_the_difference_between_the_two_outputs() {
        // The `cover` output has to be exactly what was added, or a downstream mask built from it
        // would not line up with the terrain it is describing.
        let input = terrain(48, |u, _| 0.2 + 0.3 * u);
        let out = run(&input, &base(), &ctx(48));
        let before = input.layer_or(layers::HEIGHT, 0.0);
        let after = out[0].layer_or(layers::HEIGHT, 0.0);
        let cover = cover_of(&out);
        for y in 0..48 {
            for x in 0..48 {
                let expected = after.get(x, y).unwrap_or(0.0) - before.get(x, y).unwrap_or(0.0);
                let got = cover.get(x, y).unwrap_or(0.0);
                assert!((expected - got).abs() < 1e-5, "cell ({x}, {y})");
            }
        }
        // And the heightfield output carries it as a layer as well.
        assert!(out[0].layer(layers::COVER).is_some());
    }

    #[test]
    fn material_fills_a_hollow_to_a_level() {
        // The thing a mask cannot do. A low repose makes the material behave like a liquid, so the
        // hollow's floor comes up level rather than keeping the bowl's shape.
        let res = 64;
        // A bowl: low in the middle, high at the rim.
        let input = terrain(res, |u, v| {
            let (dx, dy) = (u - 0.5, v - 0.5);
            (dx * dx + dy * dy).sqrt().min(0.5) + 0.1
        });
        // Enough passes to actually carry the material across the bowl. Material moves one cell per
        // pass, so levelling a hollow tens of cells wide needs tens of passes; a low repose is the
        // expensive case for exactly that reason. This test used to pass at a fifth of this count,
        // but only because settling was inventing material at the foot of steep ground.
        let params = base()
            .with("depth", ParamValue::Float(30.0))
            .with("repose", ParamValue::Float(1.0))
            .with("iterations", ParamValue::Int(400));
        let out = run(&input, &params, &ctx(res));
        let after = out[0].layer_or(layers::HEIGHT, 0.0);
        let before = input.layer_or(layers::HEIGHT, 0.0);
        // Across the floor of the bowl the surface should be far flatter than the terrain was.
        let span = |l: &Layer| {
            let vals: Vec<f32> = (28..36)
                .flat_map(|y| (28..36).map(move |x| (x, y)))
                .map(|(x, y)| l.get(x, y).unwrap_or(0.0))
                .collect();
            vals.iter().cloned().fold(f32::MIN, f32::max)
                - vals.iter().cloned().fold(f32::MAX, f32::min)
        };
        let (was, now) = (span(&before), span(&after));
        assert!(
            now < was * 0.5,
            "the floor is still {now} deep against {was} before: material did not level"
        );
    }

    #[test]
    fn a_steep_face_sheds_more_than_flat_ground_keeps() {
        // Snow does not sit on a cliff. With a repose well below the terrain's own slope, the steep
        // half should end up with much less cover than the flat half.
        let res = 64;
        // Flat on the left, a steep ramp on the right.
        let input = terrain(
            res,
            |u, _| if u < 0.5 { 0.1 } else { 0.1 + (u - 0.5) * 1.6 },
        );
        let params = base()
            .with("repose", ParamValue::Float(5.0))
            .with("iterations", ParamValue::Int(60));
        let out = run(&input, &params, &ctx(res));
        let cover = cover_of(&out);
        let mean = |xs: std::ops::Range<usize>| {
            let mut sum = 0.0;
            let mut n = 0;
            for y in 8..res - 8 {
                for x in xs.clone() {
                    sum += cover.get(x, y).unwrap_or(0.0);
                    n += 1;
                }
            }
            sum / n as f32
        };
        let (flat, steep) = (mean(8..24), mean(40..56));
        assert!(
            steep < flat * 0.5,
            "steep ground kept {steep} against {flat} on the flat"
        );
    }

    #[test]
    fn the_elevation_line_keeps_material_off_low_ground() {
        // The snow line. Below it there should be nothing; above it, cover.
        let res = 64;
        let input = terrain(res, |u, _| u); // 0 to 1 across, so 0 to 512 m
        let params = base()
            .with("elevation", ParamValue::Float(300.0))
            .with("elevation_falloff", ParamValue::Float(20.0))
            .with("iterations", ParamValue::Int(0));
        let out = run(&input, &params, &ctx(res));
        let cover = cover_of(&out);
        // Well below the line: bare.
        assert!(cover.get(8, 32).unwrap_or(1.0) < 1e-6, "below the line");
        // Well above it: covered.
        assert!(cover.get(56, 32).unwrap_or(0.0) > 1e-4, "above the line");
    }

    #[test]
    fn wind_biases_the_lee_side() {
        // A ridge running north to south, wind blowing along +x. The downwind flank should collect
        // more than the upwind one.
        let res = 64;
        let input = terrain(res, |u, _| 0.2 + 0.3 * (1.0 - (u - 0.5).abs() * 2.0));
        let params = base()
            .with("wind_direction", ParamValue::Float(0.0))
            .with("wind_bias", ParamValue::Float(0.9))
            .with("iterations", ParamValue::Int(0));
        let out = run(&input, &params, &ctx(res));
        let cover = cover_of(&out);
        let (upwind, downwind) = (
            cover.get(16, 32).unwrap_or(0.0),
            cover.get(48, 32).unwrap_or(0.0),
        );
        assert!(
            downwind > upwind * 1.2,
            "downwind flank kept {downwind} against {upwind} upwind"
        );
    }

    #[test]
    fn a_mask_scopes_the_fall_and_absence_does_not_gate_it() {
        // The soft-layer contract: unwired means everywhere, wired means only there.
        let res = 32;
        let input = terrain(res, |_, _| 0.3);
        let everywhere = run(&input, &base(), &ctx(res));
        assert!(cover_of(&everywhere).get(4, 4).unwrap_or(0.0) > 0.0);

        let mask = Field::new(res, res, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                if x < res / 2 { 0.0 } else { 1.0 }
            })),
        );
        let scoped = Deposit
            .eval(
                Inputs::new(&[&input], &[Some(&mask)]),
                &base().with("iterations", ParamValue::Int(0)),
                &ctx(res),
            )
            .expect("deposit with a mask");
        let cover = cover_of(&scoped);
        assert!(cover.get(4, 16).unwrap_or(1.0) < 1e-6, "masked out");
        assert!(cover.get(28, 16).unwrap_or(0.0) > 1e-6, "masked in");
    }

    #[test]
    fn zero_depth_leaves_the_terrain_alone() {
        let input = terrain(32, |u, v| 0.2 + 0.3 * u * v);
        let out = run(
            &input,
            &base().with("depth", ParamValue::Float(0.0)),
            &ctx(32),
        );
        assert_eq!(
            out[0].layer_or(layers::HEIGHT, 0.0).content_hash(),
            input.layer_or(layers::HEIGHT, 0.0).content_hash()
        );
    }

    #[test]
    fn other_layers_pass_through() {
        let res = 16;
        let mut input = terrain(res, |_, _| 0.3);
        input.set_layer(layers::WEAR, Arc::new(Layer::filled(res, res, 0.42)));
        let out = run(&input, &base(), &ctx(res));
        let wear = out[0].layer(layers::WEAR).expect("wear passed through");
        assert!((wear.get(2, 2).unwrap_or(0.0) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn settling_never_invents_material() {
        // The bug this guards: `relax_pass` decides how much a cell sheds from the *surface*
        // slope, which on bedrock steeper than repose is large even where no material lies. The
        // shed was clamped away when cover could not go below zero, but the cell below still
        // received it, so material appeared out of the rock's shape and piled at the foot of every
        // steep face. Capping the shed by what a cell actually has fixed it.
        //
        // The invariant is one-sided: settling may lose material off the edge of the domain, but it
        // must never create any.
        let res = 96;
        // Steep, so the surface slope far exceeds repose and the phantom shed would be large.
        let input = terrain(res, |u, v| {
            0.1 + 0.8 * (u * 5.0).sin().abs() * (v * 4.0).cos().abs()
        });
        let params = Params::default()
            .with("depth", ParamValue::Float(20.0))
            .with("repose", ParamValue::Float(8.0))
            .with("iterations", ParamValue::Int(40));
        let out = run(&input, &params, &ctx(res));
        let settled: f32 = cover_of(&out).as_slice().iter().sum();

        // What fell, before any of it moved: the same node with the settling turned off.
        let fell: f32 = cover_of(&run(
            &input,
            &params.clone().with("iterations", ParamValue::Int(0)),
            &ctx(res),
        ))
        .as_slice()
        .iter()
        .sum();

        assert!(
            settled <= fell * 1.001,
            "settling turned {fell} of material into {settled}"
        );
    }

    #[test]
    fn steep_ground_is_left_bare_rather_than_gaining_a_drift() {
        // The visible face of the same bug: cover appearing where the ground is far too steep to
        // hold any.
        let res = 96;
        let input = terrain(res, |u, _| 0.05 + u * 0.9);
        let params = Params::default()
            .with("depth", ParamValue::Float(20.0))
            .with("repose", ParamValue::Float(6.0))
            .with("iterations", ParamValue::Int(40));
        let out = run(&input, &params, &ctx(res));
        let cover = cover_of(&out);
        // Away from the edges, where the ramp is uniformly far steeper than repose.
        for y in 20..res - 20 {
            for x in 20..res - 20 {
                let c = cover.get(x, y).unwrap_or(0.0);
                assert!(
                    c < 1e-4,
                    "cell ({x}, {y}) holds {c} on ground too steep for it"
                );
            }
        }
    }

    #[test]
    fn settling_is_same_machine_repeatable() {
        // An iterative node, so the contract is repeatability rather than cross-machine identity.
        let input = terrain(48, |u, v| 0.2 + 0.3 * (u * 4.0).sin() * (v * 3.0).cos());
        let a = run(&input, &base(), &ctx(48));
        let b = run(&input, &base(), &ctx(48));
        assert_eq!(a[0].content_hash(), b[0].content_hash());
        assert_eq!(a[1].content_hash(), b[1].content_hash());
    }
}
