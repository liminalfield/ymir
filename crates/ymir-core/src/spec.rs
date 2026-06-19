//! Node schema: ports, the node spec, and the arity-derived kind.

use crate::param::ParamSpec;

/// The schema for one input or output port. Every port carries a
/// [`Field`](crate::Field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSpec {
    /// Port name, used for wiring (and, in the GUI, as a localized label key).
    pub name: String,
}

impl PortSpec {
    /// Creates a port schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
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
    /// Optional search tags.
    pub tags: &'static [&'static str],
    /// Input ports, in order.
    pub inputs: Vec<PortSpec>,
    /// Output ports, in order.
    pub outputs: Vec<PortSpec>,
    /// Parameter schema.
    pub params: Vec<ParamSpec>,
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
            tags: &[],
            inputs: (0..inputs)
                .map(|i| PortSpec::new(format!("in{i}")))
                .collect(),
            outputs: (0..outputs)
                .map(|i| PortSpec::new(format!("out{i}")))
                .collect(),
            params: vec![],
        }
    }

    #[test]
    fn kind_is_derived_from_arity() {
        assert_eq!(spec(0, 1).kind(), NodeKind::Generator);
        assert_eq!(spec(1, 1).kind(), NodeKind::Modifier);
        assert_eq!(spec(2, 1).kind(), NodeKind::Modifier);
        assert_eq!(spec(1, 0).kind(), NodeKind::Endpoint);
    }
}
