//! Heightmap export.
//!
//! Currently one format: 16-bit single-channel grayscale PNG of the `height`
//! layer. Height is always 16-bit; 8-bit terraces and is never used for height.
//!
//! Planned sibling exporters, recorded here so the intent is not lost (NOT built
//! yet):
//!
//! - `.r16`: raw 16-bit little-endian samples, no header. Needs no encoder, just
//!   the `u16` values written little-endian. It is Unreal's other native
//!   heightmap format, so it comes next.
//! - EXR (32-bit float): high-precision interchange with DCC tools like Houdini.
//! - Weightmap/splatmap (8-bit masks for material layers), derived from `mask`
//!   layers.
//!
//! Unreal Engine context these serve: UE imports 16-bit grayscale PNG or `.r16`,
//! maps the 16-bit range to roughly `-256..256`, then multiplies by the import Z
//! scale (default 100, giving about +/-256 m). Recommended landscape sizes are
//! the `(section x components) + 1` family (commonly 2017 or 4033), which Ymir's
//! resolution independence lets us render natively instead of resampling. A
//! future tiled exporter must also avoid a naming gotcha: UE marks tiles by
//! filename (`_x0_y0` style), and a non-tiled filename with an `x` flanked by
//! numbers (for example `terrain4033x4033`) errors on import, so non-tiled
//! output names must not match that pattern.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::Field;
use crate::error::{Error, Result};
use crate::layers;

/// How `height` values map onto the 16-bit output range.
///
/// This is an explicit argument rather than a fixed behavior so it maps cleanly
/// onto a node parameter when export becomes an endpoint operator.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum HeightRange {
    /// Map the nominal `[0, 1]` height range to the full `0..=65535`. Values
    /// outside `[0, 1]` are clamped. This is the default.
    #[default]
    Normalized,
    /// Map `[min, max]` to `0..=65535`, clamping values outside the range.
    ///
    /// This mode exists for tiled exports: every tile must share one range so the
    /// tiles align at their seams instead of each normalizing to its own extremes.
    Explicit {
        /// Height value mapped to `0`.
        min: f32,
        /// Height value mapped to `65535`.
        max: f32,
    },
}

impl HeightRange {
    /// Maps a single height value to a 16-bit sample, clamping out-of-range input.
    fn to_sample(self, value: f32) -> u16 {
        let (min, max) = match self {
            HeightRange::Normalized => (0.0, 1.0),
            HeightRange::Explicit { min, max } => (min, max),
        };
        let span = max - min;
        // A zero or inverted span has no meaningful mapping; collapse to 0.
        let t = if span > 0.0 {
            (value - min) / span
        } else {
            0.0
        };
        (t.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
    }
}

/// Writes the field's `height` layer to `path` as a 16-bit grayscale PNG.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be created or written,
/// [`Error::MissingLayer`] if the field has no `height` layer, or
/// [`Error::PngEncode`] if the encoder rejects the image.
pub fn export_png(field: &Field, path: impl AsRef<Path>, range: HeightRange) -> Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    write_png(field, writer, range)
}

/// Encodes the field's `height` layer as a 16-bit grayscale PNG into `writer`.
///
/// Kept generic over [`Write`] so tests can encode into an in-memory buffer and
/// decode it back without touching the filesystem.
fn write_png<W: Write>(field: &Field, writer: W, range: HeightRange) -> Result<()> {
    // The height layer is genuinely required here: an export endpoint asked to
    // write a field with no height has nothing to write. This is the sanctioned
    // use of MissingLayer; optional layers must use `layer_or` instead.
    let layer = field
        .layer(layers::HEIGHT)
        .ok_or_else(|| Error::MissingLayer {
            name: layers::HEIGHT.to_string(),
        })?;

    // The layer's own dimensions are the image's, so the sample buffer length
    // always matches the declared PNG size.
    let to_u32 = |n: usize| {
        u32::try_from(n)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "dimension exceeds u32"))
    };
    let width = to_u32(layer.width())?;
    let height = to_u32(layer.height())?;

    // PNG stores 16-bit samples big-endian (network byte order).
    let mut data = Vec::with_capacity(layer.len() * 2);
    for &value in layer.as_slice() {
        data.extend_from_slice(&range.to_sample(value).to_be_bytes());
    }

    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(&data)?;
    png_writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, Region};
    use std::sync::Arc;

    fn field_with_heights(width: usize, height: usize, values: &[f32]) -> Field {
        assert_eq!(values.len(), width * height);
        let layer = Layer::from_fn(width, height, |x, y| values[y * width + x]);
        Field::new(width, height, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer))
    }

    /// Decodes a PNG back into `(width, height, samples)`, asserting it really is
    /// 16-bit grayscale.
    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u16>) {
        let decoder = png::Decoder::new(bytes);
        let mut reader = decoder.read_info().expect("read PNG info");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("decode PNG frame");
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        let samples = buf[..info.buffer_size()]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        (info.width, info.height, samples)
    }

    #[test]
    fn normalized_round_trips_known_values() {
        let field = field_with_heights(2, 2, &[0.0, 1.0, 0.5, 0.25]);
        let mut bytes = Vec::new();
        write_png(&field, &mut bytes, HeightRange::Normalized).unwrap();

        let (w, h, samples) = decode(&bytes);
        assert_eq!((w, h), (2, 2));
        assert_eq!(samples[0], 0); // 0.0
        assert_eq!(samples[1], 65535); // 1.0
        assert_eq!(samples[2], 32768); // 0.5 * 65535 = 32767.5, rounds up
        assert_eq!(samples[3], 16384); // 0.25 * 65535 = 16383.75, rounds up
    }

    #[test]
    fn explicit_range_maps_and_clamps() {
        let field = field_with_heights(2, 2, &[10.0, 20.0, 15.0, 25.0]);
        let mut bytes = Vec::new();
        write_png(
            &field,
            &mut bytes,
            HeightRange::Explicit {
                min: 10.0,
                max: 20.0,
            },
        )
        .unwrap();

        let (_, _, samples) = decode(&bytes);
        assert_eq!(samples[0], 0); // min
        assert_eq!(samples[1], 65535); // max
        assert_eq!(samples[2], 32768); // midpoint
        assert_eq!(samples[3], 65535); // above max, clamped
    }

    #[test]
    fn normalized_clamps_out_of_range() {
        let field = field_with_heights(1, 2, &[-0.5, 1.5]);
        let mut bytes = Vec::new();
        write_png(&field, &mut bytes, HeightRange::Normalized).unwrap();

        let (_, _, samples) = decode(&bytes);
        assert_eq!(samples, vec![0, 65535]);
    }

    #[test]
    fn export_is_byte_identical_twice() {
        let values: Vec<f32> = (0..16).map(|i| i as f32 / 15.0).collect();
        let field = field_with_heights(4, 4, &values);

        let mut a = Vec::new();
        let mut b = Vec::new();
        write_png(&field, &mut a, HeightRange::Normalized).unwrap();
        write_png(&field, &mut b, HeightRange::Normalized).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn missing_height_layer_is_an_error() {
        let field = Field::new(2, 2, Region::UNIT);
        let mut bytes = Vec::new();
        let err = write_png(&field, &mut bytes, HeightRange::Normalized).unwrap_err();
        assert!(matches!(err, Error::MissingLayer { name } if name == layers::HEIGHT));
    }
}
