//! The subgraph library: saved subgraphs as standalone, git-friendly files (#106).
//!
//! A library entry is one self-describing JSON file: the subgraph's inner graph (a
//! [`ProjectDocument`]) plus its captured seed, its interior canvas layout, and a small
//! documentation block (a name, a description, and per-port name/description). Dropping an
//! entry into a project is a *copy* with no link back (template instantiation, #79), so the
//! captured seed is what makes a shared subgraph reproduce the same terrain everywhere.
//!
//! Files reuse the same serde types as a project (so they stay diffable and forward
//! compatible via a format version) and live in the user library directory
//! (`$XDG_DATA_HOME/ymir/subgraphs/`, the XDG data base since they are user-authored content,
//! not configuration or cache); built-in entries shipped with the app are a later addition on
//! the same format.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ymir_core::ProjectDocument;

use crate::project_file::ViewState;

/// On-disk format version for a library file. Bumped only on a breaking schema change,
/// paired with a migration; additive fields use `#[serde(default)]` instead.
pub(crate) const SUBGRAPH_FORMAT_VERSION: u32 = 1;

/// Documentation for one port of a subgraph: its index, its name (from the boundary marker),
/// and a human description the author fills in. Shown in the library browser so a user knows
/// what to wire into each pin without diving in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PortDoc {
    /// The port index (0-based), matching the container's derived port order.
    pub index: usize,
    /// The port's name (the boundary marker's label, e.g. "Input 1" or a renamed "height").
    pub name: String,
    /// A human description of what the port is for. Empty until the author writes one.
    #[serde(default)]
    pub description: String,
}

/// A saved subgraph: the inner graph, its seed and layout, and a documentation block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SubgraphFile {
    /// On-disk schema version; see [`SUBGRAPH_FORMAT_VERSION`].
    pub format_version: u32,
    /// The subgraph's display name in the library.
    pub name: String,
    /// A free-text category for grouping in the browser (e.g. "Landforms", "Masks"). Empty
    /// means uncategorized. Free-text and user-definable rather than a fixed set, following
    /// the palette taxonomy's "name a category when its entries exist" discipline.
    #[serde(default)]
    pub category: String,
    /// A description of what the subgraph produces. Empty until the author writes one.
    #[serde(default)]
    pub description: String,
    /// Documentation for each input port, in port order.
    #[serde(default)]
    pub inputs: Vec<PortDoc>,
    /// Documentation for each output port, in port order.
    #[serde(default)]
    pub outputs: Vec<PortDoc>,
    /// The subgraph's captured seed (the container's `seed` param), so a dropped copy
    /// reproduces the same terrain regardless of the host project.
    #[serde(default)]
    pub seed: i64,
    /// The inner graph itself, as a document (the same form a project stores).
    pub graph: ProjectDocument,
    /// The interior canvas layout (node positions), so a dropped copy dives in laid out
    /// rather than cascaded.
    #[serde(default)]
    pub view: ViewState,
}

/// The user library directory (`$XDG_DATA_HOME/ymir/subgraphs/`, or the `$HOME/.local/share`
/// fallback), where saved subgraphs live. This is user-authored *data*, not configuration or
/// cache, so it follows the XDG data base per convention. `None` if neither base is set (the
/// feature is then unavailable). Does not create the directory; callers do that on save.
pub(crate) fn library_dir() -> Option<PathBuf> {
    crate::data_path(
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("HOME"),
        "subgraphs",
    )
}

/// Writes a subgraph to `path` as pretty JSON (git-diffable), creating or truncating it. The
/// parent directory must already exist.
///
/// # Errors
///
/// Returns a message if serialization or the write fails.
pub(crate) fn write_subgraph(path: &Path, file: &SubgraphFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Graph, Params, registry};

    /// A representative subgraph file: an inner graph plus a documentation block.
    fn sample() -> SubgraphFile {
        let mut inner = Graph::new();
        let input = inner.add_op(
            registry::make("subgraph.input").expect("input"),
            Params::new(),
        );
        let output = inner.add_op(
            registry::make("subgraph.output").expect("output"),
            Params::new(),
        );
        inner.connect(input, 0, output, 0).expect("wire");

        let mut view = ViewState::default();
        view.nodes.insert(0, [10.0, 20.0]);

        SubgraphFile {
            format_version: SUBGRAPH_FORMAT_VERSION,
            name: "Passthrough".to_string(),
            category: "Utility".to_string(),
            description: "Feeds its input straight to its output.".to_string(),
            inputs: vec![PortDoc {
                index: 0,
                name: "Input 1".to_string(),
                description: "The field to pass through.".to_string(),
            }],
            outputs: vec![PortDoc {
                index: 0,
                name: "Output 1".to_string(),
                description: String::new(),
            }],
            seed: 42,
            graph: inner.to_document(),
            view,
        }
    }

    #[test]
    fn subgraph_file_round_trips_through_json() {
        let file = sample();
        let json = serde_json::to_string_pretty(&file).expect("serialize");
        let back: SubgraphFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(file, back);
    }

    #[test]
    fn write_persists_pretty_json_that_deserializes() {
        let file = sample();
        let path =
            std::env::temp_dir().join(format!("ymir-subgraph-test-{}.ymirsub", std::process::id()));
        write_subgraph(&path, &file).expect("write");
        let json = std::fs::read_to_string(&path).expect("read back");
        std::fs::remove_file(&path).expect("cleanup");
        assert!(
            json.contains('\n'),
            "pretty JSON is multi-line and diffable"
        );
        let back: SubgraphFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(file, back);
    }
}
