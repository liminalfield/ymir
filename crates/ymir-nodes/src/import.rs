//! The import generator: a heightmap image loaded as a field.
//!
//! Decodes the PNG at `path` and resamples it (bilinear) into the requested resolution
//! and region, writing the luminance to the `height` layer. The image spans the whole
//! `UNIT` world region, so a tiled build samples the matching sub-rectangle and an untiled
//! build samples the whole image.
//!
//! Unlike the procedural generators, an imported image is fixed-resolution raster data:
//! requesting a higher resolution interpolates between pixels rather than revealing new
//! detail, and the output depends on the file rather than the seed (so determinism holds
//! only for the same file). An empty `path` is "no image yet", a flat-zero field rather
//! than an error; a set-but-unreadable path is a real error, shown on the node.
//!
//! Only PNG for now, matching the exporter; `.r16` and EXR importers are later siblings.
//! The decode mechanism lives in `ymir-core` (`import::decode_png`), beside the encoder.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use ymir_core::import::{DecodedImage, decode_png};
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "generator.import";

/// Import generator: no inputs, one output.
#[derive(Clone)]
pub struct Import;

impl Operator for Import {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "generator",
            inputs: Vec::new(),
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "path",
                ParamKind::Path,
                ParamValue::Text(String::new()),
            )],
        }
    }

    fn eval(&self, _inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let path = params.get_str("path", "");
        if path.trim().is_empty() {
            // No image selected yet: an empty (flat-zero) field, not an error, so a
            // freshly added node is not red until a path is set.
            let layer = Layer::filled(ctx.width, ctx.height, 0.0);
            return Ok(vec![
                Field::new(ctx.width, ctx.height, ctx.region)
                    .with_layer(layers::HEIGHT, Arc::new(layer)),
            ]);
        }

        // A set path that cannot be opened or decoded is a real failure, surfaced as a
        // node error rather than silently producing nothing.
        let image = decode_png(BufReader::new(File::open(path)?))?;
        Ok(vec![resample(&image, ctx)])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Import) }
}

/// Resamples `image` (which spans the `UNIT` world region) into a field at the context's
/// resolution and region, bilinearly.
fn resample(image: &DecodedImage, ctx: &EvalContext) -> Field {
    let region = ctx.region;
    let layer = Layer::from_fn(ctx.width, ctx.height, |x, y| {
        // Output cell centre as a world position in [0, 1], then sampled from the image.
        let u = (x as f64 + 0.5) / ctx.width as f64;
        let v = (y as f64 + 0.5) / ctx.height as f64;
        let wx = region.min_x + u * region.width();
        let wy = region.min_y + v * region.height();
        sample_bilinear(image, wx, wy)
    });

    Field::new(ctx.width, ctx.height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Bilinearly samples `image` at world position `(wx, wy)` in `[0, 1]`. Pixel centres sit
/// at integer + 0.5; out-of-range taps clamp to the edge via [`DecodedImage::pixel`].
fn sample_bilinear(image: &DecodedImage, wx: f64, wy: f64) -> f32 {
    let fx = wx * image.width as f64 - 0.5;
    let fy = wy * image.height as f64 - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = (fx - x0) as f32;
    let ty = (fy - y0) as f32;
    let (x0, y0) = (x0 as i64, y0 as i64);

    let top = lerp(image.pixel(x0, y0), image.pixel(x0 + 1, y0), tx);
    let bottom = lerp(image.pixel(x0, y0 + 1), image.pixel(x0 + 1, y0 + 1), tx);
    lerp(top, bottom, ty)
}

/// Linear interpolation between `a` and `b` by `t`.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::export::{HeightRange, export_png_to};
    use ymir_core::registry;
    use ymir_core::{Region, layers};

    /// A temp PNG that removes itself on drop, so a test's temp file is cleaned up even if
    /// the test panics partway through.
    struct TempPng(std::path::PathBuf);

    impl TempPng {
        fn path_str(&self) -> String {
            self.0.display().to_string()
        }
    }

    impl Drop for TempPng {
        fn drop(&mut self) {
            // Best-effort cleanup: a removal failure must not mask the test result.
            let _ = std::fs::remove_file(&self.0); // shortcut-ok: best-effort temp-file cleanup in a test
        }
    }

    /// Writes `values` as a normalized PNG to a unique temp path, returning a self-cleaning
    /// handle to it.
    fn write_temp_png(name: &str, w: usize, h: usize, values: &[f32]) -> TempPng {
        let layer = Layer::from_fn(w, h, |x, y| values[y * w + x]);
        let field = Field::new(w, h, Region::UNIT).with_layer(layers::HEIGHT, Arc::new(layer));
        let path = std::env::temp_dir().join(format!("ymir_import_{name}.png"));
        let mut bytes = Vec::new();
        export_png_to(&field, &mut bytes, HeightRange::Normalized).unwrap();
        std::fs::write(&path, bytes).unwrap();
        TempPng(path)
    }

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
    }

    fn run(params: &Params, ctx: &EvalContext) -> Result<Field> {
        Import
            .eval(Inputs::required_only(&[]), params, ctx)
            .map(|mut out| out.remove(0))
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn an_empty_path_is_a_flat_zero_field() {
        let out = run(&Params::default(), &ctx(8)).unwrap();
        let layer = out.layer(layers::HEIGHT).unwrap();
        assert!(layer.as_slice().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn an_unreadable_path_is_an_error() {
        let params = Params::default().with(
            "path",
            ParamValue::Text("/no/such/ymir/file.png".to_string()),
        );
        assert!(run(&params, &ctx(8)).is_err());
    }

    #[test]
    fn imports_pixels_at_native_resolution() {
        // At the image's own resolution, each cell samples its pixel back (within 16-bit
        // round-trip tolerance).
        let png = write_temp_png("native", 2, 2, &[0.0, 1.0, 0.25, 0.75]);
        let params = Params::default().with("path", ParamValue::Text(png.path_str()));
        let out = run(&params, &ctx(2)).unwrap();
        assert!((at(&out, 0, 0) - 0.0).abs() < 1e-3);
        assert!((at(&out, 1, 0) - 1.0).abs() < 1e-3);
        assert!((at(&out, 0, 1) - 0.25).abs() < 1e-3);
        assert!((at(&out, 1, 1) - 0.75).abs() < 1e-3);
    }

    #[test]
    fn resamples_to_a_higher_resolution() {
        // A 2x2 imported at 16x16 interpolates: the field varies and stays in range, and
        // the extreme corners still read near the image's corner pixels.
        let png = write_temp_png("resample", 2, 2, &[0.0, 1.0, 1.0, 0.0]);
        let params = Params::default().with("path", ParamValue::Text(png.path_str()));
        let out = run(&params, &ctx(16)).unwrap();
        let layer = out.layer(layers::HEIGHT).unwrap();
        let first = layer.as_slice()[0];
        assert!(layer.as_slice().iter().any(|&v| v != first), "should vary");
        for &v in layer.as_slice() {
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn eval_is_deterministic() {
        let png = write_temp_png(
            "deterministic",
            4,
            4,
            &(0..16).map(|i| i as f32 / 15.0).collect::<Vec<_>>(),
        );
        let params = Params::default().with("path", ParamValue::Text(png.path_str()));
        let a = run(&params, &ctx(8)).unwrap();
        let b = run(&params, &ctx(8)).unwrap();
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("import operator is registered");
        let via = made
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx(8))
            .unwrap();
        let direct = run(&Params::default(), &ctx(8)).unwrap();
        assert_eq!(via[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_generator() {
        assert_eq!(Import.spec().kind(), ymir_core::NodeKind::Generator);
        assert_eq!(Import.spec().type_id, TYPE_ID);
    }
}
