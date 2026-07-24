//! Temporary runner: build a three-node graph (fBm generator -> thermal erosion ->
//! PNG export endpoint), save it as a project file, then reload that file and render
//! from the reloaded graph, so `cargo run` exercises the full save/load path end to
//! end and leaves an inspectable `project.json`. This will grow into a real
//! graph-driven CLI.

use std::error::Error;

use ymir_core::registry::make;
use ymir_core::{EvalCache, EvalRequest, Graph, ParamValue, Params, Region};

// Anchor ymir-nodes so its operator registrations link into this binary. Without
// this the binary only references ymir-core (the registry), nothing names
// ymir-nodes, and the linker can drop its registrations entirely.
use ymir_nodes as _;

mod docs;

fn make_op(type_id: &str) -> Result<Box<dyn ymir_core::Operator>, Box<dyn Error>> {
    make(type_id).ok_or_else(|| format!("operator {type_id:?} is not registered").into())
}

fn main() -> Result<(), Box<dyn Error>> {
    // `--version`/`-V` prints the build-stamped version and exits before any work, so it
    // stays usable for provenance even if a render would fail.
    if std::env::args()
        .skip(1)
        .any(|a| a == "--version" || a == "-V")
    {
        println!("ymir {}", ymir_build_info::version_string());
        return Ok(());
    }

    // `docs [--format json]`: emit the node reference as JSON from the running binary and exit,
    // before any logging or render work.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("docs") {
        return docs::run(&args[1..]);
    }

    // Headless diagnostics go to stderr (a toolchain captures it); load degradations are logged
    // rather than swallowed.
    ymir_core::logging::init(None, log::LevelFilter::Info);

    let size: usize = 512;
    let seed: u64 = 42;
    let path = "out/heightmap.png";
    let project_path = "out/project.json";

    let mut graph = Graph::new();
    let generator = graph.add_op(make_op("generator.fbm")?, Params::default());
    let erosion = graph.add_op(make_op("modifier.thermal_erosion")?, Params::default());
    let export = graph.add_op(
        make_op("endpoint.export")?,
        Params::new().with("path", ParamValue::Text(path.to_string())),
    );

    graph.connect(generator, 0, erosion, 0)?;
    graph.connect(erosion, 0, export, 0)?;

    // Save the project, then reload it and render from the reloaded graph, so the run
    // proves the full save/load round-trip rather than just evaluating in memory.
    std::fs::create_dir_all("out")?;
    graph.save(project_path)?;
    let export_id = graph
        .stable_id(export)
        .ok_or("export node has no stable id")?;
    let (graph, warnings) = Graph::load_reporting(project_path)?;
    for warning in &warnings {
        log::warn!("loading {project_path}: {warning}");
    }
    let export = graph
        .node_id_of(export_id)
        .ok_or("export node missing after reload")?;

    // Pulling the endpoint evaluates the chain and writes the file as a side
    // effect (endpoints are not memoized).
    let request = EvalRequest::new(size, size, Region::UNIT, seed);
    let mut cache = EvalCache::new(64);
    graph.evaluate(export, &request, &mut cache)?;

    println!("saved project to {project_path}");
    println!(
        "wrote {path} ({size}x{size}, 16-bit grayscale, fBm + thermal erosion, seed {seed}) from the reloaded project"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use ymir_core::registry;

    // Link-anchor smoke test: proves the `use ymir_nodes as _` above actually pulls
    // ymir-nodes' operator registrations into *this binary*. Without the anchor the
    // linker can drop them (the inventory gotcha) and the registry comes up empty, so
    // asserting a couple of sentinel operators construct fails fast here. The full
    // registered set is pinned once in crates/ymir-nodes/tests/registry_smoke.rs; this
    // stays a per-binary link check and deliberately does not re-list every node.
    #[test]
    fn ymir_nodes_is_linked_into_this_binary() {
        assert!(
            registry::count() > 0,
            "operator registry is empty; ymir-nodes was not linked",
        );
        for type_id in [
            "generator.fbm",
            "modifier.thermal_erosion",
            "endpoint.export",
        ] {
            assert!(
                registry::make(type_id).is_some(),
                "operator {type_id:?} is not registered; the ymir-nodes anchor was dropped",
            );
        }
    }
}
