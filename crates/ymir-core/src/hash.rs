//! Canonical, deterministic content hashing.
//!
//! Memoization keys, golden snapshot tests, and save/load all depend on a hash
//! that is identical on every machine and stable across toolchain versions. We
//! therefore specify the algorithm explicitly (FNV-1a, 64-bit) rather than
//! relying on [`std::hash::DefaultHasher`], whose output is not guaranteed
//! stable across Rust releases and would silently invalidate golden tests on a
//! compiler bump.

use core::fmt;

/// A 64-bit canonical content hash of a [`Field`](crate::Field) or
/// [`Layer`](crate::Layer).
///
/// Two values with equal content always produce an equal `ContentHash`,
/// independent of the order in which layers or detail were inserted and
/// independent of the machine it runs on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash(u64);

impl ContentHash {
    /// Returns the raw 64-bit hash value.
    #[must_use]
    pub fn to_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({:#018x})", self.0)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Incremental FNV-1a (64-bit) accumulator.
///
/// Values are folded in by writing their canonical little-endian byte form.
/// Callers are responsible for writing length prefixes where needed so that
/// distinct structures cannot collide by concatenation.
pub(crate) struct Fnv1a64 {
    state: u64,
}

impl Fnv1a64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub(crate) fn new() -> Self {
        Self {
            state: Self::OFFSET_BASIS,
        }
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= u64::from(b);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }

    pub(crate) fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    pub(crate) fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    pub(crate) fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }

    pub(crate) fn write_f32_bits(&mut self, v: f32) {
        self.write_u32(v.to_bits());
    }

    pub(crate) fn write_f64_bits(&mut self, v: f64) {
        self.write_u64(v.to_bits());
    }

    /// Writes a length-prefixed string, so two adjacent strings cannot collide
    /// with a single longer one.
    pub(crate) fn write_str(&mut self, s: &str) {
        self.write_usize(s.len());
        self.write_bytes(s.as_bytes());
    }

    pub(crate) fn finish(&self) -> ContentHash {
        ContentHash(self.state)
    }
}
