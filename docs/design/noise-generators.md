# Design note: the noise generator family

The noise generators are the procedural *material* of a terrain graph: no-input
generators that fill the `height` layer with detail sampled per cell from world
coordinates. Where the Shape family draws deliberate envelopes (where a landform is), the
noise family supplies the texture multiplied or added inside it (what the ground looks
like). The two compose: an envelope times noise is a landform.

Today the family has two members, `generator.fbm` (rolling hills) and `generator.ridged`
(sharp mountain ridgelines). This note lays out the rest so they grow as one consistent
family rather than ad-hoc nodes, and so we can prioritize them against other work. Not
building today; this is documentation plus issues.

## Shared contract (inherited from fbm and ridged)

Every noise generator holds to the same conventions:

1. **Sampled, not iterated.** The value at a cell is a function of that cell's world
   coordinate, so it is resolution-independent (the same world position yields the same
   value at any resolution) and region-correct (a tile matches the untiled build).
2. **Output on `height` in roughly `[0, 1]`.** Working convention, not hard-clamped (per
   the height-range model); export and display auto-range.
3. **Deterministic seeding.** A node's seed is `ctx.seed` combined with its own `seed`
   param offset, never the clock or thread id, so the same graph and seed give
   byte-identical output regardless of thread count. Worley's feature-point hashing must
   hold the same line.
4. **A shared parameter vocabulary** where it applies: `frequency`, `octaves`,
   `lacunarity`, `gain`, `seed`. A new generator reuses these names so the graph reads
   consistently, adding only what is genuinely its own (Worley's `jitter`, a
   multifractal's `offset`).
5. **One generator, one file, one behavior.** fBm, ridged, billow, and the multifractals
   are genuinely different terrain characters (not buried params), so each is its own
   small node. They share the octave-summing math in `noise.rs`; only the per-octave
   combination differs.

## The multifractal family

The Musgrave multifractals are one octave loop with different per-octave combination, so
each is a small file over shared math. Distinguished by the terrain character they read
as:

- **fBm** (built) — rolling hills, the all-purpose base. Sum of octaves at falling
  amplitude.
- **Ridged** (built) — sharp mountain ridgelines and eroded crests. Folds each octave
  (`1 - |noise|`) and squares, so values pile toward ridges.
- **Billow** — puffy, rounded mounds and dunes: `|noise|` per octave, the rounded inverse
  of ridged (ridged points up, billow bulges). Cheap to add, a distinct look. Params: the
  shared vocabulary.
- **Hybrid / heterogeneous multifractal** — roughness scales with altitude, so valleys
  come out smooth and flat while peaks get rough and broken. The member that reads as
  realistic terrain on its own (plains-to-mountains) without hand-masking. Params: the
  shared vocabulary plus `offset` (the altitude bias that sets where roughness kicks in).

## The cellular / Worley family

Distance-to-scattered-feature-point noise: a completely different texture class from the
gradient-noise multifractals, and the family we have nothing like today. The plane is
tiled into cells, each with a jittered feature point; the value derives from the
distances to the nearest points (`F1` nearest, `F2` second nearest):

- **F1** (distance to the nearest point) — bumps, cones, rock piles, scales, blisters.
- **F2 − F1** (the gap between nearest and second) — cracks, dried mud, cell walls, rocky
  fracture networks. The high-value one for rock.
- **Regions** (the nearest point's cell id) — partitions the plane into discrete regions,
  feeding the control-field/region work ("treat each plate separately") and Scatter.

Params: `frequency` (cell size), `jitter` (how far feature points wander from cell
centers, 0 = a regular grid), `seed`, and the feature selector (see the open question).
Worley can also be octave-summed for fractal cellular detail, reusing `octaves` /
`lacunarity` / `gain`.

**Determinism and performance.** Each cell's feature point is a hash of its integer cell
coordinates and the seed, so placement is deterministic and rayon-safe. F1/F2 need only
the 3x3 neighborhood of cells around a sample, so cost is bounded per cell.

### Open question: one node or three?

F1, F2−F1, and regions share one genuinely expensive computation (finding the nearest
feature points); they differ only in which result is returned. This sits on the line
between "buried params" (which we avoid) and "intra-node configuration" (which is fine,
like fBm's octaves). The recommendation is **one `Cellular` node with a feature
parameter** (F1 / Edges / Regions), matching Substance and FastNoiseLite, because the
shared cost makes splitting wasteful and the feature is a configuration of the same
noise. The purist alternative is three nodes (Cellular Bumps / Cracks / Regions). This is
the one decision to settle before building; it is captured in the issue.

## Two one-offs, lower priority

- **Simplex basis (done, #100).** The gradient-noise basis in `noise.rs` is now a
  hand-rolled 2D simplex (triangular lattice, 12 evenly-spaced gradients), replacing the
  former square-lattice Perlin, so fBm, ridged, billow, and hybrid all lost the
  axis-aligned banding at once. Gradients are still hashed from lattice coordinates, so
  determinism is unchanged; only the noise goldens moved. (OpenSimplex2 would refine the
  remaining subtle simplex artifacts further, but classic 2D simplex already removes the
  axis bias and is simpler to keep byte-stable.)
- **Curl / flow noise.** Swirly, divergence-free vector fields, useful for flow and
  erosion direction rather than heightfields directly. Niche; revisit when erosion or
  directional warp needs it.

## Priority (to be slotted against other work)

By payoff: **Cellular/Worley** first (largest visual gap, unlocks a whole texture class
and the region mode for directability and Scatter; it is also the cellular half of #17,
whose ridged half is done) → **Billow** (cheap, distinct) → **Hybrid multifractal**
(realistic terrain in one node). **Simplex basis** and **Curl noise** are separate,
later tracks.
