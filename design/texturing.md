> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: materials and the texture preview

Status: design only, not yet built. Captured from a design discussion (2026-07-26) that
settled the shape of the feature and superseded the earlier Texture / TextureSet sketch
recorded in [`node-inventory.md`](node-inventory.md).

## The idea in one line

> Bind a flat colour to a selection as a named **material weight layer**, composite those
> weights onto the terrain in the viewport, and export the same weights as the material
> distribution maps an engine consumes.

## What this is for

Two needs, served by one source so they cannot disagree:

1. **Judge material layout inside Ymir.** Is the snow line where it should be, is rock
   exposed where the slope earns it, does the beach band read at the coast. Today that
   judgement needs a round trip to Unreal.
2. **Export the distribution** (#78): per-material weight maps plus a manifest naming them,
   for a landscape material in an engine.

The preview and the export read the same layers, so what you see in the viewport is what
the exporter writes. That property is the point of the design, and most of the decisions
below exist to protect it.

## What must not change

- **Ymir owns where, an engine owns what.** A material here is a *placement*, carrying a
  name and a preview colour. Albedo, roughness, and the rest stay in the engine's material
  library. This is the boundary [`docs/concepts/macro-form.md`](../docs/concepts/macro-form.md)
  states, and this feature keeps it.
- **No new element schema.** Materials are ordinary named scalar layers on the existing
  `Field`. No points, no primitives, no regions schema.
- **No core data-model addition.** Weights are layers, which `Field` already holds in an
  arbitrary named map; colours are scalars, which `detail` already holds.
- **Additive only.** One new `ParamKind`, one new node, one new palette category, and an
  export endpoint later. Existing graphs and exports behave identically.

## The shape

### Materials are named weight layers

Each material owns one `[0, 1]` layer named `material.<name>`, holding its weight per cell.
`Field` already stores layers in a canonically ordered named map and hashes them in sorted
order, so nothing about hashing, caching, or pass-through needs to change. A weight layer is
what a weight map already is, so the export is a write of data that already exists.

### One Material node per material

Input 0 is the field. Input 1 is an optional mask, which is the selection deciding where the
material goes. An unwired mask means the material applies everywhere, which is how a base
material is expressed: a Material node with nothing in its mask input. The node writes its
own weight layer, composites it against the material layers already present, and passes
everything else through untouched.

Parameters: the material `name`, its preview colour, and (open, see below) a strength.

### Composition is the chain

Order is wiring order. A graph reads:

```
terrain ─► Material "rock" ─► Material "grass" ◄─ slope selection
                                   └─► Material "snow" ◄─ height selection
```

The stack is legible from the node structure, which is the property CLAUDE.md's node
philosophy asks for. Compositing happens as each node evaluates, so nothing downstream needs
to know the order: it sees finished weights.

### The colour travels on the field

An endpoint receives `Field`s and never its upstream nodes' parameters, so an export node
cannot read colours off the Material nodes feeding it. For the preview and the exported
manifest to agree, the colour has to ride the field. It goes in the existing `detail` map as
three scalars per material (`material.<name>.r`, `.g`, `.b`), which is honest scalar data,
folds into `Field::content_hash` as it stands, and needs no format work.

Two alternatives were considered and rejected:

- **Colour as GUI view-state**, assigned per material name from a palette. Free, but a
  headless render could not reproduce the colours a project was authored with.
- **String-valued globals on `Field`**, alongside the `f64` `detail` map. Cleaner to express
  and the natural home for a future biome legend, but it is machinery for a feature that
  does not exist yet. If the biome legend or a richer material reference lands, this is the
  addition to make then, and it would serve both.

### Identity is the name, appearance is the colour

The `name` is the durable currency: it names the weight layer, the exported weight map, and
the engine's landscape layer. The colour is a preview convenience and never leaves the
preview. Keeping the two separate costs nothing now and means a change of look never
invalidates an export contract.

## Why not Texture and TextureSet

The earlier sketch paired a **Texture** endpoint (a material bound to a selection, no
outputs) with a **TextureSet**, an ordered composition living on the canvas as a GUI-only
presentation object referencing Texture nodes by `stable_id`. It was superseded for four
reasons, recorded here so the reasoning is not relitigated:

1. **The GUI object was a workaround.** Making Texture an endpoint meant nothing could
   consume it, so the ordered composition could not be a node, so it became a canvas object,
   so the canvas had to grow node-like objects with no engine node behind them. Every step
   followed from one arity choice.
2. **It broke headless.** A composition in the GUI view layer cannot drive a `ymir-cli`
   export, so the splat endpoint would have needed its own parallel composition, and the two
   would drift.
3. **It duplicated existing mechanics.** "Which set is active" is the preview pin, already
   built and already the Houdini display flag.
4. **The name fought the documentation.** `macro-form.md` exists to say Ymir does not do
   surface texture. A headline node called Texture invites the confusion that page prevents.

PBR materials were considered as a later evolution of the appearance slot and then dropped
from scope. If they return, four constraints keep them additive, and this design already
meets all four: identity is a name separate from appearance; weights are ordinary
normalizable layers, which is exactly what a splat shader consumes; the viewport composites
weights against per-material appearance supplied as uniform data; and the manifest carries
ids with appearance alongside. Nothing here needs undoing to grow into it.

## Sets, reuse, and A/B

A material set is a **subgraph**: terrain in, terrain plus material weights out, every
selection internal. On the canvas it is one node. Saved to the subgraph library (#106) it is
a standalone, shareable file that drops into any project, which is the reusable named set the
TextureSet concept was reaching for, built from machinery that already exists.

A/B works through the mechanics the editor already has:

- **Two sets in parallel** off the same terrain. Click, or pin, to flip which one the
  viewport shows. The terrain is shared and cached, so only the material subgraph
  re-evaluates and the flip is cheap.
- **The pin** keeps a material result on screen while you select and edit upstream terrain
  nodes, so you tune erosion while watching the textured result.
- **Bypass** on a material subgraph gives the untextured comparison in one toggle.
- **Which set exports** is decided by wiring: the export endpoint is connected to one branch.
  A general Switch node (N inputs, an index parameter) would flip preview and export
  together, and is worth having for terrain variants generally. It is filed separately and is
  not part of this feature.

Two material sets meeting on one field is defined behaviour: chained, the second composites
over the first under the same rule the stack uses. Sets are kept in parallel branches when
they are meant to be alternatives.

## Decisions settled

1. Materials are named weight layers, composed by the node chain. No TextureSet object, no
   canvas presentation nodes.
2. The colour is a node parameter and rides the field's `detail` map.
3. The material's name is its identity; the colour is preview only.
4. A material set is a subgraph; A/B is parallel branches plus the preview pin.
5. Which set exports is a rewire, for now.
6. PBR is out of scope.

## Open decisions

1. **Composite semantics.** Whether a Material node takes weight from those below it (an
   over composite) and whether the export normalizes weights to sum to one. The preview and
   the export must use the same rule, so this is settled once and applied in both.
2. **How many materials the shader composites**, and what happens past that count.
3. **Export form.** N single-channel weight maps plus a manifest, or channels packed into
   RGBA. #78 wants both eventually; one of them is first.
4. **The default palette.** New Material nodes should be assigned distinct colours that are
   safe under red/green colour vision, so a default set is never a red-versus-green pair.
5. **Presentation.** How the composite reads against relief shading (colour multiplied into
   the existing hillshade, so slope stays legible), how it is turned on in the viewport, and
   whether the 2D map shows the same composite.
6. **Node presentation.** What a Material node's body thumbnail shows.
7. **Layer naming.** Whether the `material.` prefix is a reserved constant in `layers` and
   how a name with unusual characters is handled.

## Phasing

Each step is a reviewable commit that leaves the tree runnable.

0. **This note and its issues.** No code.
1. **`ParamKind::Color`** with `ParamValue::Color`, canonical equality and `hash_into`, serde,
   and a colour picker in the inspector. Self-contained, and the only core change.
2. **The Material node**: its `NodeSpec`, the composite, the `material` palette category, and
   its tests. Visible through the existing preview as a weight layer.
3. **The viewport composite**: weights times colours multiplied into the shading, in the 3D
   terrain shader and the 2D map shader.
4. **The export endpoint** (#78): weight maps plus the manifest.

Two optional additions, sequenced by whether they prove needed: a materials list in the
inspector that gathers a chain's materials with their swatches, so colours are tuned in one
place; and a ramp overlay that colourizes any single tapped output, which is the
one-material case of the same shader path.

## Relation to existing design

- **[Selection and mask model](mask-and-selection-model.md)** and
  **[control fields](control-fields-and-directability.md)**: the selectors that place
  materials are the same ones that steer erosion. This feature adds no selection machinery.
- **[Erosion roadmap](erosion-roadmap.md)**: the byproduct layers (`wear`, `deposition`,
  `flow`, `debris`) are the inputs this consumes, and they exist. Heed its caution that wear
  and slope texture convincingly while raw flow does not.
- **[Biomes and hex maps](biomes-and-hexmaps.md)**: the discrete sibling. Biome
  classification is an argmax over these same weights, and the legend question it leaves
  open is the same one deferred here.
- **[Subgraphs](subgraphs.md)** and the library (#106): where a material set lives.
- **[Node inventory](node-inventory.md)**: the Texturing row describes the superseded
  Texture / TextureSet shape and should point here.
- **#78** (splat export) is step 4 of this note. **#118** (HDRI and IBL) was gated on
  materials for specular reflection; with PBR out of scope it is an independent visual
  upgrade again.
