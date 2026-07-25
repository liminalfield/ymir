> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

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

### Implementation

The fork is one **pixel-stable** step: move our rendering onto targets we own without
changing what the viewport looks like. MSAA, HDR, refraction, and picking are follow-ons the
fork enables, each its own change, never bundled into the fork itself. The verification for
the fork is precisely that the viewport looks identical the moment it lands.

Mechanism, inside the existing `egui_wgpu::CallbackTrait` (`ViewportCallback`):

- **`prepare`** renders the whole scene into our own offscreen color + depth textures, in its
  own render pass returned as a command buffer: clear, terrain, then the water plane, exactly
  the draws that live in `paint` today. Depth is ours (`DEPTH_FORMAT`), not egui's.
- **`paint`** stops drawing the scene and instead composites: one fullscreen textured quad
  sampling the offscreen color into egui's pass.

So the real rendering (depth today, MSAA and multi-pass later) happens offscreen under our
control, and egui only composites the finished image. This is *blit-in-paint*. The
alternative is `register_native_texture` plus drawing the color as an `egui::Image`;
blit-in-paint is preferred because it stays inside the callback's prepare-then-paint timing
and avoids managing a `TextureId` across the renderer mutex.

Offscreen targets:

- Color format matches the surface (`RenderState::target_format`, sRGB included) so the
  composite is a 1:1 copy and the result is pixel-identical to today; usage is
  `RENDER_ATTACHMENT | TEXTURE_BINDING`.
- Depth is `DEPTH_FORMAT`.
- Both are sized to the viewport rect in physical pixels (rect size times points-per-pixel,
  passed into the callback), and recreated when that size changes. Guard zero and tiny sizes.
- Clear to the current viewport background and keep a single sample count, so nothing about
  the image shifts.

Once the scene no longer draws into egui's pass, egui's shared depth
(`NativeOptions::depth_buffer = 24`) is unused and can be dropped.

What the fork then unlocks, each a separate follow-up: **MSAA** (multisample the offscreen
targets, resolve before the composite, the first visible win), the **water tiers** below
(Tier 2 refraction samples the offscreen color directly), **GPU picking** (an id/depth target
read back), and **HDR** (offscreen `Rgba16Float` plus a tonemap in the composite).

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
