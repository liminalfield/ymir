//! Heightmap import: decoding a grayscale/greyscale-able PNG into normalized samples.
//!
//! The sibling of [`export`](crate::export). It is the reusable, non-terrain-semantic
//! decode mechanism; the terrain-facing Import node (which resamples this into the
//! requested resolution and region) lives in `ymir-nodes`, so the engine stays free of
//! node semantics. Only PNG for now, matching the exporter; `.r16` and EXR importers are
//! later siblings.
//!
//! An imported image is fixed-resolution raster data, the deliberate exception to the
//! generators' resolution independence: it maps onto the world and is resampled, so a
//! higher build resolution interpolates rather than reveals new detail. Its output also
//! depends on the file, not the seed, so byte-identical output holds only for the same
//! file.

use std::io::Read;

use crate::error::Result;

/// A decoded image as luminance in `[0, 1]`, row-major (x varies fastest).
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedImage {
    /// Image width in pixels.
    pub width: usize,
    /// Image height in pixels.
    pub height: usize,
    /// One luminance value per pixel in `[0, 1]`, row-major.
    pub data: Vec<f32>,
}

impl DecodedImage {
    /// The luminance at integer pixel `(x, y)`, clamped to the edge for out-of-range
    /// coordinates so sampling never indexes out of bounds.
    #[must_use]
    pub fn pixel(&self, x: i64, y: i64) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let cx = x.clamp(0, self.width as i64 - 1) as usize;
        let cy = y.clamp(0, self.height as i64 - 1) as usize;
        self.data[cy * self.width + cx]
    }
}

/// Decodes a PNG from `reader` into a [`DecodedImage`] of luminance in `[0, 1]`.
///
/// Handles 8- and 16-bit depths and grayscale, grayscale+alpha, RGB, and RGBA color
/// types; paletted and sub-8-bit grayscale are expanded first. Colour is reduced to
/// Rec. 601 luma; a grayscale heightmap (the common case) passes its single channel
/// through unchanged. Alpha is ignored.
///
/// # Errors
///
/// Returns [`Error::PngDecode`](crate::Error::PngDecode) if `reader` is not a decodable
/// PNG (wrong format, truncated, or an unsupported variant).
pub fn decode_png<R: Read>(reader: R) -> Result<DecodedImage> {
    let mut decoder = png::Decoder::new(reader);
    // Expand palettes and sub-8-bit grayscale up to a byte so the sample loop only has to
    // handle 8- and 16-bit channels.
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf)?;
    let width = frame.width as usize;
    let height = frame.height as usize;
    let channels = frame.color_type.samples();
    let sixteen = frame.bit_depth == png::BitDepth::Sixteen;
    let bytes_per_sample = if sixteen { 2 } else { 1 };
    let pixel_stride = channels * bytes_per_sample;

    let pixels = &buf[..frame.buffer_size()];
    let mut data = Vec::with_capacity(width * height);
    for px in pixels.chunks_exact(pixel_stride) {
        // Read channel `c` (0-based) as a normalized [0, 1] sample.
        let channel = |c: usize| -> f32 {
            let off = c * bytes_per_sample;
            if sixteen {
                u16::from_be_bytes([px[off], px[off + 1]]) as f32 / f32::from(u16::MAX)
            } else {
                f32::from(px[off]) / f32::from(u8::MAX)
            }
        };
        let lum = if channels >= 3 {
            // RGB(A): Rec. 601 luma. A grayscale heightmap stored as RGB has equal
            // channels, so this returns that shared value.
            0.299 * channel(0) + 0.587 * channel(1) + 0.114 * channel(2)
        } else {
            // Grayscale or grayscale+alpha: channel 0 is the height.
            channel(0)
        };
        data.push(lum);
    }

    Ok(DecodedImage {
        width,
        height,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{HeightRange, export_png_to};
    use crate::{Field, Layer, Region, layers};
    use std::sync::Arc;

    /// Encodes `values` as a normalized 16-bit PNG (via the exporter) and decodes it back.
    fn round_trip(width: usize, height: usize, values: &[f32]) -> DecodedImage {
        let layer = Layer::from_fn(width, height, |x, y| values[y * width + x]);
        let field =
            Field::new(width, height, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer));
        let mut bytes = Vec::new();
        export_png_to(&field, &mut bytes, HeightRange::Normalized).unwrap();
        decode_png(&bytes[..]).unwrap()
    }

    #[test]
    fn decodes_dimensions_and_normalized_values() {
        let img = round_trip(2, 2, &[0.0, 1.0, 0.5, 0.25]);
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.data[0], 0.0);
        assert_eq!(img.data[1], 1.0);
        assert!((img.data[2] - 0.5).abs() < 1e-4);
        assert!((img.data[3] - 0.25).abs() < 1e-4);
    }

    #[test]
    fn pixel_clamps_to_the_edge() {
        let img = round_trip(2, 1, &[0.2, 0.8]);
        assert_eq!(img.pixel(0, 0), img.pixel(-5, 0));
        assert_eq!(img.pixel(1, 0), img.pixel(99, 0));
        // y out of range clamps too.
        assert_eq!(img.pixel(0, 0), img.pixel(0, -3));
    }

    #[test]
    fn rejects_a_non_png() {
        let err = decode_png(&b"not a png at all"[..]).unwrap_err();
        assert!(matches!(err, crate::Error::PngDecode(_)));
    }
}
