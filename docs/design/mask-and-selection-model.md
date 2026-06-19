# Design note: the mask & selection model

Status: **draft, for discussion.** Nothing here is built yet. The current `Mask`
node ships as-is until this is settled.

This note captures a decomposition that came out of building the mask node: how
masks should be *created, manipulated, and applied* so the node graph stays
composable — the Unix philosophy (small tools that each do one thing, piped
together) applied to terrain.

## The idea in one line

> A selection is just a grayscale field. Make it with normal nodes, shape it with
> normal nodes, and use one **Set Mask** node to attach it to the terrain. Effects
> read the mask by the existing convention.

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
   - `Remap`/`Curve` (#15) — threshold, band, smoothstep, **invert**, levels.
   - `Combine` (built) — blend two selections (add / multiply / min / max / mix).
   - i.e. you already get invert, combine, and shaping *for free*; they are not
     mask features, they are field features.

3. **Set Mask** — the *only* mask-aware bridge. Two inputs: the **terrain** and a
   **selection**; it writes the selection onto the terrain's `mask` layer and
   passes the terrain's other layers through. This is the single place the `mask`
   convention is established.

4. **Effects** — `Thermal`, `Combine`, future erosion — read `mask` by the soft
   contract and blend their effect by it. Unchanged.

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

Blend two noises, but only in the valleys:

```
fBm A ─────────────────────────┐
fBm B ─────────────────────────┤
fBm A ──► Slope ──► Remap(inv) ─┴─(as mask)─► Combine(mix)
```

Combine two selections (steep AND high):

```
fBm ──► Slope ───► Remap ──┐
fBm ──► Remap(height) ─────┴─► Combine(multiply) ──► (a selection) ──► Set Mask ──► …
```

Reuse one selection, then its opposite, downstream:

```
… Slope ──► Remap ──► Set Mask ──► Thermal (steep areas)
              └─────► Remap(invert) ──► Set Mask ──► Combine (flat areas)
```

The point: every box except `Slope`/`Set Mask` is a general node, and the same
selection field is reusable, invertible, and combinable like any other data.

## Open decisions (the actual fork to settle)

1. **What layer does a selection travel on?**
   - *On `height` (recommended):* a selection is a grayscale field, so the entire
     height-op toolset works on it with zero new machinery; `Set Mask` is the only
     bridge. Slight oddity: a "selection" field's `height` is not terrain.
   - *On a dedicated `mask`/`selection` layer:* more explicit, but then every field
     op needs a "which layer" parameter to touch it (see #4).

2. **Implicit vs explicit masking — the visibility question.**
   - *Implicit (today):* the `mask` layer rides along on the terrain field; you
     cannot *see* on the canvas that a mask is present. Simple, matches the soft
     contract.
   - *Explicit:* `Set Mask` makes attaching a mask a visible node, and you could
     even give effects a real **mask input port** (two-input `Thermal`) so the mask
     is a wire you see. More Houdini, more obvious — but a departure from "effects
     read the `mask` layer." `Set Mask` is the middle ground: explicit *creation*,
     implicit *consumption*.

3. **Does `Set Mask` set, or combine with an existing mask?** Options: replace,
   multiply, max, min, or a mode param. (Multiplying lets masks stack naturally.)

4. **Layer-targeting on field ops.** If selections do *not* live on `height`, then
   `Remap`/`Combine`/etc. need a "which layer" parameter. Best UX: the GUI populates
   that dropdown from the layers actually present on the node's input (it already
   evaluates for the preview), so you literally see `height, mask, flow` and pick.

5. **One mask or many?** The `mask` convention is a single layer. Multiple
   independent masks would need named layers + routing. Recommendation: keep a
   single `mask` slot; "stack" by combining selections *before* `Set Mask`.

6. **Do effects keep reading `mask`, or gain an explicit mask input?** (#2 above,
   restated for effects specifically.) Reading the layer keeps the soft contract;
   an input port is more visible but changes every effect's arity.

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

1. Keep the current `Mask` node working (no break).
2. Land `Remap`/`Curve` (#15) — the universal shaper; it already gives invert,
   threshold, band, levels on any field.
3. Add `Slope` (selection source) and `Set Mask` (the bridge).
4. Once those exist, the bundled `Mask` node is just `Slope`/`Remap` + `Set Mask`;
   decide whether to keep it as a convenience macro or retire it.

## Decisions still needed before building

- Resolve open decisions 1, 2, and 3 (selection layer, visibility model, Set Mask
  combine behavior) — these shape `Set Mask` and #15.
- The rest (4–6) can follow once the spine is chosen.
