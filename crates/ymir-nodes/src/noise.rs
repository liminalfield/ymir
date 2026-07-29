//! Coherent-noise terrain math, used by the fBm generator operator.
//!
//! A hand-rolled 2D simplex generator with fractional Brownian motion (fBm)
//! layering. Simplex uses a triangular lattice with no preferred axes, so it avoids
//! the horizontal/vertical/diagonal banding of a square-lattice (Perlin) basis. The
//! algorithm is specified here rather than pulled from a crate on purpose:
//! byte-identical output for a given seed is a core promise, and an external noise
//! crate does not contract to keep its output stable across versions. The same
//! reasoning drives the hand-rolled hashing in `ymir-core`.
//!
//! Gradients are derived by hashing integer lattice coordinates together with the
//! seed, so there is no permutation table and the function is fully stateless and
//! deterministic. Sampling is done in world coordinates derived from the
//! [`Region`], which is what makes the generator resolution-independent: raising
//! the resolution samples more of the same continuous function rather than
//! producing different terrain.
//!
//! Single-threaded for now: each cell is an independent pure function of its
//! coordinates, so `rayon` parallelism drops in unchanged once benchmarks justify
//! it.

use std::sync::Arc;

use ymir_core::{Field, Layer, Region, layers};

/// Parameters for fractional Brownian motion layering of simplex noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FbmParams {
    /// Base feature frequency: how many noise cycles span one unit of the region.
    pub frequency: f64,
    /// Number of summed octaves. More octaves add finer detail.
    pub octaves: u32,
    /// Frequency multiplier between successive octaves (typically 2.0).
    pub lacunarity: f64,
    /// Amplitude multiplier between successive octaves (typically 0.5).
    pub gain: f32,
    /// Pan of the sampling window along x, in region widths (0 = no pan): slides
    /// across the infinite field to sample a different region.
    pub offset_x: f64,
    /// Pan of the sampling window along y, in region heights.
    pub offset_y: f64,
}

impl Default for FbmParams {
    fn default() -> Self {
        Self {
            frequency: 2.0,
            octaves: 5,
            lacunarity: 2.0,
            gain: 0.5,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

/// Twelve unit gradient directions, evenly spaced around the circle (every 30 degrees),
/// selected per lattice point by the coordinate hash. More directions than a square
/// lattice's axes-and-diagonals set, which keeps the noise isotropic.
const SIMPLEX_GRADIENTS: [(f32, f32); 12] = [
    (1.0, 0.0),
    (0.866_025_4, 0.5),
    (0.5, 0.866_025_4),
    (0.0, 1.0),
    (-0.5, 0.866_025_4),
    (-0.866_025_4, 0.5),
    (-1.0, 0.0),
    (-0.866_025_4, -0.5),
    (-0.5, -0.866_025_4),
    (0.0, -1.0),
    (0.5, -0.866_025_4),
    (0.866_025_4, -0.5),
];

/// Skew factor for the 2D simplex grid: `0.5 * (sqrt(3) - 1)`.
const SIMPLEX_F2: f32 = 0.366_025_42;
/// Unskew factor for the 2D simplex grid: `(3 - sqrt(3)) / 6`.
const SIMPLEX_G2: f32 = 0.211_324_87;
/// Scales the summed corner contributions to roughly `[-1, 1]` for the unit gradients
/// above. The raw peak amplitude is about `0.00984` (measured over a dense grid), so a
/// factor near `101.7` fills the range; `100` leaves a small margin (and the field
/// mappers clamp regardless).
const SIMPLEX_SCALE: f32 = 100.0;

/// Generates a field whose `height` layer is fBm noise mapped to `[0, 1]`.
///
/// The noise is sampled across `region` in world space, so the same `seed`,
/// `params`, and `region` yield a consistent sampling of one continuous function
/// at any resolution.
pub(crate) fn fbm_field(
    width: usize,
    height: usize,
    region: Region,
    params: FbmParams,
    seed: u64,
) -> Field {
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Cell centre as a normalized position within the region, panned by the
        // offset (in region widths) so a different region of the field is sampled.
        let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
        let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        let n = fbm2(seed, wx as f32, wy as f32, params);
        // fBm is in roughly [-1, 1]; map to the nominal height range.
        (0.5 * n + 0.5).clamp(0.0, 1.0)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Generates a field whose `height` layer is ridged-multifractal noise in `[0, 1]`.
///
/// Shares the octave-layering parameters with [`fbm_field`] (frequency, octaves,
/// lacunarity, gain, offset); only the per-octave folding differs (sharp ridges instead
/// of a plain sum). Sampled across `region` in world space like the fBm path, so it is
/// resolution-independent.
pub(crate) fn ridged_field(
    width: usize,
    height: usize,
    region: Region,
    params: FbmParams,
    seed: u64,
) -> Field {
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Same world-space sampling as fbm_field, so the two generators register against
        // the same coordinates and stay resolution-independent.
        let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
        let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        ridged2(seed, wx as f32, wy as f32, params)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Generates a field whose `height` layer is billow noise in `[0, 1]`.
///
/// A sibling of [`fbm_field`] that folds each octave with `2|n| - 1` before summing, so
/// the noise's extremes become rounded bumps and its zero-crossings become creased
/// valleys: puffy mounds and dunes, the rounded inverse of the ridged fold (ridged points
/// up at crests, billow bulges round). Shares the octave-layering parameters and
/// world-space sampling with the fBm path, so it is resolution-independent.
pub(crate) fn billow_field(
    width: usize,
    height: usize,
    region: Region,
    params: FbmParams,
    seed: u64,
) -> Field {
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Same world-space sampling as fbm_field, so the generators register against the
        // same coordinates and stay resolution-independent.
        let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
        let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        let n = billow2(seed, wx as f32, wy as f32, params);
        // Billow is in roughly [-1, 1]; map to the nominal height range.
        (0.5 * n + 0.5).clamp(0.0, 1.0)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Generates a field whose `height` layer is hybrid-multifractal noise in `[0, 1]`.
///
/// Musgrave's hybrid multifractal: the contribution of each octave is weighted by the
/// terrain accumulated so far, so detail piles onto high ground while low ground stays
/// smooth and flat. The result is realistic plains-to-mountains terrain from one
/// generator, without hand-masking. `bias` (Musgrave's offset) lifts the field before the
/// weighting, setting how much of it reads as rough highland versus smooth lowland.
/// Shares the octave-layering parameters and world-space sampling, so it is
/// resolution-independent.
pub(crate) fn hybrid_field(
    width: usize,
    height: usize,
    region: Region,
    params: FbmParams,
    bias: f32,
    seed: u64,
) -> Field {
    let layer = Layer::from_par_fn(width, height, |x, y| {
        let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
        let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        hybrid2(seed, wx as f32, wy as f32, params, bias)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Samples raw fBm (roughly `[-1, 1]`) at the given coordinates, for callers that need
/// the signed noise directly rather than a mapped `[0, 1]` height field (a domain-warp
/// displacement, say). The base frequency is applied by the caller scaling the
/// coordinates, exactly as [`fbm_field`] does, so `params.frequency` is not read here.
pub(crate) fn fbm_sample(seed: u64, x: f32, y: f32, params: FbmParams) -> f32 {
    fbm2(seed, x, y, params)
}

/// The curl of the fBm potential at `(x, y)`: the divergence-free 2D vector
/// `(dphi/dy, -dphi/dx)`, estimated by central finite differences. It swirls like an
/// incompressible flow, with no sources or sinks. Shared by the flow generator and any
/// future direction-field consumer (a directional warp, erosion grain), so the curl math
/// lives once. Coordinates are pre-scaled by the caller, exactly as [`fbm_sample`] is.
pub(crate) fn curl2(seed: u64, x: f32, y: f32, params: FbmParams) -> (f32, f32) {
    // Finite-difference step in noise space: small enough to track the gradient, large
    // enough to avoid f32 cancellation.
    const EPS: f32 = 0.01;
    let dphi_dx = (fbm2(seed, x + EPS, y, params) - fbm2(seed, x - EPS, y, params)) / (2.0 * EPS);
    let dphi_dy = (fbm2(seed, x, y + EPS, params) - fbm2(seed, x, y - EPS, params)) / (2.0 * EPS);
    // Rotate the gradient 90 degrees: the divergence-free curl.
    (dphi_dy, -dphi_dx)
}

/// Decorrelates the warped base noise from the flow potential, so the swirling texture is
/// not just the potential warping itself.
const FLOW_BASE_SALT: u64 = 0x1357_9BDF_0246_8ACE;

/// Generates a field whose `height` layer is base noise warped along the curl-flow
/// streamlines of a potential (a swirly, marbled, fluid look), and whose `flow_x` /
/// `flow_y` layers carry the divergence-free flow vector for later direction-field
/// consumers. `strength` scales how far the lookup is displaced along the flow (0 is plain
/// fBm). Sampled in world space like the other generators, so it is
/// resolution-independent, and seeded deterministically.
pub(crate) fn flow_field(
    width: usize,
    height: usize,
    region: Region,
    params: FbmParams,
    strength: f32,
    seed: u64,
) -> Field {
    let base_seed = seed ^ FLOW_BASE_SALT;
    let mut h_buf = Vec::with_capacity(width * height);
    let mut fx_buf = Vec::with_capacity(width * height);
    let mut fy_buf = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
            let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
            let wx = ((region.min_x + u * region.width()) * params.frequency) as f32;
            let wy = ((region.min_y + v * region.height()) * params.frequency) as f32;

            let (vx, vy) = curl2(seed, wx, wy, params);
            // Warp the base-noise lookup along the flow vector.
            let n = fbm2(base_seed, wx + strength * vx, wy + strength * vy, params);
            h_buf.push((0.5 * n + 0.5).clamp(0.0, 1.0));
            fx_buf.push(vx);
            fy_buf.push(vy);
        }
    }

    Field::new(width, height, region)
        .with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_vec(width, height, h_buf)),
        )
        .with_layer(
            layers::FLOW_X,
            Arc::new(Layer::from_vec(width, height, fx_buf)),
        )
        .with_layer(
            layers::FLOW_Y,
            Arc::new(Layer::from_vec(width, height, fy_buf)),
        )
}

/// Which cellular (Worley) feature a field renders. The shared [`worley`] computation
/// yields all three; the feature only selects which result becomes the height, so the
/// three Cellular nodes share one implementation rather than recomputing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorleyFeature {
    /// `1 - F1`: cones peaking at each feature point (bumps, rock piles, scales).
    Bumps,
    /// `1 - (F2 - F1)`: the bright cell-edge network (cracks, fracture lines).
    Cracks,
    /// A flat random value per cell: discrete regions (plates, cell ids).
    Regions,
}

/// Where the feature points sit before jitter moves them.
///
/// Named `placement` rather than `lattice` because `design/scatter.md` calls the same idea
/// "Placement strategy", and Scatter will need to express arrangements that are not lattices at
/// all (random points, Poisson-disc spacing). Points cannot travel on an edge, so the two features
/// cannot share data, but they can share this vocabulary instead of coining rivals (#346).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// One point per square integer cell. Cells are squares at jitter 0.
    Square,
    /// A triangular lattice: alternate rows offset by half a cell, rows spaced `sqrt(3)/2` apart,
    /// so nearest-neighbour distance is 1 in every direction. Its Voronoi diagram is a grid of
    /// regular hexagons, which is what contraction cracks actually form: joints meet at 120
    /// degrees, so columnar basalt and dried mud are hexagon-dominated rather than square.
    Hex,
}

/// Row spacing of the triangular lattice, `sqrt(3)/2`.
const HEX_ROW: f32 = 0.866_025_4;

/// Parameters for cellular (Worley) noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorleyParams {
    /// Cell density: how many cells span one unit of the region.
    pub frequency: f64,
    /// How far each feature point wanders from its cell origin, in `[0, 1]` (0 is a
    /// regular grid, 1 fills the cell).
    pub jitter: f32,
    /// Pan of the sampling window along x, in region widths (0 = no pan): slides across the
    /// infinite field to place the cells differently, matching the fractal-noise offset.
    pub offset_x: f64,
    /// Pan of the sampling window along y, in region heights.
    pub offset_y: f64,
    /// Where the feature points sit before jitter moves them.
    pub placement: Placement,
}

/// Decorrelates the y feature-offset hash from the x one so points are not on a diagonal.
const WORLEY_SALT_Y: u64 = 0x2545_F491_4F6C_DD1D;
/// Decorrelates the per-cell region value from the feature-offset hashes.
const WORLEY_SALT_REGION: u64 = 0x9E37_79B9_7F4A_7C15;

/// Generates a field whose `height` layer is cellular (Worley) noise in `[0, 1]`, for the
/// chosen [`WorleyFeature`]. Sampled across `region` in world space like the fBm path, so
/// it is resolution-independent, and seeded deterministically through [`hash_coords`].
/// Where a [`WorleyFeature::Regions`] cell takes its value from.
///
/// Without one, a cell's value is a hash of its id: white noise, so neighbouring cells are
/// uncorrelated. With one, the cell samples this layer at its own feature point, so cells that sit
/// near each other take nearby values and the variation becomes spatially correlated (#347).
///
/// This cannot be done downstream. A later node sees only the finished raster and has no way to
/// tell which pixels belong to which cell, so it can remap values but never make neighbours agree.
#[derive(Clone, Copy)]
pub(crate) struct RegionValues<'a> {
    /// The layer to sample. One value per cell is read from it.
    pub layer: &'a Layer,
}

pub(crate) fn worley_field(
    width: usize,
    height: usize,
    region: Region,
    params: WorleyParams,
    feature: WorleyFeature,
    seed: u64,
    values: Option<RegionValues<'_>>,
) -> Field {
    let layer = Layer::from_par_fn(width, height, |x, y| {
        // Same world-space sampling as the other generators, so frequency sets cell size
        // and the field registers against the same coordinates at any resolution. The
        // offset pans the window (in region widths) exactly as the fractal-noise path does.
        let u = (x as f64 + 0.5) / width as f64 + params.offset_x;
        let v = (y as f64 + 0.5) / height as f64 + params.offset_y;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        let (f1, f2, cell) = worley(seed, wx as f32, wy as f32, params.jitter, params.placement);
        match feature {
            WorleyFeature::Bumps => (1.0 - f1).clamp(0.0, 1.0),
            WorleyFeature::Cracks => (1.0 - (f2 - f1)).clamp(0.0, 1.0),
            WorleyFeature::Regions => match values {
                // The cell reads one value from the wired layer, at its own feature point, so the
                // whole cell is flat at whatever that layer says there. Sampling the *feature
                // point* rather than the cell centre keeps it consistent with where the cell
                // actually sits once jitter has moved it.
                Some(values) => {
                    let (px, py) = feature_point(seed, cell, params.jitter, params.placement);
                    let (sx, sy) = noise_to_cell(px, py, width, height, region, params);
                    values.layer.get(sx, sy).unwrap_or(0.0)
                }
                None => hash_unit(seed ^ WORLEY_SALT_REGION, cell.0, cell.1),
            },
        }
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Cellular noise core: the distances to the nearest (`F1`) and second-nearest (`F2`)
/// feature points around `(x, y)` in noise space, and the integer cell of the nearest.
/// Each cell holds one feature point, jittered from its origin by a per-cell hash; only
/// the 3x3 neighborhood can hold the two nearest, so the search is bounded.
fn worley(seed: u64, x: f32, y: f32, jitter: f32, placement: Placement) -> (f32, f32, (i32, i32)) {
    let (ix, iy) = base_cell(x, y, placement);
    let mut f1 = f32::INFINITY;
    let mut f2 = f32::INFINITY;
    let mut nearest = (ix, iy);
    // A 3x3 index neighbourhood, which covers both lattices: no point outside it can be nearer
    // than one inside, because jitter never moves a point beyond its own cell.
    for dj in -1..=1 {
        for di in -1..=1 {
            let cx = ix + di;
            let cy = iy + dj;
            // Feature point: the lattice position plus a jittered, per-cell-deterministic
            // offset (independent hashes for x and y so points are not diagonal).
            let (px, py) = feature_point(seed, (cx, cy), jitter, placement);
            let d2 = (px - x).powi(2) + (py - y).powi(2);
            if d2 < f1 {
                f2 = f1;
                f1 = d2;
                nearest = (cx, cy);
            } else if d2 < f2 {
                f2 = d2;
            }
        }
    }
    (f1.sqrt(), f2.sqrt(), nearest)
}

/// The feature point of `cell` in noise space, recomputed from the same hashes [`worley`] uses.
///
/// Kept as a separate function rather than returned from `worley` because only the Regions feature
/// needs it, and the hot path should not pay for a value the other two features discard.
fn feature_point(seed: u64, cell: (i32, i32), jitter: f32, placement: Placement) -> (f32, f32) {
    let (cx, cy) = cell;
    match placement {
        Placement::Square => (
            cx as f32 + jitter * hash_unit(seed, cx, cy),
            cy as f32 + jitter * hash_unit(seed ^ WORLEY_SALT_Y, cx, cy),
        ),
        // Jitter is centred here, unlike the square case where it only ever pushes a point in the
        // positive direction. The square form is locked by backward compatibility; for a lattice
        // whose points sit at cell *centres* rather than corners, a centred offset is what keeps
        // the point inside its own hexagon. Scaled per axis so jitter 1 reaches the cell's extent
        // in both directions rather than overshooting in y.
        Placement::Hex => {
            let row_offset = if cy & 1 == 0 { 0.0 } else { 0.5 };
            (
                cx as f32 + row_offset + jitter * (hash_unit(seed, cx, cy) - 0.5),
                cy as f32 * HEX_ROW
                    + jitter * (hash_unit(seed ^ WORLEY_SALT_Y, cx, cy) - 0.5) * HEX_ROW,
            )
        }
    }
}

/// The lattice index nearest `(x, y)` in noise space: the search centre for [`worley`].
fn base_cell(x: f32, y: f32, placement: Placement) -> (i32, i32) {
    match placement {
        // The cell containing the point, since square lattice points sit at cell corners.
        Placement::Square => (x.floor() as i32, y.floor() as i32),
        // The nearest lattice point, since hex points sit at cell centres. The row is found first
        // because the half-cell offset applied to x depends on which row it is.
        Placement::Hex => {
            let iy = (y / HEX_ROW).round() as i32;
            let row_offset = if iy & 1 == 0 { 0.0 } else { 0.5 };
            ((x - row_offset).round() as i32, iy)
        }
    }
}

/// Maps a noise-space position back to the grid cell it falls in, inverting the sampling in
/// [`worley_field`]. Clamped to the grid, so a feature point just outside the field (cells on the
/// border own points beyond it) reads the nearest edge value rather than nothing.
fn noise_to_cell(
    px: f32,
    py: f32,
    width: usize,
    height: usize,
    region: Region,
    params: WorleyParams,
) -> (usize, usize) {
    // Guard the divisors: a zero-width region or zero frequency would otherwise produce a NaN
    // index, which `as usize` turns into 0 silently.
    let frequency = if params.frequency.abs() < f64::EPSILON {
        f64::EPSILON
    } else {
        params.frequency
    };
    let axis = |p: f32, min: f64, span: f64, offset: f64, n: usize| -> usize {
        let span = if span.abs() < f64::EPSILON {
            f64::EPSILON
        } else {
            span
        };
        let u = (f64::from(p) / frequency - min) / span - offset;
        let idx = u * n as f64 - 0.5;
        // `clamp` before the cast: an out-of-range float cast saturates, but doing it explicitly
        // keeps the intent readable and the bound obvious.
        idx.round().clamp(0.0, (n - 1) as f64) as usize
    };
    (
        axis(px, region.min_x, region.width(), params.offset_x, width),
        axis(py, region.min_y, region.height(), params.offset_y, height),
    )
}

/// A deterministic value in `[0, 1)` for an integer cell, from the coordinate hash. Uses
/// 24 bits so the fraction is exact in `f32`, keeping it byte-identical across machines.
fn hash_unit(seed: u64, ix: i32, iy: i32) -> f32 {
    (hash_coords(seed, ix, iy) & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// Sums simplex octaves, returning a value in roughly `[-1, 1]`.
fn fbm2(seed: u64, x: f32, y: f32, params: FbmParams) -> f32 {
    let mut frequency = 1.0_f32;
    let mut amplitude = 1.0_f32;
    let mut sum = 0.0_f32;
    let mut total_amplitude = 0.0_f32;

    for octave in 0..params.octaves {
        // Decorrelate octaves with a per-octave seed salt so higher octaves are
        // not just a scaled copy of the base layer.
        let octave_seed = seed ^ 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(u64::from(octave) + 1);
        sum += amplitude * simplex2(octave_seed, x * frequency, y * frequency);
        total_amplitude += amplitude;
        frequency *= params.lacunarity as f32;
        amplitude *= params.gain;
    }

    if total_amplitude > 0.0 {
        sum / total_amplitude
    } else {
        0.0
    }
}

/// Sums ridged-multifractal octaves, returning a value in `[0, 1]`.
///
/// Each octave folds the simplex value to a ridge (`1 - |n|`, squared to sharpen the
/// crest), so the noise's zero-crossings become sharp ridgelines and its extremes become
/// valleys. The running `weight`, driven by the coarser octaves, then suppresses each
/// finer octave in the valleys, so detail concentrates on the ridges: the characteristic
/// eroded-mountain look (Musgrave's multifractal weighting). Normalized by the total
/// amplitude so the result fills `[0, 1]`.
fn ridged2(seed: u64, x: f32, y: f32, params: FbmParams) -> f32 {
    let mut frequency = 1.0_f32;
    let mut amplitude = 1.0_f32;
    let mut sum = 0.0_f32;
    let mut total_amplitude = 0.0_f32;
    let mut weight = 1.0_f32;

    for octave in 0..params.octaves {
        // Decorrelate octaves with the same per-octave seed salt as the fBm path.
        let octave_seed = seed ^ 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(u64::from(octave) + 1);
        let n = simplex2(octave_seed, x * frequency, y * frequency);
        // Fold to a ridge (high at the zero-crossing, zero at the extremes) and sharpen.
        let mut ridge = 1.0 - n.abs();
        ridge *= ridge;
        // Multifractal weighting: this octave contributes only where the coarser octaves
        // are already high, so finer detail rides on the established ridges.
        ridge *= weight;
        weight = (ridge * 2.0).clamp(0.0, 1.0);

        sum += ridge * amplitude;
        total_amplitude += amplitude;
        frequency *= params.lacunarity as f32;
        amplitude *= params.gain;
    }

    if total_amplitude > 0.0 {
        (sum / total_amplitude).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Sums billow octaves, returning a value in roughly `[-1, 1]`.
///
/// Each octave folds the simplex value with `2|n| - 1`, which sends the noise's extremes
/// to `+1` (rounded bumps) and its zero-crossings to `-1` (creased valleys), so the sum
/// reads as puffy mounds rather than the symmetric ripples of plain fBm. Normalized by the
/// total amplitude, identical otherwise to the fBm octave loop.
fn billow2(seed: u64, x: f32, y: f32, params: FbmParams) -> f32 {
    let mut frequency = 1.0_f32;
    let mut amplitude = 1.0_f32;
    let mut sum = 0.0_f32;
    let mut total_amplitude = 0.0_f32;

    for octave in 0..params.octaves {
        // Same per-octave seed salt as the fBm and ridged paths.
        let octave_seed = seed ^ 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(u64::from(octave) + 1);
        let n = simplex2(octave_seed, x * frequency, y * frequency);
        sum += amplitude * (2.0 * n.abs() - 1.0);
        total_amplitude += amplitude;
        frequency *= params.lacunarity as f32;
        amplitude *= params.gain;
    }

    if total_amplitude > 0.0 {
        sum / total_amplitude
    } else {
        0.0
    }
}

/// Hybrid multifractal at `(x, y)`, mapped to `[0, 1]`.
///
/// Each octave's `(noise + bias)` contribution is scaled by `weight`, the product of the
/// coarser octaves so far (clamped to 1). Where the terrain is already high the weight
/// stays near 1 and detail accumulates (rough peaks); where it is low the weight collapses
/// toward 0 and finer octaves barely register (smooth, flat valleys). The raw value is
/// divided by its analytic envelope `(1 + bias) / (1 - gain)` and clamped, so the field
/// fills roughly `[0, 1]` while keeping the plains-to-peaks distribution.
fn hybrid2(seed: u64, x: f32, y: f32, params: FbmParams, bias: f32) -> f32 {
    let mut frequency = 1.0_f32;
    let mut amplitude = 1.0_f32;
    let mut result = 0.0_f32;
    let mut weight = 1.0_f32;

    for octave in 0..params.octaves {
        // Same per-octave seed salt as the other multifractal paths.
        let octave_seed = seed ^ 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(u64::from(octave) + 1);
        let signal = (simplex2(octave_seed, x * frequency, y * frequency) + bias) * amplitude;
        if octave == 0 {
            result = signal;
            weight = signal;
        } else {
            // The accumulated terrain gates this octave: high ground keeps the weight near
            // 1 (full detail), low ground drives it toward 0 (stays smooth).
            let w = weight.min(1.0);
            result += w * signal;
            weight = w * signal;
        }
        frequency *= params.lacunarity as f32;
        amplitude *= params.gain;
    }

    // Normalize by the octave envelope so the field fills roughly [0, 1] at any gain. The
    // envelope is the finite geometric sum of the per-octave amplitudes (sum of gain^k over
    // the octaves evaluated), the most the accumulated result can reach. The earlier closed
    // form (1 + bias) / (1 - gain) is this sum's infinite-octave limit, which diverges as
    // gain approaches 1 and so crushed high-gain fields into a sliver. The finite sum stays
    // bounded (tending to `octaves` at gain = 1), which decouples amplitude from gain: gain
    // now sets form (octave roughness) alone, leaving amplitude to a downstream control. A
    // single scalar divisor over the whole field, so the form is unchanged.
    let gain = params.gain;
    let octave_sum = if (1.0 - gain).abs() < 1e-6 {
        params.octaves as f32
    } else {
        (1.0 - gain.powi(params.octaves as i32)) / (1.0 - gain)
    };
    let envelope = (1.0 + bias) * octave_sum;
    (result / envelope).clamp(0.0, 1.0)
}

/// 2D simplex noise at `(x, y)`, returning a value in roughly `[-1, 1]`.
///
/// The plane is skewed onto a triangular lattice, the sample's simplex (triangle) is
/// found, and the three corners each contribute a gradient dotted with the distance to
/// them, weighted by a radial falloff. The triangular lattice has no preferred axes, so
/// the square-lattice banding of the former Perlin basis is gone. Gradients are still
/// selected by hashing the integer lattice coordinates with the seed, so the function is
/// stateless and deterministic (negative coordinates included, via `hash_coords`).
fn simplex2(seed: u64, x: f32, y: f32) -> f32 {
    // Skew the input to find the simplex cell origin (i, j).
    let s = (x + y) * SIMPLEX_F2;
    let i = (x + s).floor();
    let j = (y + s).floor();
    let ii = i as i32;
    let jj = j as i32;

    // Unskew the origin back to (x, y) space; (x0, y0) is the offset from the first corner.
    let t = (i + j) * SIMPLEX_G2;
    let x0 = x - (i - t);
    let y0 = y - (j - t);

    // The middle corner: the lower triangle steps in x first, the upper in y first.
    let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };

    // Offsets to the middle and last corners, unskewed.
    let x1 = x0 - i1 as f32 + SIMPLEX_G2;
    let y1 = y0 - j1 as f32 + SIMPLEX_G2;
    let x2 = x0 - 1.0 + 2.0 * SIMPLEX_G2;
    let y2 = y0 - 1.0 + 2.0 * SIMPLEX_G2;

    let n0 = corner(seed, ii, jj, x0, y0);
    let n1 = corner(seed, ii + i1, jj + j1, x1, y1);
    let n2 = corner(seed, ii + 1, jj + 1, x2, y2);

    SIMPLEX_SCALE * (n0 + n1 + n2)
}

/// One simplex corner's contribution: a radial falloff `(0.5 - d^2)^4` times the corner
/// gradient dotted with the distance vector to it. Beyond the falloff radius (`t <= 0`)
/// the corner contributes nothing.
fn corner(seed: u64, ix: i32, iy: i32, dx: f32, dy: f32) -> f32 {
    let t = 0.5 - dx * dx - dy * dy;
    if t <= 0.0 {
        0.0
    } else {
        let (gx, gy) = SIMPLEX_GRADIENTS[(hash_coords(seed, ix, iy) % 12) as usize];
        let t2 = t * t;
        t2 * t2 * (gx * dx + gy * dy)
    }
}

/// Mixes a seed and integer lattice coordinates into a well-distributed hash,
/// using the SplitMix64 finalizer. Deterministic for negative coordinates too,
/// since the cast goes through two's-complement.
fn hash_coords(seed: u64, ix: i32, iy: i32) -> u64 {
    const PRIME_X: u64 = 0x9E37_79B9_7F4A_7C15;
    const PRIME_Y: u64 = 0xC2B2_AE3D_27D4_EB4F;

    let mut h = seed;
    h ^= (ix as i64 as u64).wrapping_mul(PRIME_X);
    h ^= (iy as i64 as u64).wrapping_mul(PRIME_Y);
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fbm_field_is_deterministic() {
        let params = FbmParams::default();
        let a = fbm_field(64, 64, Region::UNIT, params, 42);
        let b = fbm_field(64, 64, Region::UNIT, params, 42);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn offset_pans_the_field() {
        // Panning the sampling window samples a different region, so the output
        // changes; a zero offset is the unchanged default (covered by the golden).
        let base = FbmParams::default();
        let shifted = FbmParams {
            offset_x: 1.5,
            ..base
        };
        let a = fbm_field(64, 64, Region::UNIT, base, 42);
        let b = fbm_field(64, 64, Region::UNIT, shifted, 42);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn different_seeds_give_different_output() {
        let params = FbmParams::default();
        let a = fbm_field(64, 64, Region::UNIT, params, 1);
        let b = fbm_field(64, 64, Region::UNIT, params, 2);
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn output_stays_in_unit_range() {
        let field = fbm_field(96, 96, Region::UNIT, FbmParams::default(), 7);
        let layer = field.layer(layers::HEIGHT).unwrap();
        for &value in layer.as_slice() {
            assert!((0.0..=1.0).contains(&value), "value {value} out of [0, 1]");
        }
    }

    #[test]
    fn output_is_not_constant() {
        let field = fbm_field(64, 64, Region::UNIT, FbmParams::default(), 7);
        let layer = field.layer(layers::HEIGHT).unwrap();
        let first = layer.as_slice()[0];
        assert!(
            layer.as_slice().iter().any(|&v| v != first),
            "noise should vary across the field"
        );
    }

    #[test]
    fn fbm_field_matches_golden_value() {
        // Fixed fingerprint, unchanged from before the math moved out of
        // ymir-core: this proves the relocation altered zero bytes of output.
        let field = fbm_field(8, 8, Region::UNIT, FbmParams::default(), 42);
        assert_eq!(field.content_hash().to_u64(), 0xb075_6620_1b58_4592);
    }

    fn worley_params() -> WorleyParams {
        WorleyParams {
            frequency: 8.0,
            jitter: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            placement: Placement::Square,
        }
    }

    #[test]
    fn worley_field_is_deterministic() {
        for feature in [
            WorleyFeature::Bumps,
            WorleyFeature::Cracks,
            WorleyFeature::Regions,
        ] {
            let a = worley_field(64, 64, Region::UNIT, worley_params(), feature, 42, None);
            let b = worley_field(64, 64, Region::UNIT, worley_params(), feature, 42, None);
            assert_eq!(a.content_hash(), b.content_hash(), "feature {feature:?}");
        }
    }

    #[test]
    fn worley_features_differ_and_stay_in_range() {
        let bumps = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Bumps,
            7,
            None,
        );
        let cracks = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Cracks,
            7,
            None,
        );
        let regions = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Regions,
            7,
            None,
        );
        // The three features render the same cells differently.
        assert_ne!(bumps.content_hash(), cracks.content_hash());
        assert_ne!(bumps.content_hash(), regions.content_hash());
        for field in [&bumps, &cracks, &regions] {
            let layer = field.layer(layers::HEIGHT).unwrap();
            for &v in layer.as_slice() {
                assert!((0.0..=1.0).contains(&v), "value {v} out of [0, 1]");
            }
        }
    }

    #[test]
    fn worley_jitter_changes_the_pattern() {
        // A regular grid (jitter 0) is not the jittered pattern (jitter 1).
        let grid = WorleyParams {
            frequency: 8.0,
            jitter: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
            placement: Placement::Square,
        };
        let a = worley_field(64, 64, Region::UNIT, grid, WorleyFeature::Bumps, 7, None);
        let b = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Bumps,
            7,
            None,
        );
        assert_ne!(a.content_hash(), b.content_hash());
    }

    /// The distance from each lattice point to its nearest neighbour, for a placement.
    fn nearest_neighbour_distances(placement: Placement) -> Vec<f32> {
        let mut out = Vec::new();
        for cy in -2..=2_i32 {
            for cx in -2..=2_i32 {
                let (px, py) = feature_point(0, (cx, cy), 0.0, placement);
                let mut best = f32::INFINITY;
                for dy in -2..=2_i32 {
                    for dx in -2..=2_i32 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (qx, qy) = feature_point(0, (cx + dx, cy + dy), 0.0, placement);
                        let d = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                        best = best.min(d);
                    }
                }
                out.push(best);
            }
        }
        out
    }

    #[test]
    fn hex_points_are_evenly_spaced_in_every_direction() {
        // The property that makes the Voronoi cells regular hexagons: every lattice point is the
        // same distance from its nearest neighbour whichever way you look. A square lattice is not
        // (1 along the axes, sqrt(2) diagonally), which is why its cells come out square.
        for d in nearest_neighbour_distances(Placement::Hex) {
            assert!(
                (d - 1.0).abs() < 1e-4,
                "hex nearest-neighbour distance {d}, expected 1.0"
            );
        }
    }

    #[test]
    fn a_hex_point_has_six_neighbours_at_that_distance() {
        // Six equidistant neighbours is what a hexagon needs; a square lattice has four.
        let count = |placement: Placement| {
            let (px, py) = feature_point(0, (0, 0), 0.0, placement);
            let mut n = 0;
            for dy in -2..=2_i32 {
                for dx in -2..=2_i32 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (qx, qy) = feature_point(0, (dx, dy), 0.0, placement);
                    let d = ((px - qx).powi(2) + (py - qy).powi(2)).sqrt();
                    if (d - 1.0).abs() < 1e-4 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert_eq!(count(Placement::Hex), 6, "hex");
        assert_eq!(count(Placement::Square), 4, "square");
    }

    #[test]
    fn hex_regions_have_six_sided_cells_on_average() {
        // The payoff, measured on the rendered field rather than the lattice: count how many
        // distinct neighbouring region values each cell touches. A jittered square lattice averages
        // below six; an unjittered hex lattice should sit at six.
        let mean_sides = |placement: Placement, jitter: f32| {
            let params = WorleyParams {
                frequency: 12.0,
                jitter,
                offset_x: 0.0,
                offset_y: 0.0,
                placement,
            };
            let field = worley_field(
                256,
                256,
                Region::UNIT,
                params,
                WorleyFeature::Regions,
                7,
                None,
            );
            let layer = field.layer_or(layers::HEIGHT, 0.0);
            // For each cell value, the set of other values it borders.
            let mut borders: std::collections::HashMap<u32, std::collections::HashSet<u32>> =
                std::collections::HashMap::new();
            for y in 1..255 {
                for x in 1..255 {
                    let here = layer.get(x, y).unwrap_or(0.0).to_bits();
                    for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
                        let other = layer.get(nx, ny).unwrap_or(0.0).to_bits();
                        if other != here {
                            borders.entry(here).or_default().insert(other);
                        }
                    }
                }
            }
            // A cell touching the image border has neighbours outside the field, so it counts
            // short however regular the lattice is. Drop any cell that reaches the margin: the
            // question is what the lattice does, not what cropping does to it.
            let margin = 24;
            let mut edge: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for y in 0..256 {
                for x in 0..256 {
                    if x < margin || y < margin || x >= 256 - margin || y >= 256 - margin {
                        edge.insert(layer.get(x, y).unwrap_or(0.0).to_bits());
                    }
                }
            }
            let kept: Vec<usize> = borders
                .iter()
                .filter(|(value, _)| !edge.contains(value))
                .map(|(_, set)| set.len())
                .collect();
            assert!(!kept.is_empty(), "no interior cells to measure");
            kept.iter().sum::<usize>() as f32 / kept.len() as f32
        };
        let hex = mean_sides(Placement::Hex, 0.0);
        assert!(
            (hex - 6.0).abs() < 0.4,
            "unjittered hex cells averaged {hex} sides, expected about 6"
        );
    }

    #[test]
    fn placement_changes_the_pattern_and_stays_deterministic() {
        let make = |placement: Placement| {
            let params = WorleyParams {
                frequency: 8.0,
                jitter: 0.5,
                offset_x: 0.0,
                offset_y: 0.0,
                placement,
            };
            worley_field(
                64,
                64,
                Region::UNIT,
                params,
                WorleyFeature::Regions,
                3,
                None,
            )
        };
        assert_ne!(
            make(Placement::Square).content_hash(),
            make(Placement::Hex).content_hash(),
            "hex must not reproduce the square pattern"
        );
        assert_eq!(
            make(Placement::Hex).content_hash(),
            make(Placement::Hex).content_hash(),
            "same inputs, same output"
        );
    }

    #[test]
    fn hex_jitter_still_disturbs_the_grid() {
        // The dial has to keep working on the new lattice: regular at 0, irregular as it rises.
        let make = |jitter: f32| {
            let params = WorleyParams {
                frequency: 8.0,
                jitter,
                offset_x: 0.0,
                offset_y: 0.0,
                placement: Placement::Hex,
            };
            worley_field(64, 64, Region::UNIT, params, WorleyFeature::Cracks, 3, None)
        };
        assert_ne!(make(0.0).content_hash(), make(0.9).content_hash());
    }
}
