//! Coastal reshaping: a beach-and-bluff bevel keyed to distance from the shoreline (#139).
//!
//! The shoreline is the world sea-level contour of the `height` layer (sea level is a world
//! setting carried in [`EvalContext::sea_level`], never a node param). The land and sea sides have
//! independent extents on purpose, so widening the beach does not enlarge the underwater shelf and
//! the coast is not dominated by change below the waterline.
//!
//! On land it cuts the terrain *down* toward a two-slope beach-and-bluff profile rising from the
//! waterline (`min`): a gentle beach face reaching `beach_width` metres inland to a berm crest at
//! `berm_height` (so the face grade is `berm_height / beach_width`), then a steeper backing slope
//! of grade `bluff_angle` above it. The crest where the two meet is rounded into a shoulder over
//! `rounding` metres rather than left a hard corner. Because the backing slope is steep it clears
//! the terrain behind the beach within a short run, so the cut bites only the low apron near the
//! water and leaves the hill behind as a bluff; the break of slope where the envelope meets the
//! un-cut hillside is the bluff toe. The land effect self-terminates against the terrain, so its
//! inland reach is the geometry itself, with no separate distance fade.
//!
//! Offshore it raises the seabed *up* toward sea level (`max`), fully at the waterline and fading to
//! nothing at `shoreface_reach`, forming a shallow shelf that deepens smoothly out to the natural
//! seabed. The lift depends only on `shoreface_reach`, never on the beach parameters, so sizing the
//! beach never reshapes the water and raising the berm never deepens the shelf. The waterline meets
//! sea level on both sides, so the surface is continuous through it.
//!
//! The bevel is parameterised by *true isotropic distance from the shoreline*, from the shared
//! eikonal substrate ([`signed_distance_to_contour`](crate::distance)). That is the whole reason
//! it reshapes as an even band all around a coast rather than the eight-lobed star a chamfer
//! distance would carve. Because the reshape is a pure per-cell function of that signed distance,
//! the result is byte-identical on every machine, and the no-star isotropy is inherited from the
//! solve rather than re-derived here.
//!
//! Four outputs: the reshaped `heightfield`, and three selections it computes along the way, one per
//! coastal zone. `shore` is a band peaking at the waterline and fading to zero at `beach_width`
//! inland and `shoreface_reach` offshore, for a wet edge or foam. `beach` is a solid footprint of
//! the beach face and berm slope, one from just off the waterline up to near the crest: it feathers
//! wide at the waterline (so detail masked by it never roughens the clean shoreline) and narrow at
//! the crest (so the steeper shoulder is covered, not left smooth). `bluff` is the companion
//! footprint of the backing slope: past the berm crest and following the carved slope up to the
//! bluff toe, so together `beach` and `bluff` cover the whole reshaped coast and each can be
//! textured on its own. All three are emitted rather than discarded and recomputed. Water depth is
//! not emitted: it is `sea_level - height`, recoverable from the field
//! plus the global, so by the layer test it does not earn a stored layer.

use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, Unit, layers,
};

use crate::distance::{sea_signed_distance, signed_distance_to_contour};
use crate::erosion;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.coastal";

/// Default beach width in world metres: how far inland the gentle beach face reaches, from the
/// waterline to the berm crest. With `berm_height` this sets the beach-face grade
/// (`berm_height / beach_width`), so a wider beach at a given berm height is gentler.
const DEFAULT_BEACH_WIDTH: f64 = 60.0;
/// Default berm-crest height above sea level, in world metres. This is how far the visible beach
/// rises above the water; on a tall world it needs to be raised to read against the vertical scale.
const DEFAULT_BERM_HEIGHT: f64 = 2.0;
/// Maximum berm-crest height in world metres. A berm is a small feature (a real one is a few metres;
/// a stylized raised beach, tens), so the range is capped well below the coast's reach. The small
/// range also earns the parameter sub-metre editing steps in the inspector, which a berm on a
/// small-scale terrain wants.
const MAX_BERM_HEIGHT: f64 = 100.0;
/// Default backing (bluff) grade in degrees. Steep enough to read as a coastal bluff that clears
/// the terrain behind the beach, rather than the gentle foreshore that flattens it.
const DEFAULT_BLUFF_ANGLE: f64 = 45.0;
/// Maximum grade for the backing slope. Capped below 90 so `tan(angle)` stays finite; near the cap
/// the backing is a near-vertical sea cliff.
const MAX_ANGLE: f64 = 80.0;
/// Default offshore shoreface reach in world metres: how far out to sea the seabed is lifted toward
/// a shallow shelf, independent of the on-land beach so the coast is not dominated by underwater
/// change. Zero leaves the seabed alone.
const DEFAULT_SHOREFACE_REACH: f64 = 50.0;
/// Maximum reach in world metres for the beach width and the shoreface. A wide value spans a large
/// map; the whole-metre editing steps suit a broad extent.
const MAX_REACH: f64 = 100_000.0;
/// Default crest-rounding radius in world metres: how far to either side of the berm crest the
/// beach face and the backing slope are blended into a smooth shoulder. A small value keeps the
/// crest a soft break rather than a hard corner.
const DEFAULT_ROUNDING: f64 = 8.0;
/// Maximum crest-rounding radius in world metres. A shoulder is a local feature, so the range is
/// capped like the berm height; the small range also earns sub-metre editing steps.
const MAX_ROUNDING: f64 = 100.0;

/// Coastal bevel: reshapes terrain near the sea-level shoreline into a beach-and-bluff profile.
#[derive(Clone)]
pub struct Coastal;

impl Operator for Coastal {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "geology",
            inputs: vec![
                PortSpec::new("in"),
                // Optional: a field whose height is the selection. When unwired, the input's own
                // mask layer is used by convention, else reshape the whole coast.
                PortSpec::optional("mask"),
            ],
            outputs: vec![
                PortSpec::new("heightfield"),
                PortSpec::new("shore"),
                PortSpec::new("beach"),
                PortSpec::new("bluff"),
            ],
            params: vec![
                ParamSpec::new(
                    "beach_width",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_REACH,
                    },
                    ParamValue::Float(DEFAULT_BEACH_WIDTH),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "berm_height",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_BERM_HEIGHT,
                    },
                    ParamValue::Float(DEFAULT_BERM_HEIGHT),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "bluff_angle",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_ANGLE,
                    },
                    ParamValue::Float(DEFAULT_BLUFF_ANGLE),
                )
                .with_unit(Unit::Degrees),
                ParamSpec::new(
                    "rounding",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_ROUNDING,
                    },
                    ParamValue::Float(DEFAULT_ROUNDING),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "shoreface_reach",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_REACH,
                    },
                    ParamValue::Float(DEFAULT_SHOREFACE_REACH),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "strength",
                    ParamKind::Float { min: 0.0, max: 1.0 },
                    ParamValue::Float(1.0),
                ),
                ParamSpec::new(
                    "erode_inland_basins",
                    ParamKind::Bool,
                    ParamValue::Bool(false),
                ),
            ],
            // "shore", "beach", and "bluff" are byproduct output ports, not canonical layer
            // constants; name them by the port so the reference lists all three selections.
            emitted_layers: vec!["shore", "beach", "bluff"],
            mask_aware: true,
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let source = input.layer_or(layers::HEIGHT, 0.0);

        // The beach face reaches `beach_width` metres inland, rising to the berm crest at
        // `berm_height`; the two set the beach-face grade (`berm_height / beach_width`, rise over
        // run). This is the direct inland-extent control: a wider beach is a longer, gentler face.
        // A zero width would put the crest at the waterline (an instant step to the berm), so clamp
        // to a hair to keep the division finite.
        let beach_width = params.get_f64("beach_width", DEFAULT_BEACH_WIDTH).max(1e-6) as f32;
        let berm_height = params.get_f64("berm_height", DEFAULT_BERM_HEIGHT).max(0.0) as f32;
        let beach_grade = berm_height / beach_width; // beach-face rise over run, in world metres
        let bluff_angle = params
            .get_f64("bluff_angle", DEFAULT_BLUFF_ANGLE)
            .clamp(0.0, MAX_ANGLE);
        let bluff_grade = bluff_angle.to_radians().tan() as f32; // backing rise over run, in metres
        let rounding = params.get_f64("rounding", DEFAULT_ROUNDING).max(0.0) as f32;
        // The offshore shoreface lifts the seabed toward the beach face continued below the
        // waterline (the same near-shore grade), faded to nothing at `shoreface_reach` so the
        // underwater shelf has its own extent, independent of the on-land beach. A zero reach means
        // no shoreface. Clamp to a hair so the falloff never divides by zero.
        let shoreface_reach = params
            .get_f64("shoreface_reach", DEFAULT_SHOREFACE_REACH)
            .max(1e-6) as f32;
        let strength = params.get_f64("strength", 1.0).clamp(0.0, 1.0) as f32;

        let sea = ctx.sea_level() as f32;
        let cell_size = ctx.meters_per_cell() as f32;
        // The grade is a real angle; converting its metric rise to normalized height needs the
        // world's vertical extent, exactly as the erosion nodes fold in real_slope_scale. Guard
        // against a zero-height world so the division is always finite.
        let world_height = (ctx.world_height() as f32).max(1e-6);

        // The mask localizes the reshaping. An explicit mask input wins (its height layer is the
        // selection); with none, the input's own mask layer by convention; with neither, a uniform
        // 1.0 (reshape everywhere). Soft-layer contract: the node never gates on a mask.
        let mask = match inputs.optional(0) {
            Some(mask_field) => mask_field.layer_or(layers::HEIGHT, 1.0),
            None => input.layer_or(layers::MASK, 1.0),
        };

        // Signed distance (world metres) from the shoreline: negative offshore, positive on land.
        // By default only sea connected to the map edge counts, so enclosed below-sea basins (dry
        // pits, inland depressions) are treated as land and get no coast. Enabling
        // `erode_inland_basins` restores the plain contour, where every below-sea cell is sea, for
        // an inland-sea world.
        let erode_inland_basins = params.get_bool("erode_inland_basins", false);
        let signed = if erode_inland_basins {
            signed_distance_to_contour(&source, sea, cell_size)
        } else {
            sea_signed_distance(&source, sea, cell_size)
        };

        // The beach footprint mask feathers asymmetrically. At the waterline it needs a wide feather
        // so masked detail never reaches the clean shoreline contour. At the crest it needs only a
        // narrow one, so the mask covers the whole berm slope up to near the crest rather than fading
        // out well below it (leaving the steeper shoulder smooth). A hair minimum keeps each feather
        // positive for a zero-width beach.
        let waterline_feather = (beach_width * 0.2).max(1e-6);
        let crest_feather = (beach_width * 0.05).max(1e-6);
        // The bluff mask fades out over the last few metres of cut depth at the bluff toe, so its
        // upper edge is soft rather than a hard line where the backing meets the terrain.
        let cut_feather = (3.0 / world_height).max(1e-6);

        let mut reshaped = vec![0.0_f32; width * height];
        let mut shore = vec![0.0_f32; width * height];
        let mut beach = vec![0.0_f32; width * height];
        let mut bluff = vec![0.0_f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let idx = y * width + x;
                let d = signed.get(x, y).unwrap_or(0.0);
                let original = source.get(x, y).unwrap_or(0.0);

                // The bevel target and its per-side fade, measured from the waterline. The two sides
                // have independent extents: on land the beach self-terminates against the terrain
                // (the `min` below), so its inland reach is the beach-and-bluff geometry itself; the
                // sea side is bounded by `shoreface_reach`, decoupling the underwater shelf from the
                // beach so widening one does not enlarge the other.
                let (carved, side_fade, bluff_here) = if d >= 0.0 {
                    // Land: a two-slope profile. A gentle beach face of grade `beach_grade` from the
                    // waterline, and a steeper backing grade `bluff_angle` through the berm crest at
                    // `(beach_width, berm_height)`. Each is a line in metres; the profile is their
                    // upper envelope, so near the water the gentle face wins and past the crest the
                    // steep backing does. `smooth_max` rounds that crest into a shoulder over
                    // `rounding` metres instead of a hard corner. Cutting to the lower of terrain and
                    // this envelope carves a beach where terrain pokes above it and leaves the hill
                    // behind untouched where the steep backing has cleared it (the break of slope
                    // there is the bluff toe), so the land effect needs no separate distance fade.
                    let beach_face = beach_grade * d;
                    let backing = berm_height + bluff_grade * (d - beach_width);
                    let rise_m = smooth_max(beach_face, backing, rounding);
                    let env = sea + rise_m / world_height;
                    // The bluff footprint marks the backing slope: past the berm crest, and only
                    // where the backing actually cut the terrain, so it follows the carved slope and
                    // ends at the bluff toe wherever the terrain sits. It feathers in at the crest
                    // (handing off from the beach mask) and out as the cut vanishes at the toe.
                    let past_crest = smoothstep(crest_feather, d - beach_width);
                    let cutting = smoothstep(cut_feather, original - env);
                    (original.min(env), 1.0, past_crest * cutting)
                } else {
                    // Sea: raise the seabed toward sea level, fully at the waterline and fading to
                    // nothing at `shoreface_reach`, forming a shallow shelf that deepens smoothly
                    // out to the natural seabed. The lift depends only on `shoreface_reach`, never on
                    // the beach parameters, so sizing the beach never reshapes the water and raising
                    // the berm never deepens the shelf. `max` keeps it a lift only (a cell already
                    // above sea is untouched). The waterline meets sea level on both sides, so the
                    // surface is continuous through it.
                    (
                        original.max(sea),
                        1.0 - smoothstep(shoreface_reach, d.abs()),
                        0.0,
                    )
                };

                // The shore selection marks the coastal zone for downstream texturing and foam: the
                // beach inland (out to `beach_width`) and the shoreface offshore (out to
                // `shoreface_reach`), peaking at the waterline. Geometry only, so it carries neither
                // strength nor mask.
                let shore_reach = if d >= 0.0 {
                    beach_width
                } else {
                    shoreface_reach
                };
                shore[idx] = 1.0 - smoothstep(shore_reach, d.abs());

                // The beach selection is a solid footprint of the whole berm slope: one from just off
                // the waterline up to near the crest, zero offshore. The waterline edge feathers wide
                // (it leaves the shoreline contour clean); the crest edge feathers narrow, so the
                // steeper shoulder near the crest is covered rather than left smooth. Unlike `shore`
                // (a band peaking at the waterline), this covers the flattened beach evenly, so it
                // masks detail or a material put back on the slope.
                beach[idx] = if d >= 0.0 {
                    smoothstep(waterline_feather, d) * smoothstep(crest_feather, beach_width - d)
                } else {
                    0.0
                };

                // The bluff selection is the backing slope's footprint (computed above), the
                // companion to `beach`: together `beach` and `bluff` cover the whole reshaped coast,
                // and each can be textured on its own.
                bluff[idx] = bluff_here;

                let weight = side_fade * strength * mask.get(x, y).unwrap_or(1.0);
                reshaped[idx] = original + (carved - original) * weight;
            }
        }

        let mut heightfield = input.clone();
        heightfield.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_vec(width, height, reshaped)),
        );
        let shore_field = erosion::byproduct_field(shore, width, height, input.region());
        let beach_field = erosion::byproduct_field(beach, width, height, input.region());
        let bluff_field = erosion::byproduct_field(bluff, width, height, input.region());
        Ok(vec![heightfield, shore_field, beach_field, bluff_field])
    }
}

/// Cubic Hermite smoothstep of `x` over `[0, edge]`, clamped to `[0, 1]`. Zero at `x = 0`, one at
/// `x >= edge`; `edge` is guaranteed positive by the caller.
fn smoothstep(edge: f32, x: f32) -> f32 {
    let t = (x / edge).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Smooth maximum of `a` and `b` with rounding radius `k`, in the same units as the values
/// (Inigo Quilez's polynomial smooth-max). At `k <= 0` it is the exact [`f32::max`], a sharp corner;
/// a positive `k` rounds the corner where the two differ by less than `k`, lifting the join by up to
/// `k / 4`, and is exactly `max` beyond that. Used to blend the beach face and the backing slope
/// into a rounded berm shoulder rather than a hard crest. Where the two lines coincide it lifts by
/// the full `k / 4` (there is no corner to round, so a caller wanting the exact envelope there
/// passes `k = 0`).
fn smooth_max(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.max(b);
    }
    let h = ((k - (a - b).abs()) / k).max(0.0);
    a.max(b) + h * h * k * 0.25
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Coastal) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;
    use ymir_core::{NodeKind, Region};

    /// A cone island: high at the centre, dropping below `sea` toward the edges, so the sea-level
    /// contour is a centred circle and the central disk is land.
    fn cone_island(size: usize) -> Field {
        let c = (size - 1) as f32 / 2.0;
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, y| {
                let (dx, dy) = (x as f32 - c, y as f32 - c);
                let r = (dx * dx + dy * dy).sqrt() / c;
                (1.0 - r).clamp(0.0, 1.0)
            })),
        )
    }

    /// A context whose world is a cube of side `size` (so metres-per-cell is 1 and every reach reads
    /// in cells, and the grade is gentle rather than squashed) with the shoreline at height 0.5.
    fn ctx(size: usize) -> EvalContext {
        EvalContext::new(size, size, Region::UNIT, 0)
            .with_world_extent(size as f64)
            .with_world_height(size as f64)
            .with_sea_level(0.5)
    }

    fn run(input: &Field, params: &Params, ctx: &EvalContext) -> Vec<Field> {
        Coastal
            .eval(Inputs::required_only(&[input]), params, ctx)
            .unwrap()
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    /// A short beach with a steep bluff, so the reshaping carves a few cells inland on the cone and
    /// the steep backing then clears the (steeper) cone flank, leaving the interior untouched. No
    /// crest rounding, so the geometric tests read the exact two-slope profile.
    fn beach_params() -> Params {
        Params::new()
            .with("beach_width", ParamValue::Float(8.0))
            .with("berm_height", ParamValue::Float(2.0))
            .with("bluff_angle", ParamValue::Float(80.0))
            .with("rounding", ParamValue::Float(0.0))
    }

    #[test]
    fn spec_is_a_geology_modifier_with_heightfield_shore_beach_and_bluff() {
        let spec = Coastal.spec();
        assert_eq!(spec.kind(), NodeKind::Modifier);
        assert_eq!(spec.category, "geology");
        assert_eq!(spec.type_id, TYPE_ID);
        let outputs: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(outputs, ["heightfield", "shore", "beach", "bluff"]);
    }

    #[test]
    fn cuts_a_beach_into_a_coastal_hill() {
        // A land cell a few cells inside the shoreline (on the cone, above sea level) is cut down
        // toward the beach plane, so it drops well below its original height.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        // (42, 32) is 10 cells right of centre: r ~= 0.31, original height ~= 0.69, roughly
        // mid-way through the 12-cell beach (the shoreline at r = 0.5 is ~16 cells out), where the
        // blend weight is near its peak and the cut is deepest.
        // (42, 32) is 10 cells right of centre: r ~= 0.31, ~6 cells inside the r = 0.5 shoreline, on
        // the beach face, where the cut toward the beach plane is well below the cone height.
        let before = at(&island, 42, 32);
        let after = at(&out[0], 42, 32);
        assert!(
            after < before - 0.05,
            "a coastal hill cell should be cut down: {before} -> {after}"
        );
        assert!(after >= 0.5 - 1e-3, "the cut should not go below sea level");
    }

    /// A coast where the land is a flat mesa well above sea level: offshore (`x < edge`) sits below
    /// sea, and from the shoreline inland the height is a constant plateau. The shoreline is a
    /// straight line, so distance-to-shore is the horizontal offset, and the plateau is the backing
    /// terrain a steep bluff should preserve rather than flatten.
    fn flat_mesa_coast(size: usize, shore_x: usize, plateau: f32) -> Field {
        Field::new(size, size, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(size, size, |x, _| {
                if x < shore_x { 0.3 } else { plateau }
            })),
        )
    }

    #[test]
    fn a_steep_backing_preserves_the_mesa_behind_the_beach() {
        // The #252 fix: a steep `bluff_angle` clears the backing terrain within a short run, so the
        // cut bites only the low apron near the water and leaves the plateau behind untouched.
        let coast = flat_mesa_coast(64, 20, 0.8);
        let ctx = ctx(64);
        let params = Params::new()
            .with("beach_width", ParamValue::Float(6.0))
            .with("berm_height", ParamValue::Float(1.0))
            .with("bluff_angle", ParamValue::Float(80.0))
            .with("rounding", ParamValue::Float(0.0));
        let out = run(&coast, &params, &ctx);

        // Just inland of the shoreline the beach face cuts the mesa edge well down toward the water.
        let near = at(&out[0], 23, 32);
        assert!(near < 0.6, "the beach face should carve the apron: {near}");
        // Well inland, past the berm and up the steep backing, the plateau is left as it was: the
        // steep envelope has already risen above it, so `min` keeps the original height.
        let far_before = at(&coast, 44, 32);
        let far_after = at(&out[0], 44, 32);
        assert!(
            (far_after - far_before).abs() < 1e-4,
            "the mesa behind the bluff must be preserved: {far_before} -> {far_after}"
        );

        // A gentle backing (bluff shallower than nothing to clear on the flat mesa) instead cuts a
        // long way inland: the shallower the bluff, the more of the backing becomes coast. This is
        // the knob that trades a preserved bluff for a flattened strip.
        let gentle = Params::new()
            .with("beach_width", ParamValue::Float(6.0))
            .with("berm_height", ParamValue::Float(1.0))
            .with("bluff_angle", ParamValue::Float(4.0))
            .with("rounding", ParamValue::Float(0.0));
        let flat = run(&coast, &gentle, &ctx);
        assert!(
            at(&flat[0], 44, 32) < far_before - 0.05,
            "a gentle backing should cut inland where the steep bluff preserved the mesa"
        );
    }

    #[test]
    fn beach_face_grade_is_berm_over_width() {
        // The beach face is the direct control: it rises from the waterline at `berm_height /
        // beach_width`. Read its slope between two cells on the face (robust to the sub-cell
        // shoreline position of the stepped mesa), where the mesa is well above the face so `min`
        // takes the face. The rise per cell must be `grade / world_height`.
        let coast = flat_mesa_coast(64, 20, 0.8);
        let c = ctx(64);
        let (beach_width, berm_height) = (10.0_f32, 2.0_f32);
        let params = Params::new()
            .with("beach_width", ParamValue::Float(f64::from(beach_width)))
            .with("berm_height", ParamValue::Float(f64::from(berm_height)))
            .with("bluff_angle", ParamValue::Float(80.0))
            .with("rounding", ParamValue::Float(0.0));
        let out = run(&coast, &params, &c);
        let wh = c.world_height() as f32;
        let grade = berm_height / beach_width;
        // Cells 24 and 28 are both on the face (a few metres inland, short of the ~10 m crest); their
        // height difference over the 4-cell run is the face slope.
        let per_cell = (at(&out[0], 28, 32) - at(&out[0], 24, 32)) / 4.0;
        assert!(
            (per_cell - grade / wh).abs() < 1e-4,
            "the beach face must rise at berm_height / beach_width: {per_cell} vs {}",
            grade / wh
        );
    }

    #[test]
    fn beach_width_sets_the_inland_extent() {
        // Widening the beach carves further inland: on a flat mesa a wider beach reaches a fixed
        // inland cell that a narrow beach leaves untouched. This is the direct control the offshore
        // width used to lack.
        let coast = flat_mesa_coast(96, 20, 0.7);
        let c = ctx(96);
        let base = |bw: f64| {
            Params::new()
                .with("beach_width", ParamValue::Float(bw))
                .with("berm_height", ParamValue::Float(2.0))
                .with("bluff_angle", ParamValue::Float(80.0))
                .with("rounding", ParamValue::Float(0.0))
        };
        // Cell 40 is 20 m inland of the shoreline at x = 20.
        let narrow = run(&coast, &base(8.0), &c);
        let wide = run(&coast, &base(30.0), &c);
        assert!(
            (at(&narrow[0], 40, 48) - 0.7).abs() < 1e-4,
            "a narrow beach leaves the cell inland of it untouched"
        );
        assert!(
            at(&wide[0], 40, 48) < 0.7 - 1e-3,
            "a wider beach carves the same cell"
        );
    }

    #[test]
    fn shoreface_reach_is_decoupled_from_the_beach() {
        // The whole point of the re-model: the offshore shoreface has its own extent, so changing it
        // moves the seabed but not the land, and changing the beach width moves the land but not the
        // seabed. A cell offshore is lifted more by a longer shoreface; a cell on land is untouched
        // by the shoreface reach.
        let coast = flat_mesa_coast(96, 40, 0.7);
        let c = ctx(96);
        let base = || {
            Params::new()
                .with("beach_width", ParamValue::Float(10.0))
                .with("berm_height", ParamValue::Float(2.0))
                .with("bluff_angle", ParamValue::Float(80.0))
                .with("rounding", ParamValue::Float(0.0))
        };
        let short = run(
            &coast,
            &base().with("shoreface_reach", ParamValue::Float(4.0)),
            &c,
        );
        let long = run(
            &coast,
            &base().with("shoreface_reach", ParamValue::Float(30.0)),
            &c,
        );
        // A cell 15 m offshore (x = 25) is beyond the short reach but within the long one, so only
        // the long shoreface lifts it.
        assert!(
            (at(&short[0], 25, 48) - at(&coast, 25, 48)).abs() < 1e-4,
            "beyond the short shoreface the seabed is untouched"
        );
        assert!(
            at(&long[0], 25, 48) > at(&coast, 25, 48) + 1e-3,
            "within the long shoreface the seabed is lifted"
        );
        // A cell on land (x = 45, inland of the x = 40 shoreline) is identical either way: the
        // shoreface reach never touches the land.
        assert_eq!(
            at(&short[0], 45, 48),
            at(&long[0], 45, 48),
            "the shoreface reach must not affect the land"
        );
    }

    #[test]
    fn the_berm_does_not_move_the_seabed() {
        // The other half of the decoupling: the beach parameters never reshape the water. Raising
        // the berm (an above-water feature) must leave an offshore cell exactly as it was, so the
        // seabed does not deepen or shallow as the beach is sized.
        let coast = flat_mesa_coast(96, 40, 0.7);
        let c = ctx(96);
        let base = |berm: f64| {
            Params::new()
                .with("beach_width", ParamValue::Float(10.0))
                .with("berm_height", ParamValue::Float(berm))
                .with("bluff_angle", ParamValue::Float(45.0))
                .with("rounding", ParamValue::Float(0.0))
                .with("shoreface_reach", ParamValue::Float(20.0))
        };
        let low = run(&coast, &base(2.0), &c);
        let high = run(&coast, &base(20.0), &c);
        // x = 30 is 10 m offshore of the x = 40 shoreline, within the shelf.
        assert_eq!(
            at(&low[0], 30, 48),
            at(&high[0], 30, 48),
            "the berm height must not change the seabed"
        );
    }

    #[test]
    fn smooth_max_rounds_only_near_the_corner() {
        // Zero radius is the exact max: a sharp corner.
        assert_eq!(smooth_max(1.0, 3.0, 0.0), 3.0);
        // Far apart (difference beyond the radius) is untouched, still the exact max.
        assert_eq!(smooth_max(0.0, 10.0, 2.0), 10.0);
        // Never below the max, and it lifts the join near the corner.
        assert!(smooth_max(2.0, 2.5, 2.0) > 2.5);
        // Coincident lines lift by the full quarter-radius (k / 4).
        assert!((smooth_max(5.0, 5.0, 4.0) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn rounding_softens_the_berm_crest() {
        // At the berm crest the sharp profile makes a hard corner; rounding blends the beach face
        // and the backing into a shoulder, lifting the envelope there so the crest is cut less
        // abruptly (a softer break). With a 14 m beach the crest sits at x = 34 (14 cells inland of
        // the x = 20 shoreline).
        let coast = flat_mesa_coast(64, 20, 0.8);
        let c = ctx(64);
        let base = |rounding: f64| {
            Params::new()
                .with("beach_width", ParamValue::Float(14.0))
                .with("berm_height", ParamValue::Float(1.0))
                .with("bluff_angle", ParamValue::Float(80.0))
                .with("rounding", ParamValue::Float(rounding))
        };
        let sharp_out = run(&coast, &base(0.0), &c);
        let rounded_out = run(&coast, &base(8.0), &c);
        let crest_x = 34;
        assert!(
            at(&rounded_out[0], crest_x, 32) > at(&sharp_out[0], crest_x, 32) + 1e-3,
            "rounding should lift and soften the berm crest"
        );
    }

    #[test]
    fn lifts_the_seabed_into_a_shoreface() {
        // A cell just offshore (below sea level, outside the shoreline circle) is lifted up toward
        // the shoreface plane, so it rises above its original depth.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        // (50, 32) is ~18 cells right of centre: r ~= 0.56, below the 0.5 shoreline, within reach.
        let before = at(&island, 50, 32);
        let after = at(&out[0], 50, 32);
        assert!(
            after > before + 1e-3,
            "a near-shore seabed cell should be lifted: {before} -> {after}"
        );
        assert!(
            after <= 0.5 + 1e-3,
            "the lift should not rise above sea level"
        );
    }

    #[test]
    fn shore_band_peaks_at_the_waterline_and_fades() {
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        let shore = &out[1];
        // Somewhere along a radius the band crosses the waterline and reads ~1.
        let peak = (0..65).map(|x| at(shore, x, 32)).fold(0.0_f32, f32::max);
        assert!(peak > 0.9, "the band should peak near 1 at the shoreline");
        // The island centre is far inland (well beyond the 8 m beach), so its band is ~0.
        assert!(at(shore, 32, 32) < 1e-3, "the centre is far from any shore");
    }

    #[test]
    fn beach_mask_covers_the_face_and_spares_the_waterline() {
        // The beach footprint (output 2): zero at the waterline (so masked detail never touches the
        // clean shoreline) and at the crest, and one across the interior of the beach face. Read a
        // straight coast so distance-to-shore is the horizontal offset.
        let coast = flat_mesa_coast(96, 30, 0.7);
        let c = ctx(96);
        let params = Params::new()
            .with("beach_width", ParamValue::Float(20.0))
            .with("berm_height", ParamValue::Float(2.0))
            .with("bluff_angle", ParamValue::Float(80.0))
            .with("rounding", ParamValue::Float(0.0));
        let out = run(&coast, &params, &c);
        let beach = &out[2];
        // Offshore (x = 25, seaward of the x = 30 shoreline): no beach.
        assert!(at(beach, 25, 48) < 1e-3, "the beach mask is zero offshore");
        // At the shoreline (x = 30): feathered to near zero, protecting the contour.
        assert!(
            at(beach, 30, 48) < 0.2,
            "the beach mask is low at the waterline"
        );
        // Mid beach (x = 40, ~10 m inland of the 20 m beach): full.
        assert!(
            at(beach, 40, 48) > 0.9,
            "the beach mask is one across the face"
        );
        // Just below the crest (x = 48, ~2 m short of the 20 m crest at x = 50): the narrow crest
        // feather keeps the steeper shoulder covered rather than fading it out.
        assert!(
            at(beach, 48, 48) > 0.7,
            "the beach mask covers the berm slope up to near the crest"
        );
        // Past the berm crest (x = 55, well beyond the 20 m beach): zero.
        assert!(
            at(beach, 55, 48) < 1e-3,
            "the beach mask is zero past the crest"
        );
    }

    #[test]
    fn bluff_mask_covers_the_backing_slope() {
        // The bluff footprint (output 3): zero offshore and on the beach face, one across the carved
        // backing slope past the crest, and zero past the bluff toe where the terrain resumes. A
        // tall mesa (0.8) and a 45 deg backing so the bluff carves a long way up.
        let coast = flat_mesa_coast(96, 30, 0.8);
        let c = ctx(96);
        let params = Params::new()
            .with("beach_width", ParamValue::Float(10.0))
            .with("berm_height", ParamValue::Float(2.0))
            .with("bluff_angle", ParamValue::Float(45.0))
            .with("rounding", ParamValue::Float(0.0));
        let out = run(&coast, &params, &c);
        let bluff = &out[3];
        // Offshore (x = 25): no bluff.
        assert!(at(bluff, 25, 48) < 1e-3, "the bluff mask is zero offshore");
        // On the beach face (x = 35, before the 10 m crest): still beach, not bluff.
        assert!(
            at(bluff, 35, 48) < 0.1,
            "the bluff mask is low on the beach face"
        );
        // On the backing slope (x = 52, past the crest, still carving): full.
        assert!(
            at(bluff, 52, 48) > 0.9,
            "the bluff mask covers the carved backing slope"
        );
        // Past the bluff toe (x = 90, terrain resumed): zero.
        assert!(
            at(bluff, 90, 48) < 0.1,
            "the bluff mask is zero past the toe"
        );
    }

    #[test]
    fn far_from_shore_is_unchanged() {
        // The steep bluff (80 deg) rises faster than the cone flank, so it clears the terrain within
        // a short run of the beach and the interior peak is left identical.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        assert_eq!(
            at(&out[0], 32, 32),
            at(&island, 32, 32),
            "terrain beyond the bluff toe must be identical"
        );
    }

    #[test]
    fn strength_zero_is_a_passthrough() {
        let island = cone_island(48);
        let params = beach_params().with("strength", ParamValue::Float(0.0));
        let out = run(&island, &params, &ctx(48));
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            island.layer(layers::HEIGHT).unwrap().content_hash(),
            "strength 0 must leave the height layer untouched"
        );
    }

    #[test]
    fn a_zero_mask_layer_protects_the_coast() {
        let mut island = cone_island(48);
        let before = island.layer(layers::HEIGHT).unwrap().content_hash();
        island.set_layer(layers::MASK, Arc::new(Layer::filled(48, 48, 0.0)));
        let out = run(&island, &beach_params(), &ctx(48));
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            before,
            "mask 0 everywhere must disable reshaping"
        );
    }

    #[test]
    fn a_zero_mask_input_overrides_the_mask_layer() {
        // The input carries a mask layer of 1.0 (reshape), but a wired mask input of 0.0 (protect)
        // wins: the coast is unchanged, proving the input takes precedence.
        let mut island = cone_island(48);
        island.set_layer(layers::MASK, Arc::new(Layer::filled(48, 48, 1.0)));
        let before = island.layer(layers::HEIGHT).unwrap().content_hash();
        let mask = Field::new(48, 48, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(48, 48, 0.0)));
        let out = Coastal
            .eval(
                Inputs::new(&[&island], &[Some(&mask)]),
                &beach_params(),
                &ctx(48),
            )
            .unwrap();
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            before,
            "the mask input must override the mask layer"
        );
    }

    #[test]
    fn the_world_sea_level_drives_the_shoreline() {
        // No `level` param: moving the world sea level relocates the shoreline, so the reshaped
        // output differs. This is the check that the node reads ctx.sea_level().
        let island = cone_island(48);
        let high = run(&island, &beach_params(), &ctx(48).with_sea_level(0.5));
        let low = run(&island, &beach_params(), &ctx(48).with_sea_level(0.3));
        assert_ne!(
            high[0].layer(layers::HEIGHT).unwrap().content_hash(),
            low[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "a different sea level must move the coast"
        );
    }

    #[test]
    fn reshaping_has_four_fold_symmetry() {
        // A centred cone with a centred shoreline must reshape identically along +x, -x, +y, -y:
        // the signed distance and the cone are both four-fold symmetric, so the cut is exact.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        let h = out[0].layer(layers::HEIGHT).unwrap();
        for k in 1..=10 {
            let e = h.get(32 + k, 32).unwrap();
            assert_eq!(e, h.get(32 - k, 32).unwrap(), "east/west differ at {k}");
            assert_eq!(e, h.get(32, 32 + k).unwrap(), "east/north differ at {k}");
            assert_eq!(e, h.get(32, 32 - k).unwrap(), "east/south differ at {k}");
        }
    }

    #[test]
    fn reshaping_is_isotropic_no_star() {
        // The no-star canary: two land cells at nearly equal radius, one on the axis and one on the
        // diagonal, are the same distance from the circular shoreline and sit on the same cone
        // height, so they must be cut by nearly the same amount. A chamfer distance would carve the
        // diagonal differently and fail this.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        let h = out[0].layer(layers::HEIGHT).unwrap();
        let axis = h.get(32 + 7, 32).unwrap(); // r = 7.00
        let diag = h.get(32 + 5, 32 + 5).unwrap(); // r = 7.07
        assert!(
            (axis - diag).abs() < 0.02,
            "axis and diagonal cuts should match (no star): {axis} vs {diag}"
        );
    }

    /// A land plateau (above sea) with an enclosed below-sea pit in the middle, ringed by land all
    /// the way to the border, so the pit is not connected to any edge.
    fn enclosed_basin(size: usize) -> Field {
        let mut data = vec![0.8_f32; size * size];
        let c = size / 2;
        for y in (c - 3)..=(c + 3) {
            for x in (c - 3)..=(c + 3) {
                data[y * size + x] = 0.2;
            }
        }
        Field::new(size, size, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::from_vec(size, size, data)))
    }

    #[test]
    fn an_enclosed_basin_is_left_untouched() {
        // The pit is below sea level but reaches no edge, and there is no real coast, so with
        // connectivity on (the default) nothing is reshaped.
        let field = enclosed_basin(33);
        let out = run(&field, &beach_params(), &ctx(33));
        assert_eq!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            field.layer(layers::HEIGHT).unwrap().content_hash(),
            "an enclosed basin with no real coast must not be reshaped"
        );
    }

    #[test]
    fn eroding_inland_basins_reshapes_the_basin() {
        // With `erode_inland_basins` on, the pit's contour is treated as a shoreline (v0 behaviour),
        // so the field changes: this is the escape hatch, and it proves the basin *would* have been
        // carved without the exclusion.
        let field = enclosed_basin(33);
        let params = beach_params().with("erode_inland_basins", ParamValue::Bool(true));
        let out = run(&field, &params, &ctx(33));
        assert_ne!(
            out[0].layer(layers::HEIGHT).unwrap().content_hash(),
            field.layer(layers::HEIGHT).unwrap().content_hash(),
            "eroding inland basins treats the pit as sea and reshapes it"
        );
    }

    #[test]
    fn basin_exclusion_does_not_change_an_open_coast() {
        // The cone island's sea reaches the map edge and encloses no basins, so excluding enclosed
        // basins (the default) is a no-op there: the result is identical either way.
        let island = cone_island(65);
        let excluded = run(&island, &beach_params(), &ctx(65));
        let eroded = run(
            &island,
            &beach_params().with("erode_inland_basins", ParamValue::Bool(true)),
            &ctx(65),
        );
        assert_eq!(
            excluded[0].layer(layers::HEIGHT).unwrap().content_hash(),
            eroded[0].layer(layers::HEIGHT).unwrap().content_hash(),
            "an edge-connected coast is identical with or without basin exclusion"
        );
    }

    #[test]
    fn passes_through_other_layers() {
        let mut island = cone_island(32);
        island.set_layer("flow", Arc::new(Layer::filled(32, 32, 0.7)));
        let out = run(&island, &beach_params(), &ctx(32));
        assert_eq!(
            out[0].layer("flow").unwrap().get(0, 0).unwrap(),
            0.7,
            "an unrelated layer must pass through the heightfield output"
        );
    }

    #[test]
    fn is_deterministic() {
        // Per-cell over the signed-distance field, so the output is byte-identical run to run.
        let island = cone_island(48);
        let once = run(&island, &beach_params(), &ctx(48));
        let twice = run(&island, &beach_params(), &ctx(48));
        assert_eq!(once[0].content_hash(), twice[0].content_hash());
        assert_eq!(once[1].content_hash(), twice[1].content_hash());
        assert_eq!(once[2].content_hash(), twice[2].content_hash());
        assert_eq!(once[3].content_hash(), twice[3].content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let island = cone_island(32);
        let made = registry::make(TYPE_ID).expect("coastal operator is registered");
        let via_registry = made
            .eval(Inputs::required_only(&[&island]), &beach_params(), &ctx(32))
            .unwrap();
        let direct = run(&island, &beach_params(), &ctx(32));
        assert_eq!(via_registry[0].content_hash(), direct[0].content_hash());
        assert_eq!(via_registry[1].content_hash(), direct[1].content_hash());
        assert_eq!(via_registry[2].content_hash(), direct[2].content_hash());
        assert_eq!(via_registry[3].content_hash(), direct[3].content_hash());
    }
}
