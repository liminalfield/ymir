//! Display-string resolution: the `tr(key)` seam.
//!
//! v1 is a trivial English lookup. A node or category holds only ids; the words
//! live here, keyed by convention (`node-<type_id>`, `node-<type_id>-desc`,
//! `category-<id>`, `category-<id>-desc`, ...). When localization becomes a goal,
//! a Fluent (i18n-embed) backend slots in behind this same `tr` signature with no
//! change to nodes or specs. A missing key falls back to the key itself, so a
//! missing string is visible in the UI rather than a panic.

/// Resolves a display key to its English string, or returns the key unchanged if
/// unknown. The returned reference borrows the key, so a `'static` key (the usual
/// case, derived from a `&'static` `type_id`) yields a `'static` string.
#[must_use]
pub fn tr(key: &str) -> &str {
    lookup(key).unwrap_or(key)
}

/// Looks a display key up in the catalog, returning `None` when it is absent. This is the
/// same table [`tr`] uses; the difference is that a miss is reported rather than folded
/// into the key-echo, so [`resolve_param`] can distinguish an authored string from the
/// prettified fallback and fall through the resolution tiers.
fn lookup(key: &str) -> Option<&'static str> {
    Some(match key {
        // Categories.
        "category-generator" => "Generators",
        "category-selector" => "Selectors",
        "category-adjust" => "Adjust",
        "category-filter" => "Filters",
        "category-combine" => "Combine",
        "category-geology" => "Geology",
        "category-utility" => "Utility",
        "category-output" => "Outputs",

        // fBm generator.
        "node-generator.fbm" => "fBm Noise",
        "node-generator.fbm-desc" => "Fractional Brownian motion of Perlin noise.",

        // Import generator.
        "node-generator.import" => "Import",
        "node-generator.import-desc" => {
            "Loads a heightmap image as a field, placed by offset, rotation, and scale."
        }
        "node-generator.paint" => "Paint",
        "node-generator.paint-desc" => {
            "A hand-painted [0, 1] selection, brushed on the 2D map or 3D surface."
        }

        // Flow (curl-warped) generator.
        "node-generator.flow" => "Flow Noise",
        "node-generator.flow-desc" => {
            "Noise warped along divergence-free curl streamlines, for a swirly, marbled, fluid look."
        }

        // Cellular (Worley) generators.
        "node-generator.cellular_bumps" => "Cellular Bumps",
        "node-generator.cellular_bumps-desc" => {
            "Worley noise as cones peaking at scattered points: rock piles, bumps, and scales."
        }
        "node-generator.cellular_cracks" => "Cellular Cracks",
        "node-generator.cellular_cracks-desc" => {
            "Worley cell-edge network: cracks, fractures, dried mud, and rocky cell walls."
        }
        "node-generator.cellular_regions" => "Cellular Regions",
        "node-generator.cellular_regions-desc" => {
            "Worley cells as flat, discrete regions (plates, zones) to shape or scatter."
        }
        "node-generator.constant" => "Constant",
        "node-generator.constant-desc" => {
            "A flat field at a chosen [0, 1] greyscale value: a fixed level to blend against, \
             offset or scale with, threshold, or use as a uniform control field."
        }

        // Billow generator.
        "node-generator.billow" => "Billow Noise",
        "node-generator.billow-desc" => {
            "Puffy, rounded mounds and dunes, the rounded inverse of ridged noise."
        }

        // Hybrid-multifractal generator.
        "node-generator.hybrid" => "Hybrid Multifractal",
        "node-generator.hybrid-desc" => {
            "Realistic plains-to-mountains terrain in one node: roughness scales with \
             altitude, so valleys stay smooth and flat while peaks get rough and broken."
        }

        // Ridged-multifractal generator.
        "node-generator.ridged" => "Ridged Noise",
        "node-generator.ridged-desc" => {
            "Ridged multifractal noise, with sharp mountain ridgelines."
        }

        // Radial gradient generator.
        "node-generator.radial" => "Radial Gradient",
        "node-generator.radial-desc" => {
            "A smooth radial dome (1 at the center, 0 at the radius): an envelope to \
             multiply with detail, or shape downstream with a Curve."
        }

        // Radial falloff generator.
        "node-generator.falloff" => "Radial Falloff",
        "node-generator.falloff-desc" => {
            "A linear radial distance ramp (0 at the center, 1 at the radius): feed a \
             Curve to draw any radial cross-section, a dome, crater, caldera, or terraces."
        }

        // Directional gradient generator.
        "node-generator.gradient" => "Gradient",
        "node-generator.gradient-desc" => {
            "A smooth directional ramp (0 to 1 across a band): the non-centered envelope \
             for a coast-to-highland trend or a dune-field direction."
        }

        // Ring (annulus) generator.
        "node-generator.ring" => "Ring",
        "node-generator.ring-desc" => {
            "A smooth circular ridge (1 on the radius, 0 on each flank): the envelope for \
             a crater rim, caldera wall, or atoll."
        }

        // Rectangle generator.
        "node-generator.rect" => "Rectangle",
        "node-generator.rect-desc" => {
            "A flat-topped rectangular footprint with soft, rounded flanks."
        }

        // Polygon (regular n-gon) generator.
        "node-generator.polygon" => "Polygon",
        "node-generator.polygon-desc" => "A flat-topped regular polygon with soft flanks.",

        // Blend.
        "node-modifier.blend" => "Blend",
        "node-modifier.blend-desc" => {
            "Composites two fields by a mode (normal, add, subtract, multiply, max, min, \
             difference) eased in by opacity."
        }

        // Height selector.
        "node-modifier.height" => "Height",
        "node-modifier.height-desc" => {
            "Selects a band of elevation: high where the normalized height is within \
             min..max, softening over the falloff."
        }

        // Curvature selector.
        "node-modifier.curvature" => "Curvature",
        "node-modifier.curvature-desc" => {
            "Selects convex (ridges, outcrops) or concave (valleys, hollows) ground from the surface curvature."
        }

        // Slope selector.
        "node-modifier.slope" => "Slope",
        "node-modifier.slope-desc" => {
            "Selects a band of steepness: high where the slope angle is within min..max degrees, softening over the falloff."
        }

        // Occlusion selector.
        "node-modifier.occlusion" => "Occlusion",
        "node-modifier.occlusion-desc" => {
            "Ambient-occlusion / sky-view measure: high in crevices and valley floors hemmed in by higher ground, low on open peaks and flats."
        }

        // Aspect selector.
        "node-modifier.aspect" => "Aspect",
        "node-modifier.aspect-desc" => {
            "Selects slopes facing a compass direction: high where the terrain faces the direction, softening over the falloff."
        }

        // Distance selector.
        "node-modifier.distance" => "Distance",
        "node-modifier.distance-desc" => {
            "Selects a band around a height contour by true distance, fading over a range in metres."
        }

        // Invert.
        "node-modifier.invert" => "Invert",
        "node-modifier.invert-desc" => "Flips the height layer (1 - height).",
        "node-modifier.sculpt" => "Sculpt",
        "node-modifier.sculpt-desc" => {
            "Sculpt terrain by brushing height onto it: paint raises, erase lowers, and overlapping strokes build up."
        }

        // Per-node labels for the shared brush UI (the enable button's verb and the two mode names),
        // resolved by convention from the node's type_id so a sculpt reads Sculpt/Raise/Lower while the
        // mask reads Paint/Paint/Erase.
        "paint-verb-modifier.sculpt" => "Sculpt",
        "paint-mode-pos-modifier.sculpt" => "Raise",
        "paint-mode-neg-modifier.sculpt" => "Lower",
        "paint-verb-generator.paint" => "Paint",
        "paint-mode-pos-generator.paint" => "Paint",
        "paint-mode-neg-generator.paint" => "Erase",
        "node-modifier.normalize" => "Normalize",
        "node-modifier.normalize-desc" => {
            "Fits the height layer's actual min-max to [0, 1], pulling a raw or out-of-range field back into the working range."
        }
        "node-modifier.clamp" => "Clamp",
        "node-modifier.clamp-desc" => {
            "Hard-clamps the height layer into [min, max]: caps overshoots, floors basins, or bounds a value."
        }

        // Domain Warp (spatial displacement).
        "node-modifier.warp" => "Warp",
        "node-modifier.warp-desc" => {
            "Domain warp: pushes the height layer sideways by a noise field so straight features wander and regular shapes turn natural."
        }

        // Blur (spatial smoothing).
        "node-modifier.blur" => "Blur",
        "node-modifier.blur-desc" => {
            "Gaussian-blurs the height layer by a world-unit radius (the scale knob for \
             derived selectors, and feathers masks)."
        }
        "node-modifier.directional_blur" => "Directional Blur",
        "node-modifier.directional_blur-desc" => {
            "Smooths the height layer along or across a guide direction, steered by the slope or a flow field."
        }

        // Frequency Split (scale separation).
        "node-modifier.frequency_split" => "Frequency Split",
        "node-modifier.frequency_split-desc" => {
            "Splits the height into a low-frequency band and the high-frequency residual."
        }

        // Terrace (quantize into stepped bands).
        "node-modifier.terrace" => "Terrace",
        "node-modifier.terrace-desc" => {
            "Quantizes the height into stepped bands: flat treads joined by risers, for strata, benches, and mesa forms."
        }

        // Levels (range rescaling).
        "node-modifier.levels" => "Levels",
        "node-modifier.levels-desc" => {
            "Rescales the height range: stretch an input window to full, bias the midtones with gamma, and map into an output window."
        }

        // Curve (height shaping).
        "node-modifier.curve" => "Curve",
        "node-modifier.curve-desc" => "Reshapes height through an editable transfer curve.",
        "node-modifier.histogram_scan" => "Histogram-Scan",
        "node-modifier.histogram_scan-desc" => {
            "Windows a range of input values into a crisp [0, 1] mask, set by position, width, and a soft falloff."
        }

        // Expression (per-cell formula).
        "node-modifier.expression" => "Expression",
        "node-modifier.expression-desc" => {
            "A per-cell math formula over x, y, and the input layers: the escape hatch for custom math."
        }

        // Thermal erosion.
        "node-modifier.thermal_erosion" => "Thermal Erosion",
        "node-modifier.thermal_erosion-desc" => {
            "Relaxes slopes steeper than the talus angle toward repose."
        }

        // Hydraulic erosion.
        "node-modifier.hydraulic_erosion" => "Hydraulic Erosion",
        "node-modifier.hydraulic_erosion-desc" => {
            "Water carving the terrain, simulated as rain droplets that cut rills and deposit sediment."
        }

        // Stream erosion.
        "node-modifier.stream_erosion" => "Stream Erosion",
        "node-modifier.stream_erosion-desc" => {
            "Carves drainage networks from flow accumulation; outputs the river/flow map."
        }

        // Coastal bevel.
        "node-modifier.coastal" => "Coastal",
        "node-modifier.coastal-desc" => {
            "Reshapes the shore into a beach-and-bluff bevel at the world sea level, fading over a width in metres."
        }

        // Null (pass-through utility).
        "node-modifier.null" => "Null",
        "node-modifier.null-desc" => {
            "Passes the field through unchanged: a point to view, reroute, or anchor wiring."
        }

        // Subgraph container and its boundary markers (the ports, set inside it).
        "node-subgraph" => "Subgraph",
        "node-subgraph-desc" => {
            "A node holding an inner graph; dive in to build it, its ports set by Input/Output nodes."
        }
        "node-subgraph.input" => "Input",
        "node-subgraph.input-desc" => {
            "Marks a field a subgraph takes in: each one becomes an input port on the container."
        }
        "node-subgraph.output" => "Output",
        "node-subgraph.output-desc" => {
            "Marks a field a subgraph hands out: each one becomes an output port on the container."
        }

        // PNG export.
        "node-endpoint.export" => "Export PNG",
        "node-endpoint.export-desc" => "Writes the height layer to a 16-bit grayscale PNG.",

        // Raw .r16 export.
        "node-endpoint.export_r16" => "Export R16",
        "node-endpoint.export_r16-desc" => {
            "Writes the height layer to a raw 16-bit little-endian .r16 file, Unreal's other \
             native heightmap format. Same range mapping as the PNG, no header."
        }

        // EXR (32-bit float) export.
        "node-endpoint.export_exr" => "Export EXR",
        "node-endpoint.export_exr-desc" => {
            "Writes the height layer to a 32-bit float EXR: lossless, and can bake absolute \
             elevation in meters (height x world height) so the file is self-describing."
        }

        // ---- Parameters -------------------------------------------------------------
        // Resolved through `resolve_param(type_id, name)`: a node-specific
        // `param-<type_id>-<name>` override wins, else a shared `param-<name>`, else the
        // prettified id. A shared entry is used only for a parameter whose meaning is the
        // same in every node that carries it; a contextual or enum parameter takes an
        // override. Each label has a matching `-desc` one-liner that serves both the
        // inspector tooltip and the reference table.

        // Shared: meaning-invariant across every node that uses them.
        "param-frequency" => "Frequency",
        "param-frequency-desc" => {
            "Sets the feature size of the noise; higher values pack in smaller, denser features."
        }
        "param-octaves" => "Octaves",
        "param-octaves-desc" => {
            "The number of noise layers summed together; more octaves add finer detail."
        }
        "param-lacunarity" => "Lacunarity",
        "param-lacunarity-desc" => {
            "How much finer each octave is than the one before it; higher values widen the gap \
             between coarse and fine detail."
        }
        "param-gain" => "Gain",
        "param-gain-desc" => {
            "How much each finer octave contributes; higher values make the fine detail rougher \
             and more pronounced."
        }
        "param-seed" => "Seed",
        "param-seed-desc" => {
            "The random seed; changing it regenerates a different variation of the same pattern."
        }
        "param-offset_x" => "Offset X",
        "param-offset_x-desc" => {
            "Pans the noise pattern along the X axis without changing its shape."
        }
        "param-offset_y" => "Offset Y",
        "param-offset_y-desc" => {
            "Pans the noise pattern along the Y axis without changing its shape."
        }

        // Enum parameters: always node-specific, because the choice they present differs by node.
        "param-modifier.blend-mode" => "Mode",
        "param-modifier.blend-mode-desc" => "How the overlay is combined with the base field.",
        "param-modifier.curvature-mode" => "Mode",
        "param-modifier.curvature-mode-desc" => {
            "Which ground to select: convex (ridges, outcrops) or concave (valleys, hollows)."
        }
        "param-modifier.terrace-range" => "Range",
        "param-modifier.terrace-range-desc" => {
            "Whether the bands span the terrain's actual height (Auto) or sit at fixed absolute \
             elevations (Fixed)."
        }
        "param-modifier.histogram_scan-range" => "Range",
        "param-modifier.histogram_scan-range-desc" => {
            "Whether the window scans the input's actual range (Auto) or the fixed [0, 1] range \
             (Fixed)."
        }
        "param-modifier.distance-side" => "Side",
        "param-modifier.distance-side-desc" => {
            "Which side of the contour the band covers: both, only above, or only below."
        }
        "param-modifier.directional_blur-direction" => "Direction",
        "param-modifier.directional_blur-direction-desc" => {
            "Whether smoothing runs along the guide direction or across it."
        }
        "param-generator.import-edge" => "Edge",
        "param-generator.import-edge-desc" => {
            "How the field is filled where the placement maps outside the source image."
        }
        "param-endpoint.export_exr-height_units" => "Height Units",
        "param-endpoint.export_exr-height_units-desc" => {
            "Whether height is written as the normalized [0, 1] value or scaled to absolute metres \
             by World Height."
        }

        // Unknown: no entry. `tr` echoes the key; `resolve_param` falls through to prettify.
        _ => return None,
    })
}

/// Turns a `snake_case` parameter id into a friendly Title-Case label (`offset_x` ->
/// `Offset X`): underscores become spaces and each word is capitalised. The last-resort
/// label when the catalog has no entry for a parameter. A pure presentation transform, so
/// the underlying id used for lookup, hashing, and save/load is unchanged.
#[must_use]
pub fn prettify_param(name: &str) -> String {
    name.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Which tier of the parameter-string catalog produced a resolved label.
///
/// Reported so a documentation lint can flag a parameter that resolved to the prettified
/// fallback (a missing catalog entry) rather than an authored string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamSource {
    /// A node-specific `param-<type_id>-<name>` entry.
    Override,
    /// A shared `param-<name>` entry, permitted only for meaning-invariant parameters.
    Shared,
    /// No catalog entry: the label is the prettified id and there is no description.
    Prettified,
}

/// A parameter's resolved display strings and the catalog tier that produced them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedParam {
    /// The display label.
    pub label: String,
    /// The one-line description, for the inspector tooltip and the reference table. `None`
    /// when no catalog entry exists (the prettified fallback carries no prose).
    pub description: Option<String>,
    /// Which catalog tier produced the label.
    pub source: ParamSource,
}

/// Resolves a parameter's display strings by the generic-with-override rule: a node-specific
/// `param-<type_id>-<name>` wins, else a shared `param-<name>`, else the prettified id. The
/// description is read from the same tier as the label (its `-desc` sibling). Reports the tier
/// so a documentation lint can catch a parameter that fell through to the prettified fallback.
#[must_use]
pub fn resolve_param(type_id: &str, name: &str) -> ResolvedParam {
    if let Some(label) = lookup(&format!("param-{type_id}-{name}")) {
        return ResolvedParam {
            label: label.to_string(),
            description: lookup(&format!("param-{type_id}-{name}-desc")).map(str::to_string),
            source: ParamSource::Override,
        };
    }
    if let Some(label) = lookup(&format!("param-{name}")) {
        return ResolvedParam {
            label: label.to_string(),
            description: lookup(&format!("param-{name}-desc")).map(str::to_string),
            source: ParamSource::Shared,
        };
    }
    ResolvedParam {
        label: prettify_param(name),
        description: None,
        source: ParamSource::Prettified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;

    #[test]
    fn resolves_known_keys_and_falls_back() {
        assert_eq!(tr("node-generator.fbm"), "fBm Noise");
        assert_eq!(tr("category-geology"), "Geology");
        // Unknown key echoes itself, never panics.
        assert_eq!(tr("node-does.not.exist"), "node-does.not.exist");
    }

    #[test]
    fn resolve_param_prefers_override_then_shared_then_prettify() {
        // Override: a node-specific enum parameter resolves to its own entry.
        let mode = resolve_param("modifier.blend", "mode");
        assert_eq!(mode.source, ParamSource::Override);
        assert_eq!(mode.label, "Mode");
        assert!(mode.description.is_some());

        // Shared: a meaning-invariant parameter resolves to the shared entry on any node.
        let freq = resolve_param("generator.fbm", "frequency");
        assert_eq!(freq.source, ParamSource::Shared);
        assert_eq!(freq.label, "Frequency");
        assert!(freq.description.is_some());

        // Prettified: no catalog entry, so the label is the prettified id and there is no prose.
        let opacity = resolve_param("modifier.blend", "opacity");
        assert_eq!(opacity.source, ParamSource::Prettified);
        assert_eq!(opacity.label, "Opacity");
        assert!(opacity.description.is_none());
    }

    #[test]
    fn a_contextual_enum_is_never_shared() {
        // `mode` means different things in Blend and Curvature, so it must not have a shared
        // entry: each node overrides it, and both resolve at the override tier.
        assert!(
            lookup("param-mode").is_none(),
            "`mode` must never be shared"
        );
        assert_eq!(
            resolve_param("modifier.curvature", "mode").source,
            ParamSource::Override
        );
        assert_eq!(
            resolve_param("modifier.blend", "mode").source,
            ParamSource::Override
        );
    }

    #[test]
    fn prettify_param_titlecases_snake_case() {
        assert_eq!(prettify_param("offset_x"), "Offset X");
        assert_eq!(prettify_param("world_extent"), "World Extent");
        // Empty segments (leading, trailing, doubled underscores) are dropped.
        assert_eq!(prettify_param("a__b_"), "A B");
    }

    #[test]
    fn every_operator_has_a_display_name() {
        // Adding an operator without a `tr` entry fails here, restoring the
        // "a missing string is caught" property.
        for entry in registry::entries() {
            let key = format!("node-{}", entry.type_id);
            assert_ne!(tr(&key), key.as_str(), "no display string for {key:?}");
        }
    }
}
