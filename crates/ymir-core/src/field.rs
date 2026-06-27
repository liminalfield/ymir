//! The universal type that flows on every edge of the graph.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hash::{ContentHash, Fnv1a64};
use crate::layer::Layer;
use crate::region::Region;

/// The single data type carried on every edge of the node graph.
///
/// A `Field` is a 2D grid at a given resolution and [`Region`], carrying a set
/// of named scalar [`Layer`]s plus a small map of scalar globals (`detail`).
/// Layers are held as `Arc<Layer>` so that passing one through untouched is a
/// pointer clone, not a copy of the grid.
///
/// Layers and detail are stored in `BTreeMap`s, not `HashMap`s, on purpose:
/// iteration is always ordered by name, so nothing downstream can come to
/// depend on hash-map iteration order, and that same ordering gives canonical,
/// deterministic [`content_hash`](Self::content_hash)ing for free.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    width: usize,
    height: usize,
    region: Region,
    layers: BTreeMap<String, Arc<Layer>>,
    detail: BTreeMap<String, f64>,
}

impl Field {
    /// Creates an empty field at the given resolution and region, with no layers
    /// and no detail.
    #[must_use]
    pub fn new(width: usize, height: usize, region: Region) -> Self {
        Self {
            width,
            height,
            region,
            layers: BTreeMap::new(),
            detail: BTreeMap::new(),
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

    /// The field's world-space region bounds.
    #[must_use]
    pub fn region(&self) -> Region {
        self.region
    }

    /// Returns the named layer, or `None` if it is absent.
    #[must_use]
    pub fn layer(&self, name: &str) -> Option<&Arc<Layer>> {
        self.layers.get(name)
    }

    /// Iterates the layers in name order as `(name, layer)` pairs.
    pub fn layers(&self) -> impl Iterator<Item = (&str, &Arc<Layer>)> {
        self.layers.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Inserts or replaces a layer, leaving every other layer untouched. This is
    /// the pass-through primitive: replacing `height` keeps the same `Arc` for
    /// `mask` and the rest, so insertion is cheap and nodes are insertable
    /// anywhere.
    pub fn set_layer(&mut self, name: impl Into<String>, layer: Arc<Layer>) {
        self.layers.insert(name.into(), layer);
    }

    /// Builder form of [`set_layer`](Self::set_layer).
    #[must_use]
    pub fn with_layer(mut self, name: impl Into<String>, layer: Arc<Layer>) -> Self {
        self.set_layer(name, layer);
        self
    }

    /// Removes and returns the named layer, if present.
    pub fn remove_layer(&mut self, name: &str) -> Option<Arc<Layer>> {
        self.layers.remove(name)
    }

    /// Returns the named layer if present, otherwise a freshly allocated layer of
    /// this field's resolution filled with `default`.
    ///
    /// This is the soft-contract accessor. A node reads an optional layer (a
    /// mask, say) and degrades gracefully to a constant when it is absent, for
    /// example `field.layer_or(layers::MASK, 1.0)` to apply everywhere.
    #[must_use]
    pub fn layer_or(&self, name: &str, default: f32) -> Arc<Layer> {
        match self.layers.get(name) {
            Some(layer) => Arc::clone(layer),
            None => Arc::new(Layer::filled(self.width, self.height, default)),
        }
    }

    /// Returns the named scalar global, or `None` if absent.
    #[must_use]
    pub fn detail(&self, key: &str) -> Option<f64> {
        self.detail.get(key).copied()
    }

    /// Iterates the scalar globals in canonical (sorted) name order, for serialization and
    /// hashing. Mirrors [`layers`](Self::layers).
    pub fn details(&self) -> impl Iterator<Item = (&str, f64)> {
        self.detail
            .iter()
            .map(|(name, &value)| (name.as_str(), value))
    }

    /// Returns the named scalar global, or `default` if absent.
    #[must_use]
    pub fn detail_or(&self, key: &str, default: f64) -> f64 {
        self.detail.get(key).copied().unwrap_or(default)
    }

    /// Sets a scalar global.
    pub fn set_detail(&mut self, key: impl Into<String>, value: f64) {
        self.detail.insert(key.into(), value);
    }

    /// Builder form of [`set_detail`](Self::set_detail).
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: f64) -> Self {
        self.set_detail(key, value);
        self
    }

    /// Canonical content hash over resolution, region, detail, and every layer.
    ///
    /// The hash is independent of the order in which layers or detail were
    /// inserted (the `BTreeMap`s iterate by name) and identical on every machine,
    /// which is what makes it safe as a memoization key and as a golden-snapshot
    /// fingerprint. Lengths are written before variable-length content so that
    /// distinct fields cannot collide by concatenation.
    #[must_use]
    pub fn content_hash(&self) -> ContentHash {
        let mut h = Fnv1a64::new();
        h.write_usize(self.width);
        h.write_usize(self.height);
        self.region.hash_into(&mut h);

        h.write_usize(self.detail.len());
        for (key, value) in &self.detail {
            h.write_str(key);
            h.write_f64_bits(*value);
        }

        h.write_usize(self.layers.len());
        for (name, layer) in &self.layers {
            h.write_str(name);
            layer.hash_into(&mut h);
        }

        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers;

    fn sample() -> Field {
        Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.5)))
            .with_layer(layers::MASK, Arc::new(Layer::filled(16, 16, 1.0)))
    }

    #[test]
    fn pass_through_keeps_other_layers_identical() {
        let original = sample();
        let mut modified = original.clone();
        modified.set_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.9)));

        // The replaced layer is genuinely a different allocation...
        let orig_height = original.layer(layers::HEIGHT).unwrap();
        let new_height = modified.layer(layers::HEIGHT).unwrap();
        assert!(!Arc::ptr_eq(orig_height, new_height));

        // ...while every untouched layer is shared by pointer, not copied.
        let orig_mask = original.layer(layers::MASK).unwrap();
        let new_mask = modified.layer(layers::MASK).unwrap();
        assert!(Arc::ptr_eq(orig_mask, new_mask));
    }

    #[test]
    fn layer_or_returns_the_existing_layer() {
        let field = sample();
        let got = field.layer_or(layers::MASK, 0.0);
        // Soft contract: a present layer is returned as-is (same allocation).
        assert!(Arc::ptr_eq(&got, field.layer(layers::MASK).unwrap()));
    }

    #[test]
    fn layer_or_synthesizes_a_constant_when_absent() {
        let field = sample();
        let got = field.layer_or("nonexistent", 0.75);
        assert_eq!(got.width(), field.width());
        assert_eq!(got.height(), field.height());
        assert!(got.as_slice().iter().all(|&v| v == 0.75));
    }

    #[test]
    fn detail_or_falls_back_to_default() {
        let mut field = sample();
        assert_eq!(field.detail_or("vertical_scale", 1.0), 1.0);
        field.set_detail("vertical_scale", 1234.5);
        assert_eq!(field.detail("vertical_scale"), Some(1234.5));
        assert_eq!(field.detail_or("vertical_scale", 1.0), 1234.5);
    }

    #[test]
    fn content_hash_is_stable_across_repeated_calls() {
        let field = sample();
        assert_eq!(field.content_hash(), field.content_hash());
    }

    #[test]
    fn content_hash_is_independent_of_insertion_order() {
        let height = Arc::new(Layer::filled(16, 16, 0.5));
        let mask = Arc::new(Layer::filled(16, 16, 1.0));

        let a = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::clone(&height))
            .with_layer(layers::MASK, Arc::clone(&mask));
        let b = Field::new(16, 16, Region::UNIT)
            .with_layer(layers::MASK, mask)
            .with_layer(layers::HEIGHT, height);

        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn content_hash_reflects_region_and_detail() {
        let base = sample();

        let mut other_region = Field::new(16, 16, Region::new(0.0, 0.0, 2.0, 2.0))
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(16, 16, 0.5)))
            .with_layer(layers::MASK, Arc::new(Layer::filled(16, 16, 1.0)));
        assert_ne!(base.content_hash(), other_region.content_hash());

        other_region.set_detail("seed", 7.0);
        let hash_without_detail = base.content_hash();
        let mut with_detail = base.clone();
        with_detail.set_detail("seed", 7.0);
        assert_ne!(hash_without_detail, with_detail.content_hash());
    }

    #[test]
    fn content_hash_matches_golden_value() {
        // A fixed fingerprint of a known field. Because the hash algorithm and
        // its byte layout are specified (FNV-1a, length-prefixed, little-endian),
        // this value is the same on every machine and across toolchain versions.
        // If it ever changes, the canonical hashing changed, which would silently
        // invalidate memo caches and golden snapshots; that must be deliberate.
        let field = Field::new(8, 8, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(8, 8, 0.5)))
            .with_detail("seed", 42.0);
        assert_eq!(field.content_hash().to_u64(), 0xb09e_9c8a_a5cc_e630);
    }
}
