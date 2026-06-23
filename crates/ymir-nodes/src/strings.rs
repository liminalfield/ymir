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
        "category-output" => "Outputs",

        // fBm generator.
        "node-generator.fbm" => "fBm Noise",
        "node-generator.fbm-desc" => "Fractional Brownian motion of Perlin noise.",

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

        // Thermal erosion.
        "node-modifier.thermal_erosion" => "Thermal Erosion",
        "node-modifier.thermal_erosion-desc" => {
            "Relaxes slopes steeper than the talus angle toward repose."
        }

        // PNG export.
        "node-endpoint.export" => "Export PNG",
        "node-endpoint.export-desc" => "Writes the height layer to a 16-bit grayscale PNG.",

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
