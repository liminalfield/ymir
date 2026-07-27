//! Every example project shipped in the repository still opens.
//!
//! These files are the ones a new user is pointed at first, and nothing else checks them: a
//! format change, a renamed node, or a dropped parameter orphans them silently and the failure
//! surfaces as a broken example rather than a failing test.
//!
//! It lives here rather than in `ymir-core` because rebuilding a graph needs the concrete
//! operators registered. That it needs nothing else is the point of the world settings living in
//! the file's own envelope: the terrain an example describes is reproducible without the editor.

use std::path::{Path, PathBuf};

use ymir_core::{Graph, ProjectFile};

/// The repository's `examples/` directory, from this crate's manifest.
fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// Every `.ymir` file in `examples/`, sorted so a failure names the same file every run.
fn example_projects() -> Vec<PathBuf> {
    let dir = examples_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok);
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ymir"))
        .collect();
    paths.sort();
    paths
}

/// Confirms the operator registry is populated, and in doing so keeps it populated.
///
/// Naming a concrete operator type is what forces the linker to keep `ymir-nodes` in an
/// integration test's link, and with it the `inventory` registrations. Without a reference the
/// crate is dropped and every node reads as unavailable, which is the registration gotcha
/// `CLAUDE.md` warns about rather than a real regression in the example.
fn registered_node_count() -> usize {
    let _ = std::any::type_name::<ymir_nodes::Fbm>();
    ymir_core::registry::count()
}

#[test]
fn every_example_project_opens_and_rebuilds_its_graph() {
    assert!(
        registered_node_count() > 0,
        "no operators registered, so every node would read as unavailable"
    );
    let projects = example_projects();
    assert!(
        !projects.is_empty(),
        "no example projects found in {}",
        examples_dir().display()
    );

    for path in projects {
        let name = path.display();
        let file: ProjectFile = ProjectFile::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        file.check_version()
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        // Rebuilt through the registry, and reporting rather than erroring, so a node that no
        // longer exists shows up as a named warning instead of an opaque failure.
        let (graph, warnings) = Graph::from_document_reporting(&file.graph)
            .unwrap_or_else(|e| panic!("{name}: rebuild: {e}"));
        assert!(
            warnings.is_empty(),
            "{name} opened with warnings: {warnings:?}"
        );
        assert!(graph.node_count() > 0, "{name} rebuilt to an empty graph");
        assert!(
            file.world.world_extent > 0.0 && file.world.build_res > 0,
            "{name} describes a world with no extent"
        );
    }
}
