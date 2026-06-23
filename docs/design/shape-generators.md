# Design note: the Shape generator family

The Shape generators are the procedural members of the **envelope** family from the
control-fields note: no-input generators that draw a smooth `[0, 1]` control field over
region coordinates, to be multiplied with noise (a Blend in Multiply) so that detail
appears only where the shape says, and tapers to nothing at its edge. `generator.radial`
is the first member and the reference implementation; this note specifies the rest so
they stay one consistent family rather than four ad-hoc nodes.

These are the **Generate** source from the control-fields note (a field authored from
scratch to encode intent), as opposed to the **Derive** source (selectors: slope,
curvature, height-select) and the future **Paint** source.

## Shared contract (inherited from radial)

Every Shape generator holds to the same conventions, so a graph reads predictably and a
shape swapped for another behaves the same way under resolution, region, and world
extent:

1. **Output.** A plain `Field` with a `height` layer in `[0, 1]`: 1 in the core of the
   shape, 0 outside, a smoothstep transition between. No other layers. It is a control
   field like any other, not a special "mask type".
2. **Sampled, not iterated.** The value at a cell is a closed-form function of that
   cell's position, so it is resolution-independent (same world coordinate gives the
   same value at any resolution) and region-correct (a tile lands on the same ground as
   the untiled build). Cell centers are sampled at `(x + 0.5, y + 0.5)`.
3. **Lengths are world units (meters)**, converted to cells through `ctx.world_to_cells`
   exactly as Blur and radial do, so a shape covers the same physical reach at any
   resolution and on any world extent.
4. **Position is normalized** over the whole world (`center_x`, `center_y` in `[0, 1]`,
   0.5 = middle), mapped through the evaluated `region` to a cell. This reads as a 0..1
   slider and stays resolution- and extent-independent.
5. **One fixed smoothstep falloff, on purpose.** A different transition profile is a
   downstream Curve node; an inverted shape (a basin, a moat) is a downstream Invert
   node; a feathered edge is a Blur. None of those are parameters buried in the shape.
   This is the small-single-purpose-node rule, and it is what makes the shapes
   composable rather than each one a little remap engine.
6. **Single-purpose by geometry, not by enum.** The node's identity is its shape. There
   is no `kind: disc | ring | box` parameter switching behavior inside one node; that
   would be the multi-purpose node the project avoids. Radial, Ring, Gradient, and Rect
   are separate files, each one geometric primitive.
7. **Degenerate parameters degrade, never panic.** A non-positive radius/width yields a
   defined field (a flat zero, or a hard step) rather than a division by zero or a NaN,
   matching radial's collapsed-radius behavior.

The falloff helper (`smoothstep`) and the center-to-cell mapping are identical to
radial's; when the second shape lands, those move to a small shared `shape` module
(`crates/ymir-nodes/src/shape.rs`) that radial also adopts, so the math lives once.

## The members

### generator.radial (built)

The circular dome: 1 at `center`, easing to 0 at `radius` through a smoothstep, 0
beyond. Seeds the island / massif landform. Params: `radius` (m), `center_x`,
`center_y`.

### generator.gradient — the directional trend

A directional ramp: 0 on one side of a line, 1 on the other, a smoothstep band between.
The trend control field. Seeds coast-to-highland tilt, dune-field direction, any
regional lean, and is the canonical non-centered envelope that answers the
hero-mountain-in-the-center problem.

- `angle` (degrees): the direction of increase. 0 points along +x (east); the angle
  rotates counter-clockwise.
- `center_x`, `center_y` (normalized): the point the half-value line passes through.
- `width` (m): the distance over which the value sweeps 0 to 1. A wide band is a gentle
  full-map ramp; a narrow band is a soft straight coastline. Default pairs with the
  default world extent to give a full-map ramp out of the box.

Math: project the cell's offset from the center onto the unit direction `(cos a, sin a)`
to get a signed distance `s` in cells; `value = smoothstep(0, 1, 0.5 + s / width_cells)`.
A non-positive width degrades to a hard half-plane step at the center line.

### generator.ring — the annulus

A circular ridge: 1 on a circle of `radius`, falling to 0 over `width` on each side. The
crater / caldera / atoll envelope, and the ringed-massif base. It is the radial's
companion: radial fills the disc, ring outlines it.

- `center_x`, `center_y` (normalized).
- `radius` (m): the radius of the peak circle.
- `width` (m): the thickness, the distance from the peak circle out to zero on each
  flank.

Math: `d = distance(cell, center)` in cells; `value = 1 - smoothstep(0, width_cells,
abs(d - radius_cells))`. A non-positive width collapses the ring to a flat zero (an
infinitely thin circle has no area).

### generator.rect — the rectangular footprint

An axis-or-rotated rectangle with a flat top and soft flanks: the plateau / mesa / table
footprint, and rectangular-landmass base. The box analogue of radial.

- `center_x`, `center_y` (normalized).
- `extent_x`, `extent_y` (m): the full span of the flat core along each local axis
  (before rotation).
- `rotation` (degrees): orientation of the rectangle. 0 is axis-aligned.
- `falloff` (m): the soft band outside the core; also rounds the corners naturally, so
  no separate corner-radius parameter.

Math: rotate the cell offset into the rectangle's local frame; with half-extents
`h = (extent_x, extent_y) / 2` in cells, `q = abs(p_local) - h`; the outside distance is
`length(max(q, 0))` (0 inside the core); `value = 1 - smoothstep(0, falloff_cells,
outside_dist)`. A non-positive falloff degrades to a hard-edged box.

### generator.polygon — the regular n-gon (optional, last)

A regular polygon envelope: angular plateau, faceted mesa, hex/oct bases. The most
specialized of the set and the most math (a regular-polygon signed distance), so it is
proposed last and is droppable if it does not earn its place against rect + rotation.

- `center_x`, `center_y` (normalized).
- `radius` (m): circumradius (center to vertex).
- `sides` (int, >= 3): the number of sides.
- `rotation` (degrees).
- `falloff` (m): the soft outside band.

Math: a standard regular-polygon SDF (fold the angle into one wedge, distance to the
edge line); `value = 1 - smoothstep(0, falloff_cells, outside_dist)`. `sides < 3`
degrades to the radial dome (a polygon with no facets is a circle).

## Build order

One node per step, each its own commit, each ending compiling, tested, clippy/fmt/scanner
clean, with a golden-value test captured from real output, following radial's test shape
(determinism, range, the defining geometric property, a degenerate-param case, registry
round-trip, generator-kind, golden). The ymir-cli registry smoke-test node list grows by
one each step.

1. **gradient** — simplest math, highest value (the non-centered envelope). First, so
   the shared `shape.rs` module is extracted here and radial moves onto it.
2. **ring** — reuses the radial distance; small.
3. **rect** — box SDF plus rotation; introduces the rotate-into-local-frame helper.
4. **polygon** — optional; only if it earns its place after rect.

Each step pauses for review before the next.
