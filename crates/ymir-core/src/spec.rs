//! Node schema: ports, the node spec, and the arity-derived category.

use crate::param::ParamSpec;

/// The schema for one input or output port. Every port carries a
/// [`Field`](crate::Field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSpec {
    /// Port name, used for labelling and wiring.
    pub name: String,
}

impl PortSpec {
    /// Creates a port schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// What role a node plays, derived purely from its arity. It is never stored, so
/// the engine cannot branch on a hand-kept category enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// No inputs: produces a field from nothing (a source).
    Generator,
    /// Inputs and outputs: transforms fields.
    Modifier,
    /// No outputs: consumes a field (a sink, such as export).
    Endpoint,
}

/// A node type's full schema: identity, label, ports, and parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSpec {
    /// Stable type identifier, also the registry key (e.g. `"generator.fbm"`).
    pub type_id: &'static str,
    /// Human-facing label for palettes and the node editor.
    pub label: String,
    /// Input ports, in order.
    pub inputs: Vec<PortSpec>,
    /// Output ports, in order.
    pub outputs: Vec<PortSpec>,
    /// Parameter schema.
    pub params: Vec<ParamSpec>,
}

impl NodeSpec {
    /// The node's [`Category`], derived from arity. No outputs means an endpoint
    /// (a sink) even if it also takes no inputs; no inputs with outputs is a
    /// generator; anything with both is a modifier.
    #[must_use]
    pub fn category(&self) -> Category {
        if self.outputs.is_empty() {
            Category::Endpoint
        } else if self.inputs.is_empty() {
            Category::Generator
        } else {
            Category::Modifier
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(inputs: usize, outputs: usize) -> NodeSpec {
        NodeSpec {
            type_id: "test.node",
            label: "Test".into(),
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
    fn category_is_derived_from_arity() {
        assert_eq!(spec(0, 1).category(), Category::Generator);
        assert_eq!(spec(1, 1).category(), Category::Modifier);
        assert_eq!(spec(2, 1).category(), Category::Modifier);
        assert_eq!(spec(1, 0).category(), Category::Endpoint);
    }
}
