//! Shared erosion byproducts.
//!
//! The universal `wear`/`deposition` pair every erosion node emits: the change the erosion made
//! to the bed, split into where it stripped material (`wear`) and where material settled
//! (`deposition`). It is computed from a before/after height pair, so any model gets it nearly
//! free. A model that tracks eroded and deposited material explicitly (the grid hydraulic's
//! erode/deposit terms, the stream model's sediment flux) should emit that tracked quantity
//! instead, since it is more accurate than the net height difference. See
//! `design/ymir-erosion-DESIGN.md` for the layer vocabulary this serves.

use std::sync::Arc;

use ymir_core::{Field, Layer, Region, layers};

/// Splits the erosion change (`after - before`) into the two non-negative byproduct layers:
/// `wear[i]` is how far the bed dropped where it was cut, `deposition[i]` how far it rose where
/// material settled. Exactly one is non-zero per cell (both zero where the bed is unchanged), so
/// the pair reconstructs the signed change as `deposition - wear`. Returned in that order, each
/// the length of the inputs.
pub(crate) fn wear_and_deposition(before: &[f32], after: &[f32]) -> (Vec<f32>, Vec<f32>) {
    debug_assert_eq!(
        before.len(),
        after.len(),
        "before/after come from the same field and must match in length",
    );
    before
        .iter()
        .zip(after)
        .map(|(&b, &a)| {
            let change = a - b;
            if change >= 0.0 {
                (0.0, change)
            } else {
                (-change, 0.0)
            }
        })
        .unzip()
}

/// Wraps a per-cell byproduct into a standalone [`Field`] carrying it on the height layer: the
/// form an erosion node's byproduct output port takes, so downstream nodes shape and read it the
/// same way they do a heightfield.
pub(crate) fn byproduct_field(
    values: Vec<f32>,
    width: usize,
    height: usize,
    region: Region,
) -> Field {
    Field::new(width, height, region).with_layer(
        layers::HEIGHT,
        Arc::new(Layer::from_vec(width, height, values)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_change_into_disjoint_wear_and_deposition() {
        // A cell that rose (0.2 -> 0.5) deposits; one that dropped (0.8 -> 0.3) wears; one
        // unchanged does neither.
        let before = [0.2, 0.8, 0.4];
        let after = [0.5, 0.3, 0.4];
        let (wear, deposition) = wear_and_deposition(&before, &after);
        assert_eq!(wear, vec![0.0, 0.5, 0.0]);
        assert_eq!(deposition, vec![0.3, 0.0, 0.0]);
        // Every cell has at most one side non-zero.
        assert!(
            wear.iter()
                .zip(&deposition)
                .all(|(&w, &d)| w == 0.0 || d == 0.0),
            "wear and deposition must be disjoint per cell",
        );
    }

    #[test]
    fn reconstructs_the_signed_change() {
        let before = [0.2, 0.8, 0.4, 0.0];
        let after = [0.5, 0.3, 0.4, 1.0];
        let (wear, deposition) = wear_and_deposition(&before, &after);
        for i in 0..before.len() {
            let reconstructed = deposition[i] - wear[i];
            assert!((reconstructed - (after[i] - before[i])).abs() < 1e-7);
        }
    }

    #[test]
    fn byproduct_field_carries_values_on_height() {
        let field = byproduct_field(vec![0.1, 0.2, 0.3, 0.4], 2, 2, Region::UNIT);
        let layer = field.layer(layers::HEIGHT).unwrap();
        assert_eq!(layer.get(1, 1).unwrap(), 0.4);
    }
}
