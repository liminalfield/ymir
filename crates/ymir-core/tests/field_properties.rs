//! Property-based tests for the [`Field`] invariants CLAUDE.md names: pass-through
//! leaves untouched layers identical, `layer_or` returns the default for a missing
//! layer, and canonical hashing is deterministic. These generalize the fixed-example
//! unit tests in `field.rs` over arbitrary fields, so an edge the examples miss (odd
//! dimensions, an unusual layer bag, non-finite cells) still exercises the contract.
//!
//! The generator deliberately produces non-finite cell values (NaN, infinities,
//! signed zero, subnormals). Layer cells hash by raw bit pattern, so those values
//! must not perturb determinism, and the hashing tests here prove it. Operator-level
//! "evaluation is deterministic" across every node is a separate sweep in `ymir-nodes`;
//! this file covers the data-model foundation those nodes rest on.

use std::sync::Arc;

use proptest::prelude::*;
use ymir_core::{Field, Layer, Region, layers};

/// A layer name drawn from a small realistic pool, so generated fields collide and
/// overlap the way real graphs do rather than each holding unique throwaway names.
fn arb_layer_name() -> impl Strategy<Value = String> {
    prop::sample::select(
        &[
            layers::HEIGHT,
            layers::MASK,
            "flow",
            "water",
            "sediment",
            "wear",
            "custom",
        ][..],
    )
    .prop_map(str::to_owned)
}

/// A `detail` global key from a small pool, mirroring [`arb_layer_name`].
fn arb_detail_key() -> impl Strategy<Value = String> {
    prop::sample::select(&["seed", "world_height", "vertical_scale"][..]).prop_map(str::to_owned)
}

/// Finite region bounds. Any finite value is fair game: the hash folds the raw bits,
/// so the bounds need not describe a positive-area rectangle for these invariants.
fn arb_region() -> impl Strategy<Value = Region> {
    let coord = -1_000.0_f64..=1_000.0;
    (coord.clone(), coord.clone(), coord.clone(), coord)
        .prop_map(|(min_x, min_y, max_x, max_y)| Region::new(min_x, min_y, max_x, max_y))
}

/// An arbitrary field: small odd-friendly resolution, a deduplicated set of named
/// layers each filled with arbitrary `f32` (including non-finite values), and a few
/// scalar globals. Dimensions stay small so the property runs stay fast.
fn arb_field() -> impl Strategy<Value = Field> {
    (1_usize..=8, 1_usize..=8)
        .prop_flat_map(|(width, height)| {
            (
                Just(width),
                Just(height),
                arb_region(),
                prop::collection::hash_set(arb_layer_name(), 0..=5),
                prop::collection::hash_map(arb_detail_key(), -1.0e6_f64..=1.0e6, 0..=3),
            )
        })
        .prop_flat_map(|(width, height, region, names, details)| {
            let names: Vec<String> = names.into_iter().collect();
            let count = names.len();
            let cells = width * height;
            let layer_data =
                prop::collection::vec(prop::collection::vec(any::<f32>(), cells..=cells), count);
            (
                Just(width),
                Just(height),
                Just(region),
                Just(names),
                layer_data,
                Just(details),
            )
        })
        .prop_map(|(width, height, region, names, layer_data, details)| {
            let mut field = Field::new(width, height, region);
            for (name, data) in names.into_iter().zip(layer_data) {
                field.set_layer(name, Arc::new(Layer::from_vec(width, height, data)));
            }
            for (key, value) in details {
                field.set_detail(key, value);
            }
            field
        })
}

proptest! {
    /// Replacing the height layer leaves every other layer shared by pointer, never
    /// copied. This is the pass-through primitive that makes a modifier insertable
    /// anywhere; it must hold for any layer bag, not just the tidy two-layer example.
    #[test]
    fn pass_through_preserves_untouched_layers(field in arb_field()) {
        let (width, height) = (field.width(), field.height());
        let replacement = Arc::new(Layer::filled(width, height, 0.123));

        let mut modified = field.clone();
        modified.set_layer(layers::HEIGHT, Arc::clone(&replacement));

        // Every layer other than the one we replaced is the same allocation.
        for (name, original) in field.layers() {
            if name == layers::HEIGHT {
                continue;
            }
            let after = modified
                .layer(name)
                .expect("a layer present before set_layer is still present after");
            prop_assert!(
                Arc::ptr_eq(original, after),
                "untouched layer `{name}` was copied instead of shared",
            );
        }

        // The replaced layer is exactly the one we set.
        let height_layer = modified
            .layer(layers::HEIGHT)
            .expect("height layer present after set_layer");
        prop_assert!(Arc::ptr_eq(height_layer, &replacement));
    }

    /// `layer_or` synthesizes a constant-filled layer of the field's resolution for a
    /// missing layer, and returns the existing allocation untouched for a present one.
    #[test]
    fn layer_or_defaults_for_missing_and_passes_present(field in arb_field(), default in -1.0e3_f32..=1.0e3) {
        // A name the generator never produces is guaranteed absent.
        let synthesized = field.layer_or("__never_generated__", default);
        prop_assert_eq!(synthesized.width(), field.width());
        prop_assert_eq!(synthesized.height(), field.height());
        prop_assert!(
            synthesized.as_slice().iter().all(|&v| v == default),
            "synthesized layer was not uniformly the default",
        );

        // For any layer that is present, the soft accessor returns it as-is.
        for (name, layer) in field.layers() {
            let got = field.layer_or(name, default);
            prop_assert!(
                Arc::ptr_eq(&got, layer),
                "layer_or copied the present layer `{name}` instead of returning it",
            );
        }
    }

    /// The content hash is a deterministic function of a field's contents: stable
    /// across repeated calls, identical for a clone, and independent of the order in
    /// which layers and details were inserted. This is the property the memo cache and
    /// golden snapshots depend on, and it must survive non-finite cell values.
    #[test]
    fn content_hash_is_deterministic_and_order_independent(field in arb_field()) {
        // Stable across calls, and a clone hashes identically.
        prop_assert_eq!(field.content_hash(), field.content_hash());
        prop_assert_eq!(field.content_hash(), field.clone().content_hash());

        // Rebuild the same field inserting layers and details in reverse order.
        let pairs: Vec<(String, Arc<Layer>)> = field
            .layers()
            .map(|(name, layer)| (name.to_owned(), Arc::clone(layer)))
            .collect();
        let details: Vec<(String, f64)> = field
            .details()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();

        let mut reversed = Field::new(field.width(), field.height(), field.region());
        for (name, layer) in pairs.into_iter().rev() {
            reversed.set_layer(name, layer);
        }
        for (key, value) in details.into_iter().rev() {
            reversed.set_detail(key, value);
        }

        prop_assert_eq!(field.content_hash(), reversed.content_hash());
    }
}
