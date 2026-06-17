//! The node behavior trait.

use crate::context::EvalContext;
use crate::error::Result;
use crate::field::Field;
use crate::param::Params;
use crate::spec::NodeSpec;

/// Stateless node behavior plus its schema.
///
/// The engine depends only on this trait and holds `Box<dyn Operator>`; it never
/// names a concrete operator type. Per-instance configuration lives in the graph
/// (params and connections), not in the operator, which keeps operators stateless
/// and memoization clean.
pub trait Operator {
    /// The node's schema: identity, ports, and parameters.
    fn spec(&self) -> NodeSpec;

    /// Evaluates the node.
    ///
    /// `inputs` are the upstream fields in port order (empty for a generator),
    /// `params` is this instance's configuration, and `ctx` carries resolution,
    /// region, and seed. Returns one field per output port.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`](crate::Error) if the node cannot produce its output;
    /// the evaluator surfaces that as a failed node rather than panicking.
    fn eval(&self, inputs: &[&Field], params: &Params, ctx: &EvalContext) -> Result<Vec<Field>>;
}
