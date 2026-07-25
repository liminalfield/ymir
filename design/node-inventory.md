> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Ymir node inventory

A snapshot of every node Ymir has today, plus every node that has been discussed or
designed but not yet built. Written to seed roadmap planning for the next set of
nodes. Generated 2026-07-14 from the source (`crates/ymir-nodes/src/`) and the design
docs (`design/`).

## The model these nodes live in

- **One type on every edge: `Field`.** A grid of named scalar `layers` (`height` by
  convention primary; also `mask`, `flow`, `water`, `sediment`, `wear`, `deposition`,
  `flow_x`/`flow_y`, and any custom name) plus a small `detail` map of scalar globals
  (seed, world bounds, vertical scale).
- **Node kind is derived from arity**, never hard-coded: no inputs = generator, no
  outputs = endpoint, both = modifier.
- **Soft layer contracts.** A node declares the layers it would like, degrades
  gracefully when one is absent (`field.layer_or(MASK, 1.0)`), and never gates a
  connection. Height is nominally `[0, 1]` but not clamped; range is preserved and
  mapped at export.
- **Height values carry the full float range through the graph.** Modifiers do not
  clamp; range is auto-mapped at display and export.
- **New node = one file + one `inventory::submit!`.** Nothing in the app asks "which
  node is this?"; the palette, param UI, and save/load are all registry-driven.

---

# Part 1 — Implemented (35 registered operators)

32 concrete nodes in `ymir-nodes` + 3 subgraph boundary operators in `ymir-core`.
Pinned by `crates/ymir-nodes/tests/registry_smoke.rs`.

## Generators (15) — no inputs, write `height`

All generators are per-cell pure functions of world coordinates (resolution- and
region-independent, byte-identical across thread counts). All write the `height`
layer. Shape generators take a `center_x`/`center_y` in `[0,1]` and lengths in world
meters.

### Noise family (8)

| Node | `type_id` | Key params | Notes |
|---|---|---|---|
| **fBm Noise** | `generator.fbm` | frequency (log 0.25–64, def 2), octaves (1–12, def 5), lacunarity, gain, amplitude (0–4), bias (−1–1), seed, offset_x/y | The base rolling-hills source. Only generator with amplitude/bias built in. |
| **Billow Noise** | `generator.billow` | frequency, octaves, lacunarity, gain, seed, offset_x/y | `2|n|−1` fold → puffy rounded mounds, creased valleys. |
| **Ridged Noise** | `generator.ridged` | frequency, octaves, lacunarity, gain, seed, offset_x/y | Ridge fold → sharp crests, carved valleys. Mountain ridgelines. |
| **Hybrid Multifractal** | `generator.hybrid` | frequency, octaves, lacunarity, gain, bias (0–2, def 0.7), seed, offset_x/y | Musgrave: highlands rough, lowlands smooth from one node. |
| **Flow Noise** | `generator.flow` | frequency, octaves, lacunarity, gain, strength (0–4, def 0.4), seed, offset_x/y | Curl-warped noise → marbled/swirled strata. **Also emits `flow_x`/`flow_y`** direction field. |
| **Cellular Bumps** | `generator.cellular_bumps` | frequency (def 8), jitter (0–1, def 1), seed, offset_x/y | Worley `1−F1` → cone peaks. Rock piles, scales, blisters. |
| **Cellular Cracks** | `generator.cellular_cracks` | frequency, jitter, seed, offset_x/y | Worley `1−(F2−F1)` → crack/fracture networks, dried mud. |
| **Cellular Regions** | `generator.cellular_regions` | frequency, jitter, seed, offset_x/y | Worley cell ids → flat discrete regions/plates/zones. |

### Shape family (7) — control envelopes and footprints

| Node | `type_id` | Key params | Notes |
|---|---|---|---|
| **Radial Gradient** | `generator.radial` | radius (m, def 500), center_x/y | Smoothstep dome: 1 at center → 0 at radius. |
| **Radial Falloff** | `generator.falloff` | radius (m, def 500), center_x/y | **Linear** radial fraction 0→1. Feed a Curve to draw any radial cross-section (dome, crater, caldera, ring, terraces). The landform workhorse. |
| **Gradient** | `generator.gradient` | angle (deg), width (m, def 1024), center_x/y | Directional smoothstep ramp. Coast-to-highland trend. |
| **Ring** | `generator.ring` | radius (m, def 300), width (m, def 100), center_x/y | Smoothstep annulus. Crater rim, caldera wall, atoll. |
| **Rectangle** | `generator.rect` | extent_x/y (m), falloff (m), rotation (deg), center_x/y | Rounded box SDF. Plateau/mesa/table footprint. |
| **Polygon** | `generator.polygon` | radius (m, def 400), sides (3–12, def 6), falloff (m), rotation (deg), center_x/y | Regular n-gon. Angular/faceted plateau, hex base. |

### Source (1)

| Node | `type_id` | Key params | Notes |
|---|---|---|---|
| **Import** | `generator.import` | path (PNG), offset_x/y, rotation, scale (0.05–8), edge {extend, zero, wrap} | Decodes+resamples an external PNG heightmap with placement folded into one resample. PNG only; determinism per-file. |

## Modifiers (14) — one or more inputs

Order below groups by role. "Mask-aware" = composites its effect over the original
through the input's `mask` layer (or an explicit optional `mask` input), so the effect
localizes and the node is insertable anywhere.

### Compositing & domain (4)

| Node | `type_id` | Inputs | Key params | Notes |
|---|---|---|---|---|
| **Blend** | `modifier.blend` | base, overlay, *mask?* | mode {normal, add, subtract, multiply, max, min, difference}, opacity | Photoshop-style layer compositing, `base + (effect−base)·(opacity·mask)`. |
| **Warp** | `modifier.warp` | in | amount (m, def 50), frequency, octaves, seed | fBm domain warp. Breaks the machine look on regular features. Adds no relief. |
| **Blur** | `modifier.blur` | in | radius (m, def 8) | 3-box Gaussian. Mask-aware. Also sets the measurement scale for Slope/Curvature. |
| **Null** | `modifier.null` | in | — | Byte-exact pass-through. Reroute/anchor, or a tap to view a byproduct layer. |

### Shapers — adjust the height transfer (4)

| Node | `type_id` | Inputs | Key params | Notes |
|---|---|---|---|---|
| **Curve** | `modifier.curve` | in | curve (editable transfer curve) | Freeform elevation-profile remap; extrapolates past `[0,1]`. Mask-aware. |
| **Levels** | `modifier.levels` | in | in_low, in_high, gamma, out_low, out_high | Range rescale + gamma. Normalize out-of-range height back to `[0,1]`. Mask-aware. |
| **Invert** | `modifier.invert` | in | — | `1−height`. Peaks↔valleys, or invert a selection. Mask-aware. |
| **Expression** | `modifier.expression` | *in?* | expr (text formula) | Per-cell arithmetic VM over `x,y` + every input layer by name. Escape hatch. Runs as a generator when unwired. |

### Selectors — derive a `[0,1]` selection onto `height` (3)

These read terrain and *write a selection* (not terrain); feed the result into another
effect's mask. Not mask-aware (they build a selection from scratch).

| Node | `type_id` | Inputs | Key params | Notes |
|---|---|---|---|---|
| **Height** | `modifier.height` | in | min, max, falloff | Selects an elevation band (snow line, coast). |
| **Slope** | `modifier.slope` | in | min/max/falloff (degrees) | Selects a steepness band (cliffs vs gentle ground). Resolution-stable angle. |
| **Curvature** | `modifier.curvature` | in | mode {convex, concave}, strength | Selects ridges/outcrops or valleys/hollows. Self-calibrating (RMS-normalized). |

### Erosion / geology (3) — multi-output physical models

Each is a **complete, self-contained model** (not a fragment needing hand-wiring), reads
an optional `mask`, and emits its extra fields as separate outputs carrying data on
`height`.

| Node | `type_id` | Inputs | Outputs | Key params |
|---|---|---|---|---|
| **Thermal Erosion** | `modifier.thermal_erosion` | in, *mask?* | **heightfield**, **wear**, **debris** | talus (deg, def 35), strength, iterations (res-scaled) |
| **Hydraulic Erosion** | `modifier.hydraulic_erosion` | in, *mask?* | **heightfield**, **wear**, **deposition**, **flow** | density, inertia, capacity, erosion, deposition, evaporation, radius |
| **Stream Erosion** | `modifier.stream_erosion` | in, *mask?* | **heightfield**, **flow**, **wear**, **deposition** | strength (K), diffusion, iterations, concavity (m), concentration, fill |

- **Thermal** — talus/scree relaxation: material past the talus angle slides downhill.
  Straight scree slopes, softened ridges. `debris` is settled talus (kept distinct from
  fluvial deposition).
- **Hydraulic** — droplet sim (Beyer/Lague): rain droplets erode under capacity, deposit
  over it. Rills, gullies, sediment hollows. `flow` = droplet visitation density.
- **Stream** — stream-power fluvial (FastScape/Braun-Willett): fills depressions, routes
  multi-flow-direction drainage, incises `E=K·A^m·S`. Connected dendritic river
  networks and valleys. `flow` = drainage accumulation (the river map). **The intended
  differentiator vs Gaea/World Machine.**

## Endpoints (3) — one input, no output

| Node | `type_id` | Key params | Notes |
|---|---|---|---|
| **Export PNG** | `endpoint.export` | path, build, auto_range | 16-bit grayscale PNG. auto_range maps actual range → full 16-bit. |
| **Export R16** | `endpoint.export_r16` | path, build, auto_range | Raw 16-bit LE `.r16` (Unreal landscape). |
| **Export EXR** | `endpoint.export_exr` | path, build, height_units {normalized, meters} | 32-bit float lossless. `meters` bakes absolute elevation via world_height. |

## Infrastructure (3) — subgraph boundary operators (in `ymir-core`)

| Operator | `type_id` | Role |
|---|---|---|
| **Subgraph** | `subgraph` | Container node: contains a graph you dive into; ports derived from inner Input/Output nodes. Carries its own baked seed. Delivery mechanism for landform presets. |
| **Subgraph Input** | `subgraph.input` | Boundary node defining a subgraph input port. |
| **Subgraph Output** | `subgraph.output` | Boundary node defining a subgraph output port. |

The mechanism (#79) is built and registered. Outstanding subgraph work (#106) is the
library/catalogue and inspector UX, not the operators.

---

# Part 2 — Discussed but not implemented

Grouped by theme, with a rough status. "Designed" = concrete direction settled in a
doc; "proposed" = floated, not worked out; "speculative" = research-gated.

## Control fields / directability

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Facing-direction (Aspect) selector** (#68) | designed | selector | The missing third of "the local three" (slope, curvature already built). Outputs `cos(aspect − target)` for a chosen direction (sun/wind/rain-shadow). Deliberately not raw azimuth. Undefined on flats → must be slope-weighted. |
| **Terrace** | designed | filter | Quantise-with-smoothing → strata and benches. A big-form structural move; keystone of the deferred "filter" category. (Distinct from terraced *landforms*, which are subgraphs.) |
| **Histogram-Scan / range** | designed | adjust | Threshold-to-mask with controlled falloff; sharpens a vague gradient into a crisp region. Possible overlap with Levels — open question. |
| **Spline / path guide** | proposed | generator | Control points (in *params*, not on an edge) → distance-from-path falloff. Ranges, rivers, faults as linear features. Stays inside the "no points schema" rule. |
| **Paint / drawable control** (#81) | designed, deferred | generator/input | Hand-painted `[0,1]` control field: "mountains here because I said so." Tension with fixed-res/seed-reshuffle/stored-data values; kept behind generate-and-derive. Dependency for biome/hex painting. |
| **Slope Blur** | proposed | filter | Blur steered by a guide's slope; cheap directional-weathering proxy. Substance-inspired. |
| **Standalone Flow-accumulation selector** | off the main path | selector | Upstream catchment area for un-eroded terrain. Global, order-dependent, res-dependent — deprioritized; prefer reading erosion's emitted `flow`/`water`. |

## Scatter / placement

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Scatter** | designed (not scheduled) | modifier | Instances a `stamp` field across the terrain (craters/boulders/dunes/trees); optional `density` and `size` control-field inputs. Two tiers: cheap transform variation vs re-evaluating the stamp *subgraph* per instance. Depends on subgraphs. Open: baked vs procedural, overlap compositing, Poisson-disk vs stochastic. |

## Erosion / geology

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Coastal shaper** | built (lean v0) as `modifier.coastal` | modifier | *Geometric* (not simulated) beach-and-bluff reshaping keyed to a water level + local slope. The shipped node is the lean artist-first bevel (sea level is a World setting, no Sea node or exposure). The fuller exposure-driven model (Sea node, wave exposure, eikonal distance) is a separate, unbuilt plan in `coastal-erosion.md`. |
| **Strata / bedrock hardness field** | designed, highest value | input layer/field | Depth-varying erodibility consumed by every erosion model via `layer_or(RESISTANCE, 1.0)`. Exposes rock structure: mesas, buttes, hoodoos, cliff bands. Framed as a layer/hook more than a standalone node. |
| **Precipitation** | designed (later) | generator/modifier | Writes the `water` layer with orographic / rain-shadow support; makes rainfall a directable input. The erosion water-input seam already exists. |
| **LookDev / fake-erosion** | proposed | filter/geology | Slope-aware diffusion + procedural "eroded look" as a fast preview tier (HighMap/Gaea style). Overlaps Slope Blur. |
| **Glacial, Aeolian/dunes, Meandering rivers, Rivers, Snow, ML amplification** | speculative | geology | Far-future models, prioritized by research maturity, not name. Open scope question whether glacial/aeolian matter at all. Physical wave erosion, standalone deltas/fans, debris-flow authoring: research, not scheduled. |

Note: **Deposition/alluvium is explicitly NOT a separate node** in the current brief — it
is a native output of the transport-capacity erosion models (already emitted as
`deposition`).

## Biomes / hex (orthogonal output track — all designed, none built)

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Biome classify** | designed (core new machinery) | selector | Classifies terrain into biomes via a priority rule stack; writes a new **categorical `biome` layer**. Requires the one data-model addition (a categorical layer). Discrete sibling of texturing. |
| **Export Hexmap** | designed | endpoint | Aggregates cells per hex (dominant biome, mean elevation) → versioned hex JSON + printable PNG. Lattice computed in the endpoint, never on an edge. Open: one endpoint vs split JSON/image. |

## Texturing

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Texture / TextureSet** | proposed (least specified) | material | Blends *materials* by selection for an in-app 3D material preview; continuous counterpart of Biome classify. Consumes canonical erosion byproduct layers (`wear`, `slope`, `flow`). No dedicated design note yet. |

## Masking / selection & utility

| Proposed node | Status | Category | What it would do |
|---|---|---|---|
| **Set Mask** | designed, necessity questioned | modifier | Attaches a selection onto a field's `mask` layer. Now that effects take an explicit optional `mask` input, whether this is worth building is itself an open question. |
| **Resampling Transform** | designed, deprioritized | modifier | Rotate/scale/translate an already-baked raster. Lossy. "Transform the function, not the grid" — placement belongs at generation, orientation belongs in the engine. Back-pocket only. Higher-priority transform work is *params* (offset on cellular nodes, etc.), not a node. |

## Infrastructure

| Proposed work | Status | What it is |
|---|---|---|
| **Subgraph library / catalogue** (#106) | designed, not built | Library + inspector UX for shipping landform presets as template-instantiated subgraphs. The operators (#79) already exist; this is the delivery/authoring layer. |

---

# Part 3 — Firm "NOT a node" decisions

These bound the roadmap; a proposal that reduces to one of these should be reframed.

- **Named landforms are not nodes.** Crater, caldera, volcano, dome, terraces, dune
  field = built from primitives (canonically **Radial Falloff → Curve**, the curve being
  the swept cross-section) and shipped as example **subgraphs/presets**. A dedicated
  crater/mountain node is the forbidden mega-node.
- **No "Mask" node / no mask-flavoured variants.** Masks are ordinary `[0,1]` fields;
  mask *creators* are selectors, mask *editors* are ordinary adjust/filter nodes. No
  "Masks" tab.
- **No points/primitives schema.** "Many features" is field-native (Voronoi regions,
  envelope-modulated generators). Splines live in *params*, never on edges. Scatter
  instances fields, not point primitives.
- **No general per-node GPU.** GPU is scoped to erosion (one stencil-shaped operator),
  not "every node becomes a compute shader."
- **Deposition is not its own node** — a native erosion output.
- **Standalone flow accumulation is off the main path** — read erosion's `flow`/`water`.
</content>
</invoke>
