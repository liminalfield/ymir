//! Hand-painted mask strokes: the data model behind the Paint node.
//!
//! A painted mask is stored as vector strokes, not a raster. Each [`Stroke`] carries its brush and
//! a normalized path, and is rasterized to a `[0, 1]` layer at build resolution by the node. This
//! keeps a painted mask resolution-independent (rasterized at any resolution), git-friendly (small,
//! diffable JSON, no embedded raster or sidecar), deterministic, and editable (undo a stroke, retune
//! a brush). The stroke and brush model is channel-agnostic, so the same authoring later drives a
//! colour-paint node for texturing.
//!
//! Equality and hashing normalize `f32` by bits (every NaN to one pattern, `-0.0` to `+0.0`), the
//! same canonicalization the rest of the param model uses, so equal strokes always produce equal
//! cache keys.

use serde::{Deserialize, Serialize};

use crate::hash::Fnv1a64;
use crate::param::canonical_f32_bits;

/// How a stroke combines with the mask already painted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrokeMode {
    /// Add to the mask, toward the brush strength.
    #[default]
    Paint,
    /// Remove from the mask, toward zero.
    Erase,
}

/// The brush footprint. Round for now; further shapes are additive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrushShape {
    /// A round brush.
    #[default]
    Round,
}

/// One point along a stroke path, in normalized `[0, 1]` region coordinates.
///
/// `weight` (default `1.0`) scales the brush at this point, so pen pressure can modulate radius or
/// strength per point later; mouse and basic-pen painting write `1.0`. Serializes compactly as the
/// three-element array `[x, y, weight]`.
#[derive(Clone, Copy, Debug)]
pub struct StrokePoint {
    /// Normalized x in `[0, 1]`.
    pub x: f32,
    /// Normalized y in `[0, 1]`.
    pub y: f32,
    /// Per-point brush weight (pressure-ready); `1.0` for mouse and basic pen.
    pub weight: f32,
}

impl StrokePoint {
    /// A point with unit weight (the mouse / basic-pen case).
    #[must_use]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y, weight: 1.0 }
    }
}

impl Serialize for StrokePoint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        [self.x, self.y, self.weight].serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StrokePoint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let [x, y, weight] = <[f32; 3]>::deserialize(deserializer)?;
        Ok(Self { x, y, weight })
    }
}

/// One brush stroke: the brush settings plus the path it was drawn along.
///
/// `radius` is a fraction of the region width (so the mask is resolution-independent), `strength`
/// the value it paints toward, `hardness` the edge falloff (0 soft, 1 hard). `mode` and `shape`
/// default so an older or hand-written file that omits them still loads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stroke {
    /// Brush radius as a fraction of the region width, in `(0, 1]`.
    pub radius: f32,
    /// Brush strength: the value the stroke paints toward, in `[0, 1]`.
    pub strength: f32,
    /// Edge hardness in `[0, 1]`: 0 is a soft falloff, 1 a hard edge.
    pub hardness: f32,
    /// Whether the stroke paints or erases.
    #[serde(default)]
    pub mode: StrokeMode,
    /// The brush footprint.
    #[serde(default)]
    pub shape: BrushShape,
    /// The path, in normalized region coordinates.
    pub path: Vec<StrokePoint>,
}

impl Stroke {
    /// Folds the stroke into a hash: brush settings, then path points, `f32` by canonical bits.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_u32(canonical_f32_bits(self.radius));
        h.write_u32(canonical_f32_bits(self.strength));
        h.write_u32(canonical_f32_bits(self.hardness));
        h.write_bytes(&[self.mode as u8, self.shape as u8]);
        h.write_usize(self.path.len());
        for p in &self.path {
            h.write_u32(canonical_f32_bits(p.x));
            h.write_u32(canonical_f32_bits(p.y));
            h.write_u32(canonical_f32_bits(p.weight));
        }
    }
}

impl PartialEq for Stroke {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.shape == other.shape
            && canonical_f32_bits(self.radius) == canonical_f32_bits(other.radius)
            && canonical_f32_bits(self.strength) == canonical_f32_bits(other.strength)
            && canonical_f32_bits(self.hardness) == canonical_f32_bits(other.hardness)
            && self.path.len() == other.path.len()
            && self.path.iter().zip(&other.path).all(|(a, b)| {
                canonical_f32_bits(a.x) == canonical_f32_bits(b.x)
                    && canonical_f32_bits(a.y) == canonical_f32_bits(b.y)
                    && canonical_f32_bits(a.weight) == canonical_f32_bits(b.weight)
            })
    }
}

impl Eq for Stroke {}

/// An ordered set of brush strokes: a hand-painted mask, rasterized stroke by stroke. Serializes
/// transparently as a JSON array of strokes; `PartialEq`/`Eq` delegate to [`Stroke`]'s canonical
/// comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Strokes {
    strokes: Vec<Stroke>,
}

impl Strokes {
    /// An empty stroke set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a stroke set from a list of strokes.
    #[must_use]
    pub fn from_strokes(strokes: Vec<Stroke>) -> Self {
        Self { strokes }
    }

    /// The strokes, in paint order.
    #[must_use]
    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    /// Appends a stroke.
    pub fn push(&mut self, stroke: Stroke) {
        self.strokes.push(stroke);
    }

    /// Removes and returns the last stroke (undo).
    pub fn pop(&mut self) -> Option<Stroke> {
        self.strokes.pop()
    }

    /// Removes every stroke.
    pub fn clear(&mut self) {
        self.strokes.clear();
    }

    /// The number of strokes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strokes.len()
    }

    /// Whether there are no strokes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    /// Folds the stroke set into a hash in paint order.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_usize(self.strokes.len());
        for stroke in &self.strokes {
            stroke.hash_into(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke() -> Stroke {
        Stroke {
            radius: 0.1,
            strength: 0.8,
            hardness: 0.5,
            mode: StrokeMode::Paint,
            shape: BrushShape::Round,
            path: vec![StrokePoint::new(0.2, 0.3), StrokePoint::new(0.4, 0.5)],
        }
    }

    fn hash(s: &Strokes) -> u64 {
        let mut h = Fnv1a64::new();
        s.hash_into(&mut h);
        h.finish().to_u64()
    }

    #[test]
    fn round_trips_through_json() {
        let strokes = Strokes::from_strokes(vec![stroke()]);
        let json = serde_json::to_string(&strokes).unwrap();
        let back: Strokes = serde_json::from_str(&json).unwrap();
        assert_eq!(strokes, back);
    }

    #[test]
    fn points_serialize_as_compact_arrays() {
        // A point is [x, y, weight], not an object, so a painted path stays compact and diffable.
        let json = serde_json::to_string(&Strokes::from_strokes(vec![stroke()])).unwrap();
        assert!(
            json.contains("[0.2,0.3,1.0]"),
            "compact point array: {json}"
        );
    }

    #[test]
    fn mode_and_shape_default_when_absent() {
        // A hand-written stroke omitting mode/shape still loads (defaults to paint/round).
        let json = r#"[{"radius":0.1,"strength":0.8,"hardness":0.5,"path":[[0.2,0.3,1.0]]}]"#;
        let back: Strokes = serde_json::from_str(json).unwrap();
        assert_eq!(back.strokes()[0].mode, StrokeMode::Paint);
        assert_eq!(back.strokes()[0].shape, BrushShape::Round);
    }

    #[test]
    fn hash_is_deterministic_and_stroke_sensitive() {
        let a = Strokes::from_strokes(vec![stroke()]);
        assert_eq!(hash(&a), hash(&a), "same strokes hash equally");

        let mut moved = stroke();
        moved.path[0].x += 0.01;
        assert_ne!(
            hash(&a),
            hash(&Strokes::from_strokes(vec![moved])),
            "a moved point changes it"
        );

        let mut erased = stroke();
        erased.mode = StrokeMode::Erase;
        assert_ne!(
            hash(&a),
            hash(&Strokes::from_strokes(vec![erased])),
            "mode changes it"
        );
    }

    #[test]
    fn equality_normalizes_signed_zero() {
        let mut zero = stroke();
        zero.path[0].x = 0.0;
        let mut neg_zero = stroke();
        neg_zero.path[0].x = -0.0;
        assert_eq!(
            Strokes::from_strokes(vec![zero.clone()]),
            Strokes::from_strokes(vec![neg_zero.clone()]),
            "-0.0 and +0.0 compare equal"
        );
        assert_eq!(
            hash(&Strokes::from_strokes(vec![zero])),
            hash(&Strokes::from_strokes(vec![neg_zero])),
            "and hash equally"
        );
    }

    #[test]
    fn undo_and_clear() {
        let mut strokes = Strokes::from_strokes(vec![stroke(), stroke()]);
        assert_eq!(strokes.len(), 2);
        strokes.pop();
        assert_eq!(strokes.len(), 1);
        strokes.clear();
        assert!(strokes.is_empty());
    }
}
