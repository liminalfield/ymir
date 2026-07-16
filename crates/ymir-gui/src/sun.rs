//! The relief sun dial: a small draggable disk that shows and sets a hillshade light
//! direction, shared by the 2D preview pane and the main viewport's 2D map (#40, #96).
//!
//! A light is a unit vector `[x, y, z]` in image space (`+x` right, `+y` down, `+z` toward the
//! viewer). The dial maps the cursor's angle from centre to the azimuth and its distance to the
//! altitude, so one drag sets both. Kept widget-agnostic (it takes `&mut [f32; 3]`, not any
//! engine) so every relief surface steers its own light through the same control.

use eframe::egui;

/// Size (px) of the sun dial. Callers that reserve a row for it (the preview pane's shading
/// controls) use this so toggling Height/Relief never shifts the layout.
pub(crate) const DIAL_SIZE: f32 = 40.0;

/// The sun's warm core colour.
const SUN: egui::Color32 = egui::Color32::from_rgb(0xf2, 0xc4, 0x4d);

/// Draws the sun dial into `ui` and applies a drag over it to `light`. The disk shows the light's
/// horizontal projection as a sun dot; dragging sets a new direction (the exact centre is ignored,
/// keeping the current light). Only meaningful for a relief surface.
pub(crate) fn dial(ui: &mut egui::Ui, light: &mut [f32; 3]) {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(DIAL_SIZE, DIAL_SIZE), egui::Sense::drag());
    let center = rect.center();
    let radius = DIAL_SIZE * 0.5 - 3.0;
    let painter = ui.painter_at(rect);
    // An inset well: a deep fill with a hairline rim, so the dial reads as a control.
    painter.circle_filled(center, radius, crate::theme::BG_ABYSS);
    painter.circle_stroke(center, radius, egui::Stroke::new(1.0, crate::theme::LINE));
    // The light's horizontal projection (lx, ly) maps straight onto the disk.
    let dot = center + egui::vec2(light[0], light[1]) * radius;
    // A faint direction line from the centre out to the sun.
    painter.line_segment(
        [center, dot],
        egui::Stroke::new(1.0, crate::theme::LINE_STRONG),
    );
    // The sun: a warm core with a soft glow.
    painter.circle_filled(dot, 6.0, SUN.gamma_multiply(0.35));
    painter.circle_filled(dot, 3.5, SUN);

    let resp = resp.on_hover_text("Drag to set the relief light");
    if resp.dragged()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(next) = light_from_drag(pos, rect)
    {
        *light = next;
    }
}

/// The light direction for a drag at `pos` over `rect`: the cursor's angle from the centre sets the
/// azimuth, and its distance the altitude (centre = high/soft, edge = low/grazing), clamped so the
/// light is never fully overhead nor fully grazing. `None` for the exact centre (no direction).
/// Pure and unit-tested.
pub(crate) fn light_from_drag(pos: egui::Pos2, rect: egui::Rect) -> Option<[f32; 3]> {
    let half = rect.size() * 0.5;
    if half.x <= 0.0 || half.y <= 0.0 {
        return None;
    }
    let (u, v) = (
        (pos.x - rect.center().x) / half.x,
        (pos.y - rect.center().y) / half.y,
    );
    let dist = (u * u + v * v).sqrt();
    if dist < 1e-4 {
        return None;
    }
    let horizontal = dist.clamp(0.2, 0.95);
    let lz = (1.0 - horizontal * horizontal).max(0.0).sqrt();
    Some([u / dist * horizontal, v / dist * horizontal, lz])
}

/// A light's azimuth and altitude in degrees, for a dial readout: azimuth is the horizontal
/// direction (0-360), altitude the angle above the horizon.
pub(crate) fn light_angles(light: [f32; 3]) -> (f32, f32) {
    let az = light[1].atan2(light[0]).to_degrees().rem_euclid(360.0);
    let alt = light[2].clamp(-1.0, 1.0).asin().to_degrees();
    (az, alt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_from_drag_maps_cursor_to_a_unit_light() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));

        // Dragging to the right edge → light points right (+x), level (y ≈ 0).
        let right = light_from_drag(egui::pos2(100.0, 50.0), rect).expect("direction");
        assert!(right[0] > 0.0 && right[1].abs() < 1e-3);

        // Upper-left → light points up-left (-x, -y).
        let up_left = light_from_drag(egui::pos2(0.0, 0.0), rect).expect("direction");
        assert!(up_left[0] < 0.0 && up_left[1] < 0.0);

        // Always a unit vector.
        let n = up_left[0] * up_left[0] + up_left[1] * up_left[1] + up_left[2] * up_left[2];
        assert!((n.sqrt() - 1.0).abs() < 1e-4);

        // The exact centre has no direction.
        assert!(light_from_drag(egui::pos2(50.0, 50.0), rect).is_none());
    }

    #[test]
    fn light_angles_reads_azimuth_and_altitude() {
        // Straight down in image space (+y) with a shallow tilt: azimuth 90°, altitude near 0.
        let (az, alt) = light_angles([0.0, 0.95, 0.312]);
        assert!((az - 90.0).abs() < 1.0, "azimuth {az}");
        assert!(alt > 0.0 && alt < 25.0, "altitude {alt}");

        // The NW default points up-left and partway up: azimuth in the third quadrant, altitude up.
        let (az, alt) = light_angles(crate::shade::DEFAULT_LIGHT);
        assert!((180.0..270.0).contains(&az), "azimuth {az}");
        assert!(alt > 0.0, "altitude {alt}");
    }
}
