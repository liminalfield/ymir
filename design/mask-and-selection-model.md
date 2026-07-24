> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: the mask & selection model

Status: **largely built.** The enabling mechanism (optional input ports), the Slope
and Height selectors (grayscale on the `height` layer), thermal's optional `mask`
input, and the Curve/Invert shapers have all landed, and the bundled `Mask` node has
been **retired** in their favour. See the update section. Some decomposition (curvature
and other selector sources, Set Mask) is still ahead.

This note captures a decomposition that came out of building the mask node: how
masks should be *created, manipulated, and applied* so the node graph stays
composable — the Unix philosophy (small tools that each do one thing, piped
together) applied to terrain.

## The idea in one line

> A selection is just a grayscale field. Make it and shape it with normal nodes, then
> wire it into an effect's mask input (or attach it to the terrain's `mask` layer).
> Effects localize their result by the mask.

## Update (2026-06): decided and partly built

Since this note was drafted, the mechanism and the first effect landed, and three of
the open decisions are settled:

- **Selections travel on `height`** (open decision #1, the recommended option). A
  selector outputs a grayscale field on its `height` layer, so the whole field
  toolset applies to it with no new machinery.
- **Effects take an explicit, optional `mask` input** (open decisions #2 and #6). The
  engine now supports optional input ports — a port you can leave unwired — so an
  effect gains a *visible* mask wire without forcing every graph to connect it or
  changing existing graphs. **Blend** is the first to use it: wire a selection into
  its `mask` input and the effect is localized; leave it unwired and it reads the
  field's `mask` layer by convention, else applies everywhere.
- **The layer convention is the fallback, not the only path.** An effect reads its
  `mask` input if wired, else `layer_or(MASK, 1.0)`. So masking is now visible in the
  wiring (the thing this note worried was hidden), while the soft contract still holds
  when nothing is wired.

Consequences for the rest of this note:

- The mix/over compositing the body attributes to a `Combine` node is now the
  **Blend** node (blend modes + opacity + the optional `mask` input). Read "Combine"
  below as "Blend", and the conceptual "Remap" as the built **Curve** node (with
  **Invert** for the plain complement).
- **Set Mask** is no longer the *only* way to apply a selection. It remains the bridge
  for the layer-convention path (attach a selection to a field's `mask` layer so
  downstream effects read it implicitly), but the primary, visible path is to wire the
  selection straight into an effect's `mask` input. Whether Set Mask is still worth
  building is now itself an open question.
- Still open: #3 (Set Mask combine behavior, if built), #4 (layer-targeting — moot
  while selections live on `height`), #5 (one mask vs many).

## Why

Today's `Mask` node does three jobs at once: it **selects** (by slope or height),
**shapes** the selection (smoothstep low/high), and **attaches** it to the terrain
(writes the `mask` layer). That bundling is why simple things feel awkward — e.g.
inverting a mask that is already flowing means fiddling the `low`/`high` sliders
backwards instead of just... inverting it.

The fix is not a cleverer mask node. It is to stop treating a mask as a special
thing. A mask is a `[0, 1]` field. The engine already says *everything is data, one
type on every edge*; selections should obey that too.

## The roles

Four roles, each a small node (or an existing one), instead of one bundled node:

1. **Sources of a selection** — produce a grayscale `[0, 1]` field from some
   criterion of the input terrain:
   - `Slope` — gradient magnitude of the input height.
   - (height-band, curvature, ambient occlusion, noise… later)
   - Output is on the `height` layer: it is just a grayscale image, nothing
     mask-specific about it yet.

2. **Field operations** — the normal toolset, reused on the selection because the
   selection is just a field:
   - `Curve` (built) and `Invert` (built) — threshold, band, smoothstep, invert,
     levels.
   - `Blend` (built) — composite two selections (blend modes + opacity).
   - i.e. you already get invert, combine, and shaping *for free*; they are not
     mask features, they are field features.

3. **Set Mask** — a mask-aware bridge for the layer-convention path. Two inputs: the
   **terrain** and a **selection**; it writes the selection onto the terrain's `mask`
   layer and passes the terrain's other layers through. Not the only way to apply a
   mask now that effects take an explicit `mask` input (see the update); still useful
   when you want a mask to ride along a field implicitly.

4. **Effects** — `Thermal`, `Blend`, future erosion — localize their result by the
   mask: an explicit optional `mask` input when wired (Blend has one), else the
   `mask` layer by the soft contract.

The headline: of these, only `Set Mask` (and arguably `Slope`) is mask-specific.
Everything else is general field plumbing. The monolithic `Mask` node dissolves
into `Slope` + `Remap` + `Set Mask`.

## The mask convention (unchanged, just restated)

A mask **blends a node's effect**: `result = lerp(input, effect, mask)`.

- `mask = 1` → the effect applies fully here.
- `mask = 0` → the input is preserved here.

Every effect reads it the same way; no node reinterprets it. Want the complement?
Invert the selection (a field op) — do not give nodes per-node "mask meaning"
toggles.

### Erosion nodes (Thermal, Stream, Hydraulic)

All three erosion nodes follow this convention through an optional `mask` input under the
soft-layer contract: an explicit mask input wins, else the input's own `mask` layer, else `1.0`
(erode everywhere), so a mask never gates the connection. The mask **confines** the result
(`lerp(original, eroded, mask)`): a masked-out cell keeps its original height exactly, a
masked-in cell takes the eroded height, partials blend. This is a deliberate per-cell *protect*,
chosen over *modulate* (varying a parameter spatially while the simulation still runs
everywhere — Gaea's "Selective Processing"). Modulate is the richer directability option and is
deferred to a later directability phase; confine is the settled Phase 0 convention. Tapped
byproduct outputs (flow, water, sediment, debris) report the full simulation, not the masked
composite.

## Mask sources are unconstrained

A mask has no special source: **any field can be one.** A heightfield from an
alligator/Worley/fBm generator, a `Slope` selection, a remapped height band, two of
those combined, later a painted field or an imported image — `Set Mask` does not
know or care where the grayscale came from. It is all data on a grid.

A consequence: there are **no "mask" variants of nodes.** The same noise generator
produces terrain when wired into the height chain and a mask when wired through
`Set Mask`. One node, both roles, decided by where the wire goes — a lot of nodes
that never need to exist.

## Range and normalization (decided)

`[0, 1]` is the **working convention** for masks and for every field, **never a
hard clamp** — exactly the rule the project already uses for `height` (intermediate
operations may exceed the range; export maps the actual range). So:

- Nothing clamps automatically. Go extreme in the middle of a chain — combine
  things in an over-driven way — and normalize *explicitly*, with a normalize/remap
  node, only when you want a clean `[0, 1]` (e.g. just before `Set Mask`).
- `Set Mask` stays dumb: it attaches the field as the mask, as-is, no hidden clamp.
- An out-of-range mask therefore makes an effect *extrapolate*
  (`lerp(input, effect, 1.5)` pushes past the effect). That is a deliberate lever
  for extreme work, not a defect.

## Worked graphs (to think with)

Erode only steep slopes:

```
fBm ──► Slope ──► Remap(threshold) ──┐
  └──────────────────────────────────┴─► Set Mask ──► Thermal
```

Erode everywhere *except* the peaks (note: just an inverted selection):

```
fBm ──► Remap(height→select high, inverted) ──┐
  └────────────────────────────────────────────┴─► Set Mask ──► Thermal
```

Blend two noises, but only in the valleys (the selection wires straight into Blend's
`mask` input — no Set Mask needed):

```
fBm A ───────────────────────► base ┐
fBm B ───────────────────────► overlay ┤► Blend (Normal)
fBm A ──► Slope ──► Curve(inv) ─────► mask ┘
```

Combine two selections (steep AND high):

```
fBm ──► Slope ───► Curve ──► base    ┐
fBm ──► Curve(height) ─────► overlay ┴─► Blend(multiply) ──► (a selection) ──► …
```

Reuse one selection, then its opposite, downstream:

```
… Slope ──► Remap ──► Set Mask ──► Thermal (steep areas)
              └─────► Remap(invert) ──► Set Mask ──► Combine (flat areas)
```

The point: every box except `Slope`/`Set Mask` is a general node, and the same
selection field is reusable, invertible, and combinable like any other data.

## Open decisions (the actual fork to settle)

1. **What layer does a selection travel on?** **Resolved: on `height`** (see the
   update). The entire field toolset works on a selection with zero new machinery.
   - *On a dedicated `mask`/`selection` layer:* rejected — it would make every field
     op need a "which layer" parameter (see #4).

2. **Implicit vs explicit masking — the visibility question.** **Resolved: explicit,
   with the layer as fallback** (see the update). Effects take an optional `mask`
   input (a wire you see), and read the `mask` layer only when it is unwired. Optional
   input ports made this possible without changing existing graphs' arity — the
   objection that previously pushed toward implicit-only.

3. **Does `Set Mask` set, or combine with an existing mask?** Options: replace,
   multiply, max, min, or a mode param. (Multiplying lets masks stack naturally.)

4. **Layer-targeting on field ops.** **Moot for now** — selections live on `height`
   (#1), so field ops need no "which layer" parameter. If a typed/dedicated layer is
   ever introduced, the best UX is a dropdown the GUI populates from the layers
   actually present on the node's input (it already evaluates for the preview).

5. **One mask or many?** The `mask` convention is a single layer. Multiple
   independent masks would need named layers + routing. Recommendation: keep a
   single `mask` slot; "stack" by combining selections *before* applying them.

6. **Do effects keep reading `mask`, or gain an explicit mask input?** **Resolved:
   both** (this is #2 for effects). An effect takes an optional `mask` input and falls
   back to reading the `mask` layer when it is unwired — an input port that, being
   optional, does *not* change existing graphs' arity.

## How this fits the existing design

- **Soft layer contract stays** for *effects*: they read `mask` via
  `layer_or(MASK, 1.0)` and apply everywhere when it is absent. This note only
  restructures the *creation* side.
- **Additive-node invariant holds:** every node here is still one file + one
  registration; nothing asks "which node is this?".
- **No over-generalization:** this stays inside "one 2D grid + named scalar
  layers." It does *not* introduce a points/primitives schema. More nodes is fine —
  that is the Unix-philosophy goal — as long as each is a small operator over the
  one `Field` type.

## Migration

1. Keep the current `Mask` node working (no break) during the migration. *(done, then
   retired once its replacements landed — see step 5.)*
2. Land `Curve`/`Invert` — the universal shaper; gives invert, threshold, band,
   levels on any field. *(done.)*
3. Optional input ports + an explicit `mask` input on effects. *(done — engine
   support, Blend's `mask` input, and thermal's `mask` input.)*
4. Add the selection sources, on the `height` layer. *(done — `Slope` (a steepness
   band in degrees) and `Height` (an elevation band). With the `mask` input a
   selection wires straight into an effect, so `Set Mask` is only needed for the
   layer-convention path.)*
5. Retire the bundled `Mask` node. *(done — `Slope`/`Height` for the criteria,
   `Curve`/`Invert` for shaping, and effect `mask` inputs for application cover
   everything it did.)*

## Decisions still open

- #3 (Set Mask combine behavior) — only if Set Mask is built; with explicit mask
  inputs it is no longer on the critical path.
- #5 (one mask vs many).
- Whether `Set Mask` is worth building at all now that effects take a mask input
  directly.
