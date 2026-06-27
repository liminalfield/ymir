//! Binary serialization of a node's output fields for the evaluation cache.
//!
//! This is the on-disk format for the cache's warm tier (see
//! `docs/design/evaluation-cache.md`), not the project format: it carries its own version,
//! evolves freely, and a malformed blob is a recoverable miss (an error), never a panic.
//!
//! The layout is raw little-endian: a small header, then each [`Field`] as its dimensions,
//! region, scalar globals, and layers, with layer cell data written as raw `f32`. Maps iterate
//! in sorted (`BTreeMap`) order, so identical fields always serialize to identical bytes, which
//! keeps the format canonical and round-trip testable. Layer cell data is reconstructed through
//! [`Layer::from_vec`]; layers share the field's grid, so each holds `width * height` cells.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::field::Field;
use crate::layer::Layer;
use crate::region::Region;

/// Format identifier ("Ymir field cache"), so a foreign or truncated blob is rejected cleanly.
const MAGIC: [u8; 4] = *b"YMFC";
/// Format version. Bumped on any layout change; an unrecognised version decodes to an error
/// (the cache treats it as a miss and recomputes).
const VERSION: u16 = 1;

/// Serializes a node's output fields to the cache's binary format. Deterministic: sorted map
/// order and little-endian values mean identical fields always produce identical bytes.
#[must_use]
pub fn write_fields(fields: &[Field]) -> Vec<u8> {
    let estimate: usize = 10
        + fields
            .iter()
            .map(|f| 64 + f.layers().count() * (f.width() * f.height() * 4 + 32))
            .sum::<usize>();
    let mut out = Vec::with_capacity(estimate);

    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());

    for field in fields {
        out.extend_from_slice(&(field.width() as u32).to_le_bytes());
        out.extend_from_slice(&(field.height() as u32).to_le_bytes());
        let r = field.region();
        for v in [r.min_x, r.min_y, r.max_x, r.max_y] {
            out.extend_from_slice(&v.to_le_bytes());
        }

        let details: Vec<(&str, f64)> = field.details().collect();
        out.extend_from_slice(&(details.len() as u32).to_le_bytes());
        for (name, value) in details {
            write_str(&mut out, name);
            out.extend_from_slice(&value.to_le_bytes());
        }

        let layers: Vec<(&str, &Arc<Layer>)> = field.layers().collect();
        out.extend_from_slice(&(layers.len() as u32).to_le_bytes());
        for (name, layer) in layers {
            write_str(&mut out, name);
            // Layers share the field's grid; the reader reconstructs the count from the field
            // dimensions, so the data length is implicit.
            debug_assert_eq!(layer.width(), field.width());
            debug_assert_eq!(layer.height(), field.height());
            for &v in layer.as_slice() {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

/// Deserializes fields written by [`write_fields`]. Every malformed input (bad magic,
/// unsupported version, truncated body, invalid name) returns [`Error::FieldCacheDecode`]
/// rather than panicking, so the cache can degrade to a recompute.
///
/// # Errors
///
/// Returns [`Error::FieldCacheDecode`] if `bytes` is not a valid, current-version blob.
pub fn read_fields(bytes: &[u8]) -> Result<Vec<Field>> {
    let mut cur = Reader { bytes, pos: 0 };

    if cur.take(4)? != MAGIC {
        return Err(decode("bad magic"));
    }
    let version = cur.u16()?;
    if version != VERSION {
        return Err(decode(format!("unsupported version {version}")));
    }

    let field_count = cur.u32()? as usize;
    // Do not pre-allocate from an untrusted count; push as fields decode, so a corrupt count
    // fails fast on the first short read instead of reserving a huge buffer.
    let mut fields = Vec::new();
    for _ in 0..field_count {
        let width = cur.u32()? as usize;
        let height = cur.u32()? as usize;
        let region = Region::new(cur.f64()?, cur.f64()?, cur.f64()?, cur.f64()?);
        let mut field = Field::new(width, height, region);

        let detail_count = cur.u32()? as usize;
        for _ in 0..detail_count {
            let name = cur.string()?;
            let value = cur.f64()?;
            field.set_detail(name, value);
        }

        let cells = width
            .checked_mul(height)
            .ok_or_else(|| decode("field dimensions overflow"))?;
        let layer_count = cur.u32()? as usize;
        for _ in 0..layer_count {
            let name = cur.string()?;
            let data = cur.f32_vec(cells)?;
            field.set_layer(name, Arc::new(Layer::from_vec(width, height, data)));
        }

        fields.push(field);
    }
    Ok(fields)
}

/// Writes a length-prefixed UTF-8 string.
fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Builds a decode error.
fn decode(message: impl Into<String>) -> Error {
    Error::FieldCacheDecode(message.into())
}

/// A bounds-checked little-endian reader over a byte slice. Every read validates the remaining
/// length, so truncated input yields an error, never an out-of-bounds panic.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| decode("read length overflow"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| decode("unexpected end of data"))?;
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16> {
        let mut a = [0u8; 2];
        a.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(a))
    }

    fn u32(&mut self) -> Result<u32> {
        let mut a = [0u8; 4];
        a.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(a))
    }

    fn f64(&mut self) -> Result<f64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(f64::from_le_bytes(a))
    }

    fn f32_vec(&mut self, count: usize) -> Result<Vec<f32>> {
        let byte_len = count
            .checked_mul(4)
            .ok_or_else(|| decode("layer data length overflow"))?;
        // take() validates the bytes exist before we allocate, so `count` is bounded by the
        // real data length here.
        let bytes = self.take(byte_len)?;
        let mut data = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(4) {
            let mut a = [0u8; 4];
            a.copy_from_slice(chunk);
            data.push(f32::from_le_bytes(a));
        }
        Ok(data)
    }

    fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| decode("invalid utf-8 in name"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers;

    fn sample() -> Vec<Field> {
        let height = Layer::from_fn(4, 3, |x, y| x as f32 * 0.25 + y as f32);
        let mask = Layer::from_fn(4, 3, |x, _| if x > 1 { 1.0 } else { 0.0 });
        let a = Field::new(4, 3, Region::new(0.0, 0.0, 2.0, 1.5))
            .with_layer(layers::HEIGHT, Arc::new(height))
            .with_layer(layers::MASK, Arc::new(mask))
            .with_detail("seed", 42.0)
            .with_detail("vertical_scale", 512.0);
        // A second output with a different grid, to exercise multi-output round-tripping.
        let flow = Layer::filled(2, 2, 0.5);
        let b = Field::new(2, 2, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(flow));
        vec![a, b]
    }

    #[test]
    fn round_trips_to_identical_content() {
        let fields = sample();
        let decoded = read_fields(&write_fields(&fields)).expect("valid blob decodes");
        assert_eq!(decoded.len(), fields.len());
        for (original, back) in fields.iter().zip(&decoded) {
            assert_eq!(original.content_hash(), back.content_hash());
        }
    }

    #[test]
    fn serialization_is_deterministic() {
        let fields = sample();
        assert_eq!(write_fields(&fields), write_fields(&fields));
    }

    #[test]
    fn empty_output_round_trips() {
        let decoded = read_fields(&write_fields(&[])).expect("empty blob decodes");
        assert!(decoded.is_empty());
    }

    #[test]
    fn bad_magic_is_a_decode_error_not_a_panic() {
        let err = read_fields(b"NOPExxxxxxxx").unwrap_err();
        assert!(matches!(err, Error::FieldCacheDecode(_)));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let mut blob = write_fields(&sample());
        blob[4] = 0xFF; // corrupt the version's low byte
        blob[5] = 0xFF;
        assert!(matches!(
            read_fields(&blob),
            Err(Error::FieldCacheDecode(_))
        ));
    }

    #[test]
    fn truncation_at_every_length_is_an_error_never_a_panic() {
        let blob = write_fields(&sample());
        for cut in 0..blob.len() {
            // Every prefix shorter than the whole must error cleanly, not panic.
            assert!(
                read_fields(&blob[..cut]).is_err(),
                "truncated blob at {cut} bytes should decode to an error"
            );
        }
        // And the full blob still decodes.
        assert!(read_fields(&blob).is_ok());
    }
}
