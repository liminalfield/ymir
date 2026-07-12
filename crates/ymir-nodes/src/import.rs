//! The import generator: a heightmap image loaded as a field.
//!
//! Decodes the PNG at `path` and resamples it (bilinear) into the requested resolution
//! and region, writing the luminance to the `height` layer. The image spans the whole
//! `UNIT` world region, so a tiled build samples the matching sub-rectangle and an untiled
//! build samples the whole image.
//!
//! Placement (offset, rotation, scale about the image centre) is folded into that single
//! resample, so an imported map can be positioned without a downstream Transform node and
//! at full source fidelity (one resample of the original, not a resample of a resample). An
//! `edge` policy decides what a sample that maps outside the image reads: extend the edge
//! (the default), zero, or wrap. With the default placement the map fills the region exactly
//! as before, so existing graphs are unchanged.
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
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ymir_core::import::{DecodedImage, decode_png};
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
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
            params: vec![
                ParamSpec::new("path", ParamKind::Path, ParamValue::Text(String::new())),
                // Placement: where the image sits in the world. Folded into the single
                // resample, so it samples the original source at full fidelity rather than a
                // resample-of-a-resample. Offsets pan in region widths; rotation and scale
                // are about the image centre.
                ParamSpec::new(
                    "offset_x",
                    ParamKind::Float {
                        min: -1.0,
                        max: 1.0,
                    },
                    ParamValue::Float(0.0),
                ),
                ParamSpec::new(
                    "offset_y",
                    ParamKind::Float {
                        min: -1.0,
                        max: 1.0,
                    },
                    ParamValue::Float(0.0),
                ),
                ParamSpec::new(
                    "rotation",
                    ParamKind::Float {
                        min: 0.0,
                        max: 360.0,
                    },
                    ParamValue::Float(0.0),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "scale",
                    ParamKind::Float {
                        min: 0.05,
                        max: 8.0,
                    },
                    ParamValue::Float(1.0),
                ),
                // What to read where the placement maps outside the image: extend the edge
                // (the default, and the old behaviour), zero, or wrap (seamless only for a
                // tiling source).
                ParamSpec::new(
                    "edge",
                    ParamKind::Enum {
                        options: EDGE_POLICIES,
                    },
                    ParamValue::Text(EDGE_EXTEND.to_string()),
                ),
            ],
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
        // node error rather than silently producing nothing. The decode is cached, so
        // changing a placement param re-resamples without re-reading the file.
        let image = load_cached_image(path)?;
        Ok(vec![resample(&image, ctx, &Placement::from_params(params))])
    }
}

/// How many decoded images to keep. A decoded image can be large (a full-resolution
/// heightmap), so this is a small most-recently-used cache, not unbounded memoization; a
/// graph rarely imports more than a couple of distinct files at once.
const DECODE_CACHE_CAP: usize = 4;

/// Identifies a decoded image by its source: the path plus the file's length and modified
/// time, so editing the file on disk (which changes one or both) reloads it rather than
/// serving a stale decode. `mtime` is optional because not every platform reports it; the
/// length still catches a resize, which most edits are.
#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    path: String,
    len: u64,
    mtime: Option<SystemTime>,
}

/// A small most-recently-used cache of decoded images. The PNG decode is the expensive part
/// of the Import node, and the evaluator memoizes on a key that includes the placement
/// params, so without this, dragging offset/rotation/scale would re-decode the whole file
/// every frame.
struct DecodeCache {
    /// Most-recent first; bounded to [`DECODE_CACHE_CAP`].
    entries: Vec<(CacheKey, Arc<DecodedImage>)>,
}

impl DecodeCache {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Returns the cached image for `key`, promoting it to most-recent.
    fn get(&mut self, key: &CacheKey) -> Option<Arc<DecodedImage>> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(pos);
        let image = entry.1.clone();
        self.entries.insert(0, entry);
        Some(image)
    }

    /// Inserts `image` as most-recent, evicting the least-recent beyond the cap.
    fn insert(&mut self, key: CacheKey, image: Arc<DecodedImage>) {
        self.entries.retain(|(k, _)| *k != key);
        self.entries.insert(0, (key, image));
        self.entries.truncate(DECODE_CACHE_CAP);
    }
}

/// Process-wide decode cache shared by every Import node and evaluation thread.
static DECODE_CACHE: Mutex<DecodeCache> = Mutex::new(DecodeCache::new());

/// Decodes the PNG at `path`, or returns a cached decode when the file is unchanged (same
/// length and modified time). The decode runs without the lock held, so concurrent decodes
/// of different files do not serialize; a rare double-decode of the same file just
/// overwrites with identical data.
fn load_cached_image(path: &str) -> Result<Arc<DecodedImage>> {
    let meta = std::fs::metadata(path)?;
    let key = CacheKey {
        path: path.to_string(),
        len: meta.len(),
        // shortcut-ok: mtime is optional metadata; when the platform omits it the key falls
        // back to path + length, which still reloads on a resize.
        mtime: meta.modified().ok(),
    };
    // A poisoned lock means a thread panicked mid-update; the data is still consistent, so
    // recover the guard rather than turning it into a panic here.
    if let Some(image) = DECODE_CACHE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(image);
    }
    let image = Arc::new(decode_png(BufReader::new(File::open(path)?))?);
    DECODE_CACHE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, image.clone());
    Ok(image)
}

/// Edge-policy ids: how a sample that maps outside the image is resolved.
const EDGE_EXTEND: &str = "extend";
const EDGE_ZERO: &str = "zero";
const EDGE_WRAP: &str = "wrap";
const EDGE_POLICIES: &[&str] = &[EDGE_EXTEND, EDGE_ZERO, EDGE_WRAP];

/// Smallest scale honoured, so a zero or negative `scale` param cannot divide by zero.
const MIN_SCALE: f64 = 0.05;

/// What a pixel tap outside the image reads.
#[derive(Clone, Copy)]
enum EdgePolicy {
    /// Clamp to the nearest edge pixel (the default, and the pre-placement behaviour).
    Extend,
    /// Read zero outside the image (a void around a shrunk or offset placement).
    Zero,
    /// Tile the image (seamless only when the source itself tiles).
    Wrap,
}

impl EdgePolicy {
    /// Resolves an edge-policy id, defaulting to [`EdgePolicy::Extend`] for an unknown one.
    fn from_id(id: &str) -> Self {
        match id {
            EDGE_ZERO => Self::Zero,
            EDGE_WRAP => Self::Wrap,
            _ => Self::Extend,
        }
    }
}

/// The image's placement in the world, precomputed once: the inverse of an offset, a
/// rotation, and a uniform scale about the image centre, so the per-cell loop only does the
/// inverse map and a sample.
struct Placement {
    offset_x: f64,
    offset_y: f64,
    /// `cos`/`sin` of the rotation, used to apply its inverse (a rotation by `-angle`).
    cos: f64,
    sin: f64,
    /// `1 / scale`, the inverse scale applied to the offset-from-centre.
    inv_scale: f64,
    edge: EdgePolicy,
}

impl Placement {
    fn from_params(params: &Params) -> Self {
        let angle = params.get_f64("rotation", 0.0).to_radians();
        // Range is advisory until the graph/UI validate, so clamp defensively away from 0.
        let scale = params.get_f64("scale", 1.0).max(MIN_SCALE);
        Self {
            offset_x: params.get_f64("offset_x", 0.0),
            offset_y: params.get_f64("offset_y", 0.0),
            cos: angle.cos(),
            sin: angle.sin(),
            inv_scale: 1.0 / scale,
            edge: EdgePolicy::from_id(params.get_str("edge", EDGE_EXTEND)),
        }
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Import) }
}

inventory::submit! {
    crate::category::NodeGroup { type_id: TYPE_ID, group: "source", sort: 50 }
}

/// Resamples `image` into a field at the context's resolution and region, bilinearly,
/// placing the image by `placement`. The image spans the `UNIT` square by default; the
/// placement offsets, rotates, and scales it about its centre. Each output cell inverts the
/// placement to find its source position in the image, then samples there.
fn resample(image: &DecodedImage, ctx: &EvalContext, placement: &Placement) -> Field {
    let region = ctx.region;
    let layer = Layer::from_par_fn(ctx.width, ctx.height, |x, y| {
        // Output cell centre as a world position in [0, 1].
        let u = (x as f64 + 0.5) / ctx.width as f64;
        let v = (y as f64 + 0.5) / ctx.height as f64;
        let wx = region.min_x + u * region.width();
        let wy = region.min_y + v * region.height();

        // Invert the placement: world position -> image UV. Translate by -offset, take the
        // offset from the centre, inverse-rotate, inverse-scale, back to UV. With the
        // defaults (no offset, 0 rotation, unit scale) this reduces to UV = world.
        let dx = wx - placement.offset_x - 0.5;
        let dy = wy - placement.offset_y - 0.5;
        let rx = placement.cos * dx + placement.sin * dy;
        let ry = -placement.sin * dx + placement.cos * dy;
        let uvx = 0.5 + rx * placement.inv_scale;
        let uvy = 0.5 + ry * placement.inv_scale;
        sample_bilinear(image, uvx, uvy, placement.edge)
    });

    Field::new(ctx.width, ctx.height, region).with_layer(layers::HEIGHT, Arc::new(layer))
}

/// Bilinearly samples `image` at UV position `(uvx, uvy)` in `[0, 1]`. Pixel centres sit at
/// integer + 0.5; taps outside the image are resolved by `edge`.
fn sample_bilinear(image: &DecodedImage, uvx: f64, uvy: f64, edge: EdgePolicy) -> f32 {
    let fx = uvx * image.width as f64 - 0.5;
    let fy = uvy * image.height as f64 - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = (fx - x0) as f32;
    let ty = (fy - y0) as f32;
    let (x0, y0) = (x0 as i64, y0 as i64);

    let top = lerp(
        pixel(image, x0, y0, edge),
        pixel(image, x0 + 1, y0, edge),
        tx,
    );
    let bottom = lerp(
        pixel(image, x0, y0 + 1, edge),
        pixel(image, x0 + 1, y0 + 1, edge),
        tx,
    );
    lerp(top, bottom, ty)
}

/// One pixel tap, with out-of-bounds coordinates resolved by `edge`. Bilinear taps just
/// outside the image are common (at the frame edges and around a rotated placement), so the
/// policy is applied per tap, giving a smooth blend to zero, a clean clamp, or a seam-free
/// wrap rather than one decision for the whole sample.
fn pixel(image: &DecodedImage, x: i64, y: i64, edge: EdgePolicy) -> f32 {
    let (w, h) = (image.width as i64, image.height as i64);
    if w == 0 || h == 0 {
        return 0.0;
    }
    let (px, py) = match edge {
        EdgePolicy::Extend => (x.clamp(0, w - 1), y.clamp(0, h - 1)),
        EdgePolicy::Wrap => (x.rem_euclid(w), y.rem_euclid(h)),
        EdgePolicy::Zero => {
            if x < 0 || x >= w || y < 0 || y >= h {
                return 0.0;
            }
            (x, y)
        }
    };
    image.data[(py * w + px) as usize]
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

    /// A 4x4 image with a horizontal ramp (dark left, bright right), constant down each
    /// column, so placement effects along x are easy to reason about.
    fn write_gradient(name: &str) -> TempPng {
        let vals: Vec<f32> = (0..16).map(|i| (i % 4) as f32 / 3.0).collect();
        write_temp_png(name, 4, 4, &vals)
    }

    fn placed(png: &TempPng, extra: &[(&str, ParamValue)]) -> Field {
        let mut params = Params::default().with("path", ParamValue::Text(png.path_str()));
        for (k, v) in extra {
            params = params.with(*k, v.clone());
        }
        run(&params, &ctx(16)).unwrap()
    }

    #[test]
    fn offset_pans_the_image() {
        // Moving the bright-right image rightward (+offset_x) slides darker, left-of-image
        // content under a fixed interior cell, so it reads darker than with no offset.
        let png = write_gradient("offset");
        let base = placed(&png, &[]);
        let panned = placed(&png, &[("offset_x", ParamValue::Float(0.3))]);
        assert_ne!(base.content_hash(), panned.content_hash());
        assert!(at(&panned, 11, 8) < at(&base, 11, 8));
    }

    #[test]
    fn rotation_changes_the_placement() {
        // The ramp varies along x only; a quarter turn makes it vary along y instead, so the
        // result differs from the unrotated import.
        let png = write_gradient("rotate");
        let base = placed(&png, &[]);
        let turned = placed(&png, &[("rotation", ParamValue::Float(90.0))]);
        assert_ne!(base.content_hash(), turned.content_hash());
    }

    #[test]
    fn scale_down_with_zero_edge_leaves_a_void() {
        // Shrunk to half about the centre with a zero edge: the corners map outside the image
        // and read 0, while the centre still samples the ramp.
        let png = write_gradient("void");
        let out = placed(
            &png,
            &[
                ("scale", ParamValue::Float(0.5)),
                ("edge", ParamValue::Text(EDGE_ZERO.to_string())),
            ],
        );
        assert!(at(&out, 0, 0) < 0.01, "corner should be the zero void");
        assert!(at(&out, 8, 8) > 0.1, "centre should still sample the image");
    }

    #[test]
    fn edge_zero_and_extend_differ_outside_the_image() {
        // Slide the image left (-offset_x) so the right of the frame maps past its bright
        // edge. Extend clamps to that bright edge; zero reads the void.
        let png = write_gradient("edge");
        let shifted = [("offset_x", ParamValue::Float(-0.5))];
        let extend = placed(&png, &shifted);
        let zero = placed(
            &png,
            &[
                ("offset_x", ParamValue::Float(-0.5)),
                ("edge", ParamValue::Text(EDGE_ZERO.to_string())),
            ],
        );
        assert!(at(&extend, 15, 8) > 0.8, "extend clamps to the bright edge");
        assert!(at(&zero, 15, 8) < 0.05, "zero reads the void");
    }

    #[test]
    fn decode_cache_serves_hits_and_evicts_by_recency() {
        let img = |w: usize| {
            Arc::new(DecodedImage {
                width: w,
                height: 1,
                data: vec![0.0; w],
            })
        };
        let key = |path: &str, len: u64| CacheKey {
            path: path.to_string(),
            len,
            mtime: None,
        };

        let mut cache = DecodeCache::new();
        let a = img(1);
        cache.insert(key("a", 1), a.clone());
        // A matching key hits and returns the very same decode.
        assert!(Arc::ptr_eq(&cache.get(&key("a", 1)).unwrap(), &a));
        // A changed file (different length) is a different key, so it misses and reloads.
        assert!(cache.get(&key("a", 2)).is_none());
        // Filling past the cap evicts the least-recently-used entry.
        for i in 0..DECODE_CACHE_CAP {
            cache.insert(key(&format!("x{i}"), 1), img(1));
        }
        assert!(
            cache.get(&key("a", 1)).is_none(),
            "a should have been evicted"
        );
    }

    #[test]
    fn wrap_by_a_full_width_equals_no_offset() {
        // An offset of exactly one region width with wrap re-tiles the image back onto
        // itself: every tap shifts by a whole image width, so the result is byte-identical.
        let png = write_gradient("wrap");
        let base = placed(&png, &[("edge", ParamValue::Text(EDGE_WRAP.to_string()))]);
        let wrapped = placed(
            &png,
            &[
                ("offset_x", ParamValue::Float(1.0)),
                ("edge", ParamValue::Text(EDGE_WRAP.to_string())),
            ],
        );
        assert_eq!(base.content_hash(), wrapped.content_hash());
    }
}
