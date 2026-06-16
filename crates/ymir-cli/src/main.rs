//! Temporary step-3 runner: generate an fBm Perlin heightmap and export it as a
//! 16-bit PNG, so `cargo run` produces something viewable. This will be replaced
//! by a real graph-driven CLI once the engine lands.

use ymir_core::Region;
use ymir_core::export::{HeightRange, export_png};
use ymir_core::noise::{FbmParams, fbm_field};

fn main() -> std::io::Result<()> {
    let size: usize = 512;
    let seed: u64 = 42;

    let field = fbm_field(size, size, Region::UNIT, FbmParams::default(), seed);

    std::fs::create_dir_all("out")?;
    let path = "out/heightmap.png";
    export_png(&field, path, HeightRange::Normalized)?;

    println!("wrote {path} ({size}x{size}, 16-bit grayscale, fBm seed {seed})");
    Ok(())
}
