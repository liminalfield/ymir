# Design note: the Scatter node (distribution and instancing)

Status: design only, not scheduled. This captures the thinking so the hard decisions
are written down and reviewed before any code, not discovered mid-build.

## Why it exists

The shape and landform generators answer "what is one form." They do not answer "where
are many of them, at what sizes, across this landscape." A crater authored once is a
single crater; a believable surface has a population of them, varied in size and broken
up in placement. That is a distribution problem, and the node that solves it is Scatter
(Substance's Tile Sampler, Houdini's copy-to-points). It is the missing primitive behind
"a field of craters," and it pays off equally for hills, boulders, dunes, and trees.

Scatter is deliberately ignorant of what it places. It instances a *stamp* field; that
the stamp happens to be a crater is none of its concern. This is the
"everything is data, one type on every edge" decision again: the thing scattered and the
thing scattered onto are both ordinary `Field`s.

## The model

```
stamp (the form) ──────►┐
density field (where) ──►│  Scatter  ──►  populated terrain
size field (optional) ──►┘
        + params: seed, rate/count, size distribution, jitter, spacing, compositing
```

- **Stamp** (required input): the form to instance, however it was composed. A field
  with a footprint (mostly flat, the form in the middle).
- **Density** (optional input): a control field for where instances go and how often.
  Absent, placement is uniform. This is the steering control field from the
  control-fields note.
- **Size field** (optional input): local size modulation, so instances are large in one
  region and small in another (big craters in the basin, small on the ridge). Absent,
  size is governed by the size params alone.

Optional inputs degrading gracefully (uniform when density is absent, params-only when
the size field is absent) is the same soft-contract rule the rest of the nodes follow.

## Variation: the point of the node

Scattering one stamp unmodified gives a tiled wallpaper. Avoiding that is the whole job,
and there are two tiers, worth keeping distinct.

### Transform variation (cheap, baked stamp)

Per instance, randomize from the seed: scale, rotation, amplitude/depth, and optionally
slight ellipticity (non-uniform x/y). The important nuance is that **size is a
distribution, not a value**:

- A min/max range is the floor.
- Better is an authorable size distribution, because real crater populations are
  *power-law*: many small, a few large. A uniform min/max reads as wrong; a power-law
  (or a distribution curve) is what makes a scattered field look like a real one rather
  than the same form at three sizes.

This tier still produces *scaled clones* of one stamp, but with enough transform spread
it is convincing for smooth forms.

### Shape variation (deep, procedural stamp)

Resizing one baked field gives copies of the same form. Genuinely *different* instances
require re-evaluating the stamp's **subgraph** per instance with a per-instance seed, so
each one's noise, rim, and floor regenerate differently. That is the difference between
copying a picture around and growing a new form at each site.

This tier depends on subnets existing (see #79). It is the powerful version and the
reason the baked-vs-procedural choice below is the node's central decision.

## The two decisions that will bite

1. **Scaling a baked stamp is resampling it.** A crater baked at 200 m resampled down to
   40 m goes soft: its detail does not regenerate. Acceptable for smooth forms, and the
   main argument for the procedural-stamp path on detailed ones. A first version can be
   baked-stamp-with-resample and document the limit honestly.

2. **Overlap compositing.** Instances overlap, and add is wrong (two crater rims do not
   sum into a double-height wall). Scatter needs a compositing mode: Max for raised
   bumps, Min for pits, or the realistic "younger instance recuts the older" (stamp in
   order, each one re-carves). Whatever the mode, instances must be composited in a
   fixed, seed-derived order so the result is deterministic regardless of thread count.

## Determinism

Every per-instance random (position jitter, scale, rotation, amplitude, and the
per-instance seed for the procedural tier) must derive from the global seed and a stable
per-instance key, never from the clock, a thread id, or iteration order. Compositing
order is fixed and seed-derived. Same seed and same graph produce byte-identical output,
the same hard requirement as every other node.

## Placement strategy (open)

The density field says where; a sampling strategy turns it into actual points. Two
families, possibly a mode:

- **Stochastic** (weighted random points): natural clumping, instances can crowd.
- **Poisson-disk / dart-throwing** (a minimum spacing): even, natural separation, no two
  forms too close.

A `min spacing` parameter or a distribution mode selects between them. The density field
modulates the local rate in either case.

## Not yet decided

- Baked stamp (resample) first, or hold out for the procedural subgraph path?
- The compositing modes to ship, and whether "younger recuts older" is in the first cut.
- Placement strategy and whether spacing is a parameter or a mode.
- Whether the stamp's world footprint is carried on the field (its region) or set by a
  Scatter parameter (stamp size at scale 1).

## Relationships

- Consumes the **control fields** from `control-fields-and-directability.md` as its
  density and size inputs.
- The procedural-stamp tier depends on **subnets / landform presets** (#79): the thing
  Scatter instances is a small authored graph, not just a baked image.
- Pairs with the radial-landform model (a Radial Falloff plus a Curve cross-section) as
  the natural way to author the crater/caldera stamp it scatters.
