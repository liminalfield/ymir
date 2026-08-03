//! Signed distance from a contour, by an eikonal solve, plus the `Distance` selector (#137).
//!
//! The substrate ([`signed_distance_to_contour`] over [`eikonal_solve`]) is shared terrain
//! math: the coastal model, a foam band, and the flow-map fix all want the distance from a
//! contour, and they must all get the *same* distance, or their results will not line up.
//!
//! It is a true eikonal solve (`|∇φ| = 1`, fast sweeping, Zhao 2005), not a chamfer or BFS
//! distance transform. A chamfer metric has its largest error at ±22.5 degrees, which on a
//! circular island renders beach width as a visible eight-lobed star. Fast sweeping is
//! isotropic and cheap: four alternating Gauss-Seidel sweeps to convergence, in a fixed order,
//! so the result is byte-identical on every machine.
//!
//! The boundary condition is seeded to *sub-cell* precision: where the input crosses the
//! contour between two cells, the straddling cells are initialised to the true fractional
//! distance to the crossing rather than to zero. Without this the field terraces in whole-cell
//! steps along a gently sloping contour, the artifact class the whole design avoids.
//!
//! Note: `hydrology.rs` has its own fast-sweeping eikonal solver for a different boundary
//! condition (geodesic, wall-respecting distance restricted to a drainage flat, seeded only at
//! `0`). This solver is a deliberately separate, general-grid variant with fractional sub-cell
//! seeding; the two are kept apart so the erosion path is never coupled to or disturbed by the
//! coastal work. The shared piece is only the standard Godunov update, a few lines either way.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

/// Solves the eikonal equation `|∇φ| = 1` on a `w × h` grid by fast sweeping. `init` holds each
/// cell's starting `φ` (seed cells at their known distance, every other cell `f32::INFINITY`);
/// `frozen` marks the seed cells, which hold their boundary value and are never updated.
/// `cell_size` is the world spacing between adjacent cells, so `φ` comes out in world units.
///
/// Four alternating sweeps (each axis ascending or descending), repeated until a full set changes
/// nothing. Sequential and in a fixed order, so it is deterministic and byte-identical everywhere.
/// A cell the front never reaches (no seed anywhere) stays `f32::INFINITY`.
pub(crate) fn eikonal_solve(
    init: &[f32],
    frozen: &[bool],
    w: usize,
    h: usize,
    cell_size: f32,
) -> Vec<f32> {
    debug_assert_eq!(init.len(), w * h);
    debug_assert_eq!(frozen.len(), w * h);
    let mut phi = init.to_vec();
    // Each axis ascending or descending: the four sweep directions of fast sweeping.
    let dirs = [(false, false), (true, false), (true, true), (false, true)];
    // φ decreases monotonically toward the solution, so a set of sweeps that changes nothing has
    // converged. `w + h` is a safe upper bound on the sweeps any front needs; the early break
    // means simple fronts cost one or two sets.
    for _ in 0..(w + h).max(1) {
        let mut changed = false;
        for &(rev_x, rev_y) in &dirs {
            for jj in 0..h {
                let y = if rev_y { h - 1 - jj } else { jj };
                for ii in 0..w {
                    let x = if rev_x { w - 1 - ii } else { ii };
                    let idx = y * w + x;
                    if frozen[idx] {
                        continue;
                    }
                    // Upwind neighbours: the smaller φ across each axis (off-grid is +∞).
                    let horiz = neighbour(&phi, x > 0, idx.wrapping_sub(1)).min(neighbour(
                        &phi,
                        x + 1 < w,
                        idx + 1,
                    ));
                    let vert = neighbour(&phi, y > 0, idx.wrapping_sub(w)).min(neighbour(
                        &phi,
                        y + 1 < h,
                        idx + w,
                    ));
                    let candidate = godunov(horiz, vert, cell_size);
                    if candidate < phi[idx] {
                        phi[idx] = candidate;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    phi
}

/// A neighbour's `φ` when `in_bounds`, else `+∞` so an off-grid side never contributes.
fn neighbour(phi: &[f32], in_bounds: bool, idx: usize) -> f32 {
    if in_bounds { phi[idx] } else { f32::INFINITY }
}

/// The Godunov upwind solution of `|∇φ| = 1` at a cell whose smaller horizontal and vertical
/// upwind neighbours are `a` and `b`, with grid spacing `h`. When the two disagree by at least
/// `h` the front is one-sided (`min + h`); otherwise both axes contribute. Returns `+∞` when no
/// upwind neighbour is finite yet.
fn godunov(a: f32, b: f32, h: f32) -> f32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    if !lo.is_finite() {
        return f32::INFINITY;
    }
    if hi - lo >= h {
        lo + h
    } else {
        0.5 * (lo + hi + (2.0 * h * h - (hi - lo) * (hi - lo)).sqrt())
    }
}

/// Signed distance (world units) from the zero contour of `layer - level`: negative where the
/// layer is below `level`, positive above, zero on the contour. The crossing is located to
/// sub-cell precision, so the field does not terrace on a gently sloping input. `cell_size` is the
/// world spacing between cells (e.g. [`EvalContext::meters_per_cell`]).
pub(crate) fn signed_distance_to_contour(layer: &Layer, level: f32, cell_size: f32) -> Layer {
    let (w, h) = (layer.width(), layer.height());
    let g: Vec<f32> = layer.as_slice().iter().map(|&v| v - level).collect();
    let mut init = vec![f32::INFINITY; w * h];
    let mut frozen = vec![false; w * h];

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let ga = g[idx];
            if ga == 0.0 {
                // Exactly on the contour.
                init[idx] = 0.0;
                frozen[idx] = true;
                continue;
            }
            let neighbours = [
                (x > 0).then(|| idx - 1),
                (x + 1 < w).then_some(idx + 1),
                (y > 0).then(|| idx - w),
                (y + 1 < h).then_some(idx + w),
            ];
            for nb in neighbours.into_iter().flatten() {
                let gb = g[nb];
                // A sign change across the edge (with gb off the contour) means the zero contour
                // crosses between the cells; seed this cell with the sub-cell distance to it.
                if gb != 0.0 && (ga > 0.0) != (gb > 0.0) {
                    let t = ga / (ga - gb); // fractional crossing position in (0, 1)
                    let d = t * cell_size;
                    if d < init[idx] {
                        init[idx] = d;
                    }
                    frozen[idx] = true;
                }
            }
        }
    }

    let dist = eikonal_solve(&init, &frozen, w, h, cell_size);
    // A field with no crossing anywhere leaves cells at +∞; report them as uniformly "far".
    let far = (w + h) as f32 * cell_size;
    let signed: Vec<f32> = dist
        .iter()
        .zip(&g)
        .map(|(&d, &gv)| {
            let magnitude = if d.is_finite() { d } else { far };
            if gv < 0.0 { -magnitude } else { magnitude }
        })
        .collect();
    Layer::from_vec(w, h, signed)
}

/// The below-`level` region of `g = height - level` that is connected to the map border, by a
/// 4-connected flood fill inward from the edges. `true` marks "open" cells; an enclosed below-level
/// basin is never reached from the border, so it reads `false`. Reachability is order-independent,
/// so this is deterministic.
fn flood_open_sea(g: &[f32], w: usize, h: usize) -> Vec<bool> {
    let mut open = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    // Seed from every below-level cell on the border (top/bottom rows, left/right columns). The
    // `!open` guard makes the corner cells, seeded twice, harmless.
    for x in 0..w {
        for idx in [x, (h - 1) * w + x] {
            if g[idx] < 0.0 && !open[idx] {
                open[idx] = true;
                stack.push(idx);
            }
        }
    }
    for y in 0..h {
        for idx in [y * w, y * w + (w - 1)] {
            if g[idx] < 0.0 && !open[idx] {
                open[idx] = true;
                stack.push(idx);
            }
        }
    }
    // Flood 4-connected through the below-level region (must match the 4-neighbour seeding below,
    // so an open cell and an enclosed cell are never adjacent).
    while let Some(idx) = stack.pop() {
        let (x, y) = (idx % w, idx / w);
        let neighbours = [
            (x > 0).then(|| idx - 1),
            (x + 1 < w).then_some(idx + 1),
            (y > 0).then(|| idx - w),
            (y + 1 < h).then_some(idx + w),
        ];
        for nb in neighbours.into_iter().flatten() {
            if g[nb] < 0.0 && !open[nb] {
                open[nb] = true;
                stack.push(nb);
            }
        }
    }
    open
}

/// Signed distance (world units) to the shoreline, like [`signed_distance_to_contour`] at the sea
/// level, but only the below-sea region **connected to the map border** counts as sea. An enclosed
/// below-sea basin (a dry pit, an inland depression) is treated as land: it seeds no shoreline and
/// reads a positive distance to the real, connected coast, so a coastal modifier leaves it alone.
///
/// Sub-cell shoreline precision is preserved: an open-sea/land edge is still a true `height`
/// crossing, so the seeding is fractional exactly as in [`signed_distance_to_contour`]. Only the
/// enclosed-basin edges are dropped. `cell_size` is the world spacing between cells.
pub(crate) fn sea_signed_distance(layer: &Layer, sea_level: f32, cell_size: f32) -> Layer {
    let (w, h) = (layer.width(), layer.height());
    let g: Vec<f32> = layer.as_slice().iter().map(|&v| v - sea_level).collect();
    let open = flood_open_sea(&g, w, h);
    let mut init = vec![f32::INFINITY; w * h];
    let mut frozen = vec![false; w * h];

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let ga = g[idx];
            // "Sea" for both sign and seeding means below the level *and* connected to the edge.
            let a_sea = ga < 0.0 && open[idx];
            let neighbours = [
                (x > 0).then(|| idx - 1),
                (x + 1 < w).then_some(idx + 1),
                (y > 0).then(|| idx - w),
                (y + 1 < h).then_some(idx + w),
            ];
            for nb in neighbours.into_iter().flatten() {
                let gb = g[nb];
                let b_sea = gb < 0.0 && open[nb];
                // Seed only where open sea meets land. Interior sea, interior land, and an enclosed
                // basin's rim (land next to a non-open below-sea cell) all have `a_sea == b_sea`
                // and are skipped, so no false shoreline forms around a basin.
                if a_sea == b_sea {
                    continue;
                }
                if gb != 0.0 && (ga > 0.0) != (gb > 0.0) {
                    // The open-sea side is g<0 and the land side g>0 (an open cell and an enclosed
                    // cell are never 4-adjacent), so the zero crossing sits between them; seed the
                    // sub-cell distance to it.
                    let t = ga / (ga - gb);
                    let d = t * cell_size;
                    if d < init[idx] {
                        init[idx] = d;
                    }
                    frozen[idx] = true;
                } else if ga == 0.0 {
                    // This cell lies exactly on the waterline beside open sea.
                    init[idx] = 0.0;
                    frozen[idx] = true;
                }
            }
        }
    }

    let dist = eikonal_solve(&init, &frozen, w, h, cell_size);
    // With no open sea anywhere (no real coast) every cell stays at +infinity; report them as
    // uniformly "far" and positive, so nothing is reshaped.
    let far = (w + h) as f32 * cell_size;
    let signed: Vec<f32> = dist
        .iter()
        .enumerate()
        .map(|(i, &d)| {
            let magnitude = if d.is_finite() { d } else { far };
            if g[i] < 0.0 && open[i] {
                -magnitude
            } else {
                magnitude
            }
        })
        .collect();
    Layer::from_vec(w, h, signed)
}

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.distance";

/// Which side of the contour the selection covers.
const SIDES: &[&str] = &["both", "outside", "inside"];
const SIDE_BOTH: &str = "both";
const SIDE_OUTSIDE: &str = "outside";
const SIDE_INSIDE: &str = "inside";

/// Where the contour is: a height the user names, or the world's sea level.
const FROMS: &[&str] = &[FROM_HEIGHT, FROM_SEA];
/// Take the contour from the `level` parameter. The original behaviour, and the default, so a
/// project saved before this choice existed reads the same contour it always did.
const FROM_HEIGHT: &str = "height";
/// Take the contour from the world's sea level, and treat only water connected to the map border as
/// sea. Not merely `level = sea_level`: an enclosed below-sea basin is land under this reading, so it
/// seeds no shoreline of its own and measures its distance to the real coast.
const FROM_SEA: &str = "sea";

/// Default contour level on the input height.
const DEFAULT_LEVEL: f64 = 128.0;
/// Default falloff distance (world metres) over which the selection fades from the contour.
const DEFAULT_RANGE: f64 = 100.0;

/// Distance selector: a `[0, 1]` proximity band around a height contour, on the `height` layer.
/// One near the contour, fading to zero `range` metres away, optionally gated to one side.
#[derive(Clone)]
pub struct Distance;

impl Operator for Distance {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "selector",
            inputs: vec![PortSpec::new("in")],
            outputs: vec![
                PortSpec::new("out").selection(),
                // The signed distance itself, in world metres: negative on the far side of the
                // contour, positive on the near side, zero on it. Not a selection, so it is not
                // marked as one: it is unbounded data, and clamping it to [0, 1] for display would
                // hide everything past a metre.
                //
                // This is the contour's own coordinate system, which is what makes it worth emitting
                // rather than recomputing downstream. The gradient is the contour normal and the
                // level sets run parallel to it, so any "distance from the shore" shaping becomes a
                // one-dimensional transfer function of this field with no tangent math anywhere.
                PortSpec::new("distance"),
            ],
            params: vec![
                ParamSpec::new(
                    "from",
                    ParamKind::Enum { options: FROMS },
                    ParamValue::Text(FROM_HEIGHT.to_string()),
                ),
                ParamSpec::new(
                    "level",
                    ParamKind::Float {
                        min: -100_000.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_LEVEL),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "range",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_RANGE),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "side",
                    ParamKind::Enum { options: SIDES },
                    ParamValue::Text(SIDE_BOTH.to_string()),
                ),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    /// Reads the world horizontal extent (a world-unit param) and the sea level, but not the world
    /// height.
    ///
    /// The sea level is only read when `from` is `sea`, but `context_deps` sees no parameters, so the
    /// union is declared. Over-declaring costs a needless invalidation when the sea-level slider
    /// moves and a `height` contour is selected; under-declaring would memoize a stale field, which
    /// is the failure that corrupts rather than the one that wastes.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        // World height joined the union when `level` became an elevation in metres (#377): it is
        // converted against the vertical scale, so a change there moves the contour.
        ymir_core::ContextDeps::ALL
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let h = input.layer_or(layers::HEIGHT, 0.0);

        // A zero range would divide by zero; clamp to a hair so it degrades to a hard edge.
        let range = params.get_f64("range", DEFAULT_RANGE).max(1e-6) as f32;
        let side = params.get_str("side", SIDE_BOTH);
        let cell_size = ctx.meters_per_cell() as f32;

        let signed = if params.get_str("from", FROM_HEIGHT) == FROM_SEA {
            sea_signed_distance(&h, ctx.sea_level() as f32, cell_size)
        } else {
            // An elevation in metres against a normalized layer, converted here (#377).
            let level = ctx.height_from_meters(params.get_f64("level", DEFAULT_LEVEL));
            signed_distance_to_contour(&h, level, cell_size)
        };
        let selection = Layer::from_fn(width, height, |x, y| {
            let d = signed.get(x, y).unwrap_or(0.0);
            let inside = d < 0.0;
            let gated = match side {
                SIDE_OUTSIDE => !inside,
                SIDE_INSIDE => inside,
                _ => true,
            };
            if gated {
                (1.0 - d.abs() / range).clamp(0.0, 1.0)
            } else {
                0.0
            }
        });

        let mut out = input.clone();
        out.set_layer(layers::HEIGHT, Arc::new(selection));

        // The signed distance as a field of its own, carried on `height` because that is the layer
        // every downstream node reads. It is metres, not a [0, 1] weight, so a consumer shaping by it
        // windows the range it cares about rather than assuming one.
        let mut raw = input.clone();
        raw.set_layer(layers::HEIGHT, Arc::new(signed));
        Ok(vec![out, raw])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Distance) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;
    use ymir_core::{NodeKind, Region};

    /// The distance from a single seed cell should match the true Euclidean distance, at any
    /// direction. This is the isotropy canary: a chamfer transform fails it (its error is
    /// direction-dependent, producing the eight-lobed star), an eikonal solve passes it.
    #[test]
    fn point_source_distance_is_euclidean_and_isotropic() {
        let (w, h) = (65, 65);
        let (cx, cy) = (32usize, 32usize);
        let mut init = vec![f32::INFINITY; w * h];
        let mut frozen = vec![false; w * h];
        init[cy * w + cx] = 0.0;
        frozen[cy * w + cx] = true;

        let phi = eikonal_solve(&init, &frozen, w, h, 1.0);
        let rel = |dx: usize, dy: usize| {
            let euclid = (((dx * dx + dy * dy) as f32).sqrt()).max(1e-6);
            (phi[(cy + dy) * w + (cx + dx)] - euclid).abs() / euclid
        };
        // First-order fast sweeping has a small, *smooth* error that peaks at the diagonal, unlike
        // a chamfer metric whose error peaks as a sharp crease near ±22.5 degrees. The axis is
        // near-exact; the diagonal is within a few percent (the codebase's other eikonal test uses
        // the same 5% bound); and the half-diagonal must not spike above the diagonal, which is the
        // no-star guarantee that a chamfer distance fails.
        let axis = rel(20, 0);
        let diag = rel(14, 14);
        let half = rel(18, 8); // ~24 degrees, where a chamfer metric is worst
        assert!(axis < 0.02, "axis error {axis}");
        assert!(diag < 0.05, "diagonal error {diag}");
        assert!(half < 0.04, "half-diagonal error {half}");
        assert!(
            half <= diag + 0.01,
            "half-diagonal must not spike above the diagonal (crease): half {half}, diag {diag}"
        );
    }

    /// A contour crossing between two cells seeds them with sub-cell distances, not zero, so a
    /// gently sloping input does not terrace. A horizontal ramp crossing `level` a quarter of the
    /// way between two columns puts the two straddling cells at 0.25 and 0.75 of a cell.
    #[test]
    fn sub_cell_seeding_is_fractional() {
        // Two cells: g = -0.25 and +0.75, so the zero crossing sits 0.25 of the way from the first.
        let layer = Layer::from_vec(2, 1, vec![0.25, 1.25]);
        let signed = signed_distance_to_contour(&layer, 0.5, 1.0);
        assert!(
            (signed.get(0, 0).unwrap() - (-0.25)).abs() < 1e-6,
            "left cell {} should be -0.25 of a cell",
            signed.get(0, 0).unwrap()
        );
        assert!(
            (signed.get(1, 0).unwrap() - 0.75).abs() < 1e-6,
            "right cell {} should be +0.75 of a cell",
            signed.get(1, 0).unwrap()
        );
    }

    /// The sign follows the input: below the level is negative, above is positive.
    #[test]
    fn distance_is_signed_by_side_of_the_contour() {
        let layer = Layer::from_vec(4, 1, vec![0.0, 0.4, 0.6, 1.0]);
        let signed = signed_distance_to_contour(&layer, 0.5, 1.0);
        assert!(signed.get(0, 0).unwrap() < 0.0, "0.0 is below the level");
        assert!(signed.get(1, 0).unwrap() < 0.0, "0.4 is below the level");
        assert!(signed.get(2, 0).unwrap() > 0.0, "0.6 is above the level");
        assert!(signed.get(3, 0).unwrap() > 0.0, "1.0 is above the level");
    }

    #[test]
    fn eikonal_solve_is_deterministic() {
        let (w, h) = (48, 48);
        let mut init = vec![f32::INFINITY; w * h];
        let mut frozen = vec![false; w * h];
        init[10 * w + 10] = 0.0;
        frozen[10 * w + 10] = true;
        init[30 * w + 40] = 0.0;
        frozen[30 * w + 40] = true;
        let a = eikonal_solve(&init, &frozen, w, h, 1.0);
        let b = eikonal_solve(&init, &frozen, w, h, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn sea_distance_excludes_enclosed_basins() {
        // A land plain (1.0), an ocean filling the left column (0.0, connected to the border), and
        // a single enclosed below-sea pit in the interior (0.0 at (3,3), ringed by land).
        let (w, h) = (7, 7);
        let mut data = vec![1.0_f32; w * h];
        for y in 0..h {
            data[y * w] = 0.0; // left column: open sea
        }
        data[3 * w + 3] = 0.0; // enclosed interior pit
        let layer = Layer::from_vec(w, h, data);
        let signed = sea_signed_distance(&layer, 0.5, 1.0);

        // The edge-connected ocean reads as offshore (negative).
        assert!(
            signed.get(0, 3).unwrap() < 0.0,
            "edge-connected sea should be offshore"
        );
        // The enclosed pit is treated as land: a positive distance to the real, far coast, not a
        // shoreline of its own.
        assert!(
            signed.get(3, 3).unwrap() > 0.0,
            "an enclosed basin should read as land, not sea: {}",
            signed.get(3, 3).unwrap()
        );
        // Land next to the real ocean is near the coast (small positive distance).
        assert!(signed.get(1, 3).unwrap() > 0.0 && signed.get(1, 3).unwrap() < 2.0);
    }

    #[test]
    fn sea_distance_is_deterministic() {
        let (w, h) = (16, 16);
        let mut data = vec![1.0_f32; w * h];
        for y in 0..h {
            data[y * w] = 0.0;
        }
        data[8 * w + 8] = 0.0;
        let layer = Layer::from_vec(w, h, data);
        assert_eq!(
            sea_signed_distance(&layer, 0.5, 1.0),
            sea_signed_distance(&layer, 0.5, 1.0)
        );
    }

    fn radial_island(size: usize) -> Field {
        // A cone: high at the centre, dropping below 0.5 (the default level) toward the edges, so
        // the 0.5 contour is a centred circle.
        let c = (size - 1) as f32 / 2.0;
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                let (dx, dy) = (x as f32 - c, y as f32 - c);
                let r = (dx * dx + dy * dy).sqrt() / c;
                (1.0 - r).clamp(0.0, 1.0)
            })),
        )
    }

    /// A world 256 m tall, so `level` in metres maps onto the `[0, 1]` fields these tests build:
    /// the default 128 m is the halfway contour (#377).
    const TEST_WORLD_HEIGHT: f64 = 256.0;

    fn run(input: &Field, params: &Params) -> Field {
        // world_extent = width makes meters_per_cell 1, so `range` reads in cells for the test.
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 0)
            .with_world_extent(input.width() as f64)
            .with_world_height(TEST_WORLD_HEIGHT);
        Distance
            .eval(Inputs::required_only(&[input]), params, &ctx)
            .unwrap()
            .remove(0)
    }

    /// Both outputs, for the tests that care about the second one.
    fn run_both(input: &Field, params: &Params, sea_level: f64) -> Vec<Field> {
        let ctx = EvalContext::new(input.width(), input.height(), input.region(), 0)
            .with_world_extent(input.width() as f64)
            .with_world_height(TEST_WORLD_HEIGHT)
            .with_sea_level(sea_level);
        Distance
            .eval(Inputs::required_only(&[input]), params, &ctx)
            .unwrap()
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn the_second_output_is_the_signed_distance_in_metres() {
        // The selection collapses the distance into a [0, 1] band and throws the sign away. The
        // second output is the field itself, which is what a profile shaped by distance needs: it has
        // to tell inland from offshore, and it has to reach past a metre.
        let island = radial_island(65);
        let out = run_both(&island, &Params::new(), 0.0);
        assert_eq!(out.len(), 2, "a selection and the raw distance");

        let raw = out[1].layer(layers::HEIGHT).unwrap();
        let centre = raw.get(32, 32).unwrap();
        let corner = raw.get(1, 1).unwrap();
        // Inside the contour and outside it carry opposite signs, which is the whole point.
        assert!(
            centre * corner < 0.0,
            "inside and outside must differ in sign, got {centre} and {corner}"
        );
        // And it is a real distance, not a weight: the island's centre is tens of cells from a
        // contour that a [0, 1] selection would have flattened to zero long before.
        assert!(
            centre.abs() > 4.0,
            "the centre is far from the contour, got {centre}"
        );
        // meters_per_cell is 1 here, so the value is directly comparable to the cell count.
        let expected = 32.0 - 65.0 * 0.3;
        assert!(
            (centre.abs() - expected).abs() < 3.0,
            "expected about {expected} from the contour, got {}",
            centre.abs()
        );
    }

    #[test]
    fn the_sea_contour_is_the_world_sea_level_and_ignores_enclosed_basins() {
        // `from = sea` is not just `level = sea_level`: only water reaching the map border counts, so
        // an enclosed hollow below the level measures its distance to the real coast instead of
        // seeding a shoreline of its own.
        let island = radial_island(65);
        let sea = Params::new().with("from", ParamValue::Text(FROM_SEA.to_string()));
        let by_sea = run_both(&island, &sea, 0.5);
        // The same contour named two ways. `sea_level` is a normalized height and `level` is an
        // elevation in metres (#377), so naming it by height means half of the world's vertical
        // scale, not the number 0.5.
        let by_height = run_both(
            &island,
            &Params::new().with("level", ParamValue::Float(TEST_WORLD_HEIGHT * 0.5)),
            0.0,
        );
        // Naming the same contour two ways agrees, since this island has no enclosed basin.
        let a = by_sea[1].layer(layers::HEIGHT).unwrap();
        let b = by_height[1].layer(layers::HEIGHT).unwrap();
        for (x, y) in [(32, 32), (10, 32), (32, 60), (5, 5)] {
            let (u, v) = (a.get(x, y).unwrap(), b.get(x, y).unwrap());
            assert!(
                (u - v).abs() < 1e-4,
                "cell ({x}, {y}) differs: {u} by sea, {v} by height"
            );
        }

        // And the sea reading follows the world setting rather than the `level` param, which it must
        // ignore entirely.
        let higher = run_both(&island, &sea, 0.7);
        let deeper = higher[1]
            .layer(layers::HEIGHT)
            .unwrap()
            .get(32, 32)
            .unwrap();
        assert!(
            deeper < a.get(32, 32).unwrap(),
            "a higher sea puts the shore further up the island, so the peak is nearer it"
        );
    }

    #[test]
    fn an_existing_project_reads_the_contour_it_always_did() {
        // `from` defaults to `height`, so a Distance node saved before the choice existed keeps its
        // `level` contour rather than silently jumping to the sea.
        let island = radial_island(65);
        let saved = Params::new().with("level", ParamValue::Float(0.5));
        let now = run_both(&island, &saved, 0.9);
        let explicit = run_both(
            &island,
            &saved
                .clone()
                .with("from", ParamValue::Text(FROM_HEIGHT.to_string())),
            0.9,
        );
        assert_eq!(
            now[0].content_hash(),
            explicit[0].content_hash(),
            "an absent `from` must mean `height`"
        );
    }

    #[test]
    fn selection_peaks_on_the_contour_and_fades_out() {
        let island = radial_island(65);
        let params = Params::new()
            .with("range", ParamValue::Float(10.0))
            .with("side", ParamValue::Text(SIDE_BOTH.to_string()));
        let out = run(&island, &params);
        // The centre is far above the contour, so its proximity is ~0; a cell near the circular
        // contour reads near 1.
        assert!(
            at(&out, 32, 32) < 0.2,
            "island peak should be far from the shore"
        );
        let near_contour = (0..65).map(|x| at(&out, x, 32)).fold(0.0_f32, f32::max);
        assert!(near_contour > 0.9, "a cell on the contour should select ~1");
    }

    #[test]
    fn side_gates_the_selection() {
        let layer = Layer::from_vec(4, 1, vec![0.0, 0.4, 0.6, 1.0]);
        let field = Field::new(4, 1, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer));
        let outside = run(
            &field,
            &Params::new()
                .with("range", ParamValue::Float(10.0))
                .with("side", ParamValue::Text(SIDE_OUTSIDE.to_string())),
        );
        // Outside = the above-level side, so the below-level cells are gated to zero.
        assert_eq!(at(&outside, 0, 0), 0.0);
        assert_eq!(at(&outside, 1, 0), 0.0);
        assert!(at(&outside, 2, 0) > 0.0);
        let inside = run(
            &field,
            &Params::new()
                .with("range", ParamValue::Float(10.0))
                .with("side", ParamValue::Text(SIDE_INSIDE.to_string())),
        );
        assert!(at(&inside, 1, 0) > 0.0);
        assert_eq!(at(&inside, 2, 0), 0.0);
    }

    #[test]
    fn passes_through_other_layers() {
        let mut field = radial_island(16);
        field.set_layer("flow", Arc::new(Layer::filled(16, 16, 0.7)));
        let out = run(&field, &Params::default());
        assert_eq!(out.layer("flow").unwrap().get(0, 0).unwrap(), 0.7);
    }

    #[test]
    fn is_deterministic() {
        let island = radial_island(32);
        assert_eq!(
            run(&island, &Params::default()).content_hash(),
            run(&island, &Params::default()).content_hash()
        );
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let island = radial_island(16);
        let made = registry::make(TYPE_ID).expect("distance operator is registered");
        let ctx = EvalContext::new(16, 16, island.region(), 0)
            .with_world_extent(16.0)
            .with_world_height(TEST_WORLD_HEIGHT);
        let via_registry = made
            .eval(Inputs::required_only(&[&island]), &Params::default(), &ctx)
            .unwrap();
        assert_eq!(
            via_registry[0].content_hash(),
            run(&island, &Params::default()).content_hash()
        );
    }

    #[test]
    fn spec_is_a_selector() {
        assert_eq!(Distance.spec().kind(), NodeKind::Modifier);
        assert_eq!(Distance.spec().category, "selector");
        assert_eq!(Distance.spec().type_id, TYPE_ID);
    }
}
