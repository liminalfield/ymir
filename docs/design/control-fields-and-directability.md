# Design note: control fields, landforms, and directability

Status: **direction, not yet built.** Captured from a design discussion so the
reasoning survives. Nothing here is decided in the sense the
[category](node-categories.md) and [mask & selection](mask-and-selection-model.md)
notes are; this records *where the design wants to go* and why, to anchor later work
and keep it from drifting. Where it touches things already built (the `Field` type,
soft layer contracts, Blend), it says so.

## The idea in one line

> You do not place a feature at a coordinate. You author a *field* that says where
> features go and how big they are, and feed it in. Directing terrain is the act of
> shaping those control fields, and a control field is just a `Field` like any other.

## The throughline

This started from "how would we build a Mountain node?" and ended at the realisation
that the interesting problem is not the mountain. It is **direction**: how a graph
says where things sit, how they scale, and which way they trend, without every scene
collapsing to a hero mountain in the centre. The sections below follow that arc:
landforms force a reusable substrate; the substrate's key idea is the *envelope*; the
envelope generalises into the *control field*; control fields are how you direct a
landscape; and landforms, once you have the substrate, are best shipped as *graphs you
can read*, not opaque nodes.

## Landforms are fundamental, not basic

A Mountain is not algorithmically basic. The basic generators are a plane and a single
fBm field, which we have. Those produce terrain *material*: stationary, self-similar,
no large-scale organisation, no silhouette. What makes Gaea's primitives a different
category is that they produce a *landform*, a thing with a footprint, a crest, flanks
that fall away, and a base it sits on. That gross structure is exactly the part noise
does not give you. So a Mountain is fundamental in the sense that doing it properly
forces the machinery every other feature node then reuses. The toolkit is the real
deliverable; the node is a thin composition on top of it.

## The substrate a landform forces

A believable single massif is roughly:

- **An envelope** that says where the landform is and how it falls off (for a no-input
  generator, a function of region coordinates: a radial falloff for one massif, a
  low-frequency ridge-line for a meandering crest).
- **Ridged multifractal** detail rather than plain fBm. The `1 - |noise|` fold gives
  sharp crests; multifractal weighting (attenuating higher octaves in the valleys)
  concentrates detail on the ridges and leaves the lows smooth. This single choice is
  most of the visual difference between a blob and something that reads as a mountain.
- **Domain warp** on the sample coordinates so ridgelines wander instead of betraying
  the radial symmetry of the envelope.
- **A profile curve** on the combined height: concave biases toward eroded/old, convex
  toward young and bulky (roughly Gaea's Bulk/Style knobs).
- **Composition**: envelope multiplied by detail, so ridge amplitude scales to nothing
  at the base.

The payoff is the family, not the one node. Hills, Ridge, Crater, Dunes, Slump are
largely the same substrate with a different envelope and a different post-curve. So the
fundamental work is the substrate living as reusable terrain-math in `ymir-nodes`, and
every landform is then a thin, opinionated composition with good defaults.

All of this is sampled per cell from continuous coordinates, so it is
resolution-independent by construction. It sits on the sampled side of the
sampled-vs-iterative split, unlike erosion. No promises to walk back.

## Envelopes: how much, not what

The envelope is the load-bearing concept, so it gets its own statement.

Split the two ingredients of a landform:

- **Detail** is the texture (ridged noise, fBm). On its own it fills the whole grid
  with no sense of place. It is *what* the surface looks like up close.
- **Envelope** is a slow, smooth field, the same grid, that says *how much* of the
  detail survives at each cell. It is *where the ground is allowed to be tall.*

You combine them by multiplication, cell by cell:

```
final_height[x,y] = envelope[x,y] × detail[x,y]
```

Where the envelope is 1 the detail comes through at full amplitude; where it is 0 the
detail is multiplied to flat. That one multiply turns "ridged noise smeared everywhere"
into "a mountain *here*, with flanks falling to a base." The name is borrowed from the
amplitude envelope of a synth note: the waveform is what you hear, the envelope is how
loud and when.

Different envelopes, same detail, give different landforms: radial → a massif; ring →
a crater; directional → dunes; a meandering ridge-line → a range. **The detail recipe
barely changes between landforms; the envelope is what differentiates them.**

In Ymir this needs no new machinery. An envelope is an ordinary `Field` (its `height`
layer), an "envelope generator" is an ordinary generator, and "apply the envelope" is a
**Blend node in Multiply mode**, which already exists. So a Mountain is literally
`[Radial Envelope] ──► Multiply ◄── [Ridged + Warp]`, a graph you can read and rewire.

## Control fields: the envelope generalised

Here is the move that matters. The fix for the hero-mountain-in-the-centre is **not a
better Transform**. A Transform places one object. The hero mountain is not a placement
problem, it is a *distribution* problem: you want to direct where a whole landscape of
features falls. The thing that directs a distribution in a node graph is not a handle,
it is a **control field**.

And the envelope already is one. A single radial envelope gives one hero peak, dead
centre, every time. But the envelope is just a field, so:

- make it a **low-frequency noise** and mountains rise wherever that noise is high,
  scattered and clustered across the map;
- make it a **gradient** and you get a coast-to-highland trend;
- make it a **region you built in the graph or painted** and the mountains go exactly
  where you said, at the scale you said.

The envelope graduates from "the shape of one thing" into **the placement system
itself.** You never place a mountain. You shape the field that decides where the ground
is tall, how tall, and how big the features get. That field is the direction.

A control field is just a `Field`. There is no special "mask type." This is the
"everything is data, one type on every edge" decision paying off exactly here: there is
no type wall between *the terrain* and *the field that directs the terrain*, so roles
are swappable. The same low-frequency fBm is terrain in one graph and mountain
distribution in another, and the graph neither knows nor cares.

### Direction has two senses

Both matter, both are fields:

- **Steering** (where features sit): a density/mass control field.
- **Grain** (which way they trend: fault lineation, fold axes, drainage): a direction
  field that warp and anisotropy align to.

Real terrain has grain; structureless noise reads as fake precisely because it has
none. Escaping the hero mountain and escaping the grainless blob are the same move:
stop letting noise decide, and author the field that does.

## Authoring control fields

A control field is almost never authored directly. It is a tiny subgraph: a **source**,
some **shaping**, then you **use** it.

**Sources, three kinds:**

1. **Generate** a field from scratch to encode intent (low-frequency fBm for clustered
   highlands, Voronoi/cellular for regions or a population of sites, a gradient or shape
   for a trend or falloff).
2. **Derive** a field from an existing one, usually the terrain itself (slope →
   steepness, curvature → ridge vs valley, height remapped → high ground, aspect →
   facing direction, flow accumulation → where water collects). This is the powerful
   one.
3. **Paint** it by hand: the art-director override, "mountains here because I said so."
   Real and wanted, but with costs (below).

**Shaping** turns a raw field into a useful one, and is the underrated middle:

- **Levels / Histogram-Scan**: turn a vague gradient into a crisp region with controlled
  falloff.
- **Blur**: feather a hard edge so features fade in. A hard mask gives a cookie-cutter
  feature with a cliff at the boundary; a blurred one gives flanks. Most of what makes a
  mask read as natural.
- **Warp**: give a clean, geometric mask organic grain.
- **Blend**: combine control fields (Multiply intersects "high AND steep", Max unions,
  Subtract excludes).

**Worked example, the mountain belt** (the hero-mountain fix end to end):

```
low-freq fBm ──► Histogram-Scan ──► Warp ──► ┐
(blobby mass)    (crisp region)    (grain)   ├─► Multiply ──► belt
ridged noise ────────────────────────────────┘
(mountain material, full field)
```

The fBm is a vague mass distribution; Histogram-Scan sharpens it into "mountains here,
plains there, soft transition"; Warp makes the boundary wander; the Multiply applies the
mountain material across the belt and tapers it to plains at the edges. No centre, because
nothing said centre.

### Derived control fields are the point

A painted "rocks here" mask is rigid; change the terrain and it is wrong. But "rocks
where it is steep AND high AND convex" is three *derived* fields multiplied, and when
anything upstream changes, the masks re-derive and the rocks follow. The placement
*responds* to the terrain instead of being pinned to coordinates. That is the opposite
of the hero mountain's rigidity, and it is what bare Transform placement can never give.
It also reframes the Selection node work (see the mask & selection note): slope,
curvature, aspect, height-select are not just final-texturing tools, they are half the
directability system.

### Painting and splines

Painting is the one source that is input data, not computed, and it carries tension in
this project's values: a painted raster is fixed at a resolution (it does not regenerate
resolution-independently the way noise does), it breaks the "change the seed and the
world reshuffles" property (painted regions stay put), and it is stored project data.
All of which is sometimes exactly what you want, but it makes painting a deliberate,
heavier feature, not a primitive. Keep generate-and-derive as the spine; add paint as
the override later.

A more procedural middle ground for *linear* features (ranges, rivers, faults): a
**spline/path guide**. Drop control points for a crest or a course, and a generator turns
distance-from-that-path into a falloff field. Resolution-independent, light to store,
editable as a few points. It stays inside the rules: the spline lives in the node's
*parameters* (like a Shape node's centre), and what flows on the edge is still just a
`Field`. It is **not** the points/primitives schema the core forbids, because nothing
point-like ever rides an edge. For drawing the crest of a range and growing mountains
along it, this is arguably better than raster paint.

## Deriving control fields: slope, curvature, aspect, flow

The believable-vs-blobby line is the absolute-vs-relative line. Height is an *absolute*
field (where a cell sits in the world's vertical range); key a selection off it and you
get contour bands (snow above 0.8, rock above 0.6) that read as obviously procedural,
because real surface processes do not care about absolute elevation that crisply. Slope,
curvature, aspect, and flow are *relative*: they describe what the terrain is doing
*around* a cell, regardless of how high it sits, which is exactly what real processes key
off. Deriving selectors from local form instead of absolute height is the move that frees
a graph from the everything-is-a-contour-band look.

The four, by what each steers and the gotcha that bites:

- **Slope** (gradient magnitude, how steep). The workhorse: talus on steep faces,
  rock-vs-soil, vegetation on gentle ground, snow holding on the flat, erosion strength.
  *Gotcha:* a first derivative amplifies noise, and it is resolution-dependent unless
  computed in world units per region (the per-region gradient the preview relief already
  needs), or the slope field changes value between resolutions and poisons determinism.
- **Curvature** (second derivative, convex vs concave). The most underrated, highest-value
  selector. Convex picks ridges, crests, outcrops, the breaks where soil is thin and rock
  shows; concave picks valleys, hollows, gullies, where sediment, water, moisture, and snow
  accumulate. It beats a height threshold because a peak at 0.9 and a foothill at 0.3 both
  have convex crests and concave gullies; curvature finds both, a height band cannot.
  *Gotcha:* a second derivative amplifies noise even harder, so it is never one field but
  *curvature at a radius* (boulders at 10m, major valleys at 1km). The radius must be a
  parameter, which the next section is about. (Plan curvature, perpendicular to the slope,
  is convergence/divergence and the precursor to drainage; profile curvature, along the
  slope, is flow acceleration; mean curvature is the simple ridge/valley one.)
- **Aspect** (which way the slope faces). Steers anything driven by an *external* direction:
  sun (south-facing warmer, less snow, more growth), wind (windward erodes, leeward
  deposits), rain shadow. *Gotcha, and a design fork:* aspect is a periodic angle, so it
  cannot be blurred or thresholded like a scalar (359 and 1 are neighbours). Never expose
  raw azimuth; expose "facing a chosen direction" by taking a direction parameter and
  outputting `cos(aspect - target)`, a clean scalar in [-1, 1] meaning how much a cell faces
  that way ("northness", "sun-facingness"). And aspect is undefined where the ground is
  flat, so weight it by slope or flat areas emit random directions from gradient noise. This
  is the concrete shape of #68's "add azimuth": not raw azimuth, a facing-direction selector
  with a direction knob and slope weighting.
- **Flow** (drainage accumulation, how much upstream area routes through a cell). The most
  powerful field for water-worked terrain: rivers, riparian wetness, sediment deposition,
  where fluvial incision concentrates. *But it is the odd one out.* Slope, curvature, and
  aspect are **local** (a small stencil per cell, cheap, parallel, deterministic for free);
  flow is **global** (a traversal over the whole field: sort by height, route downhill,
  accumulate catchment). That brings order-dependence (equal-height cells need deterministic
  tie-breaking or determinism breaks), the pit problem (local minima trap flow, needing
  depression handling), and erosion-like resolution-dependence (the network at 512 differs
  from 2048). So flow sits with hydrology, not the clean local selectors. And erosion
  already routes water internally, so per the "nodes emit useful intermediates as layers"
  rule, a flow selector mostly wants to *read* erosion's emitted `flow`/`water` layer, not
  recompute it. A standalone flow-accumulation node is for un-eroded terrain: useful, but a
  separate, heavier, hydrology piece.

The local three (slope, curvature, facing-direction) are the clean Selectors and the real
shape of #68; flow is hydrology-adjacent.

### The compounding algebra

The payoff is that derived fields compound into a small language for "where does this stuff
go", and that is what makes a graph feel like it understands the landscape:

- exposed rock = steep AND convex
- lush valley = concave AND high-flow AND gentle
- snow = gentle AND high-altitude AND not-too-sun-facing
- scree = gentle catchment *below* a steep convex break

Each real material is a near-Boolean combination of derived fields, and the combinator
already exists: Blend in Multiply/Min for AND, Max for OR. Selector family plus Blend is an
algebra of terrain semantics where every term responds to the terrain rather than being
pinned to a coordinate or an elevation band.

## Scale: measuring a derivative at a chosen radius

A derivative on a grid is a finite difference between cells. Difference *adjacent* cells and
you measure at the grid's finest wavelength, which on fractal terrain is the noise floor, so
curvature with no radius is a high-pass noise generator, not a selector. Measuring at a
chosen radius means suppressing frequencies finer than it before differencing. Three ways:

1. **Pre-blur, then difference** (equivalently, convolve with a derivative-of-Gaussian).
   Blur to the target radius, then take the small difference; what survives is structure at
   that radius. Principled: this is scale-space theory, where the Gaussian is the unique
   kernel that invents no spurious features as it coarsens and the radius is exactly its
   sigma. The correct default.
2. **Wide finite-difference stencil** (difference cells `s` apart, the spacing is the
   radius). Cheap, no blur pass, but a crude low-pass with sinc sidelobes: it aliases fine
   frequencies rather than rejecting them and ignores the terrain between sample points. The
   poor man's scale; rejected as the primary mechanism.
3. **Image pyramid** (a downsampled mip stack, derive at the level matching the radius,
   interpolate between levels for continuous radius). Efficient for *many* radii at once, but
   octave-quantized and real machinery. The right answer only if multi-scale selectors become
   a hot path; premature otherwise.

**The decision: the radius is a composable Blur node upstream, not a parameter on the
selector.** Two options were weighed: (A) Curvature carries a `radius` knob and blurs
internally, or (B) Curvature stays minimal (adjacent stencil) and "curvature at 1km" is
literally `Blur(1km) -> Curvature` in the graph. B wins on this project's values:

- selectors stay small and single-purpose, and the radius becomes *visible in the wiring*
  (the node-readability principle);
- one Blur primitive serves every derived selector *and* the mask-feathering job from the
  authoring section, and it finally populates the deferred `filter` category;
- no quality cost: convolution associates, so `Blur(sigma) -> difference` is the
  derivative-of-Gaussian, just factored into visible pieces;
- multi-scale falls out as composition: wire several `Blur -> Curvature` branches into a
  Blend rather than burying a multi-scale mode in a node.

The honest cost is discoverability (a newcomer must learn the radius means "blur first"), and
one wrinkle: oriented curvatures (plan vs profile) couple smoothing to the flow direction,
which a generic isotropic pre-blur does not fully capture. Isotropic pre-blur plus a
directional second-derivative decomposition is standard and fine, so it is a caveat, not a
reason to bake the radius in. A derivative-of-Gaussian-in-one-node is the rare later
optimization if a selector ever needs radius-coupled internal behaviour.

**Consequence: the first thing to build for scale-aware selectors is the Blur node**, not a
selector. Gaussian (or a 3-pass box approximation: separable, O(n), deterministically
order-independent), radius in world units, clamp-to-edge at the boundary for determinism. It
unlocks the radius for every derived field and mask feathering at once.

## World units and parameter naming

The Blur radius, and every length-valued parameter, is expressed in **world units (meters),
not cells**, for resolution-independence: a "50m" radius is the same physical thing at a 1024
preview and a 4096 build, where a "26 pixel" radius would mean different physical sizes at
every resolution and the preview would lie. The world's physical size (the **world extent**,
e.g. 2km across 4096 cells) is the meters-to-cells bridge: at eval a node converts its
world-unit radius to a cell count from the current extent and resolution
(`radius_cells = radius_world * resolution / extent_world`).

This has a concrete architectural consequence: **the world extent must reach eval time, not
just export.** It lives in the field's `detail` (world bounds) and is threaded through
`EvalContext`, precisely so scale-aware nodes can do that conversion during evaluation. The
extent is the unit system for *every* length-valued parameter (noise feature size, erosion
radius which CLAUDE.md already commits to world units, spline-guide falloff), not a number
the exporter reads at the end. Threading it through evaluation is the seam that makes all of
those work later without rework, so it is worth landing with world settings even before a
scale-aware node exists.

**Naming.** "scale" is overloaded (it reads as world-scale), so it is avoided as a parameter
name. Length params are named by geometry:

- **radius** — symmetric, isotropic reach from a center (Blur radius, curvature radius,
  radial falloff, erosion reach). Use wherever it fits, which is most cases.
- **extent** — a full span or non-radial footprint (the *world extent*, a rectangular
  region).
- **distance / length** — the rare one-directional magnitude (a directional warp's push)
  where neither radius nor extent reads honestly.

Radius-first keeps the vocabulary self-consistent: a world *extent* (span) with operation
*radii* (reach) into it, and "scale" never doing double duty.

## Subtractive composition (the synthesis bridge)

The subtractive instinct from audio transfers cleanly and does not need additive
thinking. Subtractive synthesis is: a **harmonically rich source** (saw, square), a
**filter** that carves content away, an **envelope** that shapes the result. Mapped to
terrain:

- rich oscillator → a **rich base field** (full-field ridged noise, a mass, a plateau:
  more "spectral content" than the final landscape needs);
- filter → **masks, selections, and carving ops** (subtract-blend, mask-driven erosion,
  incision): you remove material to reveal the form;
- envelope → the **spatial control field** above, the same word one dimension up.

This is coherent because terrain's most characterful processes are inherently
subtractive: erosion removes and transports material, incision cuts down, weathering
and slope-blur take material away. The honest nuance: even a subtractive sculptor roughs
out the block's proportions first, so the realistic flow is a light additive massing to
set *where the highlands live* (via control fields, not hero domes), then subtractive
carving for the character. The control field serves both halves: it directs the massing,
then steers the carving.

## Shipping landforms: graphs, not opaque nodes

Once the substrate exists, a "Mountain" should not be a single node. A node bundling
envelope, ridged detail, warp, and profile would be the mega-node the
[readability principle](node-categories.md) forbids, with a dozen buried knobs. The
better form is a **graph of single-purpose primitives** you can open and read. The
subnet idea is not packaging; it is the readability principle applied at the landform
scale. The primitives are the nodes; the landform is the graph.

This is the Substance Designer model, and Substance is the existence proof: a small set
of *atomic* nodes, and every richer node is a subgraph built from them, shipped as
something you can dive into. The discipline to copy: atomics stay tiny and dumb, the
intelligence lives in the graph.

**Two ways to deliver these, cheap before expensive:**

1. **Pasteable graph fragments** (needs only save/load plus copy-paste). A landform
   ships as a saved subgraph that pastes into the current flat graph: every node visible
   and editable, no nesting engine. This already delivers "built from primitives,
   transparent, tinkerable" — the whole point — and is a fraction of the work.
2. **Subnets** (a container you dive into) add encapsulation on top, later. The decision
   to lock now, even though none of it is built: **template-graph instantiation, not the
   Houdini HDA.** Dropping a "Dune Field" copies its saved inner graph into a fresh
   subnet and from that moment it is a unique, editable, transparent copy with no link
   back to a definition. This avoids the definition-versioning / fork-on-edit /
   promoted-parameter rabbit hole, and a shipped landform stays zero engine code and zero
   operators: pure data, a file in a catalog. Ship a hundred; maintenance stays flat.

**Subnet seams to keep open (free to preserve now, do not foreclose):**

- `stable_id` stays composable (it already is, separate from the slotmap `NodeId`), so a
  per-node seed can derive from a *path* of `stable_id`s rather than one id. Two instances
  of the same Dune Field need distinct seeds, or every dune field in the scene is
  byte-identical. The path derivation must reduce to today's flat formula for a length-1
  path, so existing projects do not change output.
- The serialized format does not get painted into a flat-only corner (it has a version
  field already, so nesting is a forward-compatible evolution).
- Recognising "this node holds a subgraph, recurse into it" is a *structural* distinction
  at graph-topology level, not semantic dispatch on operator identity. It does not break
  the "nothing asks which node is this" invariant, which forbids branching on Mountain vs
  Hills, not knowing a thing is a container.

Do not pre-build any of it. Build the substrate and primitives now; subnets arrive once
the engine can nest, and the catalog arrives for free as files.

## What to take from Substance Designer, and what to refuse

**Take** (specific, terrain-relevant primitives): Shape/Gradient generators (the envelope
family), **Histogram-Scan / Histogram-Range** (precise threshold-to-mask, exactly the
selection-refinement tools), **curvature and ambient-occlusion** as utility maps (the
good selectors beyond height/slope/azimuth: convexity picks ridges, concavity valleys, AO
crevices), and **Slope Blur** (a blur steered by a guide's slope, a cheap controllable
proxy for directional weathering, good for chipped/broken rock).

**Refuse** (the scope traps): the two-type grayscale+color system (the single
`Field`-of-scalar-layers is better for terrain; color, if ever, is a scalar layer not a
parallel type); the Pixel Processor / FX-Map generality (a per-pixel programming
environment is a black hole; if ever wanted it is *one* node behind the clean
heavy-dependency seam, never a paradigm); and the tiling-first mindset (Substance makes
seamless tileable textures; terrain is bounded landforms).

A quiet structural fact worth knowing and not acting on: `ymir-core` already has zero
terrain semantics. It is a generic deterministic scalar-field dataflow engine; all the
terrain-ness lives in `ymir-nodes`. An "open-source Substance" would architecturally be a
different nodes crate on the same core, not a new project. Pocket it; finishing the
terrain tool comes first.

## How this fits what is already built

- **Soft layer contracts already aim here.** The reason Houdini feels directable and Gaea
  fights you is that Houdini's heightfield nodes take mask inputs everywhere; Ymir adopted
  that on purpose (effects read a `mask` input when wired, else `layer_or(MASK, 1.0)`).
  The directability is already in the DNA; it needs placement primitives to express it.
- **No points/primitives schema**, by design. "Many features" is field-native (Voronoi
  cells as scatter sites expressed as a field, full-field generators modulated by a mass
  field), not point instancing. The spline guide above stays param-space for the same
  reason.
- **The additive-node invariant holds** throughout: every primitive here is one file plus
  one registration, and a landform is data, so the catalog adds no engine code.

## Near-term implications (what actually gets built first)

Dependency-free and buildable now: the **substrate primitives**. In rough order of
unlock:

- a **ridged multifractal** generator (mountain material);
- an **envelope / Shape** generator family (radial, ring, directional, ridge-line: the
  vocabulary and the steering);
- a **Warp** primitive (organic grain; note the resampling caveat below);
- the **derive/selector** family (slope, curvature, facing-direction, height-select: half
  the directability system) and the **shaping** family (Levels, Histogram-Scan, Blur);
- **Terrace** (quantise-with-smoothing: strata and benches, a big-form move).

Two of those are early keystones, not just list items. The **Blur** node comes before the
selectors that depend on it: it is the radius mechanism for every derived selector (see
Scale) and the mask-feathering tool, and it opens the `filter` category. And **threading the
world extent through eval** (see World units) is a seam to land with world settings, ahead of
any scale-aware node, since it is the meters-to-cells bridge every world-unit parameter needs.

Then, behind **save/load plus copy-paste**, landform presets as pasteable fragments. Then
subnets as the encapsulation polish.

## Open questions flagged for later

- **Domain warp is lossy in a grid model.** Warping continuous coordinates before sampling
  gives clean warped ridgelines; warping an already-sampled `Field` resamples a discrete
  grid (bilinear), softening high frequencies under large warps. The fork: a grid-resample
  Warp node (composable, degrades) versus warp baked into the generator (sharp, couples
  warp to generation). The same fork applies to a Transform that moves a finished field, so
  Shape generators carrying their own placement params (centre/rotate/scale) is the crisp
  path; a grid-resample Transform exists for when you must move a computed field, with the
  honest caveat. Settle before building Warp/Transform.
- **Additive vs subtractive emphasis** changes which primitives are urgent. A
  subtractive-first workflow prioritises a rich base, steerable masks, and carving ops over
  a broad Shape-generator catalog. Leaning subtractive (see the synthesis bridge).
- **Path-based seed derivation** must be backward-compatible with the current flat formula
  (length-1 path equals today's value), or existing golden outputs change.
- **Blur kernel and edge handling.** True Gaussian vs a 3-pass box approximation (quality vs
  a few ms; the box is slightly anisotropic). And edge handling for every neighbourhood op
  (clamp vs reflect vs the tiled-build halo): settle once, project-wide, because it ties to
  the tiled-build-matches-untiled promise in CLAUDE.md rather than being a per-node choice.
