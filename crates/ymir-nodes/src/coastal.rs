//! Coastal reshaping: a beach-and-bluff bevel keyed to distance from the shoreline (#139).
//!
//! The shoreline is the world sea-level contour of the `height` layer (sea level is a world
//! setting carried in [`EvalContext::sea_level`], never a node param). This node lays a symmetric
//! wedge of grade `angle` on that contour: on land it cuts the terrain *down* toward a plane
//! rising from the waterline (`min`), and offshore it lifts the seabed *up* toward the mirror
//! plane (`max`). Both fade to zero over `width` metres, so terrain resumes smoothly away from the
//! coast and the surface is continuous through the waterline. Where the rising plane meets an
//! un-cut hillside a break of slope appears on its own: that is the bluff toe, not a separate step.
//!
//! The bevel is parameterised by *true isotropic distance from the shoreline*, from the shared
//! eikonal substrate ([`signed_distance_to_contour`](crate::distance)). That is the whole reason
//! it reshapes as an even band all around a coast rather than the eight-lobed star a chamfer
//! distance would carve. Because the reshape is a pure per-cell function of that signed distance,
//! the result is byte-identical on every machine, and the no-star isotropy is inherited from the
//! solve rather than re-derived here.
//!
//! Two outputs: the reshaped `heightfield`, and a `shore` band (one near the waterline, fading to
//! zero at `width`) that the solve already produced. `shore` is a ready selection for the beach
//! texturing and foam work downstream, so it is emitted rather than discarded and recomputed.
//! Water depth is not emitted: it is `sea_level - height`, recoverable from the field plus the
//! global, so by the layer test it does not earn a stored layer.

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

/// Default coastal reach in world metres: how far to each side of the shoreline the bevel acts.
const DEFAULT_WIDTH: f64 = 150.0;
/// Default beach/shoreface grade in degrees. A few degrees reads as a gentle sandy shore.
const DEFAULT_ANGLE: f64 = 4.0;
/// Maximum grade. Capped below 90 so `tan(angle)` stays finite; near the cap the wedge is a near
/// cliff at the waterline.
const MAX_ANGLE: f64 = 80.0;

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
            outputs: vec![PortSpec::new("heightfield"), PortSpec::new("shore")],
            params: vec![
                ParamSpec::new(
                    "width",
                    ParamKind::Float {
                        min: 0.0,
                        max: 100_000.0,
                    },
                    ParamValue::Float(DEFAULT_WIDTH),
                )
                .with_unit(Unit::Meters),
                ParamSpec::new(
                    "angle",
                    ParamKind::Float {
                        min: 0.0,
                        max: MAX_ANGLE,
                    },
                    ParamValue::Float(DEFAULT_ANGLE),
                )
                .with_unit(Unit::Degrees),
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
            // "shore" is a byproduct output port, not a canonical layer constant (there is no
            // `layers::SHORE`); name it by the port so the reference lists the shore band.
            emitted_layers: vec!["shore"],
            mask_aware: true,
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs[0];
        let (width, height) = (input.width(), input.height());
        let source = input.layer_or(layers::HEIGHT, 0.0);

        // A zero reach would divide by zero in the falloff; clamp to a hair so it degrades to a
        // hard edge at the waterline rather than panicking.
        let reach = params.get_f64("width", DEFAULT_WIDTH).max(1e-6) as f32;
        let angle = params.get_f64("angle", DEFAULT_ANGLE).clamp(0.0, MAX_ANGLE);
        let grade = angle.to_radians().tan() as f32; // rise over run, in world metres
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

        let mut reshaped = vec![0.0_f32; width * height];
        let mut shore = vec![0.0_f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let idx = y * width + x;
                let d = signed.get(x, y).unwrap_or(0.0);
                let original = source.get(x, y).unwrap_or(0.0);

                // The coastal band: one at the waterline, easing to zero at `reach` on either side.
                // This is the geometric extent of the coast, so it carries neither strength nor
                // mask; it is emitted as the `shore` selection.
                let band = 1.0 - smoothstep(reach, d.abs());
                shore[idx] = band;

                // The wedge target: a plane at grade `angle` rising inland and dropping offshore
                // from sea level. On land keep the lower of terrain and plane (cut a beach into the
                // hill); offshore keep the higher (lift the seabed into a shoreface). Either way the
                // shoreline sits exactly at sea level, so the two sides meet continuously.
                let rise = grade * d.abs() / world_height;
                let carved = if d >= 0.0 {
                    original.min(sea + rise)
                } else {
                    original.max(sea - rise)
                };

                let weight = band * strength * mask.get(x, y).unwrap_or(1.0);
                reshaped[idx] = original + (carved - original) * weight;
            }
        }

        let mut heightfield = input.clone();
        heightfield.set_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_vec(width, height, reshaped)),
        );
        let shore_field = erosion::byproduct_field(shore, width, height, input.region());
        Ok(vec![heightfield, shore_field])
    }
}

/// Cubic Hermite smoothstep of `x` over `[0, edge]`, clamped to `[0, 1]`. Zero at `x = 0`, one at
/// `x >= edge`; `edge` is guaranteed positive by the caller.
fn smoothstep(edge: f32, x: f32) -> f32 {
    let t = (x / edge).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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

    /// A context whose world is a cube of side `size` (so metres-per-cell is 1 and `width` reads in
    /// cells, and the grade is gentle rather than squashed) with the shoreline at height 0.5.
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

    /// A wide beach so the reshaping reaches several cells inland on the 65-cell cone.
    fn beach_params() -> Params {
        Params::new().with("width", ParamValue::Float(12.0))
    }

    #[test]
    fn spec_is_a_geology_modifier_with_heightfield_and_shore() {
        let spec = Coastal.spec();
        assert_eq!(spec.kind(), NodeKind::Modifier);
        assert_eq!(spec.category, "geology");
        assert_eq!(spec.type_id, TYPE_ID);
        let outputs: Vec<&str> = spec.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(outputs, ["heightfield", "shore"]);
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
        let before = at(&island, 42, 32);
        let after = at(&out[0], 42, 32);
        assert!(
            after < before - 0.05,
            "a coastal hill cell should be cut down: {before} -> {after}"
        );
        assert!(after >= 0.5 - 1e-3, "the cut should not go below sea level");
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
        // The island centre is far inland (well beyond the 12-cell reach), so its band is ~0.
        assert!(at(shore, 32, 32) < 1e-3, "the centre is far from any shore");
    }

    #[test]
    fn far_from_shore_is_unchanged() {
        // The peak is ~16 cells from the shoreline, past the 12-cell reach, so it is untouched.
        let island = cone_island(65);
        let out = run(&island, &beach_params(), &ctx(65));
        assert_eq!(
            at(&out[0], 32, 32),
            at(&island, 32, 32),
            "terrain beyond the reach must be identical"
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
    }
}
