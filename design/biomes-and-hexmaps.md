> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: biomes and hex-map export

Status: **direction, not yet built.** Captured from a design discussion so the
reasoning survives. This is an *orthogonal* output track to the realistic-heightmap
workflow: it reuses the maps Ymir already produces and adds new ways to export them.
It changes nothing upstream. Where it touches things already built (the `Field` type,
the [selection model](mask-and-selection-model.md), the export endpoints), it says so.

## The idea in one line

> The heightfield already knows where the slopes, valleys, and water are. Classify
> those into **biomes**, tessellate into **hexes**, and export a stylized map (a print,
> and a structured JSON) without changing how terrain is made.

## What this is for

The realistic export path (PNG / `.r16` / EXR) hands a heightmap to a game engine or
DCC. This is a different endpoint: a *stylized* map derived from the same terrain, of
the kind useful for tabletop and worldbuilding. The missing link between "heightfield"
and "hex map" is **biomes**: a per-cell classification of the terrain.

Primary deliverable, by current intent:

1. **A printable hex map** (the main want): a hex grid coloured by biome, suitable to
   print.
2. **A hex-map JSON** (the durable artifact): a header with scale/metadata plus a
   per-hex record (location, biome, derived data) for use outside Ymir.

Possible consumers of the JSON, in rough order of interest (none committed):

- **Print** (covered directly by the image export).
- **A Foundry VTT plugin** that ingests the JSON and builds the hex scene with snapping.
- **A Unreal Engine importer** that places the hex map in-engine.
- **A cartographic styling tool** that turns the plain hex grid into a beautified map
  (icons, borders, labels). This is a different domain (map-making, not terrain), and
  the JSON is the clean handoff to it, in Ymir later or as a separate tool.

The point of designing the JSON well now is that it is the seam every one of those
consumers reads. Get the contract right and each consumer becomes an independent,
deferrable decision.

## What must not change

This is held actively, per the project's scope discipline:

- **The heightfield workflow is untouched.** Everything here is additive: new nodes and
  a new layer kind. Existing graphs and exports behave identically.
- **The categorical layer is the *one* data-model addition.** Resist a general
  regions / zones / features schema. CLAUDE.md already anticipated exactly this: "if a
  typed or categorical layer is ever genuinely needed, that keeps it a contained change."
  Biomes are that need.
- **Hex is a projection at export, never an edge type.** The engine stays a 2D grid;
  "one type on every edge" (`Field`) holds. The hex lattice is computed inside the
  export endpoint and never flows on a wire.
- **Reuse, do not reinvent.** Classification reuses the selection nodes; painting reuses
  the planned drawable control shape (#81); the export reuses the endpoint model.

## The three additive pieces

```
   (existing graph)                 new
  height ──► selectors ─┐
  slope  ──► selectors ─┤
  flow   ──► selectors ─┼─► [Biome classify] ──► biome (categorical layer)
  paint (optional) ─────┘                              │
                                                       ▼
                              height + biome ──► [Export Hexmap] ──► hex.json
                                                                 └─► hex.png (print)
```

### 1. The categorical (biome) layer

Every layer today is a continuous scalar (`height`, `mask`, `flow`). A biome is the new
thing: a **discrete per-cell label** (cell to biome id). It rides on the same `Field`
and grid as the scalar layers, so the rest of the toolset is unaffected; only nodes that
specifically produce or consume biomes touch it.

Open implementation choice (decide when built): a dedicated categorical layer type
(`u16`/`u32` ids) alongside the scalar layers, accessed through `Layer`-style methods so
the rest of the codebase never indexes raw storage. A separate legend (id to name) lives
in the field's scalar globals or travels with the export. Encoding biome ids in a scalar
`f32` layer is rejected: it is a type lie that hashing, display, and export would each
have to special-case.

### 2. Biome classification

A **Biome** node is the discrete sibling of the selectors. It is the same operation as
the planned texturing classification (the in-app material preview direction, not yet
written up as its own note): where texturing blends *materials* for the 3D preview, the
Biome node picks one *label* per cell for export.

Inputs are the continuous fields the terrain already exposes, or trivially derives:

- **Height** (have it) and **slope** (Slope selector) directly.
- **Moisture** from flow accumulation and proximity to water (derivable from the Flow
  primitives).
- **Temperature**, if wanted, from height plus a latitude gradient (a Gradient generator).

The classifier is a **priority rule stack**, first match wins, mirroring how a selection
stack reads:

```
if slope > steep            -> rock
elif height < sea_level     -> water
elif moisture > marshy      -> marsh
elif height > snow_line     -> snow
else by (temperature, moisture) -> grassland | forest | tundra | desert | ...
```

This subsumes the classic Whittaker temperature-by-moisture biome lookup (which is just
a 2D version of the last rule) while staying readable from the graph. The rule set and
the legend (id to display name) are node configuration.

**Painting** is an optional categorical override input (the drawable control shape, #81):
where painted, the painted biome wins over the computed one. Derived-first is the default
emphasis (fully procedural, regenerates with the terrain); painting is the manual touch-up
on top, not a parallel system.

### 3. Hex export

The hex map is produced *at export*, by an endpoint that consumes a `Field` carrying at
least a `biome` and a `height` layer. It does not introduce a hex type on any edge.

Lattice parameters (node config):

- **Orientation**: pointy-top or flat-top.
- **Size**: hexes across the width, or hex edge length in world units (resolution- and
  region-independent, like other world-unit params).
- Rows follow from the size and the field's aspect.

Per-hex **aggregation** of the cells whose centres fall within the hex:

- **biome**: the dominant label (statistical mode), tie-broken by lowest id so it is
  deterministic.
- **elevation**: mean (and optionally min/max) of `height`, scaled by `world_height`
  for absolute metres exactly as the EXR export does.
- Optional extras: water fraction, mean slope, anything already on a layer.

Determinism is required (same machine, same input to same output): aggregation iterates
cells in a fixed order and resolves ties by id, never by hash-map order.

Output forms:

- **JSON** (below): the structured contract.
- **A printable hex image**: the hex grid filled per the biome legend, optionally with
  light elevation shading and coordinate labels. A plain, correct render first;
  cartographic beautification is explicitly out of scope for the first cut and is the
  natural seam for a later styling pass or separate tool.

Node shape (decide when built): either one `Export Hexmap` endpoint that aggregates once
and writes both files (paths as params), or two endpoints (`Export Hex JSON`,
`Export Hex Image`) for graph legibility at the cost of aggregating twice. The aggregation
must live in the endpoint regardless, to keep the hex lattice off the edges.

## The hex JSON (first cut)

A header describing the lattice and scale, a legend, then the hex records. Axial
coordinates `(q, r)` are the primary hex coordinate system; offset `(col, row)` is
included as a convenience for grid/print consumers.

```json
{
  "format": "ymir-hexmap",
  "version": 1,
  "world": {
    "extent_m": 8000.0,
    "height_m": 2500.0,
    "origin": [0.0, 0.0]
  },
  "hex": {
    "orientation": "pointy",
    "size_m": 250.0,
    "columns": 32,
    "rows": 28,
    "coordinates": "axial"
  },
  "legend": [
    { "id": 0, "name": "water" },
    { "id": 1, "name": "grassland" },
    { "id": 2, "name": "forest" },
    { "id": 3, "name": "rock" },
    { "id": 4, "name": "snow" }
  ],
  "hexes": [
    { "q": 0, "r": 0, "col": 0, "row": 0, "biome": 1, "elevation_m": 120.4 },
    { "q": 1, "r": 0, "col": 1, "row": 0, "biome": 2, "elevation_m": 180.9 }
  ]
}
```

The schema carries a `format` tag and a `version` so consumers can evolve safely, the
same stance as project files. Extra per-hex fields (slope, water fraction) are additive
and optional.

## Phasing

Each phase is independently useful and stops cleanly.

0. **This note + tracking issues.** No code.
1. **Categorical layer + a Biome classify node** (rule stack over existing selectors,
   no painting yet). Visible as a coloured preview. This is the real new machinery.
2. **Hex JSON export.** The durable artifact; unblocks any external consumer.
3. **Printable hex image export.** The main want.
4. **Painting override** (rides #81) and richer classification inputs (moisture,
   temperature) as they prove needed.
5. **Consumers, if and when wanted:** a Foundry VTT plugin, a UE5 importer, a styling
   pass. All read the JSON; all deferrable.

## Relation to existing design

- **[Selection & mask model](mask-and-selection-model.md):** biome classification is a
  selection stack with a discrete output. Same inputs, same readability.
- **Texturing direction** (Texture nodes = material + selection): biomes are its discrete
  sibling; building one informs the other.
- **[Control fields](control-fields-and-directability.md) and #81 (drawable control
  shape):** painting biomes reuses that drawable system.
- **Export endpoints** (PNG / `.r16` / EXR): the hex exports are more endpoints in the
  same family; the metres scaling reuses `world_height` exactly as EXR does.
- **CLAUDE.md categorical-layer seam:** this is the anticipated genuine need that makes
  adding a categorical layer a contained change rather than a rewrite.
