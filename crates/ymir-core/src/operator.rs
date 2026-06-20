//! The node behavior trait.

use std::ops::Index;

use crate::context::EvalContext;
use crate::error::Result;
use crate::field::Field;
use crate::param::Params;
use crate::spec::NodeSpec;

/// The upstream fields supplied to [`Operator::eval`], in port order.
///
/// Required inputs (the common case) are reached by index: `inputs[0]` is the first
/// required input, and the evaluator guarantees it is present — a required port that
/// is unconnected fails the evaluation before `eval` is ever called, so indexing one
/// never observes a missing field. Optional inputs, declared after the required
/// ones, are reached with [`optional`](Inputs::optional), which returns `None` when
/// the port is unwired so a node degrades gracefully (the soft-layer contract).
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    required: &'a [&'a Field],
    optional: &'a [Option<&'a Field>],
}

impl<'a> Inputs<'a> {
    /// Builds the input set from required and optional fields. The evaluator uses
    /// this; a driver or test calling an operator directly can too. Required inputs
    /// are dense and all present; the optional slice carries one entry per optional
    /// port, `None` when unconnected.
    #[must_use]
    pub fn new(required: &'a [&'a Field], optional: &'a [Option<&'a Field>]) -> Self {
        Self { required, optional }
    }

    /// Builds an input set from required inputs only, with no optional ports.
    /// Convenience for tests and simple drivers that call an operator directly.
    #[must_use]
    pub fn required_only(required: &'a [&'a Field]) -> Self {
        Self {
            required,
            optional: &[],
        }
    }

    /// The `i`-th optional input (`0` is the first optional port), or `None` when
    /// that port is unconnected.
    #[must_use]
    pub fn optional(&self, i: usize) -> Option<&'a Field> {
        self.optional.get(i).copied().flatten()
    }

    /// The number of required inputs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.required.len()
    }

    /// Whether the node has no required inputs (a generator).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.required.is_empty()
    }
}

impl<'a> Index<usize> for Inputs<'a> {
    type Output = &'a Field;

    /// A required input by port index. The evaluator guarantees required ports are
    /// connected before calling `eval`, so this is the field the upstream produced.
    fn index(&self, port: usize) -> &&'a Field {
        &self.required[port]
    }
}

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
    /// `inputs` are the upstream fields ([`Inputs`]: required by index, optional via
    /// [`Inputs::optional`]); `params` is this instance's configuration; and `ctx`
    /// carries resolution, region, and seed. Returns one field per output port.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`](crate::Error) if the node cannot produce its output;
    /// the evaluator surfaces that as a failed node rather than panicking.
    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>>;
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
