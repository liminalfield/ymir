//! Field → image shading, shared by the 2D preview pane and the node thumbnails.
//!
//! Pure pixel work: every function here produces an [`egui::ColorImage`] from a named
//! [`Field`] layer (usually `height`, but any layer the field carries) with no GPU context,
//! so it runs on a worker thread the same way for the preview and for thumbnails. It renders
//! a *layer*, never asking "which node is this?", so the additive-node invariant holds.

use eframe::egui;
use std::sync::Arc;
use ymir_core::{Field, Layer};

/// How the height layer is shaded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ShadeMode {
    /// Height mapped to grayscale, scaled per [`HeightScale`] (auto-ranged to the bulk of
    /// the field's values, or a fixed `[0, 1]`).
    Height,
    /// Relief: each cell shaded by its surface normal under a fixed light, so height
    /// *changes* (slopes, carved valleys) are legible even when subtle (#40).
    Relief,
}

/// How Height shading maps values to grey (#83).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HeightScale {
    /// Map the bulk of the field's values to black/white, ignoring a sliver of outliers at each
    /// end (see [`display_range`]). Always shows the shape, but hides absolute amplitude: every
    /// field, tall or flat, fills the range.
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

/// The largest thumbnail the preview pane shades, in cells per side.
///
/// The pane draws its image a few hundred points wide, so shading the field's own resolution is
/// work that never reaches a pixel: at a preview resolution of 1024 that is a million cells shaded,
/// watered and composited on the UI thread every time the field changes, for a picture the size of
/// a postage stamp. 512 stays comfortably above what the pane can show, including on a
/// high-DPI display.
///
/// This bounds the *thumbnail* only. The 2D viewport shades the field on the GPU and the 3D
/// viewport meshes it, both at its own resolution, so nothing that is looked at closely is
/// reduced here.
pub(crate) const THUMB_RES: usize = 512;

/// `field` reduced to at most `cap` cells per side, carrying only `layer`.
///
/// Each target cell is the mean of the source cells it covers. Averaging rather than sampling a
/// representative cell, because the reduction ratios here are large: a node thumbnail is 96 cells
/// a side taken from a preview that may be 1024 (#382), so a sample keeps one cell in more than a
/// hundred and discards the rest. On eroded terrain that reads as speckle rather than as the same
/// map seen smaller, which is the whole point of drawing it. The cost is one pass over the source,
/// which is nothing beside evaluating the node that produced it.
///
/// Auto range is then taken over the averaged cells, so a lone extreme cell is blended away and the
/// mapping can shift slightly against the full field; that is a thumbnail's business, and the
/// viewports shade the field itself.
///
/// Returns the field unchanged when it is already within the cap. `Field` clones share their
/// layers through `Arc`, so that costs nothing.
pub(crate) fn reduced(field: &Field, layer: &str, cap: usize) -> Field {
    let (w, h) = (field.width(), field.height());
    if w <= cap && h <= cap {
        return field.clone();
    }
    let src = field.layer_or(layer, 0.0);
    let (tw, th) = (w.min(cap).max(1), h.min(cap).max(1));
    // The source block a target cell covers. Derived from the target index rather than a fixed
    // block size so the blocks tile the source exactly, with no remainder row or column dropped
    // when the sizes do not divide evenly.
    let span = |i: usize, target: usize, source: usize| {
        let start = i * source / target;
        let end = ((i + 1) * source / target).max(start + 1).min(source);
        start..end
    };
    let averaged = Layer::from_fn(tw, th, |x, y| {
        let (xs, ys) = (span(x, tw, w), span(y, th, h));
        let mut sum = 0.0f32;
        let mut count = 0u32;
        for sy in ys {
            for sx in xs.clone() {
                sum += src.get(sx, sy).unwrap_or(0.0);
                count += 1;
            }
        }
        // `span` yields at least one cell, so the count is never zero.
        sum / count.max(1) as f32
    });
    let mut out = Field::new(tw, th, field.region());
    out.set_layer(layer, Arc::new(averaged));
    out
}

/// Share of the cells allowed to saturate at each end of the Auto display range.
///
/// The exact minimum and maximum are the wrong ends to anchor to. A handful of extreme cells then
/// set the range for the whole picture and everything else is squeezed into a sliver of it: on a
/// real graph the middle 99% of an erosion `wear` field occupied under a quarter of its own range,
/// and a water-depth field occupied about 2%, which reads as a black square with nothing in it
/// (#389). Nor can the graph work around it, since Levels is a linear remap that deliberately does
/// not clamp, so remapping carries the outlier along and it still sets the range.
///
/// Half a percent at each end is what photo tools do, and the handful of saturated cells is
/// invisible beside the field it makes readable.
const DISPLAY_SATURATION: f64 = 0.005;
/// Bins in the histogram behind [`display_range`]. Fine enough that a bin edge shifts the range by
/// a fraction of a percent, coarse enough to build in one pass over a preview-sized field.
const DISPLAY_BINS: usize = 4096;

/// The value range the Auto scale maps to black and white: the field's span with the outermost
/// [`DISPLAY_SATURATION`] of cells at each end allowed to clip.
///
/// Estimated from a histogram rather than by sorting, so it stays one linear pass over the values
/// at preview resolution. The bin boundaries are derived from the exact extremes, so the result is
/// a deterministic function of the data with no dependence on iteration order.
///
/// Falls back to the exact extremes when the histogram cannot improve on them: a flat field, a
/// field small enough that half a percent is less than one cell, or values that are not finite.
pub(crate) fn display_range(layer: &Layer) -> (f32, f32) {
    let values = layer.as_slice();
    let (min, max) = layer.value_range();
    let span = max - min;
    if !span.is_finite() || span <= 0.0 {
        return (min, max);
    }
    let cutoff = (values.len() as f64 * DISPLAY_SATURATION) as usize;
    if cutoff == 0 {
        return (min, max);
    }

    let mut bins = vec![0_u32; DISPLAY_BINS];
    let scale = DISPLAY_BINS as f32 / span;
    for &v in values {
        if !v.is_finite() {
            continue;
        }
        let bin = (((v - min) * scale) as usize).min(DISPLAY_BINS - 1);
        bins[bin] += 1;
    }

    // Walk in from each end until the allowed number of cells has been passed over.
    let bin_value = |i: usize| min + (i as f32 / DISPLAY_BINS as f32) * span;
    let mut seen = 0_u64;
    let mut low = 0;
    for (i, &count) in bins.iter().enumerate() {
        seen += u64::from(count);
        if seen > cutoff as u64 {
            low = i;
            break;
        }
    }
    seen = 0;
    let mut high = DISPLAY_BINS - 1;
    for (i, &count) in bins.iter().enumerate().rev() {
        seen += u64::from(count);
        if seen > cutoff as u64 {
            high = i;
            break;
        }
    }
    // The upper edge of the last kept bin, so the values inside it are not clipped.
    let (lo, hi) = (bin_value(low), bin_value(high + 1).min(max));
    if hi > lo { (lo, hi) } else { (min, max) }
}

/// The value range `scale` maps to black and white for `layer`: its [`display_range`] (Auto), or a
/// fixed `[0, 1]` (Fixed).
///
/// Resolved separately from the shading so a reduced copy of a field can be drawn against the range
/// of the field it came from, and the small picture and the large one are the same picture (#389).
pub(crate) fn scale_range(layer: &Layer, scale: HeightScale) -> (f32, f32) {
    match scale {
        HeightScale::Auto => display_range(layer),
        HeightScale::Fixed => (0.0, 1.0),
    }
}

/// Builds an image from the named layer of `field`, in the chosen mode, with Height mapping
/// `range` to black and white (see [`scale_range`]). The layer is usually `height`, but any layer
/// the field carries can be shown (a `water` depth, a selection `mask`, …) so intermediates are
/// inspectable.
pub(crate) fn field_to_image(
    field: &Field,
    layer: &str,
    mode: ShadeMode,
    range: (f32, f32),
    light: [f32; 3],
) -> egui::ColorImage {
    match mode {
        ShadeMode::Height => height_image(field, layer, range),
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

/// The named layer mapped to grayscale, `range` running from black to white (see
/// [`scale_range`]). Values outside it clip. A flat layer, or any zero-width range, maps to a
/// single tone.
pub(crate) fn height_image(field: &Field, layer: &str, range: (f32, f32)) -> egui::ColorImage {
    let layer = field.layer_or(layer, 0.0);
    let (min, max) = range;
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

    /// `field`'s height shaded at `scale`, ranged over the field itself.
    fn shaded(field: &Field, scale: HeightScale) -> egui::ColorImage {
        height_image(
            field,
            layers::HEIGHT,
            scale_range(&field.layer_or(layers::HEIGHT, 0.0), scale),
        )
    }

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
        let img = shaded(&height_field(&[0.4, 0.6]), HeightScale::Auto);
        assert_eq!(img.pixels[0].r(), 0);
        assert_eq!(img.pixels[1].r(), 255);
    }

    #[test]
    fn auto_shows_out_of_range_without_clipping() {
        // Values below 0 and above 1 are not clamped: the extremes anchor the range and
        // the middle stays distinct.
        let img = shaded(&height_field(&[-0.5, 0.5, 2.0]), HeightScale::Auto);
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
        let img = shaded(&height_field(&[0.0, 0.5, 2.0]), HeightScale::Fixed);
        assert_eq!(img.pixels[0].r(), 0); // 0.0
        assert_eq!(img.pixels[1].r(), 128); // 0.5 stays mid-grey, not stretched
        assert_eq!(img.pixels[2].r(), 255); // 2.0 clips
    }

    #[test]
    fn a_flat_field_is_a_single_tone() {
        let img = shaded(&height_field(&[0.7, 0.7, 0.7]), HeightScale::Auto);
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
        let mut img = shaded(&field, HeightScale::Fixed);
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
        let mut img = shaded(&field, HeightScale::Fixed);
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

#[cfg(test)]
mod reduction {
    use super::*;
    use ymir_core::{Region, layers};

    fn ramp(res: usize) -> Field {
        let mut field = Field::new(res, res, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(res, res, |x, _| {
                f32::from(u16::try_from(x).expect("x fits"))
                    / f32::from(u16::try_from(res).expect("res fits"))
            })),
        );
        field
    }

    #[test]
    fn a_field_within_the_cap_is_untouched() {
        let field = ramp(64);
        let out = reduced(&field, layers::HEIGHT, 512);
        assert_eq!(out.width(), 64);
        assert_eq!(out, field, "no resampling, and every layer kept");
    }

    #[test]
    fn a_larger_field_comes_back_at_the_cap() {
        let out = reduced(&ramp(1024), layers::HEIGHT, 512);
        assert_eq!((out.width(), out.height()), (512, 512));
    }

    #[test]
    fn the_reduction_keeps_the_shape() {
        // The point of the thumbnail: less detail, same picture. A left-to-right ramp has to
        // still run left to right, at roughly the same values.
        let full = ramp(1024);
        let small = reduced(&full, layers::HEIGHT, 512);
        let (a, b) = (
            full.layer_or(layers::HEIGHT, 0.0),
            small.layer_or(layers::HEIGHT, 0.0),
        );
        for x in [0_usize, 100, 255, 400, 511] {
            let want = a.get(x * 2, 0).expect("full");
            let got = b.get(x, 0).expect("small");
            assert!(
                (want - got).abs() < 0.01,
                "column {x}: reduced {got} against full {want}"
            );
        }
    }

    #[test]
    fn the_reduction_stays_inside_the_source() {
        // The far edge is where a rounding slip would index out of the grid, which `get` would
        // quietly turn into 0.0 and paint a black stripe down the edge of every thumbnail.
        let small = reduced(&ramp(1023), layers::HEIGHT, 512);
        let layer = small.layer_or(layers::HEIGHT, 0.0);
        let last = layer.get(511, 0).expect("far column");
        assert!(
            last > 0.9,
            "far column reduced to {last}, expected near 1.0"
        );
    }

    #[test]
    fn relief_of_a_reduced_field_matches_the_full_one() {
        // The gradient is normalized by the grid size, which is what makes the cap safe: the same
        // terrain shades to the same tone at either resolution, differing only in detail.
        let full = relief_image(&ramp(512), layers::HEIGHT, DEFAULT_LIGHT);
        let small = relief_image(
            &reduced(&ramp(512), layers::HEIGHT, 128),
            layers::HEIGHT,
            DEFAULT_LIGHT,
        );
        let centre = |img: &egui::ColorImage| {
            let [w, _] = img.size;
            img.pixels[(w / 2) * w + w / 2].r()
        };
        let (a, b) = (centre(&full), centre(&small));
        assert!(a.abs_diff(b) <= 2, "full {a} against reduced {b}");
    }

    /// A field where nearly every cell sits in a narrow band and a few sit far above it: the shape
    /// that made the 2D view render as black (#389).
    fn field_with_outliers() -> Field {
        let mut field = Field::new(64, 64, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(64, 64, |x, y| {
                // Four cells at 100, everything else spread across [0, 1].
                if y == 0 && x < 4 {
                    100.0
                } else {
                    (x as f32) / 63.0
                }
            })),
        );
        field
    }

    #[test]
    fn the_display_range_ignores_a_sliver_of_outliers() {
        // Four cells in four thousand is well under the half percent allowed to saturate, so they
        // must not be allowed to set the range for the other 4092.
        let field = field_with_outliers();
        let layer = field.layer_or(layers::HEIGHT, 0.0);
        assert_eq!(layer.value_range(), (0.0, 100.0), "the raw extremes");
        let (lo, hi) = display_range(&layer);
        assert!(
            hi < 2.0,
            "the display range must follow the bulk of the data, not the outliers: {lo} to {hi}"
        );
        assert!(lo <= 0.1, "and still start at the bottom of it: {lo}");
    }

    #[test]
    fn a_field_with_no_outliers_keeps_its_own_extremes() {
        // Nothing is thrown away where nothing is unusual: an ordinary field still fills the range.
        let mut field = Field::new(64, 64, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(64, 64, |x, _| (x as f32) / 63.0)),
        );
        let layer = field.layer_or(layers::HEIGHT, 0.0);
        let (lo, hi) = display_range(&layer);
        assert!(lo < 0.02 && hi > 0.98, "{lo} to {hi}");
    }

    #[test]
    fn a_flat_field_reports_its_own_value() {
        // No span to work with, and no division by zero either.
        let mut field = Field::new(16, 16, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(16, 16, |_, _| 0.42)),
        );
        let layer = field.layer_or(layers::HEIGHT, 0.0);
        assert_eq!(display_range(&layer), (0.42, 0.42));
    }

    #[test]
    fn a_thumbnail_and_the_full_field_are_shaded_the_same() {
        // The inconsistency the maintainer reported: the same node legible in its thumbnail and
        // black in the 2D view. The thumbnail used to auto-range its own averaged copy, whose
        // extremes are pulled in by the averaging. Both now range against the full field.
        let field = field_with_outliers();
        let range = display_range(&field.layer_or(layers::HEIGHT, 0.0));
        let small = reduced(&field, layers::HEIGHT, 16);
        let thumb = height_image(&small, layers::HEIGHT, range);
        let full = height_image(
            &field,
            layers::HEIGHT,
            scale_range(&field.layer_or(layers::HEIGHT, 0.0), HeightScale::Auto),
        );
        // Same column of the map, once at 64 cells and once reduced to 16: the same ground should
        // read as the same tone.
        let sample = |img: &egui::ColorImage, fx: f32| {
            let [w, h] = img.size;
            let x = ((w as f32 - 1.0) * fx) as usize;
            img.pixels[(h / 2) * w + x].r()
        };
        for fx in [0.25_f32, 0.5, 0.75] {
            let (a, b) = (sample(&thumb, fx), sample(&full, fx));
            assert!(
                a.abs_diff(b) <= 8,
                "thumbnail {a} against viewport {b} at {fx} across"
            );
        }
    }

    #[test]
    fn reducing_averages_the_block_rather_than_sampling_one_cell() {
        // A field where every cell alternates between 0 and 1, so the mean of any block is 0.5 and
        // a single sample is 0 or 1. This is what a large reduction ratio does to detail: sampled,
        // a thumbnail of eroded terrain is speckle; averaged, it is the same map seen smaller.
        let mut field = Field::new(64, 64, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(64, 64, |x, y| ((x + y) % 2) as f32)),
        );
        let small = reduced(&field, layers::HEIGHT, 8);
        let layer = small.layer_or(layers::HEIGHT, 0.0);
        for y in 0..8 {
            for x in 0..8 {
                let v = layer.get(x, y).expect("cell");
                assert!(
                    (v - 0.5).abs() < 1e-6,
                    "cell ({x}, {y}) reduced to {v}, expected the block mean 0.5"
                );
            }
        }
    }

    #[test]
    fn every_source_cell_reaches_exactly_one_reduced_cell() {
        // The blocks must tile the source with no gap and no overlap, or a reduction whose sizes do
        // not divide evenly drops a strip of the map. Checked by summing: the mean of the means is
        // the mean of the whole field only if every cell was counted once.
        let mut field = Field::new(100, 100, Region::UNIT);
        field.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(100, 100, |x, y| (x * 100 + y) as f32)),
        );
        let full_mean: f64 = field
            .layer_or(layers::HEIGHT, 0.0)
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum::<f64>()
            / 10_000.0;
        // 100 into 8 divides unevenly, so the blocks are a mix of 12 and 13 cells wide.
        let small = reduced(&field, layers::HEIGHT, 8);
        let reduced_mean: f64 = small
            .layer_or(layers::HEIGHT, 0.0)
            .as_slice()
            .iter()
            .map(|&v| f64::from(v))
            .sum::<f64>()
            / 64.0;
        // Not exactly equal: unequal block sizes weight the means slightly differently. Close is
        // the claim, and a dropped strip would be nowhere near.
        let tolerance = full_mean * 0.02;
        assert!(
            (full_mean - reduced_mean).abs() < tolerance,
            "full mean {full_mean} against reduced {reduced_mean}"
        );
    }
}
