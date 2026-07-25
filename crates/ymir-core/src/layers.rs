//! Canonical layer names.
//!
//! Referring to layers through these constants rather than bare string literals
//! turns a typo into a compile error, while still allowing nodes to create
//! arbitrary custom layer names when they need them.

/// The primary height layer. Normalized to `[0, 1]` by working convention; world
/// vertical scale is applied only at export.
pub const HEIGHT: &str = "height";

/// A `[0, 1]` weighting mask. Mask-aware nodes read it through
/// [`Field::layer_or`](crate::Field::layer_or) and apply everywhere when it is
/// absent, so a mask never gates a connection.
pub const MASK: &str = "mask";

/// Standing water depth, in working height units. Written by hydraulic erosion (the
/// shallow-water simulation's water field) and absent on a plain heightfield, so consumers
/// degrade gracefully. A useful intermediate in its own right: where water pools and runs.
pub const WATER: &str = "water";

/// The x component of a 2D direction/flow field (paired with [`FLOW_Y`]). A vector
/// field rides on the `Field` as these two scalar layers rather than a special vector
/// type; curl/flow noise writes them, and a directional warp or erosion grain reads
/// them. Absent on a plain heightfield, so consumers degrade gracefully.
pub const FLOW_X: &str = "flow_x";

/// The y component of a 2D direction/flow field (paired with [`FLOW_X`]).
pub const FLOW_Y: &str = "flow_y";

// Erosion byproducts. Erosion nodes write these alongside the height so downstream nodes can
// texture, mask, and chain off them; each is absent on a plain heightfield, so consumers read
// them through [`Field::layer_or`](crate::Field::layer_or) and degrade gracefully. This is the
// shared vocabulary the erosion roadmap is built on (see design/erosion-roadmap.md).

/// Flow accumulation: how much upstream drainage passes through each cell, the magnitude behind
/// a drainage network. Written by stream and hydraulic erosion; a primary texturing signal and
/// the guidance a later erosion pass can read. This is the scalar magnitude; the flow direction
/// rides on [`FLOW_X`]/[`FLOW_Y`].
pub const FLOW: &str = "flow";

/// Fine material moved and dropped by water: sediment carried then deposited by hydraulic
/// erosion. Smooth, in contrast to the broken [`DEBRIS`] of thermal weathering, and kept a
/// separate layer so downstream texturing and scatter can treat the two differently.
pub const SEDIMENT: &str = "sediment";

/// Coarse material broken loose by thermal weathering and shed downslope (scree, talus).
/// Distinct from the fine [`SEDIMENT`] of water transport.
pub const DEBRIS: &str = "debris";

/// Cumulative material removed by erosion: where, and how much, the bed was worn down. A
/// primary texturing signal, exposed rock often reading from wear combined with slope.
pub const WEAR: &str = "wear";

/// Cumulative material settled out: where deposition built the surface up. The counterpart to
/// [`WEAR`], and a texturing signal for sediment fields, fans, and valley fill.
pub const DEPOSITION: &str = "deposition";

/// Erodibility (softness) input: a `[0, 1]` weighting of how readily each cell erodes, 0 being
/// resistant rock and 1 loose material. Read through [`Field::layer_or`](crate::Field::layer_or)
/// so erosion applies uniformly when it is absent, and lets hardness vary across the terrain.
pub const ERODIBILITY: &str = "erodibility";

/// Bedrock reference height: a floor erosion does not cut below, so resistant rock is exposed
/// rather than endlessly incised. Absent on a plain heightfield, where erosion is unbounded
/// below.
pub const BEDROCK: &str = "bedrock";

/// Backdrop terrain height, carried for display only: the terrain a Paint node is painted over, so
/// the viewport can mesh the real surface (geometry) while the painted mask rides the height layer
/// as a texture (not displacement). Never consumed by an operator; a pass-through for the editor.
pub const BACKDROP: &str = "backdrop";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_are_unique() {
        // A duplicated string across two constants would silently alias two layers; guard it.
        let names = [
            HEIGHT,
            MASK,
            WATER,
            FLOW,
            FLOW_X,
            FLOW_Y,
            SEDIMENT,
            DEBRIS,
            WEAR,
            DEPOSITION,
            ERODIBILITY,
            BEDROCK,
        ];
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "duplicate canonical layer name");
    }
}
