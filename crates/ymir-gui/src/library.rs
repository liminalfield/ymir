//! The subgraph library: saved subgraphs as standalone, git-friendly files (#106).
//!
//! A library entry is one self-describing JSON file: the subgraph's inner graph (a
//! [`ProjectDocument`]) plus its captured seed, its interior canvas layout, and a small
//! documentation block (a name, a description, per-port name/description, and an optional author
//! identity and license for sharing). Dropping an entry into a project is a *copy* with no link
//! back (template instantiation, #79), so the captured seed is what makes a shared subgraph
//! reproduce the same terrain everywhere.
//!
//! Files reuse the same serde types as a project (so they stay diffable and forward
//! compatible via a format version) and live in the user library directory
//! (`$XDG_DATA_HOME/ymir/subgraphs/`, the XDG data base since they are user-authored content,
//! not configuration or cache); built-in entries shipped with the app are a later addition on
//! the same format.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ymir_core::ProjectDocument;

use crate::preferences::AuthorProfile;
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
    /// The author's identity, pre-filled from their profile and editable per subgraph. Omitted
    /// entirely when blank, so an anonymous subgraph carries no author block. A subgraph is the
    /// user's own work and not a derivative of Ymir, so this identity is theirs to share or not.
    #[serde(default, skip_serializing_if = "AuthorProfile::is_empty")]
    pub author: AuthorProfile,
    /// A license for the subgraph (e.g. an SPDX id like "CC0-1.0" or "GPL-3.0-or-later"), stating
    /// reuse terms for this shared artifact. Ymir's own GPL does not reach a user's output, so the
    /// author is free to choose any terms. Omitted when blank.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
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

/// Reads a subgraph from `path`, deserializing its JSON. Missing additive fields fall back to
/// their defaults (via `#[serde(default)]`), so a file written by an older build still loads.
///
/// # Errors
///
/// Returns a message if the file cannot be read or is not valid JSON.
pub(crate) fn read_subgraph(path: &Path) -> Result<SubgraphFile, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// One loaded library entry: the file it came from and its parsed contents. The browser lists
/// these; dropping one into a project (a later step) reads `file.graph`.
#[derive(Debug, Clone)]
pub(crate) struct LibraryEntry {
    /// The `.ymirsub` file this entry was read from.
    pub path: PathBuf,
    /// The parsed subgraph (documentation, seed, and inner graph).
    pub file: SubgraphFile,
}

/// The result of scanning the library directory: the entries that parsed, plus a per-file error
/// for each one that did not. A single corrupt file is reported without hiding the rest, so the
/// browser can still list the good entries and surface the bad ones.
#[derive(Debug, Clone, Default)]
pub(crate) struct LibraryListing {
    /// The successfully parsed entries, sorted by display name then path for a stable order.
    pub entries: Vec<LibraryEntry>,
    /// Files that could not be read or parsed, paired with the reason.
    pub errors: Vec<(PathBuf, String)>,
}

/// Scans the user library directory for saved subgraphs. A missing directory (nothing saved
/// yet) or no resolvable library base yields an empty listing rather than an error. See
/// [`load_library_from`] for the per-file behavior.
pub(crate) fn load_library() -> LibraryListing {
    match library_dir() {
        Some(dir) => load_library_from(&dir),
        None => LibraryListing::default(),
    }
}

/// Scans `dir` for `*.ymirsub` files and parses each, collecting successes into `entries`
/// (sorted by display name then path) and failures into `errors`. A missing directory yields an
/// empty listing; any other directory-read failure is recorded as an error against `dir`.
/// Non-`.ymirsub` files are ignored.
pub(crate) fn load_library_from(dir: &Path) -> LibraryListing {
    let mut listing = LibraryListing::default();
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return listing,
        Err(e) => {
            listing.errors.push((dir.to_path_buf(), e.to_string()));
            return listing;
        }
    };
    for entry in read_dir {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                listing.errors.push((dir.to_path_buf(), e.to_string()));
                continue;
            }
        };
        if path.extension().and_then(|e| e.to_str()) != Some("ymirsub") {
            continue;
        }
        match read_subgraph(&path) {
            Ok(file) => listing.entries.push(LibraryEntry { path, file }),
            Err(err) => listing.errors.push((path, err)),
        }
    }
    listing
        .entries
        .sort_by(|a, b| a.file.name.cmp(&b.file.name).then(a.path.cmp(&b.path)));
    listing
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
            author: AuthorProfile {
                name: "Ada".to_string(),
                email: "ada@example.com".to_string(),
                website: String::new(),
                docs: String::new(),
            },
            license: "CC0-1.0".to_string(),
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
    fn a_blank_author_and_license_are_omitted_from_the_json() {
        let mut file = sample();
        file.author = AuthorProfile::default();
        file.license = String::new();
        let json = serde_json::to_string(&file).expect("serialize");
        assert!(!json.contains("\"author\""), "blank author is omitted");
        assert!(!json.contains("\"license\""), "blank license is omitted");
        // And it still round-trips, the missing keys defaulting back to blank.
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

    #[test]
    fn write_then_read_round_trips_through_a_file() {
        let file = sample();
        let path =
            std::env::temp_dir().join(format!("ymir-subgraph-rw-{}.ymirsub", std::process::id()));
        write_subgraph(&path, &file).expect("write");
        let back = read_subgraph(&path).expect("read");
        std::fs::remove_file(&path).expect("cleanup");
        assert_eq!(file, back);
    }

    #[test]
    fn a_missing_directory_loads_as_an_empty_listing() {
        let dir = std::env::temp_dir().join(format!("ymir-lib-missing-{}", std::process::id()));
        // Deliberately not created.
        let listing = load_library_from(&dir);
        assert!(listing.entries.is_empty());
        assert!(listing.errors.is_empty());
    }

    #[test]
    fn load_lists_ymirsub_files_sorted_by_name_and_records_bad_ones() {
        let dir = std::env::temp_dir().join(format!("ymir-lib-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");

        // Two valid entries, written out of alphabetical order to prove the sort.
        let mut zulu = sample();
        zulu.name = "Zulu".to_string();
        write_subgraph(&dir.join("zulu.ymirsub"), &zulu).expect("write zulu");
        let mut alpha = sample();
        alpha.name = "Alpha".to_string();
        write_subgraph(&dir.join("alpha.ymirsub"), &alpha).expect("write alpha");
        // A non-library file, ignored entirely.
        std::fs::write(dir.join("notes.txt"), "ignore me").expect("write txt");
        // A corrupt library file, reported as an error rather than dropped silently.
        std::fs::write(dir.join("broken.ymirsub"), "{ not json").expect("write broken");

        let listing = load_library_from(&dir);
        std::fs::remove_dir_all(&dir).expect("cleanup");

        let names: Vec<&str> = listing
            .entries
            .iter()
            .map(|e| e.file.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Alpha", "Zulu"],
            "entries sorted by name, txt ignored"
        );
        assert_eq!(listing.errors.len(), 1, "the corrupt file is reported");
        assert!(
            listing.errors[0].0.ends_with("broken.ymirsub"),
            "the error names the offending file"
        );
    }
}
