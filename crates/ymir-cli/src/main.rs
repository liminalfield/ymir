//! Temporary step-4 runner: construct the fBm operator through the registry,
//! evaluate it, and export the result, so `cargo run` still produces a viewable
//! heightmap while exercising the operator mechanism end-to-end. This will be
//! replaced by a real graph-driven CLI once the evaluator lands.

use std::error::Error;

use ymir_core::export::{HeightRange, export_png};
use ymir_core::registry::make;
use ymir_core::{EvalContext, Params, Region};

// Anchor ymir-nodes so its operator registrations link into this binary. Without
// this the binary only references ymir-core (the registry), nothing names
// ymir-nodes, and the linker can drop its registrations entirely. The node-count
// test below guards against exactly that.
use ymir_nodes as _;

fn main() -> Result<(), Box<dyn Error>> {
    let size: usize = 512;
    let seed: u64 = 42;

    let operator = make("generator.fbm")
        .ok_or("operator 'generator.fbm' is not registered (is ymir-nodes linked?)")?;

    // Empty params: the operator falls back to its schema defaults.
    let ctx = EvalContext::new(size, size, Region::UNIT, seed);
    let outputs = operator.eval(&[], &Params::default(), &ctx)?;
    let field = outputs
        .into_iter()
        .next()
        .ok_or("operator produced no output field")?;

    std::fs::create_dir_all("out")?;
    let path = "out/heightmap.png";
    export_png(&field, path, HeightRange::Normalized)?;

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
