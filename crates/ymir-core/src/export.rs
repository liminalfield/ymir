//! Heightmap export.
//!
//! Currently one format: 16-bit single-channel grayscale PNG of the `height`
//! layer. Height is always 16-bit; 8-bit terraces and is never used for height.
//!
//! Formats:
//!
//! - 16-bit grayscale PNG ([`export_png`]).
//! - `.r16`: raw 16-bit little-endian samples, no header ([`export_r16`]). Unreal's
//!   other native heightmap format. Same range mapping as the PNG; the only
//!   differences are byte order (little-endian, not PNG's big-endian) and the
//!   absence of any header, so UE infers the (square) dimensions from the file size.
//!
//! Planned sibling exporters, recorded here so the intent is not lost (NOT built
//! yet):
//!
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
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;

use crate::error::{Error, Result};
use crate::layers;
use crate::{Field, Layer};

/// How `height` values map onto the 16-bit output range.
///
/// This is an explicit argument rather than a fixed behavior so it maps cleanly
/// onto a node parameter when export becomes an endpoint operator.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum HeightRange {
    /// Map the field's actual `[min, max]` onto the full `0..=65535`, so the whole
    /// output range is used and no value is clipped. A flat field (a zero-width range)
    /// maps to all-zero. This is the export endpoint's default: it preserves terrain
    /// that ran outside the nominal `[0, 1]` upstream instead of clamping it.
    Auto,
    /// Map the nominal `[0, 1]` height range to the full `0..=65535`. Values outside
    /// `[0, 1]` are clamped. The fixed, range-independent mapping, for when a stable
    /// output matters more than using the full range.
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
    /// Resolves this mode to the concrete `(min, max)` mapped onto `0..=65535` for the
    /// given height layer. [`Auto`](Self::Auto) uses the layer's actual extent
    /// ([`Layer::value_range`]); the other modes ignore the layer.
    fn resolve(self, layer: &Layer) -> (f32, f32) {
        match self {
            HeightRange::Auto => layer.value_range(),
            HeightRange::Normalized => (0.0, 1.0),
            HeightRange::Explicit { min, max } => (min, max),
        }
    }
}

/// Maps a height value to a 16-bit sample over the concrete range `[min, max]`,
/// clamping out-of-range input. A zero or inverted span has no meaningful mapping and
/// collapses to `0`.
fn sample(value: f32, min: f32, max: f32) -> u16 {
    let span = max - min;
    let t = if span > 0.0 {
        (value - min) / span
    } else {
        0.0
    };
    (t.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
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
    export_png_to(field, writer, range)
}

/// Encodes the field's `height` layer as a 16-bit grayscale PNG into `writer`.
///
/// Generic over [`Write`], so a caller can encode to a file, an in-memory buffer, or any
/// other sink (a test decoding it back, a network stream) without going through the
/// filesystem.
///
/// # Errors
///
/// Returns [`Error::MissingLayer`] if the field has no `height` layer, or
/// [`Error::PngEncode`] if the encoder rejects the image or the write fails.
pub fn export_png_to<W: Write>(field: &Field, writer: W, range: HeightRange) -> Result<()> {
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

    // Resolve the mode to a concrete range once (Auto scans the field), then map every
    // sample over it. PNG stores 16-bit samples big-endian (network byte order).
    let (min, max) = range.resolve(layer);
    let mut data = Vec::with_capacity(layer.len() * 2);
    for &value in layer.as_slice() {
        data.extend_from_slice(&sample(value, min, max).to_be_bytes());
    }

    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(&data)?;
    png_writer.finish()?;
    Ok(())
}

/// Writes the field's `height` layer to `path` as raw 16-bit little-endian samples
/// (`.r16`), no header: Unreal's other native heightmap format. Uses the same
/// [`HeightRange`] mapping as the PNG exporter, so a value maps to the same 16-bit
/// sample; only the byte order and the lack of a header differ.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be created or written, or
/// [`Error::MissingLayer`] if the field has no `height` layer.
pub fn export_r16(field: &Field, path: impl AsRef<Path>, range: HeightRange) -> Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    export_r16_to(field, writer, range)
}

/// Encodes the field's `height` layer as raw 16-bit little-endian samples into `writer`.
///
/// Generic over [`Write`], so a caller can encode to a file, an in-memory buffer, or any
/// other sink without going through the filesystem.
///
/// # Errors
///
/// Returns [`Error::MissingLayer`] if the field has no `height` layer, or [`Error::Io`] if
/// the write fails.
pub fn export_r16_to<W: Write>(field: &Field, mut writer: W, range: HeightRange) -> Result<()> {
    // The height layer is genuinely required: an export asked to write a field with no
    // height has nothing to write. Mirrors `export_png_to`.
    let layer = field
        .layer(layers::HEIGHT)
        .ok_or_else(|| Error::MissingLayer {
            name: layers::HEIGHT.to_string(),
        })?;

    // Same range resolution and per-sample mapping as the PNG, but little-endian and with
    // no header, row-major, so the file is exactly `width * height` `u16` samples.
    let (min, max) = range.resolve(layer);
    let mut data = Vec::with_capacity(layer.len() * 2);
    for &value in layer.as_slice() {
        data.extend_from_slice(&sample(value, min, max).to_le_bytes());
    }
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

/// Writes the field's `height` layer to `path` as a single-channel 32-bit float EXR,
/// multiplying every value by `scale`.
///
/// With `scale == 1.0` the file carries normalized height; with `scale == world_height`
/// (meters that a height of `1.0` represents) it carries **absolute elevation in meters**,
/// self-describing for any DCC. Unlike the 16-bit formats, the float channel stores the
/// actual values losslessly, with no range remap and no clamping.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be created, [`Error::MissingLayer`] if the
/// field has no `height` layer, or [`Error::ExrEncode`] if encoding fails.
pub fn export_exr(field: &Field, path: impl AsRef<Path>, scale: f32) -> Result<()> {
    let file = File::create(path)?;
    export_exr_to(field, BufWriter::new(file), scale)
}

/// Encodes the field's `height` layer as a single-channel 32-bit float EXR into `writer`.
///
/// EXR writes an offset table, so the sink must be [`Seek`] as well as [`Write`] (a file,
/// or a `Cursor` over an in-memory buffer). See [`export_exr`] for the `scale` meaning.
///
/// # Errors
///
/// Returns [`Error::MissingLayer`] if the field has no `height` layer, or
/// [`Error::ExrEncode`] if encoding or the write fails.
pub fn export_exr_to<W: Write + Seek>(field: &Field, writer: W, scale: f32) -> Result<()> {
    // Import only the items needed: a `use exr::prelude::*` would shadow this crate's
    // `Error`/`Result`. `WritableImage` provides `Image::write`.
    use exr::prelude::{Image, SpecificChannels, WritableImage};

    // The height layer is genuinely required: an export with no height has nothing to
    // write. Mirrors the PNG and r16 exporters.
    let layer = field
        .layer(layers::HEIGHT)
        .ok_or_else(|| Error::MissingLayer {
            name: layers::HEIGHT.to_string(),
        })?;
    let width = layer.width();
    let height = layer.height();
    let values = layer.as_slice();

    // One float channel ("Y", the conventional single-luminance channel) of the height
    // values scaled by `scale`, stored losslessly. `pos` is (x, y) in cells.
    let channels = SpecificChannels::build()
        .with_channel("Y")
        .with_pixel_fn(|pos| (values[pos.1 * width + pos.0] * scale,));
    let image = Image::from_channels((width, height), channels);
    image
        .write()
        .to_unbuffered(writer)
        .map_err(|err| Error::ExrEncode(err.to_string()))?;
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
        export_png_to(&field, &mut bytes, HeightRange::Normalized).unwrap();

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
        export_png_to(
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
    fn auto_maps_the_actual_range_without_clipping() {
        // Values running well outside [0, 1] map across the full output range instead
        // of clamping: the min hits 0 and the max hits 65535, with the interior values
        // strictly between, proving a real stretch rather than a clamp to the ends.
        let field = field_with_heights(2, 2, &[-0.5, 0.5, 1.5, 2.0]);
        let mut bytes = Vec::new();
        export_png_to(&field, &mut bytes, HeightRange::Auto).unwrap();

        let (_, _, s) = decode(&bytes);
        assert_eq!(s[0], 0); // -0.5 is the min
        assert_eq!(s[3], 65535); // 2.0 is the max
        assert!(
            s[1] > 0 && s[1] < s[2] && s[2] < 65535,
            "interior not stretched"
        );
    }

    #[test]
    fn auto_on_a_flat_field_collapses_to_zero() {
        // A field with no relief has a zero-width range; map it to all-zero rather than
        // dividing by zero.
        let field = field_with_heights(1, 3, &[0.7, 0.7, 0.7]);
        let mut bytes = Vec::new();
        export_png_to(&field, &mut bytes, HeightRange::Auto).unwrap();

        let (_, _, s) = decode(&bytes);
        assert_eq!(s, vec![0, 0, 0]);
    }

    #[test]
    fn normalized_clamps_out_of_range() {
        let field = field_with_heights(1, 2, &[-0.5, 1.5]);
        let mut bytes = Vec::new();
        export_png_to(&field, &mut bytes, HeightRange::Normalized).unwrap();

        let (_, _, samples) = decode(&bytes);
        assert_eq!(samples, vec![0, 65535]);
    }

    #[test]
    fn export_is_byte_identical_twice() {
        let values: Vec<f32> = (0..16).map(|i| i as f32 / 15.0).collect();
        let field = field_with_heights(4, 4, &values);

        let mut a = Vec::new();
        let mut b = Vec::new();
        export_png_to(&field, &mut a, HeightRange::Normalized).unwrap();
        export_png_to(&field, &mut b, HeightRange::Normalized).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn missing_height_layer_is_an_error() {
        let field = Field::new(2, 2, Region::UNIT);
        let mut bytes = Vec::new();
        let err = export_png_to(&field, &mut bytes, HeightRange::Normalized).unwrap_err();
        assert!(matches!(err, Error::MissingLayer { name } if name == layers::HEIGHT));
    }

    /// Decodes raw little-endian `.r16` bytes into `u16` samples.
    fn decode_r16(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn r16_is_raw_little_endian_with_no_header() {
        let field = field_with_heights(2, 2, &[0.0, 1.0, 0.5, 0.25]);
        let mut bytes = Vec::new();
        export_r16_to(&field, &mut bytes, HeightRange::Normalized).unwrap();

        // Exactly width*height u16 samples, no header.
        assert_eq!(bytes.len(), 4 * 2);
        let samples = decode_r16(&bytes);
        assert_eq!(samples[0], 0); // 0.0
        assert_eq!(samples[1], 65535); // 1.0
        assert_eq!(samples[2], 32768); // 0.5, rounds up
        assert_eq!(samples[3], 16384); // 0.25, rounds up
        // Little-endian: 65535 = 0xFFFF writes both bytes 0xFF; 16384 = 0x4000 writes
        // 0x00 then 0x40, proving low-byte-first order.
        assert_eq!(&bytes[6..8], &[0x00, 0x40]);
    }

    #[test]
    fn r16_values_match_the_png_mapping() {
        // The acceptance criterion: a `.r16` sample equals the PNG sample for the same
        // value and range, differing only in byte order.
        let field = field_with_heights(2, 2, &[-0.5, 0.5, 1.5, 2.0]);
        let mut png = Vec::new();
        let mut r16 = Vec::new();
        export_png_to(&field, &mut png, HeightRange::Auto).unwrap();
        export_r16_to(&field, &mut r16, HeightRange::Auto).unwrap();

        let (_, _, png_samples) = decode(&png);
        assert_eq!(decode_r16(&r16), png_samples);
    }

    #[test]
    fn r16_is_byte_identical_twice() {
        let values: Vec<f32> = (0..16).map(|i| i as f32 / 15.0).collect();
        let field = field_with_heights(4, 4, &values);

        let mut a = Vec::new();
        let mut b = Vec::new();
        export_r16_to(&field, &mut a, HeightRange::Normalized).unwrap();
        export_r16_to(&field, &mut b, HeightRange::Normalized).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn r16_missing_height_layer_is_an_error() {
        let field = Field::new(2, 2, Region::UNIT);
        let mut bytes = Vec::new();
        let err = export_r16_to(&field, &mut bytes, HeightRange::Normalized).unwrap_err();
        assert!(matches!(err, Error::MissingLayer { name } if name == layers::HEIGHT));
    }

    /// Reads the single `Y` channel of an EXR file back as a row-major `f32` buffer.
    fn read_exr_y(path: &std::path::Path, width: usize) -> Vec<f32> {
        use exr::prelude::*;
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .specific_channels()
            .required("Y")
            .collect_pixels(
                |res: Vec2<usize>, _| vec![0.0f32; res.width() * res.height()],
                move |buf: &mut Vec<f32>, pos: Vec2<usize>, (y,): (f32,)| {
                    buf[pos.y() * width + pos.x()] = y;
                },
            )
            .first_valid_layer()
            .all_attributes()
            .from_file(path)
            .expect("read exr back");
        image.layer_data.channel_data.pixels
    }

    #[test]
    fn exr_writes_scaled_float_values_losslessly() {
        // scale 4.0 (a world_height) turns normalized 0.25 / 0.5 into absolute 1.0 / 2.0,
        // stored as exact floats with no clamping or range remap.
        let dir = std::env::temp_dir().join("ymir-export-exr-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scaled.exr");
        let field = field_with_heights(2, 1, &[0.25, 0.5]);

        export_exr(&field, &path, 4.0).unwrap();
        let pixels = read_exr_y(&path, 2);
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup

        assert!((pixels[0] - 1.0).abs() < 1e-6, "0.25 * 4 = 1.0");
        assert!((pixels[1] - 2.0).abs() < 1e-6, "0.5 * 4 = 2.0");
    }

    #[test]
    fn exr_scale_one_preserves_values_including_out_of_range() {
        // Normalized export (scale 1.0) is lossless: it keeps values that ran below 0 or
        // above 1, unlike the 16-bit formats which clamp.
        let dir = std::env::temp_dir().join("ymir-export-exr-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("raw.exr");
        let field = field_with_heights(3, 1, &[-0.5, 0.5, 1.5]);

        export_exr(&field, &path, 1.0).unwrap();
        let pixels = read_exr_y(&path, 3);
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup

        assert!((pixels[0] - -0.5).abs() < 1e-6);
        assert!((pixels[1] - 0.5).abs() < 1e-6);
        assert!((pixels[2] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn exr_to_writes_the_openexr_magic() {
        // export_exr_to into an in-memory cursor produces a real EXR (magic 0x76 2f 31 01).
        let field = field_with_heights(2, 2, &[0.0, 1.0, 0.5, 0.25]);
        let mut buf = std::io::Cursor::new(Vec::new());
        export_exr_to(&field, &mut buf, 1.0).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(&bytes[..4], &[0x76, 0x2f, 0x31, 0x01], "OpenEXR magic");
    }

    #[test]
    fn exr_missing_height_layer_is_an_error() {
        let field = Field::new(2, 2, Region::UNIT);
        let mut buf = std::io::Cursor::new(Vec::new());
        let err = export_exr_to(&field, &mut buf, 1.0).unwrap_err();
        assert!(matches!(err, Error::MissingLayer { name } if name == layers::HEIGHT));
    }
}
