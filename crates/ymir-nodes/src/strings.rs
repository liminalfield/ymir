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
    match key {
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
            "Loads a heightmap PNG as a field, resampled to the build resolution and placed \
             by offset, rotation, and scale. Set the file path; an empty path is a flat \
             field. The edge policy fills where the placement maps outside the image."
        }
        "node-generator.paint" => "Paint",
        "node-generator.paint-desc" => {
            "A hand-painted [0, 1] mask: brush strokes on the 2D map, rasterized at build \
             resolution. Wire it into an effect's mask input to scope the effect to a region you \
             paint. Resolution-independent; stored as editable vector strokes."
        }

        // Flow (curl-warped) generator.
        "node-generator.flow" => "Flow Noise",
        "node-generator.flow-desc" => {
            "Noise warped along divergence-free curl streamlines: a swirly, marbled, \
             fluid look. Also carries the flow direction on its flow_x / flow_y layers."
        }

        // Cellular (Worley) generators.
        "node-generator.cellular_bumps" => "Cellular Bumps",
        "node-generator.cellular_bumps-desc" => {
            "Worley noise as cones peaking at scattered feature points: rock piles, \
             bumps, scales. Frequency sets cell density, jitter how organic the placement."
        }
        "node-generator.cellular_cracks" => "Cellular Cracks",
        "node-generator.cellular_cracks-desc" => {
            "Worley cell-edge network: cracks, fractures, dried mud, rocky cell walls. \
             Frequency sets the network density, jitter how organic the cells."
        }
        "node-generator.cellular_regions" => "Cellular Regions",
        "node-generator.cellular_regions-desc" => {
            "Worley cells as flat, discrete regions (plates, zones): a control field to \
             shape or scatter per region. Frequency sets the region count, jitter shape."
        }
        "node-generator.constant" => "Constant",
        "node-generator.constant-desc" => {
            "A flat field at a chosen [0, 1] greyscale value: a fixed level to blend against, \
             offset or scale with, threshold, or use as a uniform control field."
        }

        // Billow generator.
        "node-generator.billow" => "Billow Noise",
        "node-generator.billow-desc" => {
            "Puffy, rounded mounds and dunes: the rounded inverse of ridged noise. \
             Multiply with a Shape envelope to place rolling hills or a dune field."
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
            "Ridged multifractal noise: sharp mountain ridgelines instead of fBm's rolling \
             hills. Multiply with a Shape envelope to place a massif."
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
            "A flat-topped rectangular footprint with soft, rounded flanks: the envelope \
             for a plateau, mesa, or rectangular landmass. Turn it with rotation."
        }

        // Polygon (regular n-gon) generator.
        "node-generator.polygon" => "Polygon",
        "node-generator.polygon-desc" => {
            "A flat-topped regular polygon with soft flanks: the envelope for an angular \
             plateau or faceted mesa. Set the number of sides and turn it with rotation."
        }

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
            "Selects convex (ridges, outcrops) or concave (valleys, hollows) ground from \
             the surface curvature. Measures curvature, not slope, so a plain ramp reads \
             zero. Set the scale with an upstream Blur."
        }

        // Slope selector.
        "node-modifier.slope" => "Slope",
        "node-modifier.slope-desc" => {
            "Selects a band of steepness: high where the slope angle is within \
             min..max degrees, softening over the falloff. Scale it with an upstream Blur."
        }

        // Occlusion selector.
        "node-modifier.occlusion" => "Occlusion",
        "node-modifier.occlusion-desc" => {
            "Ambient-occlusion / sky-view measure: high in crevices and valley floors hemmed in \
             by higher ground, low on open peaks and flats. Ray count and world-unit radius set \
             the sampling. Picks sheltered terrain (catchment, moisture, shadow)."
        }

        // Aspect selector.
        "node-modifier.aspect" => "Aspect",
        "node-modifier.aspect-desc" => {
            "Selects slopes facing a compass direction: high where the terrain faces the \
             direction, softening over the falloff. Slope weight suppresses flats. Being a \
             gradient, it amplifies sharp input (crease noise, thin ridges), so scale it with \
             an upstream Blur. For sun/wind-facing effects, poleward snow, and directional \
             weathering."
        }

        // Distance selector.
        "node-modifier.distance" => "Distance",
        "node-modifier.distance-desc" => {
            "Selects a band around a height contour by true distance: one near the level, \
             fading over the range (in metres), optionally on just one side. The distance is \
             an isotropic eikonal solve, so the band width does not vary with direction."
        }

        // Invert.
        "node-modifier.invert" => "Invert",
        "node-modifier.invert-desc" => "Flips the height layer (1 - height).",
        "node-modifier.sculpt" => "Sculpt",
        "node-modifier.sculpt-desc" => {
            "Sculpt terrain by brushing height onto it: paint raises, erase lowers, and overlapping \
             strokes build up pass by pass. Wire a terrain in to sculpt it, or leave the input empty \
             to build form from scratch. Strength sets how hard each pass bites; the height is not \
             clamped. Stored as editable vector strokes."
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
            "Fits the height layer's actual min-max to [0, 1] (the one-click companion to Levels): \
             pulls a raw measure or out-of-range height back into the working greyscale. A flat \
             field passes through. Mask-aware."
        }
        "node-modifier.clamp" => "Clamp",
        "node-modifier.clamp-desc" => {
            "Hard-clamps the height layer into [min, max]: caps overshoots, floors basins, or bounds \
             a value before it feeds something range-sensitive. Mask-aware."
        }

        // Domain Warp (spatial displacement).
        "node-modifier.warp" => "Warp",
        "node-modifier.warp-desc" => {
            "Domain warp: pushes the height layer sideways by a noise field so straight \
             features wander and regular shapes turn natural. Amount is in world units."
        }

        // Blur (spatial smoothing).
        "node-modifier.blur" => "Blur",
        "node-modifier.blur-desc" => {
            "Gaussian-blurs the height layer by a world-unit radius (the scale knob for \
             derived selectors, and feathers masks)."
        }
        "node-modifier.directional_blur" => "Directional Blur",
        "node-modifier.directional_blur-desc" => {
            "Smooths the height layer along (or across) a guide direction, not isotropically: \
             steer by the slope (fall line, or a distance field's shore normal) or a flow field. \
             Along combs valleys and smears downslope; across softens a cross-profile while keeping \
             the guide crest crisp. Optional guide input; degrades gracefully."
        }

        // Frequency Split (scale separation).
        "node-modifier.frequency_split" => "Frequency Split",
        "node-modifier.frequency_split-desc" => {
            "Splits the height into a low-frequency band (a blur at a world-unit cut radius) \
             and the high-frequency residual. The two recombine to the input, so you can work \
             the large forms and re-add the fine detail."
        }

        // Terrace (quantize into stepped bands).
        "node-modifier.terrace" => "Terrace",
        "node-modifier.terrace-desc" => {
            "Quantizes the height into stepped bands: flat treads joined by risers, for strata, \
             benches, and mesa forms. Band count sets the number of terraces; sharpness rounds \
             the steps (soft) or squares them off (hard). Range Auto spreads the terraces across \
             the terrain's actual height (so the count is what you see); Fixed places them at \
             absolute elevations."
        }

        // Levels (range rescaling).
        "node-modifier.levels" => "Levels",
        "node-modifier.levels-desc" => {
            "Rescales the height range: stretch an input window to full, bias the midtones \
             with gamma, map into an output window. Normalize, set amplitude, or clamp."
        }

        // Curve (height shaping).
        "node-modifier.curve" => "Curve",
        "node-modifier.curve-desc" => "Reshapes height through an editable transfer curve.",
        "node-modifier.histogram_scan" => "Histogram-Scan",
        "node-modifier.histogram_scan-desc" => {
            "Windows a range of input values into a crisp [0, 1] mask: position, width, and a soft \
             falloff. Auto range scans the input's actual min-max, so it reshapes a selector's raw \
             measure (slope degrees, curvature) directly; fixed range uses absolute [0, 1]."
        }

        // Expression (per-cell formula).
        "node-modifier.expression" => "Expression",
        "node-modifier.expression-desc" => {
            "A per-cell math formula over x, y, and the input layers (height, mask, …): \
             the escape hatch for custom math. Runs wired or as a coordinate formula."
        }

        // Thermal erosion.
        "node-modifier.thermal_erosion" => "Thermal Erosion",
        "node-modifier.thermal_erosion-desc" => {
            "Relaxes slopes steeper than the talus angle toward repose."
        }

        // Hydraulic erosion.
        "node-modifier.hydraulic_erosion" => "Hydraulic Erosion",
        "node-modifier.hydraulic_erosion-desc" => {
            "Water carving the terrain, simulated as rain droplets that run downhill, pick up and \
             drop sediment, and cut rills while depositing fans and filling hollows. The \
             deposition is what reads as weathered. Taps wear, deposition, and flow."
        }

        // Stream erosion.
        "node-modifier.stream_erosion" => "Stream Erosion",
        "node-modifier.stream_erosion-desc" => {
            "Carves drainage networks from flow accumulation; outputs the river/flow map."
        }

        // Coastal bevel.
        "node-modifier.coastal" => "Coastal",
        "node-modifier.coastal-desc" => {
            "Reshapes the shore into a beach-and-bluff bevel: cuts the land down and lifts the \
             seabed toward a gentle wedge at the world sea level, fading over a width in metres. \
             Bevels by true distance from the shoreline, so the beach is even all around. Taps the \
             shore band."
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

        // Unknown: echo the key so the gap is visible.
        other => other,
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
    fn every_operator_has_a_display_name() {
        // Adding an operator without a `tr` entry fails here, restoring the
        // "a missing string is caught" property.
        for entry in registry::entries() {
            let key = format!("node-{}", entry.type_id);
            assert_ne!(tr(&key), key.as_str(), "no display string for {key:?}");
        }
    }
}
