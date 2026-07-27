//! The materials on a field, and the colour image they composite to (#334).
//!
//! A Material node writes an independent `[0, 1]` weight layer and nothing else (see
//! `design/texturing.md`). This is where those weights become something to look at: the editor
//! pairs each weight layer with the colour of the node that wrote it, stacks them, and produces
//! one image the viewport tints the terrain with.
//!
//! # Why the compositing happens here and not in a shader
//!
//! The editor holds both halves already: the weights ride on the `Field` it is previewing, and
//! the colours are parameters on the graph it is editing. Doing it in Rust rather than WGSL means
//! one texture instead of one per material, so there is no cap on how many materials a graph can
//! have; the rule is a pure function with tests rather than shader behaviour checkable only by
//! eye; and the 2D map can show the same image the 3D view does without a second implementation.
//!
//! The cost is a CPU pass when weights, colours, or order change. At preview resolution that is
//! milliseconds, and it happens **on change, never per frame**, the discipline
//! `design/node-status-and-build-monitor.md` sets out.
//!
//! # The composite rule
//!
//! An **over** composite, bottom to top: each material paints onto what is beneath it in
//! proportion to its weight. A material at weight 1 hides everything below it; at 0.4 it shows
//! four tenths of the way over. This is order-dependent on purpose, because that is what makes a
//! stack mean anything: rock poking through grass, snow lying on top of both.
//!
//! A normalized weighted average was the alternative and is order-*independent*, which would have
//! made the ordering unable to express "on top" at all.
//!
//! The alpha channel is coverage: how much of the cell any material claims. A cell no material
//! claims stays at zero and the terrain shows through untinted, which is the honest picture of a
//! stack with no base material under it.
//!
//! This predicts the engine; it does not constrain the export, which writes the raw independent
//! weights and lets the engine do its own blending.

use std::sync::Arc;

use eframe::egui;
use ymir_core::{Field, Graph, Layer, layers};

/// One material as the editor sees it: which layer holds its weight, and what colour it shows as.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MaterialEntry {
    /// The material's name, which is also the suffix of its `material.<name>` layer.
    pub name: String,
    /// Preview colour, sRGB in `[0, 1]`, as the Material node's parameter holds it.
    pub color: [f32; 3],
}

/// The colour a material shows as when no node in the graph claims its layer.
///
/// Reachable: a field can carry a `material.*` layer whose Material node has since been deleted or
/// renamed, and a project opened against a build that lacks a node still has the layer. A visible
/// neutral is better than dropping the material silently, which would make it look like the weight
/// was never written.
const ORPHAN_COLOR: [f32; 3] = [0.5, 0.5, 0.5];

/// The materials carried on `field`, paired with the colour of the Material node that wrote each.
///
/// Ordered by name, which is deterministic and stable across edits. That is a placeholder for a
/// user-chosen order, not a claim that alphabetical is meaningful; the order control is the next
/// step, and this keeps the composite reproducible until then.
pub(crate) fn materials_on(field: &Field, graph: &Graph) -> Vec<MaterialEntry> {
    let mut found: Vec<MaterialEntry> = field
        .layers()
        .filter_map(|(layer, _)| layers::material_name(layer))
        .map(|name| MaterialEntry {
            name: name.to_string(),
            color: color_of(graph, name),
        })
        .collect();
    // `Field` stores layers in name order already, so this is belt and braces against that
    // changing: the composite must not depend on map iteration order.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The colour parameter of the Material node named `name`, or a neutral when none claims it.
///
/// Two nodes can carry the same name, in which case both write the same layer and the last to
/// evaluate wins. The colour follows the same rule as far as it can: the highest `stable_id`
/// among the claimants, which is the later-created node. It cannot be exact, since which node
/// evaluated last depends on the wiring, but it is deterministic and it matches the common case
/// of a duplicated node being edited into a new material.
fn color_of(graph: &Graph, name: &str) -> [f32; 3] {
    let mut best: Option<(u64, [f32; 3])> = None;
    for id in graph.nodes_of_type("modifier.material") {
        let Some(params) = graph.params(id) else {
            continue;
        };
        if params.get_str("name", "").trim() != name {
            continue;
        }
        let stable_id = graph.stable_id(id).unwrap_or_default();
        let rgb = params.get_color("color", [0.5, 0.5, 0.5]);
        let rgb = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
        if best.is_none_or(|(seen, _)| stable_id >= seen) {
            best = Some((stable_id, rgb));
        }
    }
    best.map_or(ORPHAN_COLOR, |(_, rgb)| rgb)
}

/// The composited colour and coverage of one cell: an over stack of `materials`, bottom first.
///
/// Coverage is how much of the cell any material claims, accumulating the same way the colour
/// does. Zero means nothing claims it.
fn sample(
    materials: &[MaterialEntry],
    weights: &[Arc<Layer>],
    x: usize,
    y: usize,
) -> ([f32; 3], f32) {
    let mut rgb = [0.0_f32; 3];
    let mut covered = 0.0_f32;
    for (material, weight) in materials.iter().zip(weights) {
        let a = weight.get(x, y).unwrap_or(0.0).clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        for (channel, over) in rgb.iter_mut().zip(material.color) {
            *channel = *channel * (1.0 - a) + over * a;
        }
        covered = covered * (1.0 - a) + a;
    }
    (rgb, covered)
}

/// Tints `image` with the materials on `field`, in place. `materials` is bottom first.
///
/// The material colour is **multiplied** into the shading rather than replacing it, so the relief
/// still reads: a lit slope stays lighter than a shaded one under the same material. Replacing
/// would give flat colour and throw away the shape, which is the thing being judged.
///
/// A cell no material claims is left alone, so bare terrain shows as terrain. That is the visible
/// sign of a stack with no base material under it.
pub(crate) fn apply(image: &mut egui::ColorImage, field: &Field, materials: &[MaterialEntry]) {
    if materials.is_empty() {
        return;
    }
    let weights: Vec<_> = materials
        .iter()
        .map(|m| field.layer_or(&layers::material(&m.name), 0.0))
        .collect();
    let (w, h) = (field.width(), field.height());
    debug_assert_eq!(
        image.pixels.len(),
        w * h,
        "material overlay expects the image and field to align cell-for-cell"
    );
    if image.pixels.len() != w * h {
        return;
    }

    for y in 0..h {
        for x in 0..w {
            let (rgb, covered) = sample(materials, &weights, x, y);
            if covered <= 0.0 {
                continue;
            }
            let pixel = &mut image.pixels[y * w + x];
            let shaded = [pixel.r(), pixel.g(), pixel.b()];
            let mut out = [0_u8; 3];
            for ((slot, lit), tint) in out.iter_mut().zip(shaded).zip(rgb) {
                let lit = f32::from(lit) / 255.0;
                // Multiply tints while keeping the shading's light and dark; mix by coverage so a
                // partly claimed cell is partly tinted.
                *slot = byte(lit + (lit * tint - lit) * covered);
            }
            *pixel = egui::Color32::from_rgba_unmultiplied(out[0], out[1], out[2], pixel.a());
        }
    }
}

/// A `[0, 1]` channel as a byte, rounded so a value that came from a byte returns to it.
fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Layer, ParamValue, Params, Region, registry};

    const RED: [f32; 3] = [1.0, 0.0, 0.0];
    const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

    /// A field carrying one weight layer per `(name, weight)`.
    fn field_with(materials: &[(&str, f32)]) -> Field {
        let mut field = Field::new(2, 2, Region::UNIT);
        for (name, weight) in materials {
            field.set_layer(
                layers::material(name),
                Arc::new(Layer::filled(2, 2, *weight)),
            );
        }
        field
    }

    fn entry(name: &str, color: [f32; 3]) -> MaterialEntry {
        MaterialEntry {
            name: name.to_string(),
            color,
        }
    }

    /// The composited colour and coverage of cell (0, 0), as bytes, for `materials` over `field`.
    fn at_origin(field: &Field, materials: &[MaterialEntry]) -> ([u8; 3], u8) {
        let weights: Vec<_> = materials
            .iter()
            .map(|m| field.layer_or(&layers::material(&m.name), 0.0))
            .collect();
        let (rgb, covered) = sample(materials, &weights, 0, 0);
        ([byte(rgb[0]), byte(rgb[1]), byte(rgb[2])], byte(covered))
    }

    /// A mid-grey shaded image the size of `field`, standing in for the hillshade.
    fn grey(field: &Field) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied(
            [field.width(), field.height()],
            &[128, 128, 128, 255].repeat(field.width() * field.height()),
        )
    }

    #[test]
    fn a_full_weight_material_hides_what_is_under_it() {
        let field = field_with(&[("base", 1.0), ("top", 1.0)]);
        let (rgb, cov) = at_origin(&field, &[entry("base", RED), entry("top", BLUE)]);
        assert_eq!(rgb, [0, 0, 255], "the top material wins");
        assert_eq!(cov, 255);
    }

    #[test]
    fn order_decides_the_result() {
        // The property the whole ordering design rests on. A normalized weighted average would
        // give the same answer both ways round, and could not express "snow on top".
        let field = field_with(&[("a", 1.0), ("b", 1.0)]);
        let up = at_origin(&field, &[entry("a", RED), entry("b", BLUE)]);
        let down = at_origin(&field, &[entry("b", BLUE), entry("a", RED)]);
        assert_ne!(up, down);
        assert_eq!(up.0, [0, 0, 255]);
        assert_eq!(down.0, [255, 0, 0]);
    }

    #[test]
    fn a_partial_weight_paints_partway_over() {
        let field = field_with(&[("base", 1.0), ("top", 0.5)]);
        let (rgb, cov) = at_origin(&field, &[entry("base", RED), entry("top", BLUE)]);
        assert_eq!(rgb, [128, 0, 128], "halfway between the two colours");
        assert_eq!(cov, 255, "the base still covers the cell fully");
    }

    #[test]
    fn a_cell_no_material_claims_is_left_uncovered() {
        // Coverage is what lets bare terrain stay untinted rather than being painted black, and
        // it is the visible sign of a stack with no base material under it.
        let field = field_with(&[("only", 0.0)]);
        assert_eq!(at_origin(&field, &[entry("only", RED)]).1, 0);
    }

    #[test]
    fn partial_coverage_is_reported_as_partial() {
        let field = field_with(&[("only", 0.25)]);
        assert_eq!(at_origin(&field, &[entry("only", RED)]).1, 64);
    }

    #[test]
    fn a_weight_outside_the_range_cannot_push_a_colour_out_of_gamut() {
        // The node clamps, but a field can reach here from a project file or an older build.
        let field = field_with(&[("a", 4.0)]);
        assert_eq!(at_origin(&field, &[entry("a", RED)]), ([255, 0, 0], 255));
    }

    #[test]
    fn applying_a_material_tints_the_shading_without_flattening_it() {
        // Multiply, not replace: a lit cell under a material stays lighter than a shaded one, so
        // the relief still reads. Replacing would give flat colour and throw the shape away.
        let field = field_with(&[("a", 1.0)]);
        let mut light =
            egui::ColorImage::from_rgba_unmultiplied([2, 2], &[200, 200, 200, 255].repeat(4));
        let mut dark =
            egui::ColorImage::from_rgba_unmultiplied([2, 2], &[60, 60, 60, 255].repeat(4));
        let materials = [entry("a", RED)];
        apply(&mut light, &field, &materials);
        apply(&mut dark, &field, &materials);
        assert_eq!(light.pixels[0].g(), 0, "the tint removed the green channel");
        assert!(
            light.pixels[0].r() > dark.pixels[0].r(),
            "the lit cell stays lighter than the shaded one"
        );
    }

    #[test]
    fn applying_nothing_leaves_the_image_alone() {
        let field = field_with(&[("a", 0.0)]);
        let mut image = grey(&field);
        let before = image.pixels.clone();
        apply(&mut image, &field, &[entry("a", RED)]);
        assert_eq!(image.pixels, before, "an unclaimed cell is untouched");

        let mut image = grey(&field);
        let before = image.pixels.clone();
        apply(&mut image, &field, &[]);
        assert_eq!(image.pixels, before, "no materials is a no-op");
    }

    #[test]
    fn materials_are_found_on_the_field_and_coloured_from_the_graph() {
        let mut graph = Graph::new();
        let op = registry::make("modifier.material").expect("material is registered");
        let node = graph.add_op(
            op,
            Params::new()
                .with("name", ParamValue::Text("rock".into()))
                .with("color", ParamValue::Color([1.0, 0.0, 0.0])),
        );
        assert!(graph.stable_id(node).is_some());

        let found = materials_on(&field_with(&[("rock", 1.0)]), &graph);
        assert_eq!(found, vec![entry("rock", RED)]);
    }

    #[test]
    fn a_material_no_node_claims_still_shows() {
        // Reachable by deleting or renaming the node while its layer is still on a cached field.
        // Dropping it would look like the weight was never written.
        let found = materials_on(&field_with(&[("ghost", 1.0)]), &Graph::new());
        assert_eq!(found, vec![entry("ghost", ORPHAN_COLOR)]);
    }

    #[test]
    fn ordinary_layers_are_not_mistaken_for_materials() {
        let mut field = field_with(&[("rock", 1.0)]);
        field.set_layer(layers::HEIGHT, Arc::new(Layer::filled(2, 2, 0.5)));
        field.set_layer(layers::MASK, Arc::new(Layer::filled(2, 2, 1.0)));
        let found = materials_on(&field, &Graph::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "rock");
    }
}
