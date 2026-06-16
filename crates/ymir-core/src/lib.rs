//! Ymir's headless engine core.
//!
//! This crate has no GUI dependencies, so it stays testable and usable in batch
//! mode. It defines the one data type that flows on every edge of the node
//! graph, [`Field`], together with its building blocks:
//!
//! - [`Layer`]: a 2D grid of `f32` scalars, the per-cell payload a field carries
//!   under a name.
//! - [`Region`]: the field's normalized world-space bounds, the basis for
//!   resolution and region independence.
//! - [`layers`]: canonical layer-name constants, so a typo is a compile error.
//! - [`ContentHash`]: a canonical, machine-independent fingerprint of a field or
//!   layer, the foundation that memoization, golden tests, and save/load build on.
//!
//! Later steps add the `Operator` trait, the node registry, and the evaluator.

mod field;
mod hash;
mod layer;
pub mod layers;
mod region;

pub use field::Field;
pub use hash::ContentHash;
pub use layer::Layer;
pub use region::Region;
