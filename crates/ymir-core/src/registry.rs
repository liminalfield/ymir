//! The operator registry: a distributed collection point with no central enum.
//!
//! Concrete operators live in downstream crates (`ymir-nodes`) and register an
//! [`OperatorEntry`] with `inventory::submit!`. This crate collects them, so
//! adding a node touches only its own file.
//!
//! Linking caveat: the entries live in the operator's crate, and a binary that
//! only calls [`make`] references *this* crate, not the operator's. Nothing then
//! forces the operator crate to link, so its registrations can be dropped. A
//! binary must anchor the operator crate explicitly (`use ymir_nodes as _;`); the
//! node-count smoke test guards against a silent drop.

use crate::operator::Operator;

/// A registered operator type: its stable id and a constructor.
pub struct OperatorEntry {
    /// Stable type identifier, matching the operator's [`NodeSpec::type_id`].
    ///
    /// [`NodeSpec::type_id`]: crate::NodeSpec::type_id
    pub type_id: &'static str,
    /// Constructs a fresh boxed instance of the operator.
    pub make: fn() -> Box<dyn Operator>,
}

inventory::collect!(OperatorEntry);

/// Constructs the operator registered under `type_id`, or `None` if unknown.
#[must_use]
pub fn make(type_id: &str) -> Option<Box<dyn Operator>> {
    inventory::iter::<OperatorEntry>()
        .find(|entry| entry.type_id == type_id)
        .map(|entry| (entry.make)())
}

/// Iterates every registered entry. Order is link-dependent and therefore not
/// deterministic; sort by `type_id` before using this for anything that affects
/// output or a stable display order.
pub fn entries() -> impl Iterator<Item = &'static OperatorEntry> {
    inventory::iter::<OperatorEntry>()
}

/// The number of registered operators.
#[must_use]
pub fn count() -> usize {
    inventory::iter::<OperatorEntry>().count()
}
