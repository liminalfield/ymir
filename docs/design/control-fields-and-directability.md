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
- the **derive/selector** family (slope, curvature, aspect, height-select: half the
  directability system) and the **shaping** family (Levels, Histogram-Scan, Blur);
- **Terrace** (quantise-with-smoothing: strata and benches, a big-form move).

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
