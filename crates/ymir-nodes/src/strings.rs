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
        "category-noise" => "Noise",
        "category-combine" => "Combine",
        "category-erosion" => "Erosion",
        "category-output" => "Output",

        // fBm generator.
        "node-generator.fbm" => "fBm Noise",
        "node-generator.fbm-desc" => "Fractional Brownian motion of Perlin noise.",

        // Combine / blend.
        "node-modifier.combine" => "Combine",
        "node-modifier.combine-desc" => {
            "Merges two fields: add, multiply, min, max, or a mask-weighted mix."
        }

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
        assert_eq!(tr("category-erosion"), "Erosion");
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
