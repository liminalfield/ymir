# Design note: transform and placement (and why there is almost no Transform node)

Status: design only, not scheduled. This captures a long design conversation so the
conclusion is on record and the resampling Transform node stays explicitly
deprioritized rather than quietly assumed.

## The question

How does Ymir let an author scale, rotate, and translate terrain? The obvious answer is
"a Transform node," as in most node tools. The less obvious and better answer is that
transform belongs at generation, in coordinate space, and a node that resamples a baked
field is the last resort, not the primary tool.

## The governing principle: transform the function, not the grid

A transform is lossless only when there is no grid to resample. Two regimes:

- **Coordinate space (lossless).** A generator or a coordinate formula is a function
  defined over the whole plane. Rotating, scaling, or offsetting it means changing the
  coordinates the function reads, then recomputing. No interpolation, no empty corners,
  exact at any angle or factor. This is what the Shape generators' `rotation` and
  `center` params already do, what the fractal noises' `frequency` and `offset` params
  do, and what coordinate math inside the Expression node does.
- **Baked raster (lossy).** Once a field is a sampled grid (a Blend output, an erosion
  result, an imported image), transforming it can only resample. Rotation leaves empty
  corners, scale-down leaves a void, translation runs off an edge, and every resample
  softens detail through interpolation. The empty regions must be filled with invented
  data under an edge policy (zero, extend, wrap). This is inherent to transforming a
  bounded raster and is the same cost every tool in this class pays.

The whole design follows from preferring the first regime and reaching for the second
only when forced.

## Where each gap is filled

Surveying the current nodes, transform support is nearly complete already, and the
remaining gaps are all best filled in coordinate space:

- **Shape generators** (`radial`, `falloff`, `gradient`, `ring`, `rect`, `polygon`):
  complete. Translate via `center`, scale via `radius`/`extent`/`width`, rotate via
  `rotation` or `angle` where it is meaningful. The rotationally symmetric shapes
  (`radial`, `falloff`, `ring`) correctly omit rotation, since turning a circle is a
  no-op.
- **Fractal noise** (`fbm`, `ridged`, `billow`, `hybrid`, `flow`): translate via
  `offset_x`/`offset_y`, scale via `frequency`. No rotation, by choice: isotropic noise
  is statistically rotation-invariant, so rotating one is indistinguishable from
  reseeding it. The one directional source is `flow`; a rotation param there is possible
  but low value, since a global rotation of curl noise is also close to a reseed.
- **Cellular noise** (`cellular_bumps`, `cellular_cracks`, `cellular_regions`): the one
  real gap. These have `frequency`, `jitter`, `seed` but no `offset`. Add `offset_x` and
  `offset_y` to match the fractal noises. Small, lossless, clearly useful.
- **Expression**: no params needed. The transform lives in the formula, because that is
  the node's whole purpose: `x` and `y` are raw coordinates the author bends. A coordinate
  param would be a poor fit, since it would move only the parts of the formula that read
  `x`/`y` and leave any sampled input layer untouched, and would do nothing visible for a
  formula that never mentions coordinates. The right help is a cookbook of snippets
  (rotate, scale, offset) in the docs, not machinery.
- **Import**: placement params on the node (offset, rotation, scale) plus an edge policy.
  Import already resamples the source image onto the build grid every time, so folding a
  transform into that single resample is nearly free and is the highest-fidelity option
  for imported data: it resamples the original source once, rather than resampling an
  already-resampled field. There is no "oversample vs resample" choice to expose;
  resampling always happens, and the only real dial is filter quality. The empty corners
  are handled by the edge policy, not by oversampling.

## The resampling Transform node: deprioritized, near-homeless

A standalone Transform modifier that resamples its input would be the general tool for
baked fields. With placement params on the generators and on Import, its only remaining
job is to rotate or scale a mid-graph baked result (a Blend, an erosion). A coordinate-
space workflow avoids creating those: transform the inputs before combining or eroding,
so the bake happens last and stays lossless.

There is exactly one case this cannot reorder away: a feature that only exists after
baking. Erosion is the example. Valleys, ridges, and deposits are emergent and
orientation-dependent physics, so "these exact eroded features, rotated" cannot be had by
rotating the pre-erosion inputs (that erodes a different terrain). But rotating erosion is
incoherent anyway. There are two senses of it:

- Rotating the erosion independently of its terrain is meaningless. The wear and deposits
  have no sense detached from the slopes that carved them.
- Rotating the whole eroded terrain as one locked unit is coherent, but that is not a
  terrain-authoring operation. It is "point the finished artifact a particular way in the
  world," and the reason to want it is always a relationship to the world (orient a ridge
  east-west for the sun, fit a valley in a level), never a property of the terrain.

The procedural graph is deliberately context-free: it has no north, no sun, no
neighbouring geometry, and should not. Any transform whose motivation is world context
belongs where the world is instantiated (the engine, e.g. UE5), not in the graph. At that
stage the terrain is final, so there is no detail left to protect, and the engine
resamples the heightmap into its landscape format regardless, so the lossy resample was
going to happen anyway. The engine is the right home, not the fallback.

The conclusion: the resampling Transform node has no natural home in terrain authoring.
Placement of features is lossless at generation; orientation to the world is the engine's
job. The node stays in the back pocket for imported maps (which Import handles better
itself) and is not scheduled.

## View rotation is not data rotation

The legitimate wish to see terrain from a different angle while composing is a
camera/viewport concern, not a data transform. The field stays axis-aligned and lossless;
the view turns. This is a reason to want the 3D viewport, not a reason to bake a rotation
into a field.

## Build list (priority order)

1. `offset_x`/`offset_y` on the three cellular nodes. Small, lossless, clearly useful.
2. Placement params plus an edge policy on Import. The quality path for imported maps.
3. An Expression cookbook in the docs (rotate, scale, offset snippets).
4. Rotation on `flow`. Optional, lowest value, only if it is ever missed.
5. A resampling Transform node. Not scheduled; revisit only if a concrete need appears
   that placement-at-generation and the engine cannot cover.
