//! Placeholder for a node whose operator cannot be rebuilt on load.
//!
//! When a project references a `type_id` the registry does not know (a node removed or renamed
//! since the file was written), the loader must not throw the whole project away. Instead it
//! substitutes a [`MissingOperator`]: a stand-in that preserves the original `type_id`, the
//! node's params, and enough ports for its saved connections to reattach, so the project opens,
//! the rest of the graph works, and re-saving keeps the node's data intact. The placeholder
//! evaluates to an error (surfaced red, like any failing node) rather than producing output, and
//! it is never in the palette — it exists only to hold a slot open. See the file-format-stability
//! section of `CLAUDE.md`: a node change must not orphan existing projects.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use crate::{Error, EvalContext, Field, Inputs, NodeSpec, Operator, Params, PortSpec, Result};

/// The palette category a placeholder reports. Never shown (placeholders are load-only), but a
/// distinct value keeps them from masquerading as any real category.
const MISSING_CATEGORY: &str = "missing";

/// Interns a `type_id` as `&'static str`, so a placeholder can carry an arbitrary type id from a
/// file through the spec system (which uses `&'static str`). Each distinct id is leaked exactly
/// once for the life of the process and reused thereafter, so a project loaded any number of
/// times adds at most one small allocation per unknown type — bounded, not a per-load leak.
pub(crate) fn intern_type_id(type_id: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    // Recover from a poisoned lock rather than panicking: the interner holds no invariant a
    // panic could have broken, so the set is still usable.
    let mut set = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&existing) = set.get(type_id) {
        return existing;
    }
    let leaked: &'static str = Box::leak(type_id.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// A stand-in for a node whose real operator is unavailable. It carries the original (interned)
/// `type_id` so serialization round-trips faithfully, and a port count inferred from the saved
/// connections so those connections still land.
#[derive(Clone)]
pub(crate) struct MissingOperator {
    type_id: &'static str,
    inputs: usize,
    outputs: usize,
}

impl MissingOperator {
    /// A placeholder for `type_id` (already interned) with the given inferred port counts.
    pub(crate) fn new(type_id: &'static str, inputs: usize, outputs: usize) -> Self {
        Self {
            type_id,
            inputs,
            outputs,
        }
    }
}

impl Operator for MissingOperator {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: self.type_id,
            category: MISSING_CATEGORY,
            inputs: (0..self.inputs)
                .map(|i| PortSpec::new(format!("in{i}")))
                .collect(),
            outputs: (0..self.outputs)
                .map(|i| PortSpec::new(format!("out{i}")))
                .collect(),
            params: Vec::new(),
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    fn eval(&self, _inputs: Inputs, _params: &Params, _ctx: &EvalContext) -> Result<Vec<Field>> {
        Err(Error::UnknownNodeType {
            type_id: self.type_id.to_string(),
        })
    }
}
