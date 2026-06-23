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

mod blend;
mod blur;
mod category;
mod curvature;
mod curve;
mod export;
mod fbm;
mod gradient;
mod height;
mod invert;
mod levels;
mod noise;
mod radial;
mod ridged;
mod ring;
mod shape;
mod slope;
mod strings;
mod thermal;
mod warp;

pub use blend::Blend;
pub use blur::Blur;
pub use category::{CategoryDef, categories, find_category};
pub use curvature::Curvature;
pub use curve::CurveNode;
pub use export::ExportPng;
pub use fbm::Fbm;
pub use gradient::Gradient;
pub use height::Height;
pub use invert::Invert;
pub use levels::Levels;
pub use radial::Radial;
pub use ridged::Ridged;
pub use ring::Ring;
pub use slope::Slope;
pub use strings::tr;
pub use thermal::ThermalErosion;
pub use warp::Warp;
