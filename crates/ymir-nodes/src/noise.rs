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
}

impl Default for FbmParams {
    fn default() -> Self {
        Self {
            frequency: 2.0,
            octaves: 5,
            lacunarity: 2.0,
            gain: 0.5,
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
        // Cell centre as a normalized position within the region.
        let u = (x as f64 + 0.5) / width as f64;
        let v = (y as f64 + 0.5) / height as f64;
        let wx = (region.min_x + u * region.width()) * params.frequency;
        let wy = (region.min_y + v * region.height()) * params.frequency;

        let n = fbm2(seed, wx as f32, wy as f32, params);
        // fBm is in roughly [-1, 1]; map to the nominal height range.
        (0.5 * n + 0.5).clamp(0.0, 1.0)
    });

    Field::new(width, height, region).with_layer(layers::HEIGHT, Arc::new(layer))
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
}
