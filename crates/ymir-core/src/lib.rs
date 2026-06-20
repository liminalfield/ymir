//! Ymir's headless engine core.
//!
//! This crate has no GUI dependencies and holds mechanism only, not concrete
//! nodes. It defines the one data type that flows on every edge of the node
//! graph, [`Field`], together with its building blocks and the node abstraction:
//!
//! - [`Layer`]: a 2D grid of `f32` scalars, the per-cell payload a field carries.
//! - [`Region`]: the field's normalized world-space bounds.
//! - [`layers`]: canonical layer-name constants.
//! - [`ContentHash`]: a canonical, machine-independent fingerprint.
//! - [`Operator`]: the trait all nodes implement; the engine only ever calls
//!   through `dyn Operator` and never names a concrete node.
//! - [`NodeSpec`]/[`ParamSpec`]/[`ParamValue`]/[`Params`]/[`EvalContext`]: the
//!   node schema and per-evaluation context.
//! - [`registry`]: the collection point where downstream crates register their
//!   operators.
//! - [`Graph`]/[`EvalCache`]/[`EvalRequest`]: the node graph and the pull-based,
//!   memoized evaluator.
//! - [`export`]: writing a field's height layer to disk (16-bit grayscale PNG).
//!
//! Concrete operators live in `ymir-nodes`, which depends on this crate.

pub mod export;
pub mod layers;
pub mod registry;

mod cancel;
mod context;
mod error;
mod eval;
mod field;
mod graph;
mod hash;
mod layer;
mod operator;
mod param;
mod region;
mod spec;

pub use cancel::CancelToken;
pub use context::EvalContext;
pub use error::{Error, Result};
pub use eval::{EvalCache, EvalRequest};
pub use field::Field;
pub use graph::{Graph, NodeId};
pub use hash::ContentHash;
pub use layer::Layer;
pub use operator::{Inputs, Operator, OperatorClone};
pub use param::{Curve, ParamKind, ParamSpec, ParamValue, Params};
pub use region::Region;
pub use spec::{NodeKind, NodeSpec, PortSpec};
