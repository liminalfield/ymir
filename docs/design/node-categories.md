# Design note: the palette category taxonomy

Status: **decided and implemented.** The category set below is registered in
`crates/ymir-nodes/src/category.rs`; this note records why it is what it is, so
later additions follow the same rules instead of drifting.

Categories are presentation only. A node declares a `category` id in its
`NodeSpec`; `ymir-core` never reads it (it is not the engine's `NodeKind`, which
is derived from arity). The words are resolved downstream through `tr` from the
id, and a `CategoryDef` gives the id an icon and a sort order. Adding or renaming
a category is therefore cheap and entirely within `ymir-nodes` plus the GUI's
order test.

## The set

| id          | tab        | sort | line                                   |
|-------------|------------|------|----------------------------------------|
| `generator` | Generators | 0    | no input (falls out of arity)          |
| `selector`  | Selectors  | 10   | derives a `[0, 1]` field from terrain  |
| `adjust`    | Adjust     | 20   | pointwise, single input                |
| `combine`   | Combine    | 30   | pointwise, multi input                 |
| `geology`   | Geology    | 40   | natural processes, erosion included    |
| `output`    | Outputs    | 90   | no output (falls out of arity)         |

The sort numbers leave gaps so a deferred category can slot in without renumbering
its neighbours.

## The principles behind it

**Two categories are free.** In the arity model the engine already uses,
generators are exactly the no-input nodes and outputs the no-output nodes. Those
partitions exist whether or not they are named, so the only real taxonomy work is
subdividing the modifiers.

**One teachable line splits the modifiers: pointwise vs spatial.** A pointwise op
reads one cell and writes one cell (curve, invert, clamp, levels, blend two
fields). A spatial op reads a neighbourhood (blur, sharpen, terrace, warp). That
line tells a user exactly which tab to open, and it roughly tracks the engine's
own sampled-vs-iterative resolution distinction (spatial ops are the
resolution-dependent ones). `adjust` is the single-input pointwise bucket;
`combine` is the multi-input pointwise bucket; a future `filter` is the spatial
bucket.

**Build the category when its nodes exist, not before.** This is the same
premature-structure rule `CLAUDE.md` applies to code, applied to the palette. It
is why there is no thin one-node tab and no empty tab, and it applies at both ends
of the graph: a single noise generator lives in one `generator` tab (not a
`noise` sub-tab) until generators multiply, exactly as a single erosion node lives
in `geology` rather than its own `erosion` tab.

**Role nouns and domain nouns can mix, as long as each axis is parallel.**
Generators, Selectors, Adjust, Combine, Outputs are roles; Geology is a domain.
Filing erosion under a role like "Filters" would be consistent but useless to
someone hunting for erosion, so the hybrid is correct. Within an axis the names
stay parallel (the plural role nouns; "Geology" the noun, not "Geological" the
adjective).

## What was deliberately dropped

**No "Maths" tab.** Its contents split cleanly by arity into `adjust`
(single-input value shaping) and `combine` (multi-input arithmetic and blend), so
the academic label earns nothing.

**No "Masks" tab.** This is the consequential one, and it is a conscious deviation
from Houdini's heightfield shelf (which treats a mask as a semi-special layer with
dedicated nodes). In Ymir a mask is just a `Layer` in `[0, 1]`, so mask operations
sort by what they mechanically are, not by the layer they happen to touch:

- Mask **creators** (slope, height, curvature, occlusion, direction) derive a
  selection from the terrain. They are `selector`s. The current `Mask` node is one.
- Mask **editors** (blur, expand/contract, remap, invert) are not mask-specific. A
  blur is a blur whether it runs on `height` or on `mask`. They are general nodes
  that target a layer, living in `adjust` (pointwise) or a future `filter`
  (spatial), not mask-flavoured clones.

Keeping "Masks" as a tab would special-case a layer the engine deliberately
refuses to special-case, breaking the uniform-`Field` promise `CLAUDE.md` is built
on. See [the mask & selection model note](mask-and-selection-model.md) for the
creator/editor/attacher decomposition this rests on.

## A node-vs-param decision: Combine stays one node

The `combine` node keeps its operation (`add`, `subtract`, `multiply`, `min`,
`max`, `mix`) as a parameter rather than splitting into one node per op. This was
weighed against the node-readability principle and the principle does not apply
here: its target is a node that hides genuinely *different behaviours* behind a
mode (the canonical case is "an Invert node, not a remap with its range swapped",
where range-swap is a non-obvious way to express invert). The combine ops are
interchangeable variants of a single operation, binary field math: identical arity
(2 to 1), identical mask contract, identical structure, differing only in the
operator. That is genuine intra-node configuration, and one node with an operation
selector is the universal idiom (World Machine's Combiner, Houdini, Nuke's Merge).
The real cost, that the op is not visible in the wiring, is addressed by surfacing
the chosen op in the node's canvas title, not by N thin nodes. So `combine` is a
one-node tab for now and that is fine: it is the multi-input pointwise line, and it
grows by gaining ops and a blend weight (issues #53, #54), not by fragmenting.

## Deferred until populated

Documented here so they have an agreed home, but **not registered** until their
nodes exist:

- **`filter` (Filters)** — spatial/neighbourhood ops (blur, sharpen, terrace,
  warp). The spatial half of the pointwise/spatial line. Empty today.
- **Generator sub-tabs** (Noise, Shapes, Import) — split out of `generator` only
  when generators multiply. fBm keeps its `noise` *tag* so it is already findable
  and easy to promote.
- **Hydrology, Snow/Glacial** — own domain tabs when real. They are not geology, so
  they must not be crammed under it; the only umbrella wide enough for "everything
  natural" is "Nature", which is the World Machine term this taxonomy avoids on
  purpose. Keeping `geology` precise (solid-earth processes, erosion included) is
  what keeps the divergence real rather than cosmetic.

The tripwire that says "split now": a tab gets too crowded to scan at a glance, or
one kind of node clearly dominates the traffic into a shared tab.

## A follow-up this sets up

**A target-layer `ParamKind`.** This is the spec extension that lets mask
   editing be general layer-targeting nodes rather than mask clones, so "Masks"
   stays dissolved. The valid choices depend on which layers the upstream `Field`
   actually carries, so it is not a static enum; it needs the same layer resolver
   the 2D preview's layer picker already uses. It is a real `NodeSpec` extension to
   design, flagged here because choosing "general nodes target a layer" is what
   creates the need for it.
