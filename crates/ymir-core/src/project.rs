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

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use crate::param::Params;

/// The current on-disk format version. Bumped on a breaking schema change, paired
/// with a migration path so existing projects still load.
pub const FORMAT_VERSION: u32 = 1;

/// The current on-disk version of the whole project *file*, distinct from
/// [`FORMAT_VERSION`], which versions the graph document nested inside it.
///
/// Starts at 2: version 1 was the editor's own envelope, which kept the world settings on the
/// GUI side where nothing headless could reach them. That shape is not readable by this version,
/// which is a deliberate break rather than a migration (see `design/project-format.md`).
pub const PROJECT_FILE_VERSION: u32 = 2;

/// The settings that describe the world a graph builds, as opposed to how an editor displays it.
///
/// Every field here reaches an operator: the first four through
/// [`EvalRequest`](crate::EvalRequest) into each node's [`EvalContext`](crate::EvalContext), and
/// the resolution as the request's own grid size. That is the test for belonging here. How the
/// viewport draws water, where the nodes sit on a canvas, and what resolution the interactive
/// preview runs at are all presentation, and live in the file's `view` section instead.
///
/// The consequence that matters: a project's terrain is reproducible from the graph plus this,
/// with nothing else needed. That is what lets something headless render exactly what the editor
/// shows, rather than the same node network under invented settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldSettings {
    /// The global seed. Each node derives its own from this and its `stable_id`.
    pub seed: u64,
    /// World extent along x, in metres, across the full unit region.
    pub world_extent: f64,
    /// The real elevation in metres that a normalized height of `1.0` represents.
    pub world_height: f64,
    /// The sea or base level, as a normalized height. Coastal shaping bevels to it and stream
    /// erosion grades toward it.
    pub sea_level: f64,
    /// The square resolution a full build evaluates at.
    ///
    /// It sits with the physical extents because it is the one value a headless render cannot
    /// invent: rebuilding a project at a different resolution produces genuinely different
    /// terrain wherever an iterative simulation is involved.
    pub build_res: usize,
}

impl Default for WorldSettings {
    /// A unit world at the default build resolution: the same values a fresh project starts from.
    fn default() -> Self {
        Self {
            seed: 0,
            world_extent: 1.0,
            world_height: 1.0,
            sea_level: 0.0,
            build_res: 1024,
        }
    }
}

/// A whole project on disk: the world it describes, the graph that builds it, and whatever an
/// editor wants to remember about showing it.
///
/// The view section is a type parameter rather than an opaque blob so an editor keeps its own
/// typed state, comparable and cheap, while anything headless takes the default and ignores it.
/// Opaque JSON here would push a serialization into the editor's undo comparison, which runs
/// every settled frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile<V = serde_json::Value> {
    /// On-disk version of this envelope; see [`PROJECT_FILE_VERSION`].
    pub format_version: u32,
    /// What the graph builds.
    pub world: WorldSettings,
    /// The graph itself.
    pub graph: ProjectDocument,
    /// Editor state. Written and read by whoever owns `V`; the engine never interprets it.
    #[serde(default)]
    pub view: V,
}

impl<V: Default> ProjectFile<V> {
    /// A project file at the current version, wrapping `world` and `graph` with default view
    /// state.
    #[must_use]
    pub fn new(world: WorldSettings, graph: ProjectDocument) -> Self {
        Self {
            format_version: PROJECT_FILE_VERSION,
            world,
            graph,
            view: V::default(),
        }
    }
}

impl<V> ProjectFile<V> {
    /// Fails when this file is not a version this build understands.
    ///
    /// Checked explicitly rather than left to serde, so an older project reports what it is
    /// instead of surfacing as a field-level parse error.
    pub fn check_version(&self) -> Result<()> {
        if self.format_version == PROJECT_FILE_VERSION {
            return Ok(());
        }
        Err(Error::UnsupportedFormatVersion {
            version: self.format_version,
            expected: PROJECT_FILE_VERSION,
        })
    }
}

impl<V: Serialize> ProjectFile<V> {
    /// Writes the project as pretty JSON, which keeps it diffable in version control.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written or the value cannot be serialized.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let file = File::create(path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), self)?;
        Ok(())
    }
}

impl<V: DeserializeOwned + Default> ProjectFile<V> {
    /// Reads a project, rejecting a version this build does not understand.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, is not valid JSON, or carries a format
    /// version other than [`PROJECT_FILE_VERSION`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let project: Self = serde_json::from_reader(BufReader::new(file))?;
        project.check_version()?;
        Ok(project)
    }
}

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
    /// For a container node (a subgraph), the serialized inner graph it holds; `None`
    /// (omitted) for an ordinary node, so existing projects and non-container nodes are
    /// unchanged and the format version need not bump. Boxed so the document type is not
    /// infinitely sized, and recursive so nested subgraphs round-trip. A structural field,
    /// captured from the operator's [`nested`](crate::Operator::nested) hook, never by
    /// naming a concrete node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgraph: Option<Box<ProjectDocument>>,
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
                    subgraph: None,
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
                    subgraph: None,
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
    fn a_color_round_trips_through_a_saved_project() {
        // The colour a material previews with has to survive save and reload, or a reopened
        // project renders in different colours from the one that was authored.
        let json = serde_json::to_value(ParamValue::Color([0.25, 0.5, 0.75])).expect("serialize");
        assert_eq!(json, serde_json::json!({ "color": [0.25, 0.5, 0.75] }));

        let doc = ProjectDocument {
            format_version: FORMAT_VERSION,
            next_stable_id: 1,
            nodes: vec![NodeDocument {
                stable_id: 0,
                type_id: "test.material".into(),
                name: None,
                params: Params::new().with("tint", ParamValue::Color([0.25, 0.5, 0.75])),
                connections: Vec::new(),
                bypassed: false,
                subgraph: None,
            }],
        };
        let text = serde_json::to_string(&doc).expect("serialize");
        let back: ProjectDocument = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, doc);
    }

    #[test]
    fn a_curve_round_trips_and_is_resanitized_on_load() {
        // A curve serializes as its points; loading an out-of-range, unsorted list
        // rebuilds through Curve::new, yielding the sanitized, sorted curve.
        let messy = serde_json::json!([[1.0, 2.0], [0.5, -1.0], [0.0, 0.5]]);
        let curve: Curve = serde_json::from_value(messy).expect("deserialize");
        assert_eq!(curve.points(), &[(0.0, 0.5), (0.5, 0.0), (1.0, 1.0)]);
    }

    /// Stand-in for an editor's view state: typed, comparable, and meaningless to the engine.
    #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    struct FakeView {
        note: String,
    }

    fn sample_world() -> WorldSettings {
        WorldSettings {
            seed: 42,
            world_extent: 2048.0,
            world_height: 512.0,
            sea_level: 0.3,
            build_res: 4096,
        }
    }

    #[test]
    fn a_project_file_round_trips_with_its_view_state() {
        let file = ProjectFile {
            format_version: PROJECT_FILE_VERSION,
            world: sample_world(),
            graph: sample_document(),
            view: FakeView {
                note: "canvas".to_string(),
            },
        };
        let json = serde_json::to_string(&file).expect("serialize");
        let back: ProjectFile<FakeView> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, file);
    }

    #[test]
    fn a_headless_reader_ignores_the_view_without_naming_its_type() {
        // The point of the type parameter: something with no editor still reads the world and the
        // graph, which is everything needed to reproduce the terrain.
        let file = ProjectFile {
            format_version: PROJECT_FILE_VERSION,
            world: sample_world(),
            graph: sample_document(),
            view: FakeView {
                note: "positions the engine does not care about".to_string(),
            },
        };
        let json = serde_json::to_string(&file).expect("serialize");

        let headless: ProjectFile = serde_json::from_str(&json).expect("deserialize headless");
        assert_eq!(headless.world, sample_world());
        assert_eq!(headless.graph, sample_document());
    }

    #[test]
    fn an_older_project_reports_its_version_rather_than_a_parse_error() {
        // Version 1 was the editor's envelope and is deliberately not readable. Someone opening
        // one should be told what it is, not shown a missing-field error from serde.
        let file = ProjectFile {
            format_version: 1,
            world: sample_world(),
            graph: sample_document(),
            view: FakeView::default(),
        };
        let dir = std::env::temp_dir().join(format!("ymir-projectfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("old.ymir");
        file.save(&path).expect("write");

        match ProjectFile::<FakeView>::load(&path) {
            Err(Error::UnsupportedFormatVersion { version, expected }) => {
                assert_eq!(version, 1);
                assert_eq!(expected, PROJECT_FILE_VERSION);
            }
            other => panic!("expected an unsupported-version error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir); // shortcut-ok: best-effort test cleanup
    }

    #[test]
    fn a_saved_project_reloads_from_disk() {
        let dir = std::env::temp_dir().join(format!("ymir-projectfile-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("project.ymir");
        let file: ProjectFile<FakeView> = ProjectFile::new(sample_world(), sample_document());
        file.save(&path).expect("write");
        let back = ProjectFile::<FakeView>::load(&path).expect("read");
        assert_eq!(back, file);
        assert_eq!(back.format_version, PROJECT_FILE_VERSION);
        let _ = std::fs::remove_dir_all(&dir); // shortcut-ok: best-effort test cleanup
    }
}
