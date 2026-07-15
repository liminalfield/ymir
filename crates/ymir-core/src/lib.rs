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
//! - [`project`]: the versioned, git-friendly on-disk form of a [`Graph`] and its
//!   conversion to and from it.
//!
//! Concrete operators live in `ymir-nodes`, which depends on this crate.

pub mod export;
pub mod import;
pub mod layers;
pub mod logging;
pub mod project;
pub mod registry;

mod cancel;
mod context;
mod error;
mod eval;
mod field;
mod field_cache;
mod field_store;
mod graph;
mod hash;
mod layer;
mod missing;
mod operator;
mod param;
mod region;
mod spec;
mod subgraph;

pub use cancel::CancelToken;
pub use context::EvalContext;
pub use error::{Error, Result};
pub use eval::{EvalCache, EvalRequest};
pub use field::Field;
pub use field_cache::{read_fields, write_fields};
pub use field_store::FieldStore;
pub use graph::{Extraction, Graph, NodeId};
pub use hash::ContentHash;
pub use layer::Layer;
pub use operator::{ContextDeps, Inputs, Operator, OperatorClone};
pub use param::{Curve, ParamKind, ParamSpec, ParamValue, Params, Scale, Unit};
pub use project::{Connection, FORMAT_VERSION, NodeDocument, ProjectDocument};
pub use region::Region;
pub use spec::{NodeKind, NodeSpec, PortSpec};
pub use subgraph::{INPUT_TYPE_ID, OUTPUT_TYPE_ID, SUBGRAPH_TYPE_ID, marker_port_label};
