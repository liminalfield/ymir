//! Temporary step-5 runner: build a one-node graph with the fBm generator,
//! evaluate it through the engine, and export the result, so `cargo run` flows
//! through the graph and evaluator. This will grow into a real graph-driven CLI.

use std::error::Error;

use ymir_core::export::{HeightRange, export_png};
use ymir_core::registry::make;
use ymir_core::{EvalCache, EvalRequest, Graph, Params, Region};

// Anchor ymir-nodes so its operator registrations link into this binary. Without
// this the binary only references ymir-core (the registry), nothing names
// ymir-nodes, and the linker can drop its registrations entirely.
use ymir_nodes as _;

fn main() -> Result<(), Box<dyn Error>> {
    let size: usize = 512;
    let seed: u64 = 42;

    let fbm = make("generator.fbm")
        .ok_or("operator 'generator.fbm' is not registered (is ymir-nodes linked?)")?;

    // A one-node graph: the generator is both head and requested endpoint.
    let mut graph = Graph::new();
    let node = graph.add_op(fbm, Params::default());

    let request = EvalRequest::new(size, size, Region::UNIT, seed);
    let mut cache = EvalCache::new(64);
    let outputs = graph.evaluate(node, &request, &mut cache)?;
    let field = outputs.first().ok_or("operator produced no output field")?;

    std::fs::create_dir_all("out")?;
    let path = "out/heightmap.png";
    export_png(field, path, HeightRange::Normalized)?;

    println!("wrote {path} ({size}x{size}, 16-bit grayscale, fBm seed {seed})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use ymir_core::registry;

    // Smoke test for the inventory link-time gotcha: if ymir-nodes were not
    // linked, the registry would be empty and this fails fast.
    #[test]
    fn registry_reports_exactly_one_operator() {
        let mut type_ids: Vec<&str> = registry::entries().map(|e| e.type_id).collect();
        assert_eq!(
            type_ids.len(),
            1,
            "expected exactly one registered operator"
        );

        // Guard against duplicate type_ids slipping in unnoticed.
        type_ids.sort_unstable();
        type_ids.dedup();
        assert_eq!(type_ids, ["generator.fbm"]);
        assert_eq!(registry::count(), 1);
    }
}
