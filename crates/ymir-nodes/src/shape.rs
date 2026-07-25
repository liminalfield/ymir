//! Shared math for the Shape generator family (radial, gradient, ring, rect, ...).
//!
//! Every Shape generator draws a smooth `[0, 1]` control field over region
//! coordinates with one fixed smoothstep falloff, places itself at a normalized
//! center, and measures its reach in world units. The two pieces of math common to all
//! of them live here so the family stays one implementation: the falloff curve and the
//! normalized-center-to-cell mapping. See `design/shape-generators.md`.

use ymir_core::Region;

/// Smooth Hermite interpolation of `x` between `low` and `high`, clamped to `[0, 1]`.
/// `low == high` degrades to a hard step at that threshold rather than dividing by zero.
pub(crate) fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = if (high - low).abs() < 1e-9 {
        if x >= high { 1.0 } else { 0.0 }
    } else {
        ((x - low) / (high - low)).clamp(0.0, 1.0)
    };
    t * t * (3.0 - 2.0 * t)
}

/// Maps a normalized center (`[0, 1]` over the whole world, 0.5 = middle) to a cell
/// position in this region's grid. For an untiled build (region `UNIT`) this is just
/// `center * resolution`; for a tile it lands at the same ground as the untiled build,
/// so a shape sits on the same world position regardless of how the build is split.
pub(crate) fn center_cell(
    center: (f64, f64),
    region: Region,
    width: usize,
    height: usize,
) -> (f64, f64) {
    let (cx, cy) = center;
    let cell_x = (cx - region.min_x) / region.width() * width as f64;
    let cell_y = (cy - region.min_y) / region.height() * height as f64;
    (cell_x, cell_y)
}

/// Rotates `offset` by `angle` radians. To bring a cell offset into the local frame of a
/// shape rotated by `rotation`, pass `-rotation` (undo the shape's rotation). Shared by
/// the oriented shapes (rect, polygon) so the rotation math lives once.
pub(crate) fn rotate(offset: (f64, f64), angle: f64) -> (f64, f64) {
    let (x, y) = offset;
    let (s, c) = angle.sin_cos();
    (x * c - y * s, x * s + y * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_is_clamped_and_monotone() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 1.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
        // Strictly increasing across the band.
        assert!(smoothstep(0.0, 1.0, 0.25) < smoothstep(0.0, 1.0, 0.75));
    }

    #[test]
    fn smoothstep_degenerate_band_is_a_hard_step() {
        // low == high: a step at the threshold, no division by zero.
        assert_eq!(smoothstep(0.5, 0.5, 0.4), 0.0);
        assert_eq!(smoothstep(0.5, 0.5, 0.5), 1.0);
        assert_eq!(smoothstep(0.5, 0.5, 0.6), 1.0);
    }

    #[test]
    fn center_cell_maps_the_middle_to_the_middle() {
        // Untiled: the normalized middle is the grid middle.
        let (x, y) = center_cell((0.5, 0.5), Region::UNIT, 64, 64);
        assert_eq!((x, y), (32.0, 32.0));
        // The origin corner maps to cell 0.
        assert_eq!(center_cell((0.0, 0.0), Region::UNIT, 64, 64), (0.0, 0.0));
    }

    #[test]
    fn center_cell_accounts_for_the_region() {
        // A region covering the right half of the world: a world-center of 0.75 sits at
        // the middle of this tile's grid, and 0.5 sits at its left edge.
        let region = Region::new(0.5, 0.0, 1.0, 1.0);
        assert_eq!(center_cell((0.75, 0.0), region, 64, 64).0, 32.0);
        assert_eq!(center_cell((0.5, 0.0), region, 64, 64).0, 0.0);
    }

    #[test]
    fn rotate_turns_the_axes() {
        // +90 degrees sends the +x axis to +y (in the grid's y-down coordinates).
        let (x, y) = rotate((1.0, 0.0), std::f64::consts::FRAC_PI_2);
        assert!(x.abs() < 1e-9 && (y - 1.0).abs() < 1e-9);
        // Rotating by zero is the identity.
        assert_eq!(rotate((0.3, -0.7), 0.0), (0.3, -0.7));
    }
}
