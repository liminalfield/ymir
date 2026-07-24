//! `ymir docs --format json`: serialize the registered node specs to JSON for the documentation
//! generator.
//!
//! The output is emitted from the *running binary* through the operator registry and the `tr`
//! display-string layer, never by parsing source. That is the whole point: a source parse cannot
//! resolve a `tr` key, a `const` default, or an `output_param()` helper, whereas the binary already
//! holds the real, constructed [`NodeSpec`] for every node. The JSON is the input the page generator
//! (a later step) consumes; its shape is versioned by [`SCHEMA_VERSION`].

use std::error::Error;

use serde::Serialize;
use serde_json::{Value, json};
use ymir_core::registry;
use ymir_core::{NodeKind, NodeSpec, ParamKind, ParamSpec, ParamValue, PortSpec, Scale, Unit};
use ymir_nodes::ParamSource;

/// Version of the docs JSON shape. Bump when the schema changes so a consumer can guard on it.
///
/// v2 adds each parameter's resolved `label`, `description`, and `source` tier.
const SCHEMA_VERSION: u32 = 2;

/// Handles `docs [--format json]`: prints the node reference as pretty JSON to stdout, then exits.
/// Only `json` is supported for now; the flag exists so other formats can be added without changing
/// the invocation.
pub(crate) fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut format = "json";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                format = args
                    .get(i + 1)
                    .map(String::as_str)
                    .ok_or("--format needs a value")?;
                i += 2;
            }
            other => {
                return Err(
                    format!("unknown argument {other:?} (usage: docs [--format json])").into(),
                );
            }
        }
    }
    if format != "json" {
        return Err(format!("unsupported --format {format:?} (only 'json' is supported)").into());
    }
    println!("{}", serde_json::to_string_pretty(&build())?);
    Ok(())
}

/// The whole export: a schema version plus every node, in a stable order.
#[derive(Serialize)]
struct Docs {
    schema_version: u32,
    nodes: Vec<Node>,
}

/// One node's reference data.
#[derive(Serialize)]
struct Node {
    type_id: String,
    category: String,
    /// Derived from arity: `generator`, `modifier`, or `endpoint`.
    kind: &'static str,
    display_name: String,
    description: String,
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    params: Vec<Param>,
}

/// One input or output port.
#[derive(Serialize)]
struct Port {
    name: String,
    optional: bool,
}

/// One parameter's schema plus its resolved display strings: the catalog `label` and one-line
/// `description`, and the `source` tier (`override`, `shared`, or `prettified`) so a docs lint can
/// flag a parameter that fell through to the prettified fallback.
#[derive(Serialize)]
struct Param {
    /// The `snake_case` identifier, the key in a project file and in expression syntax.
    name: String,
    /// The resolved display label (catalog entry or prettified fallback).
    label: String,
    /// The one-line description, absent when no catalog entry exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Which catalog tier produced the label: `override`, `shared`, or `prettified`.
    source: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<Value>,
    /// Present only for `enum` parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<String>>,
    default: Value,
    /// `meters` or `degrees`, absent for a unit-less ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<&'static str>,
    scale: &'static str,
}

/// Builds the export from the registry. Registry order is link-dependent, so sort by `type_id` for
/// a stable, diffable document.
fn build() -> Docs {
    let mut nodes: Vec<Node> = registry::entries()
        .map(|entry| node(&(entry.make)().spec()))
        .collect();
    nodes.sort_by(|a, b| a.type_id.cmp(&b.type_id));
    Docs {
        schema_version: SCHEMA_VERSION,
        nodes,
    }
}

fn node(spec: &NodeSpec) -> Node {
    let kind = match spec.kind() {
        NodeKind::Generator => "generator",
        NodeKind::Modifier => "modifier",
        NodeKind::Endpoint => "endpoint",
    };
    Node {
        type_id: spec.type_id.to_string(),
        category: spec.category.to_string(),
        kind,
        display_name: ymir_nodes::tr(&format!("node-{}", spec.type_id)).to_string(),
        description: ymir_nodes::tr(&format!("node-{}-desc", spec.type_id)).to_string(),
        inputs: spec.inputs.iter().map(port).collect(),
        outputs: spec.outputs.iter().map(port).collect(),
        params: spec.params.iter().map(|p| param(spec.type_id, p)).collect(),
    }
}

fn port(p: &PortSpec) -> Port {
    Port {
        name: p.name.clone(),
        optional: p.optional,
    }
}

fn param(type_id: &str, p: &ParamSpec) -> Param {
    let (kind, min, max, options) = match &p.kind {
        ParamKind::Float { min, max } => ("float", Some(json!(min)), Some(json!(max)), None),
        ParamKind::Int { min, max } => ("int", Some(json!(min)), Some(json!(max)), None),
        ParamKind::Bool => ("bool", None, None, None),
        ParamKind::Text => ("text", None, None, None),
        ParamKind::Path => ("path", None, None, None),
        ParamKind::Enum { options } => (
            "enum",
            None,
            None,
            Some(options.iter().map(|s| (*s).to_string()).collect()),
        ),
        ParamKind::Curve => ("curve", None, None, None),
        ParamKind::Strokes => ("strokes", None, None, None),
        // `ParamKind` is `#[non_exhaustive]`; a new variant surfaces as "unknown" in the reference
        // (rather than a silent miss) until it is given a mapping here.
        _ => ("unknown", None, None, None),
    };
    let default = match &p.default {
        ParamValue::Float(v) => json!(v),
        ParamValue::Int(v) => json!(v),
        ParamValue::Bool(v) => json!(v),
        ParamValue::Text(v) => json!(v),
        // A curve or stroke default is not a scalar; represent it as null in the reference.
        ParamValue::Curve(_) | ParamValue::Strokes(_) => Value::Null,
    };
    let unit = p.unit.map(|u| match u {
        Unit::Meters => "meters",
        Unit::Degrees => "degrees",
    });
    let scale = match p.scale {
        Scale::Linear => "linear",
        Scale::Logarithmic => "logarithmic",
    };
    // Resolve the display strings from the running binary's catalog, the same path the GUI takes.
    let resolved = ymir_nodes::resolve_param(type_id, &p.name);
    let source = match resolved.source {
        ParamSource::Override => "override",
        ParamSource::Shared => "shared",
        ParamSource::Prettified => "prettified",
    };
    Param {
        name: p.name.clone(),
        label: resolved.label,
        description: resolved.description,
        source,
        kind,
        min,
        max,
        options,
        default,
        unit,
        scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_node_is_exported_in_sorted_order() {
        let docs = build();
        // ymir-nodes is anchored in main (`use ymir_nodes as _;`), so the registry is populated.
        assert!(
            docs.nodes.len() >= 40,
            "expected the full node set, got {}",
            docs.nodes.len()
        );
        let mut sorted: Vec<&str> = docs.nodes.iter().map(|n| n.type_id.as_str()).collect();
        let actual = sorted.clone();
        sorted.sort_unstable();
        assert_eq!(actual, sorted, "nodes must be emitted sorted by type_id");
    }

    #[test]
    fn export_is_valid_json_with_a_schema_version() {
        let json = serde_json::to_string(&build()).expect("serializes");
        let value: Value = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert!(value["nodes"].is_array());
    }

    #[test]
    fn a_node_resolves_its_display_name_kind_and_param_ranges() {
        let docs = build();
        let fbm = docs
            .nodes
            .iter()
            .find(|n| n.type_id == "generator.fbm")
            .expect("generator.fbm is registered");
        assert_eq!(fbm.category, "generator");
        assert_eq!(fbm.kind, "generator");
        // Resolved through `tr`, not the raw key: a source parse could not produce this.
        assert_eq!(fbm.display_name, "fBm Noise");
        assert!(fbm.inputs.is_empty(), "a generator has no inputs");
        assert!(!fbm.outputs.is_empty(), "a generator has an output");
        let freq = fbm
            .params
            .iter()
            .find(|p| p.name == "frequency")
            .expect("fbm has a frequency param");
        assert_eq!(freq.kind, "float");
        assert!(
            freq.min.is_some() && freq.max.is_some(),
            "a float param carries a numeric range"
        );
        // frequency is a shared, meaning-invariant param: it resolves with a label and description.
        assert_eq!(freq.label, "Frequency");
        assert_eq!(freq.source, "shared");
        assert!(freq.description.is_some());
    }

    #[test]
    fn param_strings_report_their_resolution_tier() {
        let docs = build();
        let blend = docs
            .nodes
            .iter()
            .find(|n| n.type_id == "modifier.blend")
            .expect("modifier.blend is registered");
        // An enum param resolves to a node-specific override, with prose.
        let mode = blend
            .params
            .iter()
            .find(|p| p.name == "mode")
            .expect("blend has a mode param");
        assert_eq!(mode.label, "Mode");
        assert_eq!(mode.source, "override");
        assert!(mode.description.is_some());
        // A param with no catalog entry falls through to the prettified label and carries no prose.
        let opacity = blend
            .params
            .iter()
            .find(|p| p.name == "opacity")
            .expect("blend has an opacity param");
        assert_eq!(opacity.label, "Opacity");
        assert_eq!(opacity.source, "prettified");
        assert!(opacity.description.is_none());
    }

    #[test]
    fn a_meters_param_reports_its_unit() {
        let docs = build();
        // Blur's radius is a world-unit length (meters); confirms unit passthrough.
        let blur = docs
            .nodes
            .iter()
            .find(|n| n.type_id == "modifier.blur")
            .expect("modifier.blur is registered");
        let radius = blur
            .params
            .iter()
            .find(|p| p.name == "radius")
            .expect("blur has a radius param");
        assert_eq!(radius.unit, Some("meters"));
    }
}
