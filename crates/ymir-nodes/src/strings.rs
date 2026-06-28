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

        // Flow selector.
        "node-modifier.flow" => "Flow",
        "node-modifier.flow-desc" => {
            "Selects drainage channels by flow accumulation, computed on demand from the \
             terrain: high where upstream water collects, within a normalized band. The \
             drainage counterpart to Slope and Curvature."
        }

        // Invert.
        "node-modifier.invert" => "Invert",
        "node-modifier.invert-desc" => "Flips the height layer (1 - height).",

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

        // Levels (range rescaling).
        "node-modifier.levels" => "Levels",
        "node-modifier.levels-desc" => {
            "Rescales the height range: stretch an input window to full, bias the midtones \
             with gamma, map into an output window. Normalize, set amplitude, or clamp."
        }

        // Curve (height shaping).
        "node-modifier.curve" => "Curve",
        "node-modifier.curve-desc" => "Reshapes height through an editable transfer curve.",

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
            "Simulates rain, water flow, and sediment transport, carving the terrain."
        }

        // Stream erosion.
        "node-modifier.stream_erosion" => "Stream Erosion",
        "node-modifier.stream_erosion-desc" => {
            "Carves drainage networks from flow accumulation; outputs the river/flow map."
        }

        // Null (pass-through utility).
        "node-modifier.null" => "Null",
        "node-modifier.null-desc" => {
            "Passes the field through unchanged: a point to view, reroute, or anchor wiring."
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
