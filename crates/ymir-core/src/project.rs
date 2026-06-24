//! The on-disk project document: a versioned, git-friendly serialization of a
//! [`Graph`](crate::Graph).
//!
//! The runtime graph holds live operators and generational `NodeId`s, neither of
//! which is serialized. This module defines a plain, serde-serializable schema that
//! mirrors only the persistent state: each node's `stable_id`, `type_id`, optional
//! name, params, and its input connections expressed by source `stable_id`. A
//! [`Graph`](crate::Graph) converts to this document via
//! [`Graph::to_document`](crate::Graph::to_document) and back via `from_document`
//! (a later step); operators are rebuilt from `type_id` through the registry, so the
//! document never names a concrete node type in code.
//!
//! Stability and diffs: the document carries a
//! [`format_version`](ProjectDocument::format_version) and is decoupled from the
//! runtime types, so the engine can evolve without orphaning saved projects. Output
//! is deterministically ordered (nodes by `stable_id`, params by name, connections by
//! input port), so a project diffs cleanly in version control.

use serde::{Deserialize, Serialize};

use crate::param::Params;

/// The current on-disk format version. Bumped on a breaking schema change, paired
/// with a migration path so existing projects still load.
pub const FORMAT_VERSION: u32 = 1;

/// A serialized project: the persistent form of a [`Graph`](crate::Graph).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDocument {
    /// On-disk schema version; see [`FORMAT_VERSION`].
    pub format_version: u32,
    /// The next `stable_id` the graph would assign, preserved so nodes added after a
    /// load cannot collide with loaded ids.
    pub next_stable_id: u64,
    /// The nodes, in ascending `stable_id` order for stable diffs.
    pub nodes: Vec<NodeDocument>,
}

/// One serialized node: its persistent identity, type, and wiring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDocument {
    /// Persistent identity, the only node identity that is serialized (never the
    /// runtime `NodeId`).
    pub stable_id: u64,
    /// The operator's registered type id, rebuilt through the registry on load.
    pub type_id: String,
    /// Optional display-name override; omitted from the file when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// This instance's parameters; omitted from the file when empty.
    #[serde(default, skip_serializing_if = "Params::is_empty")]
    pub params: Params,
    /// The node's input connections, sorted by input port. Only connected ports
    /// appear, so an unconnected node carries an empty list (omitted from the file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<Connection>,
    /// Whether the node is bypassed (transparent). Defaults to `false` and is omitted
    /// from the file when not bypassed, so existing projects load unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypassed: bool,
}

/// Serde predicate: omit a `bool` field from the file when it is `false`.
fn is_false(value: &bool) -> bool {
    !*value
}

/// One input connection: which input port of the owning node is fed by which output
/// port of which source node (named by `stable_id`, not the runtime `NodeId`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// The destination input port on the owning node.
    pub input: usize,
    /// The source node's `stable_id`.
    pub source: u64,
    /// The source node's output port.
    pub output: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param::{Curve, ParamValue};

    /// A representative document with a name override, several param kinds, and a
    /// connection, used to exercise the serde wiring end to end.
    fn sample_document() -> ProjectDocument {
        let params = Params::new()
            .with("frequency", ParamValue::Float(2.5))
            .with("octaves", ParamValue::Int(6))
            .with("enabled", ParamValue::Bool(true))
            .with("label", ParamValue::Text("ridge".into()))
            .with(
                "curve",
                ParamValue::Curve(Curve::new([(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)])),
            );
        ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 2,
            nodes: vec![
                NodeDocument {
                    stable_id: 0,
                    type_id: "generator.fbm".into(),
                    name: None,
                    params,
                    connections: Vec::new(),
                    bypassed: false,
                },
                NodeDocument {
                    stable_id: 1,
                    type_id: "endpoint.export".into(),
                    name: Some("Final".into()),
                    params: Params::new(),
                    connections: vec![Connection {
                        input: 0,
                        source: 0,
                        output: 0,
                    }],
                    bypassed: true,
                },
            ],
        }
    }

    #[test]
    fn document_round_trips_through_json() {
        let doc = sample_document();
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: ProjectDocument = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(doc, back);
    }

    #[test]
    fn empty_fields_are_omitted_from_the_json() {
        // The fbm node has no name and no connections; the export node has empty
        // params. Each absent field should be skipped rather than written.
        let doc = sample_document();
        let json = serde_json::to_value(&doc).expect("serialize");
        let fbm = &json["nodes"][0];
        assert!(fbm.get("name").is_none(), "an unset name is omitted");
        assert!(
            fbm.get("connections").is_none(),
            "no connections is omitted"
        );
        // The export node has empty params, which should be omitted.
        let export = &json["nodes"][1];
        assert!(export.get("params").is_none(), "empty params is omitted");
        // The fbm node is not bypassed, so the flag is omitted; the export node is, so
        // it is written.
        assert!(
            fbm.get("bypassed").is_none(),
            "a not-bypassed node omits the flag"
        );
        assert_eq!(
            export.get("bypassed"),
            Some(&serde_json::json!(true)),
            "a bypassed node writes the flag"
        );
    }

    #[test]
    fn param_values_serialize_with_snake_case_tags() {
        let json = serde_json::to_value(ParamValue::Float(1.5)).expect("serialize");
        assert_eq!(json, serde_json::json!({ "float": 1.5 }));
        let json = serde_json::to_value(ParamValue::Text("x".into())).expect("serialize");
        assert_eq!(json, serde_json::json!({ "text": "x" }));
    }

    #[test]
    fn a_curve_round_trips_and_is_resanitized_on_load() {
        // A curve serializes as its points; loading an out-of-range, unsorted list
        // rebuilds through Curve::new, yielding the sanitized, sorted curve.
        let messy = serde_json::json!([[1.0, 2.0], [0.5, -1.0], [0.0, 0.5]]);
        let curve: Curve = serde_json::from_value(messy).expect("deserialize");
        assert_eq!(curve.points(), &[(0.0, 0.5), (0.5, 0.0), (1.0, 1.0)]);
    }
}
