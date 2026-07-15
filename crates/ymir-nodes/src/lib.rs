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

mod billow;
mod blend;
mod blur;
mod category;
mod cellular_bumps;
mod cellular_cracks;
mod cellular_regions;
mod coastal;
mod curvature;
mod curve;
mod distance;
mod erosion;
mod export;
mod export_exr;
mod export_r16;
mod expr;
mod expression;
mod falloff;
mod fbm;
mod flow;
mod gradient;
mod height;
mod hybrid;
mod hydraulic;
mod hydrology;
mod import;
mod invert;
mod levels;
mod noise;
mod null;
mod polygon;
mod radial;
mod rect;
mod ridged;
mod ring;
mod shape;
mod slope;
mod stream;
mod strings;
mod talus;
mod thermal;
mod warp;

pub use billow::Billow;
pub use blend::Blend;
pub use blur::Blur;
pub use category::{CategoryDef, NodeGroup, categories, find_category, node_group};
pub use cellular_bumps::CellularBumps;
pub use cellular_cracks::CellularCracks;
pub use cellular_regions::CellularRegions;
pub use coastal::Coastal;
pub use curvature::Curvature;
pub use curve::CurveNode;
pub use export::ExportPng;
pub use expression::Expression;
pub use falloff::Falloff;
pub use fbm::Fbm;
pub use flow::Flow;
pub use gradient::Gradient;
pub use height::Height;
pub use hybrid::Hybrid;
pub use hydraulic::HydraulicErosion;
pub use import::Import;
pub use invert::Invert;
pub use levels::Levels;
pub use null::Null;
pub use polygon::Polygon;
pub use radial::Radial;
pub use rect::Rect;
pub use ridged::Ridged;
pub use ring::Ring;
pub use slope::Slope;
pub use stream::StreamErosion;
pub use strings::tr;
pub use thermal::ThermalErosion;
pub use warp::Warp;
