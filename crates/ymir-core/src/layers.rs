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

/// The x component of a 2D direction/flow field (paired with [`FLOW_Y`]). A vector
/// field rides on the `Field` as these two scalar layers rather than a special vector
/// type; curl/flow noise writes them, and a directional warp or erosion grain reads
/// them. Absent on a plain heightfield, so consumers degrade gracefully.
pub const FLOW_X: &str = "flow_x";

/// The y component of a 2D direction/flow field (paired with [`FLOW_X`]).
pub const FLOW_Y: &str = "flow_y";
