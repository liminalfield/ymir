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

fn make_op(type_id: &str) -> Result<Box<dyn ymir_core::Operator>, Box<dyn Error>> {
    make(type_id).ok_or_else(|| format!("operator {type_id:?} is not registered").into())
}

fn main() -> Result<(), Box<dyn Error>> {
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
    let graph = Graph::load(project_path)?;
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

    // Smoke test for the inventory link-time gotcha: if ymir-nodes were not
    // linked, the registry would be empty and this fails fast.
    #[test]
    fn registry_has_the_expected_operators() {
        let mut type_ids: Vec<&str> = registry::entries().map(|e| e.type_id).collect();
        type_ids.sort_unstable();

        let mut unique = type_ids.clone();
        unique.dedup();
        assert_eq!(
            unique.len(),
            type_ids.len(),
            "duplicate type_id in registry"
        );

        assert_eq!(
            type_ids,
            [
                "endpoint.export",
                "generator.cellular_bumps",
                "generator.cellular_cracks",
                "generator.cellular_regions",
                "generator.falloff",
                "generator.fbm",
                "generator.gradient",
                "generator.polygon",
                "generator.radial",
                "generator.rect",
                "generator.ridged",
                "generator.ring",
                "modifier.blend",
                "modifier.blur",
                "modifier.curvature",
                "modifier.curve",
                "modifier.height",
                "modifier.invert",
                "modifier.levels",
                "modifier.slope",
                "modifier.thermal_erosion",
                "modifier.warp"
            ],
        );
    }
}
