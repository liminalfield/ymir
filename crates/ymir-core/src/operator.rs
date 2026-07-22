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

/// Which [`EvalContext`] fields a node's output depends on, so the memo cache keys only
/// those and a change to a world setting the node ignores does not invalidate it.
///
/// Only the world globals and the seed are represented here. Resolution and region are
/// *always* keyed (nearly every node's output depends on them, so making them declarable
/// would be risk for no gain) and so are not fields of this type.
///
/// The default is [`ALL`](Self::ALL): a node that does not narrow this is keyed on every
/// field, exactly as if this mechanism did not exist. That is the safe default, because
/// the one direction that corrupts the cache is *under*-declaring (dropping a field the
/// node actually reads leaves a stale field memoized). A node narrows this only once its
/// independence from a field is established, and [`Operator::context_deps`] is where it
/// declares the narrower set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextDeps {
    /// The node reads the derived per-node seed (via [`EvalContext::seed`] and the noise
    /// it drives). Generators set this; a pure transform of its input does not.
    pub seed: bool,
    /// The node reads the world horizontal extent, directly or through
    /// [`EvalContext::meters_per_cell`] / [`world_to_cells`](EvalContext::world_to_cells)
    /// (any node that interprets a param in world units: a blur radius, a warp amount).
    pub world_extent: bool,
    /// The node reads the world vertical extent, directly via
    /// [`EvalContext::world_height`] or through
    /// [`real_slope_scale`](EvalContext::real_slope_scale) (slope-aware erosion, metric
    /// export, the coastal grade).
    pub world_height: bool,
    /// The node reads the world sea level via [`EvalContext::sea_level`] (the coastal
    /// bevel today; base-level-aware river grading later).
    pub sea_level: bool,
}

impl ContextDeps {
    /// Depends on every keyable field. The default, and the only safe choice for a node
    /// whose field dependence has not been deliberately narrowed.
    pub const ALL: Self = Self {
        seed: true,
        world_extent: true,
        world_height: true,
        sea_level: true,
    };

    /// Depends on no world global: the node's output is a pure function of its inputs, params,
    /// resolution, region, and seed. A generator sampling normalized coordinates, or a per-cell
    /// height transform, is `NO_WORLD`. The seed stays depended-on (its narrowing is deferred),
    /// so a world-setting slider does not invalidate the node but a reseed still does.
    pub const NO_WORLD: Self = Self {
        world_extent: false,
        world_height: false,
        sea_level: false,
        ..Self::ALL
    };

    /// Depends on the world *horizontal* extent (a param sized in world units, read through
    /// [`meters_per_cell`](crate::EvalContext::meters_per_cell) /
    /// [`world_to_cells`](crate::EvalContext::world_to_cells)), but not the vertical extent or the
    /// sea level. A blur radius or a warp amount is `WORLD_EXTENT`.
    pub const WORLD_EXTENT: Self = Self {
        world_height: false,
        sea_level: false,
        ..Self::ALL
    };

    /// Depends on the world's vertical *and* horizontal extent (a slope-aware node, read through
    /// [`real_slope_scale`](crate::EvalContext::real_slope_scale)), but not the sea level. A talus
    /// angle or a slope selection is `SLOPE`.
    pub const SLOPE: Self = Self {
        sea_level: false,
        ..Self::ALL
    };
}

impl Default for ContextDeps {
    fn default() -> Self {
        Self::ALL
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

    /// An optional content fingerprint folded into this node's cache key, for an operator
    /// whose output depends on per-instance data beyond its [`params`](Params) (a subgraph
    /// container's inner graph). `None` (the default) for an ordinary stateless operator,
    /// which leaves its cache key exactly as it was. Must be stable for equal content and
    /// machine-independent (the same guarantees as the rest of the key), and cheap to
    /// return, since the evaluator calls it whenever it computes a key (precompute it).
    fn content_hash(&self) -> Option<u64> {
        None
    }

    /// Which [`EvalContext`] fields this node's output depends on, so the memo cache keys
    /// only those. [`ALL`](ContextDeps::ALL) (the default) keys every field, exactly as
    /// before this existed, and is the safe choice: dropping a field the node actually
    /// reads would memoize a stale result. A node overrides this to exclude a world
    /// setting it provably ignores, so changing that setting does not invalidate it (nor,
    /// transitively, anything downstream of it). Cheap to return, since the evaluator calls
    /// it whenever it computes a key.
    fn context_deps(&self) -> ContextDeps {
        ContextDeps::ALL
    }

    /// Whether this node is experimental: fully functional, but rough or artifact-prone enough
    /// that it is offered with a caveat rather than as a settled tool. `false` (the default) for
    /// ordinary nodes. The engine does not treat these differently; it is presentation only, so the
    /// editor can badge them and a user knows what they are reaching for. A node opts in by
    /// overriding this, exactly as it narrows [`context_deps`](Self::context_deps).
    fn experimental(&self) -> bool {
        false
    }

    /// The inner graph this operator contains, if it is a container (a subgraph), so the
    /// document writer can capture it and the editor can recurse into it. `None` (the
    /// default) for an ordinary operator. A structural distinction, not semantic dispatch:
    /// a caller asks "does this hold a graph?", never "which node is this?".
    fn nested(&self) -> Option<&crate::graph::Graph> {
        None
    }

    /// Rebuilds this operator with `inner` installed as its nested graph, used when loading
    /// a saved container. The default ignores `inner` and clones self, which is correct for
    /// an ordinary operator (one never carries nested data in a well-formed file); a
    /// container returns a fresh instance wrapping `inner`. Keeping this on the trait is
    /// what lets the document loader restore a container without naming its concrete type.
    fn rebuild_nested(&self, _inner: crate::graph::Graph) -> Box<dyn Operator> {
        self.clone_box()
    }
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
