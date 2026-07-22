# HANDOFF — mask painting (temporary, delete on resume)

Resume notes for the mask-painting feature. On branch `feature/paint-mask-gui`.
Full design: `~/.claude/plans/eager-crafting-pascal.md`. Issue #145.

## Where it stands

Branch `feature/paint-mask-gui`, on top of `main`:

- **#199 (merged):** core stroke model (`ymir-core::paint` — `Strokes`/`Stroke`/
  `StrokePoint`/`StrokeMode`/`BrushShape`, `ParamValue::Strokes`, `ParamKind::Strokes`)
  and the `generator.paint` node that rasterizes strokes → `[0,1]` mask on the height
  layer, per-cell, resolution-independent, byte-identical.
- **2D-map painting** (`992c6f8`): inspector paint controls (brush size/strength/
  hardness, paint/erase, undo/clear) + drag-to-paint on the 2D map. `PaintBrush` /
  `paint_target` in `AppState`; `apply_paint_sample`; `viewport2d::show` takes
  `paint_active` and returns a `PaintSample`.
- **3D-surface picking** (`a8b8250`): `pick::raycast_heightfield` — CPU heightfield
  ray-cast, exaggeration-aware, unit-tested. Wired into `viewport::show`: plain
  left-drag paints on the 3D surface (orbit = Alt-drag, fly = right-drag).
- **Paint backdrop input** (`c3b7e55`): Paint has an optional `backdrop` input; when
  wired it carries the terrain on `layers::BACKDROP` (display only; mask stays on
  height so the mask ports are unaffected). Unwired, Paint is a plain source.

Everything is committed and green (fmt, clippy, `cargo test --workspace`, shortcuts).

## The one remaining task: the 3D mask overlay (GPU)

Make 3D painting show the terrain (geometry) with the painted mask as a coloured
tint (texture), so painting is a selection painted onto the surface — **not
displacement**. This is `ymir-gui/src/viewport.rs` shader work and must land as one
piece (meshing the backdrop without the overlay would leave you painting invisibly).

Sub-parts:
1. **Mesh the backdrop, not the mask.** When the previewed field has a `BACKDROP`
   layer (paint mode), mesh that layer and ray-cast against it. `sample_field`
   currently reads `layers::HEIGHT`; parameterize it by layer name and pass `BACKDROP`
   when present (both the mesh path in `show` and the `pick` heights).
2. **Overlay the mask as a tint.** Upload the mask (the field's `HEIGHT` layer) as a
   texture and sample it in the terrain fragment shader, mixing the base colour toward
   a paint colour by the mask value. Mirror the existing height-texture/water plumbing:
   `make_height_texture` / `write_height_texture` (R32Float), the `@group(1)` binding
   (currently bound only for the water shader — the terrain pipeline binds group 0
   only, so this needs a bind-group + pipeline-layout addition for the terrain pass),
   and `sample_seabed` as the in-shader bilinear read pattern.
   - Terrain fragment shader: `fs_main` at ~`viewport.rs:1570` (computes `base * shade`
     with a wet-shore term — add the mask tint before the return). It has the fragment
     world position; UV = `(world.x, world.z)` over the `[0,1]` footprint.
   - Add a uniform flag + paint colour to `Uniforms` (Rust) and the WGSL `U` struct.
   - Mind the terrain-cache (`terrain_valid` / `terrain_view_proj` etc. in the GPU
     resources): the terrain pass is cached and re-runs only when its keyed inputs
     change, so the mask update must invalidate it (fold the mask hash / paint flag in,
     like `terrain_wet`).

## Settled — do NOT re-litigate or re-do

- **Coordinate model:** the mask is a horizontal-plane field indexed by `(x, y)`, the
  terrain's canonical UV. Picking keeps only the horizontal hit, so the mask is
  height-independent — immune to exaggeration (± scale) and to value transforms
  (levels/normalize/clamp). Only a horizontal op (Warp) moves the mapping; that's a
  graph-ordering matter, not a coordinate flaw.
- **Picking** (`pick.rs`) is done and tested — don't rebuild it.
- **Stroke model** and the **Paint node** are done — don't touch the mask math.
- **Backdrop architecture:** terrain on `layers::BACKDROP`, mask on height. Set.

## After the overlay

- Polish: a **brush decal** (ring projected on the surface at the cursor) for aim.
- Deferred / lower priority now that 3D painting works: the 2D-paint + 3D-result
  **split view** (old Step 4), colour painting (same engine), pressure (winit spike).

## How to resume

Open the branch, say "resume the paint-mask viewport overlay", start at the terrain
fragment shader. Delete this file when the work is picked back up.
