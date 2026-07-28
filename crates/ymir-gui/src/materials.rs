//! Material sets: which materials are shown, in what order, and which are muted (#267).
//!
//! A Material node names and colours a selection. It says nothing about how materials stack,
//! because stacking is not a property of any one of them. A **set** is that arrangement: an
//! ordered list of the materials in play, with a mute flag each.
//!
//! It is a list, so it lives in a panel rather than on the canvas. Several sets can exist over the
//! same materials, one active at a time, which is what makes A/B a flip of a dropdown rather than
//! a rewire. See `design/texturing.md`.
//!
//! # What persists
//!
//! Sets, their order, and mute travel with the project: they are decisions about the arrangement.
//! **Solo does not.** Solo is something you do for a few seconds to look at one material, and
//! reopening a project to find a single material showing with no memory of why is the same trap
//! the node pane's filter avoids by not persisting either.
//!
//! None of it reaches the engine. No downstream tool reads a stacking order, so a set never needs
//! to leave Ymir, which is why it is view state rather than graph data.

use std::collections::HashSet;

use std::sync::Arc;

use eframe::egui;
use serde::{Deserialize, Serialize};
use ymir_core::{Field, Layer, layers};

/// One material in a set, by the `stable_id` of its Material node.
///
/// Referenced by node rather than by name so renaming a material does not drop it from every set,
/// and so two materials that happen to share a name stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SetEntry {
    /// The Material node's persistent id.
    pub node: u64,
    /// Muted: excluded from the composite until unmuted. A decision, so it persists.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

/// An ordered arrangement of materials, **bottom of the stack first**.
///
/// Bottom first because that is the order they composite in, so the stored order and the
/// compositing loop agree without anything reversing in between. The panel presents it the other
/// way up, since a layer stack reads top-down.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct MaterialSet {
    /// The set's name, as shown in the dropdown.
    pub name: String,
    /// The materials, bottom first.
    #[serde(default)]
    pub entries: Vec<SetEntry>,
}

/// Every material set in the project, and which one is showing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct MaterialSets {
    /// The sets, in the order they appear in the dropdown.
    #[serde(default)]
    pub sets: Vec<MaterialSet>,
    /// Index of the active set. Out of range means none is showing, which is the state a project
    /// with no sets is in; every read goes through [`active`](Self::active) rather than indexing.
    #[serde(default)]
    pub active: usize,
    /// Soloed nodes. Not persisted: solo is a look, not a decision.
    #[serde(skip)]
    pub soloed: HashSet<u64>,
}

impl MaterialSets {
    /// Whether there is nothing to save, so a project with no materials writes no section.
    pub(crate) fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The active set, or `None` when there is none.
    pub(crate) fn active(&self) -> Option<&MaterialSet> {
        self.sets.get(self.active)
    }

    /// The active set, mutably.
    pub(crate) fn active_mut(&mut self) -> Option<&mut MaterialSet> {
        self.sets.get_mut(self.active)
    }

    /// Whether `node` is muted in the active set.
    pub(crate) fn is_muted(&self, node: u64) -> bool {
        self.active()
            .and_then(|set| set.entries.iter().find(|e| e.node == node))
            .is_some_and(|e| e.muted)
    }

    /// Whether anything in the active set is soloed.
    ///
    /// Scoped to the active set so a solo left on a material that only appears in another set
    /// cannot silently blank the one you are looking at.
    fn any_soloed(&self) -> bool {
        self.active()
            .is_some_and(|set| set.entries.iter().any(|e| self.soloed.contains(&e.node)))
    }

    /// Whether `node` composites: **mute wins, solo narrows.**
    ///
    /// It shows if nothing is soloed and it is not muted, or something is soloed and it is one of
    /// them and it is not muted. Soloing a muted material therefore shows nothing, and the lit
    /// mute button beside it says why. Chosen for being explainable rather than clever: there is
    /// no hidden precedence to remember.
    pub(crate) fn is_showing(&self, node: u64) -> bool {
        if self.is_muted(node) {
            return false;
        }
        !self.any_soloed() || self.soloed.contains(&node)
    }

    /// The nodes the active set composites, bottom first.
    pub(crate) fn showing(&self) -> Vec<u64> {
        self.active().map_or_else(Vec::new, |set| {
            set.entries
                .iter()
                .map(|e| e.node)
                .filter(|&n| self.is_showing(n))
                .collect()
        })
    }

    /// Whether `node` is in the active set at all.
    pub(crate) fn contains(&self, node: u64) -> bool {
        self.active()
            .is_some_and(|set| set.entries.iter().any(|e| e.node == node))
    }

    /// Adds `node` to the active set on top of the stack, or removes it if already there.
    ///
    /// One action for both directions, because the membership menu is one list of ticks: a tick
    /// means in the set, and clicking toggles it.
    pub(crate) fn toggle_member(&mut self, node: u64) {
        let Some(set) = self.active_mut() else {
            return;
        };
        match set.entries.iter().position(|e| e.node == node) {
            Some(at) => {
                set.entries.remove(at);
            }
            // Pushed onto the end, which is the top of the stack: a material you just added should
            // be the one you can see.
            None => set.entries.push(SetEntry { node, muted: false }),
        }
    }

    /// Moves the entry at `from` to `to` within the active set, clamped to the list.
    pub(crate) fn reorder(&mut self, from: usize, to: usize) {
        let Some(set) = self.active_mut() else {
            return;
        };
        if from >= set.entries.len() || from == to {
            return;
        }
        let entry = set.entries.remove(from);
        let to = to.min(set.entries.len());
        set.entries.insert(to, entry);
    }

    /// Flips the mute flag on `node` in the active set.
    pub(crate) fn toggle_mute(&mut self, node: u64) {
        if let Some(set) = self.active_mut()
            && let Some(entry) = set.entries.iter_mut().find(|e| e.node == node)
        {
            entry.muted = !entry.muted;
        }
    }

    /// Flips solo on `node`.
    pub(crate) fn toggle_solo(&mut self, node: u64) {
        if !self.soloed.remove(&node) {
            self.soloed.insert(node);
        }
    }

    /// Adds a set and makes it active.
    pub(crate) fn add_set(&mut self, name: impl Into<String>) {
        self.sets.push(MaterialSet {
            name: name.into(),
            entries: Vec::new(),
        });
        self.active = self.sets.len() - 1;
    }

    /// Removes the set at `index`, keeping `active` pointing at something that exists.
    pub(crate) fn remove_set(&mut self, index: usize) {
        if index >= self.sets.len() {
            return;
        }
        self.sets.remove(index);
        self.active = self.active.min(self.sets.len().saturating_sub(1));
    }

    /// A name no existing set uses, for a newly created one.
    pub(crate) fn fresh_name(&self) -> String {
        let base = "Material set";
        if !self.sets.iter().any(|s| s.name == base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|name| !self.sets.iter().any(|s| &s.name == name))
            .unwrap_or_else(|| base.to_string())
    }
}

/// One material ready to composite: its weight over the terrain, and the colour it shows as.
pub(crate) struct Shown<'a> {
    /// The material's `[0, 1]` weight, which rides on the field's height layer because that is
    /// where a selection's values live.
    pub weight: &'a Field,
    /// Its preview colour, sRGB.
    pub color: egui::Color32,
}

/// Composites `shown` over `image` in place, bottom first.
///
/// An **over** stack: each material paints onto what is beneath it in proportion to its weight, so
/// one at full weight hides everything below. Order-dependent on purpose, because that is what
/// makes a stack mean rock poking through grass and snow lying on top of both. A normalized
/// weighted average was the alternative and is order-independent, which could not express "on top"
/// at all.
///
/// The colour is **multiplied** into the shading rather than replacing it, so the relief still
/// reads: a lit slope stays lighter than a shaded one under the same material. Replacing would
/// give flat colour and throw away the shape, which is the thing being judged.
///
/// A cell no material claims is left alone, so bare terrain shows as terrain. That is the visible
/// sign of a stack with no base material under it.
///
/// This predicts what a game engine will show. It does not constrain an export, which writes the
/// raw independent weights and lets the engine do its own blending.
pub(crate) fn composite(image: &mut egui::ColorImage, shown: &[Shown<'_>]) {
    let [w, h] = image.size;
    let Some(layers) = aligned(shown, w, h) else {
        return;
    };
    for y in 0..h {
        for x in 0..w {
            let Some((rgb, covered)) = over(&layers, x, y) else {
                continue;
            };
            let pixel = &mut image.pixels[y * w + x];
            let shaded = [pixel.r(), pixel.g(), pixel.b()];
            let mut out = [0_u8; 3];
            for ((slot, lit), tint) in out.iter_mut().zip(shaded).zip(rgb) {
                let lit = f32::from(lit) / 255.0;
                *slot = byte((lit * tint - lit).mul_add(covered, lit));
            }
            *pixel = egui::Color32::from_rgba_unmultiplied(out[0], out[1], out[2], pixel.a());
        }
    }
}

/// The composite as an image: colour in RGB, coverage in alpha.
///
/// What the 3D viewport needs, where the mixing happens in the shader so the tint lands on the lit
/// surface rather than on a picture of one. It shares [`over`] with [`composite`], so the flat
/// preview and the viewport cannot disagree about what a set looks like.
///
/// `None` when there is nothing to draw, so a caller can skip the upload rather than push a
/// transparent texture.
pub(crate) fn overlay(shown: &[Shown<'_>], w: usize, h: usize) -> Option<egui::ColorImage> {
    let layers = aligned(shown, w, h)?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            match over(&layers, x, y) {
                Some((rgb, covered)) => rgba.extend_from_slice(&[
                    byte(rgb[0]),
                    byte(rgb[1]),
                    byte(rgb[2]),
                    byte(covered),
                ]),
                // Nothing claims the cell, so the terrain shows through untinted.
                None => rgba.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

/// The materials that line up with a `w` by `h` grid, paired with their colours.
///
/// One evaluated at a different resolution cannot be lined up cell for cell, and stretching it
/// would invent coverage, so it is dropped. `None` when none of them line up.
fn aligned<'a>(
    shown: &'a [Shown<'a>],
    w: usize,
    h: usize,
) -> Option<Vec<(Arc<Layer>, egui::Color32)>> {
    if shown.is_empty() || w == 0 || h == 0 {
        return None;
    }
    let layers: Vec<_> = shown
        .iter()
        .map(|s| (s.weight.layer_or(layers::HEIGHT, 0.0), s.color))
        .filter(|(layer, _)| layer.width() == w && layer.height() == h)
        .collect();
    (!layers.is_empty()).then_some(layers)
}

/// The over stack at one cell: the straight colour, and how much of the cell is claimed.
///
/// `None` when nothing claims it. The colour is un-premultiplied before returning: the
/// accumulation builds it scaled by coverage, and every caller mixes by coverage again, so
/// leaving it premultiplied counts coverage twice and darkens a partial weight rather than
/// tinting it.
fn over(layers: &[(Arc<Layer>, egui::Color32)], x: usize, y: usize) -> Option<([f32; 3], f32)> {
    let mut rgb = [0.0_f32; 3];
    let mut covered = 0.0_f32;
    for (layer, color) in layers {
        let a = layer.get(x, y).unwrap_or(0.0).clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        let tint = [
            f32::from(color.r()) / 255.0,
            f32::from(color.g()) / 255.0,
            f32::from(color.b()) / 255.0,
        ];
        for (channel, paint) in rgb.iter_mut().zip(tint) {
            *channel = channel.mul_add(1.0 - a, paint * a);
        }
        covered = covered.mul_add(1.0 - a, a);
    }
    if covered <= 0.0 {
        return None;
    }
    for channel in &mut rgb {
        *channel /= covered;
    }
    Some((rgb, covered))
}

/// A `[0, 1]` channel as a byte, rounded so a value that came from a byte returns to it.
fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set of `nodes`, bottom first, active.
    fn sets(nodes: &[u64]) -> MaterialSets {
        let mut s = MaterialSets::default();
        s.add_set("Alpine");
        for &node in nodes {
            s.toggle_member(node);
        }
        s
    }

    #[test]
    fn everything_shows_when_nothing_is_muted_or_soloed() {
        assert_eq!(sets(&[1, 2, 3]).showing(), vec![1, 2, 3]);
    }

    #[test]
    fn a_muted_material_drops_out() {
        let mut s = sets(&[1, 2, 3]);
        s.toggle_mute(2);
        assert_eq!(s.showing(), vec![1, 3]);
    }

    #[test]
    fn solo_narrows_to_the_soloed_ones() {
        let mut s = sets(&[1, 2, 3]);
        s.toggle_solo(2);
        assert_eq!(s.showing(), vec![2]);
        s.toggle_solo(3);
        assert_eq!(s.showing(), vec![2, 3], "several solos show all of them");
    }

    #[test]
    fn mute_wins_over_solo() {
        // The rule chosen for being explainable: soloing a muted material shows nothing, and the
        // lit mute button beside it says why. No hidden precedence.
        let mut s = sets(&[1, 2]);
        s.toggle_mute(2);
        s.toggle_solo(2);
        assert_eq!(s.showing(), Vec::<u64>::new());
    }

    #[test]
    fn a_solo_in_another_set_cannot_blank_this_one() {
        // Solo is not scoped to a node's membership, so without this a solo left on a material
        // that only appears in the other set would make the active one composite nothing.
        let mut s = sets(&[1, 2]);
        s.add_set("Arid");
        s.toggle_member(9);
        s.toggle_solo(9);
        s.active = 0;
        assert_eq!(s.showing(), vec![1, 2]);
    }

    #[test]
    fn membership_toggles_both_ways() {
        let mut s = sets(&[1, 2]);
        assert!(s.contains(2));
        s.toggle_member(2);
        assert!(!s.contains(2), "a second toggle removes it");
        assert_eq!(s.showing(), vec![1]);
        s.toggle_member(2);
        assert_eq!(s.showing(), vec![1, 2], "and a third puts it back on top");
    }

    #[test]
    fn reordering_moves_one_entry() {
        let mut s = sets(&[1, 2, 3]);
        s.reorder(0, 2);
        assert_eq!(s.showing(), vec![2, 3, 1]);
        s.reorder(2, 0);
        assert_eq!(s.showing(), vec![1, 2, 3]);
    }

    #[test]
    fn reordering_past_the_end_lands_at_the_end() {
        let mut s = sets(&[1, 2]);
        s.reorder(5, 0);
        assert_eq!(
            s.showing(),
            vec![1, 2],
            "a source that is not there does nothing"
        );
        s.reorder(0, 0);
        assert_eq!(
            s.showing(),
            vec![1, 2],
            "moving something onto itself does nothing"
        );
        s.reorder(1, 99);
        assert_eq!(
            s.showing(),
            vec![1, 2],
            "dragging the top entry past the end leaves it on top"
        );
        s.reorder(0, 99);
        assert_eq!(s.showing(), vec![2, 1], "and the bottom one lands on top");
    }

    #[test]
    fn removing_a_set_keeps_the_active_index_pointing_at_something() {
        let mut s = sets(&[1]);
        s.add_set("Arid");
        s.add_set("Coastal");
        assert_eq!(s.active, 2);
        s.remove_set(2);
        assert_eq!(s.active, 1, "active follows the list rather than dangling");
        s.remove_set(0);
        s.remove_set(0);
        assert!(s.active().is_none(), "no sets means nothing active");
        assert_eq!(s.showing(), Vec::<u64>::new());
    }

    #[test]
    fn mute_persists_and_solo_does_not() {
        // Mute is a decision about the set; solo is a look. Reopening a project to one material
        // showing, with no memory of why, is the trap this avoids.
        let mut s = sets(&[1, 2]);
        s.toggle_mute(1);
        s.toggle_solo(2);

        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains("muted"), "mute is written");
        assert!(!json.contains("soloed"), "solo is not");

        let back: MaterialSets = serde_json::from_str(&json).expect("deserialize");
        assert!(back.is_muted(1));
        assert!(back.soloed.is_empty());
        assert_eq!(
            back.showing(),
            vec![2],
            "reopens with the mute, without the solo"
        );
    }

    #[test]
    fn a_fresh_name_does_not_collide() {
        let mut s = MaterialSets::default();
        assert_eq!(s.fresh_name(), "Material set");
        s.add_set(s.fresh_name());
        assert_eq!(s.fresh_name(), "Material set 2");
        s.add_set(s.fresh_name());
        assert_eq!(s.fresh_name(), "Material set 3");
    }

    use ymir_core::Region;

    const RED: egui::Color32 = egui::Color32::from_rgb(255, 0, 0);
    const BLUE: egui::Color32 = egui::Color32::from_rgb(0, 0, 255);

    /// A weight field of `value` everywhere, which is the shape a Material node outputs.
    fn weight(value: f32) -> Field {
        Field::new(2, 2, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(2, 2, value)))
    }

    /// A flat shaded image, standing in for the hillshade.
    fn shaded(level: u8) -> egui::ColorImage {
        egui::ColorImage::from_rgba_unmultiplied([2, 2], &[level, level, level, 255].repeat(4))
    }

    fn first(image: &egui::ColorImage) -> [u8; 3] {
        let p = image.pixels[0];
        [p.r(), p.g(), p.b()]
    }

    #[test]
    fn a_full_weight_material_hides_what_is_under_it() {
        let (base, top) = (weight(1.0), weight(1.0));
        let mut image = shaded(255);
        composite(
            &mut image,
            &[
                Shown {
                    weight: &base,
                    color: RED,
                },
                Shown {
                    weight: &top,
                    color: BLUE,
                },
            ],
        );
        assert_eq!(first(&image), [0, 0, 255], "the top material wins");
    }

    #[test]
    fn order_decides_the_result() {
        // The property the whole ordering design rests on. A normalized weighted average would
        // give the same answer both ways round and could not express "snow on top".
        let (a, b) = (weight(1.0), weight(1.0));
        let mut up = shaded(255);
        composite(
            &mut up,
            &[
                Shown {
                    weight: &a,
                    color: RED,
                },
                Shown {
                    weight: &b,
                    color: BLUE,
                },
            ],
        );
        let mut down = shaded(255);
        composite(
            &mut down,
            &[
                Shown {
                    weight: &b,
                    color: BLUE,
                },
                Shown {
                    weight: &a,
                    color: RED,
                },
            ],
        );
        assert_ne!(first(&up), first(&down));
        assert_eq!(first(&up), [0, 0, 255]);
        assert_eq!(first(&down), [255, 0, 0]);
    }

    #[test]
    fn the_relief_survives_the_tint() {
        // Multiply, not replace: a lit cell under a material stays lighter than a shaded one, so
        // the shape still reads. Replacing would give flat colour and throw the relief away.
        let full = weight(1.0);
        let (mut light, mut dark) = (shaded(200), shaded(60));
        composite(
            &mut light,
            &[Shown {
                weight: &full,
                color: RED,
            }],
        );
        composite(
            &mut dark,
            &[Shown {
                weight: &full,
                color: RED,
            }],
        );
        assert_eq!(first(&light)[1], 0, "the tint removed the green channel");
        assert!(
            first(&light)[0] > first(&dark)[0],
            "the lit cell stays lighter than the shaded one"
        );
    }

    #[test]
    fn an_unclaimed_cell_is_left_as_terrain() {
        // The visible sign of a stack with no base material under it, and the reason coverage is
        // tracked separately from colour.
        let none = weight(0.0);
        let mut image = shaded(128);
        let before = image.pixels.clone();
        composite(
            &mut image,
            &[Shown {
                weight: &none,
                color: RED,
            }],
        );
        assert_eq!(image.pixels, before);
    }

    #[test]
    fn a_partial_weight_tints_partway() {
        let half = weight(0.5);
        let mut image = shaded(200);
        composite(
            &mut image,
            &[Shown {
                weight: &half,
                color: BLUE,
            }],
        );
        let [r, _, b] = first(&image);
        assert!(r > 0 && r < 200, "red is pulled down but not to zero");
        assert_eq!(b, 200, "blue is untouched by a blue tint");
    }

    #[test]
    fn a_material_at_another_resolution_is_skipped_rather_than_stretched() {
        // Reachable while a rebuild is in flight. Stretching would invent coverage, and lining it
        // up cell for cell is the only honest option.
        let small = Field::new(1, 1, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(1, 1, 1.0)));
        let mut image = shaded(128);
        let before = image.pixels.clone();
        composite(
            &mut image,
            &[Shown {
                weight: &small,
                color: RED,
            }],
        );
        assert_eq!(image.pixels, before);
    }

    #[test]
    fn nothing_to_composite_leaves_the_image_alone() {
        let mut image = shaded(128);
        let before = image.pixels.clone();
        composite(&mut image, &[]);
        assert_eq!(image.pixels, before);
    }
}
