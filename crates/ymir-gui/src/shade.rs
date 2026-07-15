//! Field → image shading, shared by the 2D preview pane and the node thumbnails.
//!
//! Pure pixel work: every function here produces an [`egui::ColorImage`] from a named
//! [`Field`] layer (usually `height`, but any layer the field carries) with no GPU context,
//! so it runs on a worker thread the same way for the preview and for thumbnails. It renders
//! a *layer*, never asking "which node is this?", so the additive-node invariant holds.

use eframe::egui;
use ymir_core::Field;

/// How the height layer is shaded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadeMode {
    /// Height mapped to grayscale, scaled per [`HeightScale`] (auto-ranged to the
    /// field's extent, or a fixed `[0, 1]`).
    Height,
    /// Relief: each cell shaded by its surface normal under a fixed light, so height
    /// *changes* (slopes, carved valleys) are legible even when subtle (#40).
    Relief,
}

/// How Height shading maps values to grey (#83).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeightScale {
    /// Map the field's actual `[min, max]` to black/white. Always shows the shape, but
    /// hides absolute amplitude: every field, tall or flat, fills the range.
    Auto,
    /// Map a fixed `[0, 1]` to black/white. Shows true height (a low field reads dark, a
    /// tall one bright) and clips values outside `[0, 1]`.
    Fixed,
}

/// Appearance of the map water overlay (#96): a tint colour plus how its opacity grows with
/// depth. Isolated as one value, rather than scattered constants, so a future "Water" section in
/// the World panel can drive it (and later persist it into `WorldSettings`) without re-plumbing.
/// This is presentation only: it never reaches `EvalContext`, evaluation, or the determinism
/// contract, so it can be as aesthetic as we like.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct WaterStyle {
    /// Water tint (sRGB bytes). Blue reads unambiguously under red/green colour vision.
    pub colour: [u8; 3],
    /// Overlay opacity right at the shoreline (a cell just below sea level), in `[0, 1]`.
    pub shore_opacity: f32,
    /// Overlay opacity at or beyond [`full_depth`](Self::full_depth), in `[0, 1]`. Deeper water
    /// reads more opaque, so submerged relief fades out with depth. A depth *cue*, not a physical
    /// Beer-Lambert model (that is the 3D shader tiers, #140/#141).
    pub deep_opacity: f32,
    /// Depth below sea level, in normalized height units, at which opacity reaches
    /// [`deep_opacity`](Self::deep_opacity). Shallower cells interpolate from `shore_opacity`.
    pub full_depth: f32,
}

impl Default for WaterStyle {
    fn default() -> Self {
        // A mid Frost-blue, translucent at the shore and near-opaque in the depths.
        Self {
            colour: [46, 110, 174],
            shore_opacity: 0.35,
            deep_opacity: 0.85,
            full_depth: 0.12,
        }
    }
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

/// Builds an image from the named layer of `field`, in the chosen mode and (for Height)
/// scale. The layer is usually `height`, but any layer the field carries can be shown
/// (a `water` depth, a selection `mask`, …) so intermediates are inspectable.
pub(crate) fn field_to_image(
    field: &Field,
    layer: &str,
    mode: ShadeMode,
    scale: HeightScale,
    light: [f32; 3],
) -> egui::ColorImage {
    match mode {
        ShadeMode::Height => height_image(field, layer, scale),
        ShadeMode::Relief => relief_image(field, layer, light),
    }
}

/// Composites the water overlay onto an already-shaded image in place (#96): every cell whose
/// `layer` value sits below `sea_level` is tinted toward the water colour, more opaquely the
/// deeper it lies. The base image (grey height or hillshade) shows through, so submerged relief
/// stays legible near the shore and fades with depth.
///
/// The waterline is compared in raw layer space (`value < sea_level`), so it is independent of the
/// shading mode and of the Auto/Fixed display scale, which only remap the base tone. `image` and
/// `field`'s `layer` must share cell order and count (they do at every call site: the same field
/// and layer feed [`field_to_image`] and this).
pub(crate) fn apply_water(
    image: &mut egui::ColorImage,
    field: &Field,
    layer: &str,
    sea_level: f32,
    style: &WaterStyle,
) {
    let layer = field.layer_or(layer, 0.0);
    debug_assert_eq!(
        image.pixels.len(),
        layer.len(),
        "water overlay expects the image and layer to align cell-for-cell"
    );
    let full_depth = style.full_depth.max(f32::EPSILON);
    for (pixel, &value) in image.pixels.iter_mut().zip(layer.as_slice()) {
        let depth = sea_level - value;
        if depth <= 0.0 {
            continue; // at or above the waterline: dry, left untouched.
        }
        let t = (depth / full_depth).clamp(0.0, 1.0);
        let alpha = style.shore_opacity + (style.deep_opacity - style.shore_opacity) * t;
        *pixel = blend(*pixel, style.colour, alpha);
    }
}

/// Alpha-blends `over` onto `base` at opacity `alpha` (`0` keeps `base`, `1` yields `over`),
/// returning an opaque colour: the translucency is baked against the terrain shade beneath, since
/// the composited texture itself is drawn fully opaque.
fn blend(base: egui::Color32, over: [u8; 3], alpha: f32) -> egui::Color32 {
    let a = alpha.clamp(0.0, 1.0);
    let mix = |b: u8, o: u8| (f32::from(b) * (1.0 - a) + f32::from(o) * a + 0.5) as u8;
    egui::Color32::from_rgb(
        mix(base.r(), over[0]),
        mix(base.g(), over[1]),
        mix(base.b(), over[2]),
    )
}

/// The named layer mapped to grayscale over the chosen [`HeightScale`]: the layer's actual
/// `[min, max]` (Auto), or a fixed `[0, 1]` that shows true amplitude and clips
/// out-of-range (Fixed). A flat layer, or any zero-width range, maps to a single tone.
pub(crate) fn height_image(field: &Field, layer: &str, scale: HeightScale) -> egui::ColorImage {
    let layer = field.layer_or(layer, 0.0);
    let (min, max) = match scale {
        HeightScale::Auto => layer.value_range(),
        HeightScale::Fixed => (0.0, 1.0),
    };
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

/// Relief (hillshade) image: each cell of the named layer shaded by its surface normal.
/// The gradient is per unit region (central difference scaled by the cell count), so the
/// shading reads the same at any preview resolution.
fn relief_image(field: &Field, layer: &str, light: [f32; 3]) -> egui::ColorImage {
    let layer = field.layer_or(layer, 0.0);
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
    use ymir_core::{Layer, Region, layers};

    fn height_field(values: &[f32]) -> Field {
        let n = values.len();
        Field::new(n, 1, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(n, 1, |x, _| values[x])),
        )
    }

    #[test]
    fn auto_ranges_to_the_field_extent() {
        // A compressed range [0.4, 0.6] is stretched across the display: the min reads
        // black and the max white, so the shape is visible rather than near-uniform gray.
        let img = height_image(
            &height_field(&[0.4, 0.6]),
            layers::HEIGHT,
            HeightScale::Auto,
        );
        assert_eq!(img.pixels[0].r(), 0);
        assert_eq!(img.pixels[1].r(), 255);
    }

    #[test]
    fn auto_shows_out_of_range_without_clipping() {
        // Values below 0 and above 1 are not clamped: the extremes anchor the range and
        // the middle stays distinct.
        let img = height_image(
            &height_field(&[-0.5, 0.5, 2.0]),
            layers::HEIGHT,
            HeightScale::Auto,
        );
        assert_eq!(img.pixels[0].r(), 0); // -0.5 (min)
        assert_eq!(img.pixels[2].r(), 255); // 2.0 (max)
        let mid = img.pixels[1].r();
        assert!(mid > 0 && mid < 255, "middle clipped: {mid}");
    }

    #[test]
    fn fixed_shows_true_amplitude_and_clips() {
        // Fixed maps [0, 1] to black/white regardless of the field: a field that only
        // reaches 0.5 reads mid-grey (true amplitude, not stretched to white), and a
        // value past 1 clips to white.
        let img = height_image(
            &height_field(&[0.0, 0.5, 2.0]),
            layers::HEIGHT,
            HeightScale::Fixed,
        );
        assert_eq!(img.pixels[0].r(), 0); // 0.0
        assert_eq!(img.pixels[1].r(), 128); // 0.5 stays mid-grey, not stretched
        assert_eq!(img.pixels[2].r(), 255); // 2.0 clips
    }

    #[test]
    fn a_flat_field_is_a_single_tone() {
        let img = height_image(
            &height_field(&[0.7, 0.7, 0.7]),
            layers::HEIGHT,
            HeightScale::Auto,
        );
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
    fn water_tints_below_sea_level_and_leaves_dry_cells() {
        // One cell below the sea level (0.5), one above.
        let field = height_field(&[0.2, 0.8]);
        let mut img = height_image(&field, layers::HEIGHT, HeightScale::Fixed);
        let dry_before = img.pixels[1];
        apply_water(
            &mut img,
            &field,
            layers::HEIGHT,
            0.5,
            &WaterStyle::default(),
        );
        // The submerged cell reads blue (its blue channel now dominates red).
        let wet = img.pixels[0];
        assert!(wet.b() > wet.r(), "submerged cell {wet:?} should read blue");
        // The dry cell is untouched.
        assert_eq!(
            img.pixels[1], dry_before,
            "cell above the waterline must not change"
        );
    }

    #[test]
    fn deeper_water_is_more_opaque() {
        // Two submerged cells at different depths; start both from the same base grey so only the
        // depth-driven opacity differs, not the base tone.
        let field = height_field(&[0.0, 0.45]);
        let mut img = egui::ColorImage::from_rgba_unmultiplied(
            [2, 1],
            &[128, 128, 128, 255, 128, 128, 128, 255],
        );
        let style = WaterStyle::default();
        apply_water(&mut img, &field, layers::HEIGHT, 0.5, &style);
        let dist = |c: egui::Color32| {
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
            d(c.r(), style.colour[0]) + d(c.g(), style.colour[1]) + d(c.b(), style.colour[2])
        };
        // The deeper cell (0.0) sits nearer the water colour than the shallow one (0.45).
        assert!(
            dist(img.pixels[0]) < dist(img.pixels[1]),
            "deeper water should be nearer the water colour"
        );
    }

    #[test]
    fn default_sea_level_leaves_a_normalized_field_dry() {
        // sea_level 0.0: nothing is strictly below it, so a [0, 1] field is unchanged.
        let field = height_field(&[0.0, 0.5, 1.0]);
        let mut img = height_image(&field, layers::HEIGHT, HeightScale::Fixed);
        let before = img.pixels.clone();
        apply_water(
            &mut img,
            &field,
            layers::HEIGHT,
            0.0,
            &WaterStyle::default(),
        );
        assert_eq!(img.pixels, before, "default sea level should tint nothing");
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
