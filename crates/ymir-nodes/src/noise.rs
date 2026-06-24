//! Coherent-noise terrain math, used by the fBm generator operator.
//!
//! A hand-rolled 2D Perlin generator with fractional Brownian motion (fBm)
//! layering. The algorithm is specified here rather than pulled from a crate on
//! purpose: byte-identical output for a given seed is a core promise, and an
//! external noise crate does not contract to keep its output stable across
//! versions. The same reasoning drives the hand-rolled hashing in `ymir-core`.
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

/// Parameters for fractional Brownian motion layering of Perlin noise.
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

/// Eight unit gradient directions (axes plus diagonals), selected per lattice
/// point by the low bits of the coordinate hash.
const GRADIENTS: [(f32, f32); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (
        core::f32::consts::FRAC_1_SQRT_2,
        core::f32::consts::FRAC_1_SQRT_2,
    ),
    (
        -core::f32::consts::FRAC_1_SQRT_2,
        core::f32::consts::FRAC_1_SQRT_2,
    ),
    (
        core::f32::consts::FRAC_1_SQRT_2,
        -core::f32::consts::FRAC_1_SQRT_2,
    ),
    (
        -core::f32::consts::FRAC_1_SQRT_2,
        -core::f32::consts::FRAC_1_SQRT_2,
    ),
];

/// Generates a field whose `height` layer is fBm Perlin noise mapped to `[0, 1]`.
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
    let layer = Layer::from_fn(width, height, |x, y| {
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
    let layer = Layer::from_fn(width, height, |x, y| {
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
    let layer = Layer::from_fn(width, height, |x, y| {
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
    let layer = Layer::from_fn(width, height, |x, y| {
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

/// Parameters for cellular (Worley) noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorleyParams {
    /// Cell density: how many cells span one unit of the region.
    pub frequency: f64,
    /// How far each feature point wanders from its cell origin, in `[0, 1]` (0 is a
    /// regular grid, 1 fills the cell).
    pub jitter: f32,
}

/// Decorrelates the y feature-offset hash from the x one so points are not on a diagonal.
const WORLEY_SALT_Y: u64 = 0x2545_F491_4F6C_DD1D;
/// Decorrelates the per-cell region value from the feature-offset hashes.
const WORLEY_SALT_REGION: u64 = 0x9E37_79B9_7F4A_7C15;

/// Generates a field whose `height` layer is cellular (Worley) noise in `[0, 1]`, for the
/// chosen [`WorleyFeature`]. Sampled across `region` in world space like the fBm path, so
/// it is resolution-independent, and seeded deterministically through [`hash_coords`].
pub(crate) fn worley_field(
    width: usize,
    height: usize,
    region: Region,
    params: WorleyParams,
    feature: WorleyFeature,
    seed: u64,
) -> Field {
    let layer = Layer::from_fn(width, height, |x, y| {
        // Same world-space sampling as the other generators, so frequency sets cell size
        // and the field registers against the same coordinates at any resolution.
        let u = (x as f64 + 0.5) / width as f64;
        let v = (y as f64 + 0.5) / height as f64;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        let (f1, f2, cell) = worley(seed, wx as f32, wy as f32, params.jitter);
        match feature {
            WorleyFeature::Bumps => (1.0 - f1).clamp(0.0, 1.0),
            WorleyFeature::Cracks => (1.0 - (f2 - f1)).clamp(0.0, 1.0),
            WorleyFeature::Regions => hash_unit(seed ^ WORLEY_SALT_REGION, cell.0, cell.1),
        }
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Cellular noise core: the distances to the nearest (`F1`) and second-nearest (`F2`)
/// feature points around `(x, y)` in noise space, and the integer cell of the nearest.
/// Each cell holds one feature point, jittered from its origin by a per-cell hash; only
/// the 3x3 neighborhood can hold the two nearest, so the search is bounded.
fn worley(seed: u64, x: f32, y: f32, jitter: f32) -> (f32, f32, (i32, i32)) {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let mut f1 = f32::INFINITY;
    let mut f2 = f32::INFINITY;
    let mut nearest = (ix, iy);
    for dj in -1..=1 {
        for di in -1..=1 {
            let cx = ix + di;
            let cy = iy + dj;
            // Feature point: the cell origin plus a jittered, per-cell-deterministic
            // offset (independent hashes for x and y so points are not diagonal).
            let px = cx as f32 + jitter * hash_unit(seed, cx, cy);
            let py = cy as f32 + jitter * hash_unit(seed ^ WORLEY_SALT_Y, cx, cy);
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

/// A deterministic value in `[0, 1)` for an integer cell, from the coordinate hash. Uses
/// 24 bits so the fraction is exact in `f32`, keeping it byte-identical across machines.
fn hash_unit(seed: u64, ix: i32, iy: i32) -> f32 {
    (hash_coords(seed, ix, iy) & 0xFF_FFFF) as f32 / 16_777_216.0
}

/// Sums Perlin octaves, returning a value in roughly `[-1, 1]`.
fn fbm2(seed: u64, x: f32, y: f32, params: FbmParams) -> f32 {
    let mut frequency = 1.0_f32;
    let mut amplitude = 1.0_f32;
    let mut sum = 0.0_f32;
    let mut total_amplitude = 0.0_f32;

    for octave in 0..params.octaves {
        // Decorrelate octaves with a per-octave seed salt so higher octaves are
        // not just a scaled copy of the base layer.
        let octave_seed = seed ^ 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(u64::from(octave) + 1);
        sum += amplitude * perlin2(octave_seed, x * frequency, y * frequency);
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
/// Each octave folds the Perlin value to a ridge (`1 - |n|`, squared to sharpen the
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
        let n = perlin2(octave_seed, x * frequency, y * frequency);
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
/// Each octave folds the Perlin value with `2|n| - 1`, which sends the noise's extremes
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
        let n = perlin2(octave_seed, x * frequency, y * frequency);
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
        let signal = (perlin2(octave_seed, x * frequency, y * frequency) + bias) * amplitude;
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

    // Normalize by the envelope so the field fills roughly [0, 1]; the actual range still
    // varies with terrain, which is the point (auto-ranged at display and export).
    let envelope = (1.0 + bias) / (1.0 - params.gain).max(1e-3);
    (result / envelope).clamp(0.0, 1.0)
}

/// Improved Perlin noise at `(x, y)`, returning a value in roughly `[-1, 1]`.
fn perlin2(seed: u64, x: f32, y: f32) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let xi = x0 as i32;
    let yi = y0 as i32;

    // Position within the lattice cell.
    let fx = x - x0;
    let fy = y - y0;
    let u = fade(fx);
    let v = fade(fy);

    let n00 = dot_gradient(seed, xi, yi, fx, fy);
    let n10 = dot_gradient(seed, xi + 1, yi, fx - 1.0, fy);
    let n01 = dot_gradient(seed, xi, yi + 1, fx, fy - 1.0);
    let n11 = dot_gradient(seed, xi + 1, yi + 1, fx - 1.0, fy - 1.0);

    let nx0 = lerp(n00, n10, u);
    let nx1 = lerp(n01, n11, u);
    // 2D gradient noise spans about [-sqrt(2)/2, sqrt(2)/2]; scale to fill [-1, 1].
    lerp(nx0, nx1, v) * core::f32::consts::SQRT_2
}

/// Dot product of the lattice point's gradient with the distance vector to it.
fn dot_gradient(seed: u64, ix: i32, iy: i32, dx: f32, dy: f32) -> f32 {
    let (gx, gy) = GRADIENTS[(hash_coords(seed, ix, iy) & 7) as usize];
    gx * dx + gy * dy
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

/// Perlin's quintic fade curve `6t^5 - 15t^4 + 10t^3`.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Linear interpolation.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + t * (b - a)
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
        assert_eq!(field.content_hash().to_u64(), 0x6735_0dbf_a122_5544);
    }

    fn worley_params() -> WorleyParams {
        WorleyParams {
            frequency: 8.0,
            jitter: 1.0,
        }
    }

    #[test]
    fn worley_field_is_deterministic() {
        for feature in [
            WorleyFeature::Bumps,
            WorleyFeature::Cracks,
            WorleyFeature::Regions,
        ] {
            let a = worley_field(64, 64, Region::UNIT, worley_params(), feature, 42);
            let b = worley_field(64, 64, Region::UNIT, worley_params(), feature, 42);
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
        );
        let cracks = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Cracks,
            7,
        );
        let regions = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Regions,
            7,
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
        };
        let a = worley_field(64, 64, Region::UNIT, grid, WorleyFeature::Bumps, 7);
        let b = worley_field(
            64,
            64,
            Region::UNIT,
            worley_params(),
            WorleyFeature::Bumps,
            7,
        );
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
