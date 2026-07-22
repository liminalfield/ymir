//! Cursor-to-terrain picking for 3D-surface painting.
//!
//! Casts the cursor ray against the previewed heightfield and returns the normalized `[0, 1]`
//! surface position it hits, which is exactly the coordinate a paint stroke stores (the mask is a
//! horizontal-plane field, so only the horizontal hit matters; the height is discarded). Pure and
//! CPU-side: it reuses the same sampled height grid the mesh is built from, so it is deterministic
//! and unit-testable without a GPU.
//!
//! The world footprint is the unit square in XZ with Y up, and the mesh places grid cell `(i, j)`
//! at `(i/(res-1), height * vertical_scale, j/(res-1))`. So the surface height under a horizontal
//! point is the bilinear height there times the vertical scale, and a hit's `(x, z)` is the
//! normalized stroke position directly. The march tracks the sign of `ray.y - surface_y` and finds
//! the first crossing, which is sign-agnostic, so positive or negative exaggeration just flips the
//! surface without changing the logic.

use glam::{Mat4, Vec2, Vec3, Vec4};

/// Marching resolution: samples along the ray between the near and far planes. Ample for a preview
/// heightfield; the crossing is then refined by bisection, so this only needs to bracket it.
const MARCH_STEPS: usize = 512;
/// Bisection iterations once a crossing is bracketed: 24 halvings resolve the hit to well under a
/// cell at any preview resolution.
const BISECT_ITERS: usize = 24;

/// Casts the cursor ray against the heightfield and returns the normalized `[0, 1]` hit position,
/// or `None` when the ray misses the footprint.
///
/// `view_proj` is the camera's combined matrix (wgpu clip space, z in `[0, 1]`); `ndc` is the cursor
/// in normalized device coordinates (`[-1, 1]`, y up); `heights` is the `res * res` row-major
/// sampled height grid the mesh uses; `vertical_scale` is the exaggeration.
#[must_use]
pub(crate) fn raycast_heightfield(
    view_proj: Mat4,
    ndc: Vec2,
    heights: &[f32],
    res: usize,
    vertical_scale: f32,
) -> Option<(f32, f32)> {
    if res < 2 || heights.len() < res * res {
        return None;
    }
    let inv = view_proj.inverse();
    let unproject = |z: f32| -> Vec3 {
        let p = inv * Vec4::new(ndc.x, ndc.y, z, 1.0);
        p.truncate() / p.w
    };
    let near = unproject(0.0);
    let far = unproject(1.0);
    let span = far - near;
    let length = span.length();
    let dir = span.normalize_or_zero();
    if dir == Vec3::ZERO || length <= f32::EPSILON {
        return None;
    }

    // Surface height under a horizontal point, or `None` outside the unit footprint.
    let surface = |x: f32, z: f32| -> Option<f32> {
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&z) {
            None
        } else {
            Some(bilinear_grid(heights, res, x, z) * vertical_scale)
        }
    };

    let dt = length / MARCH_STEPS as f32;
    let mut prev_t = 0.0_f32;
    let mut prev_diff: Option<f32> = None;
    for step in 0..=MARCH_STEPS {
        let t = step as f32 * dt;
        let p = near + dir * t;
        match surface(p.x, p.z) {
            Some(surf) => {
                let diff = p.y - surf;
                if let Some(pd) = prev_diff
                    && pd.signum() != diff.signum()
                {
                    let hit_t = bisect(near, dir, prev_t, t, heights, res, vertical_scale);
                    let hit = near + dir * hit_t;
                    return Some((hit.x.clamp(0.0, 1.0), hit.z.clamp(0.0, 1.0)));
                }
                prev_diff = Some(diff);
            }
            // Outside the footprint: reset so a re-entry does not bridge a false crossing.
            None => prev_diff = None,
        }
        prev_t = t;
    }
    None
}

/// Refines the ray parameter of a surface crossing bracketed by `[t0, t1]` by bisection.
fn bisect(
    origin: Vec3,
    dir: Vec3,
    mut t0: f32,
    mut t1: f32,
    heights: &[f32],
    res: usize,
    vertical_scale: f32,
) -> f32 {
    let diff_at = |t: f32| {
        let p = origin + dir * t;
        let surf =
            bilinear_grid(heights, res, p.x.clamp(0.0, 1.0), p.z.clamp(0.0, 1.0)) * vertical_scale;
        p.y - surf
    };
    let d0 = diff_at(t0);
    for _ in 0..BISECT_ITERS {
        let mid = 0.5 * (t0 + t1);
        if diff_at(mid).signum() == d0.signum() {
            t0 = mid;
        } else {
            t1 = mid;
        }
    }
    0.5 * (t0 + t1)
}

/// Bilinearly samples the `res * res` height grid at normalized `(u, v)` in `[0, 1]`.
fn bilinear_grid(heights: &[f32], res: usize, u: f32, v: f32) -> f32 {
    let last = (res - 1) as f32;
    let fx = (u * last).clamp(0.0, last);
    let fy = (v * last).clamp(0.0, last);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(res - 1);
    let y1 = (y0 + 1).min(res - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let a = heights[y0 * res + x0];
    let b = heights[y0 * res + x1];
    let c = heights[y1 * res + x0];
    let d = heights[y1 * res + x1];
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    top + (bot - top) * ty
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view-projection looking straight down the −Y axis at the footprint centre, so screen and
    /// terrain axes align: NDC x → world x, NDC y → world z.
    fn top_down() -> Mat4 {
        let eye = Vec3::new(0.5, 3.0, 0.5);
        let target = Vec3::new(0.5, 0.0, 0.5);
        // Up along +Z so that NDC +y maps to world +z (screen up = terrain "north").
        let view = Mat4::look_at_rh(eye, target, Vec3::Z);
        let proj = Mat4::perspective_rh(45f32.to_radians(), 1.0, 0.1, 10.0);
        proj * view
    }

    #[test]
    fn centre_ray_hits_the_centre() {
        let heights = vec![0.3_f32; 16 * 16];
        let hit = raycast_heightfield(top_down(), Vec2::ZERO, &heights, 16, 1.0).unwrap();
        assert!(
            (hit.0 - 0.5).abs() < 1e-3 && (hit.1 - 0.5).abs() < 1e-3,
            "centre: {hit:?}"
        );
    }

    #[test]
    fn off_centre_ray_hits_off_centre_symmetrically() {
        // Opposite cursor offsets land symmetric about the footprint centre: NDC x moves the hit in
        // one axis, NDC y in the other, and mirrored cursors mirror the hit. (The exact sign depends
        // on the camera's up, so symmetry is the axis-agnostic invariant.)
        let heights = vec![0.0_f32; 16 * 16];
        let right =
            raycast_heightfield(top_down(), Vec2::new(0.2, 0.0), &heights, 16, 1.0).unwrap();
        let left =
            raycast_heightfield(top_down(), Vec2::new(-0.2, 0.0), &heights, 16, 1.0).unwrap();
        assert!(
            ((right.0 - 0.5) + (left.0 - 0.5)).abs() < 1e-3,
            "mirror in x: {right:?} {left:?}"
        );
        assert!(
            (right.1 - left.1).abs() < 1e-3,
            "same other axis: {right:?} {left:?}"
        );
        assert!(
            (right.0 - left.0).abs() > 0.1,
            "and actually off-centre: {right:?} {left:?}"
        );

        let up = raycast_heightfield(top_down(), Vec2::new(0.0, 0.2), &heights, 16, 1.0).unwrap();
        let down =
            raycast_heightfield(top_down(), Vec2::new(0.0, -0.2), &heights, 16, 1.0).unwrap();
        assert!(
            ((up.1 - 0.5) + (down.1 - 0.5)).abs() < 1e-3,
            "mirror in the other axis"
        );
        assert!((up.0 - down.0).abs() < 1e-3, "same first axis");
    }

    #[test]
    fn a_ray_off_the_footprint_misses() {
        // A cursor near the NDC corner points past the unit footprint from this framing.
        let heights = vec![0.0_f32; 16 * 16];
        let miss = raycast_heightfield(top_down(), Vec2::new(-0.99, -0.99), &heights, 16, 1.0);
        assert!(
            miss.is_none(),
            "a ray past the footprint returns None: {miss:?}"
        );
    }

    #[test]
    fn negative_exaggeration_still_hits() {
        // With the surface flipped below the datum, the centre ray still finds the crossing.
        let heights = vec![0.4_f32; 16 * 16];
        let hit = raycast_heightfield(top_down(), Vec2::ZERO, &heights, 16, -1.0).unwrap();
        assert!(
            (hit.0 - 0.5).abs() < 1e-3 && (hit.1 - 0.5).abs() < 1e-3,
            "flipped: {hit:?}"
        );
    }

    #[test]
    fn higher_terrain_is_hit_sooner_but_at_the_same_footprint() {
        // A taller flat surface is struck higher up the ray, yet the horizontal hit is unchanged.
        let low = vec![0.1_f32; 16 * 16];
        let high = vec![0.9_f32; 16 * 16];
        let a = raycast_heightfield(top_down(), Vec2::ZERO, &low, 16, 1.0).unwrap();
        let b = raycast_heightfield(top_down(), Vec2::ZERO, &high, 16, 1.0).unwrap();
        assert!(
            (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3,
            "same footprint: {a:?} {b:?}"
        );
    }
}
