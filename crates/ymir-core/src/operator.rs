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
///
/// `Send + Sync` is required so the graph can be evaluated on a worker thread (the
/// GUI runs evaluation off the UI thread). Operators are stateless, so this is a
/// natural fit; if one ever needs interior state, it must be thread-safe.
///
/// `Clone` is required (via the [`OperatorClone`] supertrait) so the whole graph
/// can be cloned into a cheap snapshot and evaluated off-thread without locking the
/// canonical graph the UI is editing. A node implementor only needs
/// `#[derive(Clone)]`; since operators are stateless this is trivial (any heavy
/// per-instance asset belongs behind an `Arc`).
pub trait Operator: OperatorClone + Send + Sync {
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

/// Clones a `Box<dyn Operator>`. Blanket-implemented for every `Clone` operator, so
/// node authors implement [`Operator`] and derive `Clone`, never this trait
/// directly. It exists only to make the boxed trait object cloneable, which is what
/// lets the graph be snapshotted for off-thread evaluation.
pub trait OperatorClone {
    /// Clones `self` into a fresh boxed operator.
    fn clone_box(&self) -> Box<dyn Operator>;
}

impl<T> OperatorClone for T
where
    T: Operator + Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn Operator> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Operator> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}
