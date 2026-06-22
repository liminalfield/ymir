//! A single named scalar grid.

use crate::hash::{ContentHash, Fnv1a64};

/// A 2D grid of `f32` scalars stored in row-major order.
///
/// `Layer` is the per-cell payload a [`Field`](crate::Field) carries under a
/// name (`"height"`, `"mask"`, and so on). The cell buffer is private and
/// reached only through methods, so a future change to the storage stays a
/// contained edit rather than a codebase-wide one.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Layer {
    /// Creates a `width * height` layer with every cell set to `value`.
    #[must_use]
    pub fn filled(width: usize, height: usize, value: f32) -> Self {
        Self {
            width,
            height,
            data: vec![value; width * height],
        }
    }

    /// Creates a layer by evaluating `f(x, y)` for each cell in row-major order
    /// (x varies fastest).
    pub fn from_fn<F>(width: usize, height: usize, mut f: F) -> Self
    where
        F: FnMut(usize, usize) -> f32,
    {
        let mut data = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                data.push(f(x, y));
            }
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// Grid width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Grid height in cells.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Total number of cells (`width * height`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the layer has no cells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the value at `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Option<f32> {
        if x < self.width && y < self.height {
            Some(self.data[y * self.width + x])
        } else {
            None
        }
    }

    /// Read-only access to the row-major cell buffer, for hot per-cell loops.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// The `(min, max)` of the layer's finite values, ignoring any non-finite cell.
    /// An empty layer (or one with no finite values) yields `(0.0, 0.0)`, a zero-width
    /// range. min/max are order-independent, so this is deterministic regardless of how
    /// the layer was produced. Used to map a layer onto a fixed output range (the export
    /// auto-range and the preview display) without clipping.
    #[must_use]
    pub fn value_range(&self) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in &self.data {
            if v.is_finite() {
                min = min.min(v);
                max = max.max(v);
            }
        }
        if min <= max { (min, max) } else { (0.0, 0.0) }
    }

    /// Mutable access to the row-major cell buffer, for hot per-cell loops.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Canonical content hash of this layer.
    #[must_use]
    pub fn content_hash(&self) -> ContentHash {
        let mut h = Fnv1a64::new();
        self.hash_into(&mut h);
        h.finish()
    }

    /// Folds this layer's resolution and cells into an existing hash, so a field
    /// can hash all its layers in one canonical pass. Cells are hashed by raw
    /// bit pattern, so a `+0.0` becoming `-0.0` registers as a real change.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_usize(self.width);
        h.write_usize(self.height);
        for &v in &self.data {
            h.write_f32_bits(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_fn_is_row_major() {
        // Cell value encodes its row-major index, so we can check ordering.
        let layer = Layer::from_fn(3, 2, |x, y| (y * 3 + x) as f32);
        assert_eq!(layer.width(), 3);
        assert_eq!(layer.height(), 2);
        assert_eq!(layer.len(), 6);
        assert_eq!(layer.as_slice(), &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(layer.get(2, 1), Some(5.0));
        assert_eq!(layer.get(0, 0), Some(0.0));
    }

    #[test]
    fn value_range_is_the_finite_extent() {
        // 0.0, 0.5, 1.0, 1.5 across the grid.
        let layer = Layer::from_fn(2, 2, |x, y| (x + 2 * y) as f32 * 0.5);
        assert_eq!(layer.value_range(), (0.0, 1.5));
        // A flat layer has a zero-width range at its value.
        assert_eq!(Layer::filled(3, 1, 0.7).value_range(), (0.7, 0.7));
        // A non-finite cell is ignored, not folded into the range.
        let with_nan = Layer::from_fn(3, 1, |x, _| [0.2, f32::NAN, 0.8][x]);
        assert_eq!(with_nan.value_range(), (0.2, 0.8));
    }

    #[test]
    fn get_is_bounds_checked() {
        let layer = Layer::filled(2, 2, 0.0);
        assert_eq!(layer.get(1, 1), Some(0.0));
        assert_eq!(layer.get(2, 0), None);
        assert_eq!(layer.get(0, 2), None);
    }

    #[test]
    fn filled_sets_every_cell() {
        let layer = Layer::filled(4, 3, 0.25);
        assert_eq!(layer.len(), 12);
        assert!(layer.as_slice().iter().all(|&v| v == 0.25));
    }

    #[test]
    fn content_hash_is_stable_and_distinguishing() {
        let a = Layer::from_fn(8, 8, |x, y| (x + y) as f32);
        let b = Layer::from_fn(8, 8, |x, y| (x + y) as f32);
        // Same content hashes the same, every time.
        assert_eq!(a.content_hash(), a.content_hash());
        assert_eq!(a.content_hash(), b.content_hash());

        // A single changed cell changes the hash.
        let mut c = a.clone();
        c.as_mut_slice()[10] += 1.0;
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn content_hash_distinguishes_shape() {
        // Same data length, different dimensions must not collide.
        let wide = Layer::filled(4, 1, 1.0);
        let tall = Layer::filled(1, 4, 1.0);
        assert_ne!(wide.content_hash(), tall.content_hash());
    }

    #[test]
    fn content_hash_matches_golden_value() {
        // Fixed fingerprint of a known layer; identical on every machine and
        // across toolchains because the FNV-1a algorithm and byte layout are
        // specified. A change here means the canonical hashing changed.
        let layer = Layer::filled(4, 4, 0.5);
        assert_eq!(layer.content_hash().to_u64(), 0x06e3_52d7_6afc_7e65);
    }
}
