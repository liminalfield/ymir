//! Normalized region bounds.

use crate::hash::Fnv1a64;

/// The world-space rectangular bounds a [`Field`](crate::Field) covers.
///
/// Carrying the region on the field is what makes operations resolution- and
/// region-independent: a sampled node reads world coordinates derived from
/// `region`, not pixel indices, so the same world coordinates yield the same
/// value at any resolution. Bounds are `f64` for precision over large extents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// Minimum x bound.
    pub min_x: f64,
    /// Minimum y bound.
    pub min_y: f64,
    /// Maximum x bound.
    pub max_x: f64,
    /// Maximum y bound.
    pub max_y: f64,
}

impl Region {
    /// The unit square `[0, 1] x [0, 1]`.
    pub const UNIT: Self = Self {
        min_x: 0.0,
        min_y: 0.0,
        max_x: 1.0,
        max_y: 1.0,
    };

    /// Creates a region from its bounds.
    #[must_use]
    pub const fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    /// Span along x (`max_x - min_x`).
    #[must_use]
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    /// Span along y (`max_y - min_y`).
    #[must_use]
    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_f64_bits(self.min_x);
        h.write_f64_bits(self.min_y);
        h.write_f64_bits(self.max_x);
        h.write_f64_bits(self.max_y);
    }
}

impl Default for Region {
    fn default() -> Self {
        Self::UNIT
    }
}
