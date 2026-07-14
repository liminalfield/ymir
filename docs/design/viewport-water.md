# Viewport water rendering

How the 3D viewport should render water. Companion to the coastal work
(`coastal-erosion.md`), but independent of it: this is a presentation concern, and the
determinism contract never reaches a viewport shader, so it can be as aesthetic as we
like. Three things are worth separating, and the first constrains the other two more than
expected: how the viewport renders at all, where the water surface comes from, and what
the shader does.

## The architectural fork (decide first)

Today `crates/ymir-gui/src/viewport.rs` renders the 3D scene via an `egui_wgpu` **paint
callback straight into egui's render pass**, using egui's shared depth attachment (from
`NativeOptions::depth_buffer = 24`). That means we inherit egui's attachments: no color or
depth target we can sample, no MSAA or multi-pass control. Every interesting water
technique (depth-tested transparency done our way, refraction, planar reflection) then
becomes a fight against egui's pass.

**Decision: take the offscreen fork, and do it before building water.** Render the whole
3D scene offscreen in the callback's prepare phase to our own color + depth targets,
register it with `egui_wgpu::Renderer::register_native_texture`, and let egui composite it
as a texture. This is the same effort as the always-planned viewport rework, done right,
and it buys controlled depth testing, MSAA, multi-pass, GPU picking, HDR headroom, and
screen-space refraction. Worth taking regardless of how fancy the water gets.

## Where the water surface comes from

Two sources feed one fragment shader path:

- **Sea/lake level** — the `detail::SEA_LEVEL` scalar. The surface is a constant height;
  terrain height comes from the height texture; `depth = sea_level - height`. Geometry is
  just a quad. This is the provider now.
- **Simulated water** — the `water` layer from a future pipe model. The surface is
  `height + water` and `depth = water` directly. Geometry is the terrain grid mesh plus a
  second vertex shader. Later.

Same fragment shader, different provider of `(surface height, depth)`.

The key simplification versus a game engine: we have the heightmap as a **texture**, so
water depth is computed analytically per fragment (sample terrain height at the fragment's
world XY). No depth-buffer copy, no linearizing, no refraction pass needed just to know how
deep the water is. That collapses most of the usual complexity. (The current viewport
meshes height per vertex; this adds a height-texture upload.)

## Shader tiers

- **Tier 0 — one pass, no extra targets.** Sample terrain height at the fragment, discard
  where `depth <= 0`, apply Beer-Lambert extinction `alpha = 1 - exp(-depth * k)` with a
  shallow-to-deep colour ramp. Add wet-darkening to the terrain shader just below the
  waterline. Translucency and a shoreline that fades on gentle slopes and hardens on cliffs
  (because falloff is depth-driven and depth is slope-driven), for two small changes and
  most of the perceived payoff.
- **Tier 1 — surface and specular.** Perturb the water normal with two scrolling
  noise-derivative fields at different scales/speeds/directions. Use it for a GGX or
  Blinn-Phong specular from the sun direction and for a Schlick-Fresnel mix between a
  sky-gradient reflection and the transmitted colour. Fresnel is what sells water. Fade
  ripple amplitude to zero at the shoreline so it does not chew the shore. Still one pass.
  **Tier 0 + Tier 1 is where to stop for a terrain tool.**
- **Tier 2 — screen-space refraction.** Since the scene is rendered offscreen, sample that
  colour target with UVs offset by the perturbed normal, scaled by depth. Cheap once the
  offscreen path exists; guard the above-waterline halo by rejecting samples whose depth is
  negative and falling back to the unrefracted UV.
- **Tier 3 — planar reflection.** Render the terrain a second time with the camera mirrored
  across the water plane. Exact for a flat sea level, doubles terrain draw cost, and does
  not work for non-planar sim water. **Skip it** — procedural sky plus Fresnel gets ~90% of
  the way for far less.

## Two extras

- **Foam.** Depth-based foam (`depth < eps`) is free but its band width varies with shore
  slope. A uniform-width foam band needs distance-to-shore, which is a geodesic distance
  field on the water mask — the **same eikonal solve** the flow-map fix, the coastal
  distance, and a Distance selector all want. A fourth consumer; build it once.
- **Caustics.** Project animated noise onto the terrain below the waterline, modulated by
  depth and the water normal. Purely fake, cheap, and disproportionately convincing.

## Sequence

This is Track B, independent of the coastal model (Track A):

1. Offscreen render fork (the foundation above).
2. Height-texture upload + sea-level water plane, Tier 0.
3. Tier 1 (normals, Fresnel, specular).
4. Later: Tier 2 refraction; foam once the eikonal distance field exists.

The `sea_level` setting and the show-water toggle are pure data/UI and can land
independently; only the plane's rendering quality rides this track.
