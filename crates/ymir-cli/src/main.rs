//! Temporary step-2 runner: fill a field with a radial gradient and export it as
//! a 16-bit heightmap, so `cargo run` produces something viewable. This will be
//! replaced by a real graph-driven CLI once the engine lands.

use std::sync::Arc;

use ymir_core::export::{HeightRange, export_png};
use ymir_core::{Field, Layer, Region, layers};

fn main() -> std::io::Result<()> {
    let size: usize = 512;

    // Radial dome: 1.0 at the center, falling to 0.0 at the farthest corner.
    let center = (size - 1) as f32 / 2.0;
    let max_dist = (2.0 * center * center).sqrt();
    let dome = Layer::from_fn(size, size, |x, y| {
        let dx = x as f32 - center;
        let dy = y as f32 - center;
        let dist = (dx * dx + dy * dy).sqrt();
        1.0 - dist / max_dist
    });

    let field = Field::new(size, size, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(dome));

    std::fs::create_dir_all("out")?;
    let path = "out/heightmap.png";
    export_png(&field, path, HeightRange::Normalized)?;

    println!("wrote {path} ({size}x{size}, 16-bit grayscale)");
    Ok(())
}
