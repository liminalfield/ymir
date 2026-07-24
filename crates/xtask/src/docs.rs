//! `cargo xtask docs-gen`: the node-reference generator.
//!
//! It consumes the versioned JSON that `ymir docs --format json` emits (never the source or the
//! internal types), so the reference is generated from the running binary's real registry. This
//! module holds the consumer-side data model, the JSON acquisition, and the orchestration that
//! renders a page per node plus a category index into `docs/reference/nodes/`. The page and index
//! Markdown production lives in [`crate::render`]; the prose-fragment merge in [`crate::fragment`].

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::fragment::{self, Fragment};
use crate::render;

/// The JSON schema version this generator understands. It is deliberately coupled to
/// `ymir-cli`'s `SCHEMA_VERSION`: when the export shape changes, the export bumps its version and
/// the generator fails loudly here until it is updated to match, rather than silently mis-reading a
/// newer document.
const EXPECTED_SCHEMA: u32 = 4;

/// The whole reference document.
#[derive(Debug, Deserialize)]
pub(crate) struct Docs {
    pub schema_version: u32,
    pub nodes: Vec<Node>,
}

/// One node's reference data.
#[derive(Debug, Deserialize)]
pub(crate) struct Node {
    pub type_id: String,
    pub category_label: String,
    pub kind: String,
    pub display_name: String,
    pub description: String,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub params: Vec<Param>,
    #[serde(default)]
    pub emitted_layers: Vec<String>,
    #[serde(default)]
    pub mask_aware: bool,
}

/// One input or output port.
#[derive(Debug, Deserialize)]
pub(crate) struct Port {
    pub name: String,
    pub optional: bool,
}

/// One parameter's schema plus its resolved display strings.
#[derive(Debug, Deserialize)]
pub(crate) struct Param {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub source: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub min: Option<serde_json::Value>,
    #[serde(default)]
    pub max: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    pub default: serde_json::Value,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Runs `docs-gen`. Accepts an optional `--json <path>` to read a saved document instead of
/// invoking the CLI (used by CI and tests, which should not shell out).
pub(crate) fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut json_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                let path = args.get(i + 1).ok_or("--json needs a path")?;
                json_path = Some(PathBuf::from(path));
                i += 2;
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }

    let docs = match json_path {
        Some(path) => load_from_file(&path)?,
        None => load_from_cli()?,
    };
    let root = workspace_root();
    let written = generate(&docs, &root)?;
    report(&docs);
    println!("wrote {written} node pages + index to docs/reference/nodes/");
    Ok(())
}

/// Renders every node page and the category index into `docs/reference/nodes/`, merging each node's
/// prose fragment from `crates/ymir-nodes/docs/`. Returns the number of node pages written.
fn generate(docs: &Docs, root: &Path) -> Result<usize, Box<dyn Error>> {
    let out_dir = root.join("docs/reference/nodes");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("creating {}: {e}", out_dir.display()))?;
    let fragment_dir = root.join("crates/ymir-nodes/docs");

    for node in &docs.nodes {
        let frag = read_fragment(&fragment_dir, &node.type_id)?;
        let page = render::node_page(node, &frag);
        let path = out_dir.join(format!("{}.md", node.type_id));
        std::fs::write(&path, page).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    let index = out_dir.join("index.md");
    std::fs::write(&index, render::category_index(&docs.nodes))
        .map_err(|e| format!("writing {}: {e}", index.display()))?;
    Ok(docs.nodes.len())
}

/// Reads and parses a node's prose fragment, treating a missing file as an empty fragment (most
/// nodes have no fragment yet).
fn read_fragment(dir: &Path, type_id: &str) -> Result<Fragment, Box<dyn Error>> {
    let path = dir.join(format!("{type_id}.md"));
    match std::fs::read_to_string(&path) {
        Ok(text) => fragment::parse(&text).map_err(|e| format!("{}: {e}", path.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Fragment::default()),
        Err(e) => Err(format!("reading {}: {e}", path.display()).into()),
    }
}

/// Parses and version-checks a reference document.
pub(crate) fn parse(text: &str) -> Result<Docs, Box<dyn Error>> {
    let docs: Docs = serde_json::from_str(text)?;
    if docs.schema_version != EXPECTED_SCHEMA {
        return Err(format!(
            "docs JSON is schema v{}, but this generator expects v{EXPECTED_SCHEMA}; update xtask",
            docs.schema_version
        )
        .into());
    }
    Ok(docs)
}

/// Reads the reference from a saved JSON file.
fn load_from_file(path: &Path) -> Result<Docs, Box<dyn Error>> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text)
}

/// Obtains the reference by running `ymir docs --format json` from the workspace root, so the
/// document reflects the current build's registry.
fn load_from_cli() -> Result<Docs, Box<dyn Error>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .current_dir(workspace_root())
        .args([
            "run",
            "--quiet",
            "--package",
            "ymir-cli",
            "--",
            "docs",
            "--format",
            "json",
        ])
        .output()
        .map_err(|e| format!("running the ymir-cli docs command: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ymir-cli docs exited with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = String::from_utf8(output.stdout)?;
    parse(&text)
}

/// The workspace root, derived from this crate's manifest location so the task works regardless of
/// the shell's current directory. `CARGO_MANIFEST_DIR` is `<root>/crates/xtask` at build time; the
/// two `..` components resolve when the path is used to read or write files.
fn workspace_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// Prints a short summary of the loaded reference: node counts by kind, mask-aware and emitting
/// nodes, and the parameter-string resolution tally (the prettified count is the documentation gap
/// a later lint will enforce).
fn report(docs: &Docs) {
    let count_kind = |k: &str| docs.nodes.iter().filter(|n| n.kind == k).count();
    let mask_aware = docs.nodes.iter().filter(|n| n.mask_aware).count();
    let emitters = docs
        .nodes
        .iter()
        .filter(|n| !n.emitted_layers.is_empty())
        .count();
    let params: Vec<&Param> = docs.nodes.iter().flat_map(|n| &n.params).collect();
    let by_source = |s: &str| params.iter().filter(|p| p.source == s).count();

    println!("docs schema v{}", docs.schema_version);
    println!(
        "{} nodes: {} generators, {} modifiers, {} endpoints",
        docs.nodes.len(),
        count_kind("generator"),
        count_kind("modifier"),
        count_kind("endpoint"),
    );
    println!("{mask_aware} mask-aware, {emitters} emitting byproduct layers");
    println!(
        "{} parameters: {} override, {} shared, {} prettified (gaps)",
        params.len(),
        by_source("override"),
        by_source("shared"),
        by_source("prettified"),
    );
}

/// A representative v4 document (one generator, one mask-aware emitter), shared by the docs and
/// render tests so both exercise the same shape.
#[cfg(test)]
pub(crate) const SAMPLE: &str = r#"{
        "schema_version": 4,
        "nodes": [
            {
                "type_id": "generator.fbm", "category": "generator",
                "category_label": "Generators", "kind": "generator",
                "display_name": "fBm Noise", "description": "Fractional Brownian motion.",
                "inputs": [], "outputs": [{"name": "out", "optional": false}],
                "params": [
                    {"name": "frequency", "label": "Frequency", "description": "Feature size.",
                     "source": "shared", "type": "float", "min": 0.1, "max": 32.0,
                     "default": 2.0, "scale": "logarithmic"}
                ],
                "emitted_layers": [], "mask_aware": false
            },
            {
                "type_id": "modifier.thermal_erosion", "category": "geology",
                "category_label": "Geology", "kind": "modifier",
                "display_name": "Thermal Erosion", "description": "Relaxes slopes.",
                "inputs": [{"name": "in", "optional": false}],
                "outputs": [{"name": "heightfield", "optional": false}],
                "params": [],
                "emitted_layers": ["wear", "debris"], "mask_aware": true
            }
        ]
    }"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_v4_document() {
        let docs = parse(SAMPLE).expect("valid v4 doc parses");
        assert_eq!(docs.schema_version, 4);
        assert_eq!(docs.nodes.len(), 2);
        let thermal = &docs.nodes[1];
        assert_eq!(thermal.category_label, "Geology");
        assert_eq!(thermal.emitted_layers, ["wear", "debris"]);
        assert!(thermal.mask_aware);
        assert_eq!(docs.nodes[0].params[0].source, "shared");
    }

    #[test]
    fn a_mismatched_schema_version_is_an_error() {
        let bumped = SAMPLE.replace("\"schema_version\": 4", "\"schema_version\": 999");
        let err = parse(&bumped).expect_err("a newer schema must fail loudly");
        assert!(err.to_string().contains("expects v4"));
    }
}
