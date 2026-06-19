//! Concrete operators (the nodes) for Ymir.
//!
//! Each operator is one module: an [`Operator`](ymir_core::Operator) impl plus an
//! `inventory::submit!` that registers it with `ymir-core`'s registry. The engine
//! never names these types; it reaches them only through the registry and
//! `dyn Operator`. Terrain math (noise, and later erosion) lives here too, beside
//! the operators that use it, so the engine crate stays free of it.
//!
//! Because registration is link-time, a binary using these nodes must anchor this
//! crate explicitly (`use ymir_nodes as _;`); merely calling the registry by
//! string does not reference this crate and would let the linker drop it.

mod category;
mod combine;
mod export;
mod fbm;
mod invert;
mod mask;
mod noise;
mod strings;
mod thermal;

pub use category::{CategoryDef, categories, find_category};
pub use combine::Combine;
pub use export::ExportPng;
pub use fbm::Fbm;
pub use invert::Invert;
pub use mask::Mask;
pub use strings::tr;
pub use thermal::ThermalErosion;
