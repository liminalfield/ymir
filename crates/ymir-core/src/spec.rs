//! Node schema: ports, the node spec, and the arity-derived kind.

use crate::param::ParamSpec;

/// The schema for one input or output port. Every port carries a
/// [`Field`](crate::Field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSpec {
    /// Port name, used for wiring (and, in the GUI, as a localized label key).
    pub name: String,
    /// Whether this input port may be left unconnected. Required ports (the common
    /// case) error at evaluation when unwired; an optional port degrades gracefully,
    /// reaching the operator as `None`. Ignored for output ports. Optional input
    /// ports must be declared *after* all required ones (the evaluator and
    /// [`Inputs`](crate::Inputs) split inputs at the first optional port).
    pub optional: bool,
}

impl PortSpec {
    /// Creates a required port schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            optional: false,
        }
    }

    /// Creates an optional input port schema. Optional ports must be declared after
    /// all required ports.
    #[must_use]
    pub fn optional(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            optional: true,
        }
    }
}

/// What structural role a node plays, derived purely from its arity. It is never
/// stored, so the engine cannot branch on a hand-kept enum.
///
/// This is distinct from a node's palette *category* (a registered id on
/// [`NodeSpec`]): the kind is engine structure, the category is presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// No inputs: produces a field from nothing (a source).
    Generator,
    /// Inputs and outputs: transforms fields.
    Modifier,
    /// No outputs: consumes a field (a sink, such as export).
    Endpoint,
}

/// A node type's schema: identity, palette category, ports, and parameters.
///
/// The spec holds only ids and keys, never display prose. The human-facing name
/// and description are resolved by convention from `type_id` (`node-<type_id>`,
/// `node-<type_id>-desc`) through the GUI/CLI's `tr(key)` layer, so a node file
/// stays free of UI strings and `ymir-core` stays free of localization.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSpec {
    /// Stable type identifier, also the registry key (e.g. `"generator.fbm"`).
    pub type_id: &'static str,
    /// Palette category id (e.g. `"generator"`) that groups nodes in the editor. A
    /// presentation grouping, registered downstream; the engine never reads it.
    pub category: &'static str,
    /// Input ports, in order.
    pub inputs: Vec<PortSpec>,
    /// Output ports, in order.
    pub outputs: Vec<PortSpec>,
    /// Parameter schema.
    pub params: Vec<ParamSpec>,
    /// The named byproduct data this node produces beyond its primary height, in the canonical
    /// layer vocabulary (`wear`, `flow`, `deposition`, ...). Documentation metadata for the
    /// reference: it names what a node emits for a downstream node to consume, whether carried as
    /// an extra output port or as a layer on the node's field. Empty for a node that emits only
    /// height. The engine never reads this; only the docs generator does.
    pub emitted_layers: Vec<&'static str>,
    /// Whether the node scopes its effect by a mask: it honours a `mask` layer on its input (or a
    /// wired mask input) and applies everywhere the mask is absent. Documentation metadata for the
    /// reference; the graceful degradation itself lives in the operator via `layer_or`, and the
    /// absent-layer behaviour stays prose rather than being encoded here.
    pub mask_aware: bool,
}

impl NodeSpec {
    /// The node's [`NodeKind`], derived from arity. No outputs means an endpoint
    /// (a sink) even if it also takes no inputs; no inputs with outputs is a
    /// generator; anything with both is a modifier.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        if self.outputs.is_empty() {
            NodeKind::Endpoint
        } else if self.inputs.is_empty() {
            NodeKind::Generator
        } else {
            NodeKind::Modifier
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(inputs: usize, outputs: usize) -> NodeSpec {
        NodeSpec {
            type_id: "test.node",
            category: "test",
            inputs: (0..inputs)
                .map(|i| PortSpec::new(format!("in{i}")))
                .collect(),
            outputs: (0..outputs)
                .map(|i| PortSpec::new(format!("out{i}")))
                .collect(),
            params: vec![],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    #[test]
    fn kind_is_derived_from_arity() {
        assert_eq!(spec(0, 1).kind(), NodeKind::Generator);
        assert_eq!(spec(1, 1).kind(), NodeKind::Modifier);
        assert_eq!(spec(2, 1).kind(), NodeKind::Modifier);
        assert_eq!(spec(1, 0).kind(), NodeKind::Endpoint);
    }

    #[test]
    fn metadata_defaults_are_empty() {
        // A node emits nothing and is not mask-aware unless it says so.
        let s = spec(1, 1);
        assert!(s.emitted_layers.is_empty());
        assert!(!s.mask_aware);
    }
}
