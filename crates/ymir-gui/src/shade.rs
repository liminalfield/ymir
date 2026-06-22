//! Field → image shading, shared by the 2D preview pane and the node thumbnails.
//!
//! Pure pixel work: every function here produces an [`egui::ColorImage`] from a
//! [`Field`]'s `height` layer with no GPU context, so it runs on a worker thread the
//! same way for the preview and for thumbnails. It renders a *layer*, never asking
//! "which node is this?", so the additive-node invariant holds.

use eframe::egui;
use ymir_core::{Field, layers};

/// How the height layer is shaded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadeMode {
    /// Height mapped to grayscale, auto-ranged to the field's actual extent so terrain
    /// that ran outside `[0, 1]` is shown in full rather than clipped (matching the
    /// export's auto-range).
    Height,
    /// Relief: each cell shaded by its surface normal under a fixed light, so height
    /// *changes* (slopes, carved valleys) are legible even when subtle (#40).
    Relief,
}

/// Default relief light: from the upper-left, partway up (a conventional NW
/// hillshade). `+x` is right, `+y` is down (image space). Pre-normalized. Steerable by
/// dragging over the relief image (#40).
pub(crate) const DEFAULT_LIGHT: [f32; 3] = [-0.5014, -0.6017, 0.6217];
/// Vertical exaggeration for relief, so subtle height changes (erosion) are legible.
const RELIEF_EXAGGERATION: f32 = 2.0;
/// Ambient term so slopes facing away from the light are dim, not pure black.
const RELIEF_AMBIENT: f32 = 0.25;

/// Maps a normalized height value to an 8-bit grayscale level, matching the PNG
/// export's mapping (clamp to `[0, 1]`, scale to `0..=255`, round).
fn gray8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Lambert shade in `[0, 1]` for a cell whose height gradient (per unit region) is
/// `(gx, gy)`, lit from `light` (a unit vector). Flat ground reads a mid-tone; slopes
/// facing the light brighten, those facing away darken. Pure: the normal/lambert math,
/// kept separate from rendering so it is unit-testable.
fn relief_shade(gx: f32, gy: f32, light: [f32; 3]) -> f32 {
    // Surface normal of the height field is (-gx, -gy, 1), normalized.
    let inv_len = 1.0 / (gx * gx + gy * gy + 1.0).sqrt();
    let n = [-gx * inv_len, -gy * inv_len, inv_len];
    let lambert = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
    RELIEF_AMBIENT + (1.0 - RELIEF_AMBIENT) * lambert
}

/// Builds an image from a field's `height` layer, in the chosen mode.
pub(crate) fn field_to_image(field: &Field, mode: ShadeMode, light: [f32; 3]) -> egui::ColorImage {
    match mode {
        ShadeMode::Height => height_image(field),
        ShadeMode::Relief => relief_image(field, light),
    }
}

/// Height mapped to grayscale, auto-ranged to the field's actual `[min, max]` so the
/// whole shape is visible even when height ran outside `[0, 1]`, rather than clipping
/// to a flat white/black. This mirrors the export's auto-range, so the preview reads
/// like the exported image. A flat field maps to a single tone.
pub(crate) fn height_image(field: &Field) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let (min, max) = layer.value_range();
    let span = max - min;
    let mut rgba = Vec::with_capacity(layer.len() * 4);
    for &value in layer.as_slice() {
        // Normalize into the display range; a zero-width span (a flat field) reads as a
        // single tone rather than dividing by zero.
        let t = if span > 0.0 {
            (value - min) / span
        } else {
            0.0
        };
        let g = gray8(t);
        rgba.extend_from_slice(&[g, g, g, 255]);
    }
    egui::ColorImage::from_rgba_unmultiplied([layer.width(), layer.height()], &rgba)
}

/// Relief (hillshade) image: each cell shaded by its surface normal. The gradient is
/// per unit region (central difference scaled by the cell count), so the shading
/// reads the same at any preview resolution.
fn relief_image(field: &Field, light: [f32; 3]) -> egui::ColorImage {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let (w, h) = (layer.width(), layer.height());
    let at = |x: usize, y: usize| layer.get(x, y).unwrap_or(0.0);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w.saturating_sub(1)));
            let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h.saturating_sub(1)));
            // d(height)/d(unit region) ≈ Δheight / (Δcells / cell_count), exaggerated.
            let gx =
                (at(xp, y) - at(xm, y)) * RELIEF_EXAGGERATION * w as f32 / (xp - xm).max(1) as f32;
            let gy =
                (at(x, yp) - at(x, ym)) * RELIEF_EXAGGERATION * h as f32 / (yp - ym).max(1) as f32;
            let s = gray8(relief_shade(gx, gy, light));
            rgba.extend_from_slice(&[s, s, s, 255]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{Layer, Region};

    fn height_field(values: &[f32]) -> Field {
        let n = values.len();
        Field::new(n, 1, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(n, 1, |x, _| values[x])),
        )
    }

    #[test]
    fn height_image_auto_ranges_to_the_field_extent() {
        // A compressed range [0.4, 0.6] is stretched across the display: the min reads
        // black and the max white, so the shape is visible rather than near-uniform gray.
        let img = height_image(&height_field(&[0.4, 0.6]));
        assert_eq!(img.pixels[0].r(), 0);
        assert_eq!(img.pixels[1].r(), 255);
    }

    #[test]
    fn height_image_shows_out_of_range_without_clipping() {
        // Values below 0 and above 1 are no longer clamped to flat black/white: the
        // extremes anchor the range and the middle stays distinct.
        let img = height_image(&height_field(&[-0.5, 0.5, 2.0]));
        assert_eq!(img.pixels[0].r(), 0); // -0.5 (min)
        assert_eq!(img.pixels[2].r(), 255); // 2.0 (max)
        let mid = img.pixels[1].r();
        assert!(mid > 0 && mid < 255, "middle clipped: {mid}");
    }

    #[test]
    fn height_image_of_a_flat_field_is_a_single_tone() {
        let img = height_image(&height_field(&[0.7, 0.7, 0.7]));
        assert_eq!(img.pixels[0], img.pixels[1]);
        assert_eq!(img.pixels[1], img.pixels[2]);
    }

    #[test]
    fn gray8_maps_and_clamps() {
        assert_eq!(gray8(0.0), 0);
        assert_eq!(gray8(1.0), 255);
        assert_eq!(gray8(-0.5), 0);
        assert_eq!(gray8(1.5), 255);
        assert_eq!(gray8(0.5), 128);
    }

    #[test]
    fn relief_shade_is_lit_bounded_and_directional() {
        // Flat ground reads a mid-tone (not black, not white).
        let flat = relief_shade(0.0, 0.0, DEFAULT_LIGHT);
        assert!(
            flat > 0.1 && flat < 0.9,
            "flat shade {flat} should be mid-tone"
        );

        // A slope facing the light (upper-left) is brighter than one facing away.
        let toward = relief_shade(0.6, 0.0, DEFAULT_LIGHT);
        let away = relief_shade(-0.6, 0.0, DEFAULT_LIGHT);
        assert!(
            toward > away,
            "{toward} (toward light) should exceed {away} (away)"
        );

        // Stays in range even for a near-vertical slope.
        let steep = relief_shade(50.0, -50.0, DEFAULT_LIGHT);
        assert!((0.0..=1.0).contains(&steep), "shade {steep} out of range");
    }
}
