> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Ymir node taxonomy design (Revision 2)

Revised against the node inventory of 2026-07-14. Revision 1 was written without
knowledge of the built set, and proposed a good deal that already exists, some that is
already settled, and some that the existing design correctly rejects. This revision
keeps only what survives contact, and spends its length on the gaps.

Erosion is specified in `ymir-erosion-DESIGN.md` and is not decomposed here. This
document covers the atoms used to *direct* erosion and to *finish* it, plus the general
dataflow machinery.

---

## What the inventory corrects

Stated plainly, because a design doc that quietly drops its wrong bets is not worth
trusting on its right ones.

**The Layers category was unnecessary.** Revision 1 proposed Copy Layer, Rename Layer,
Delete Layer, Promote to Height and Merge Fields, on the assumption that erosion
byproducts ride as extra layers on a single field. They do not: erosion emits its
byproducts as *separate outputs carrying data on `height`*. A byproduct therefore
arrives already on the primary layer, ready for Curve, Levels, Blend and everything
else, with no plumbing. Promote to Height is not needed because nothing was ever
demoted. The category dies, and the multi-output-on-height convention is a better idea
than the one it would have replaced.

**Set Mask should not be built.** Same reasoning, and it answers the inventory's own
question. Effects take an explicit optional `mask` input, and a selection is an ordinary
field on `height`. Attaching a selection to a field's `mask` layer as a separate step
adds an indirection with nothing on the other end of it. Delete the proposal.

**Resampling Transform: the existing argument is better than mine.** "Transform the
function, not the grid" is right, and Revision 1's Transform node was a reflex from
Substance, where everything is a baked raster and there is no choice. Ymir's generators
are pure functions of world coordinates and carry their own placement params. Keep it
back-pocket. Do not build it.

**The Masks question is already closed**, and closed the way Revision 1 argued for. No
Masks tab, masks are ordinary fields, creators are selectors, editors are ordinary
adjust nodes. Revision 1 claimed to resolve a question that had already been resolved.
Noted and dropped.

**Standalone flow accumulation stays off the main path.** Revision 1 wanted it as an
analysis node. The existing decision (read erosion's emitted `flow`) is correct: it is a
global, order-dependent, resolution-dependent computation whose result is nearly always
wanted *after* erosion anyway, because flow over un-eroded fBm is flow over a landscape
with no channels in it. This has a consequence for Basin, below.

**Curvature scale is already solved by composition.** Blur sets the measurement scale for
Slope and Curvature. That is the honest answer and it is already the built one. Revision
1's proposed `scale` parameter would have hidden a Blur inside a node. Drop it.

---

## The one structural disagreement worth pressing

The three built selectors (Height, Slope, Curvature) **fuse measurement and selection**.
Slope takes min, max and falloff in degrees and emits a band. There is no way to obtain
slope in degrees as a field.

This is Houdini's `Mask by Feature` pattern, and it is the one thing in the current set I
would change. Not by adding nodes. By adding a mode.

**Proposal: each selector gains an `output` mode, `{selection, measure}`, defaulting to
`selection`.** In `measure` mode the node writes the raw quantity onto `height` and the
band parameters grey out. Slope writes degrees. Height writes elevation. Curvature writes
signed curvature. Aspect (#68) writes `cos(aspect − target)`, which it was already going
to do.

The cost is one enum and one branch per selector. What it buys:

- **Histogram Scan stops being a node in search of a justification.** The inventory flags
  it as "possible overlap with Levels, open question". There is no overlap once raw
  measures exist, and Scan is not very interesting until they do. Levels rescales a range
  linearly with a gamma. Scan thresholds a quantity into a mask with a controlled falloff
  shape. `Slope(measure) → Scan(position 35°, width 4°)` is what people actually want, and
  it is strictly more capable than the fused selector, because the measurement can be
  filtered, curved, blended, or combined with another measurement *before* the threshold.
  "Steep and south-facing and high" becomes three measures, some arithmetic, one Scan.
  Today it is three bands and two Blends, which is not the same computation and is worse
  at the boundaries.
- **It makes the measurement composable at all**, which is the property that turns the
  local three from three fixed masks into a control system.
- **It is the decomposition without paying for the decomposition.** The convenience path
  (the fused band) stays the default, so nothing regresses for the common case.

**A caveat that sharpens the point.** Curvature is described as self-calibrating,
RMS-normalized. That is a global reduction over the field, which means the node's output
at a cell depends on the content of every other cell. It is convenient and it is a smell.
It makes a Scan position meaningless (35 of what?), it makes the output change when the
region is cropped or extended, and it is exactly the kind of hidden normalisation that
forces every downstream user to guess. In `measure` mode, curvature should emit raw signed
curvature in its natural units and let Levels do the rest. If raw curvature turns out to
be unusable in practice, that is evidence of a units problem worth finding, not a reason
to keep hiding it.

**The honest exception.** Aspect is right to emit `cos(aspect − target)` rather than raw
azimuth. Azimuth is circular and discontinuous at 0/360, so it cannot be thresholded at
all; projecting onto a target direction is not a pre-normalisation, it is the only
well-defined scalar available. The note that it must be slope-weighted (aspect is
undefined on flats) is correct and should stay.

---

## The gaps, ranked

Absent from both the built set and the discussed set, or present but mis-scoped. Ordered
by value per unit of work.

### 1. Frequency Split (Filters) — the missing keystone

**"Erosion carves into form, it does not create it. Big regional form, then erosion, then
high-frequency detail afterward."** That is a stated project principle and there is no
node that expresses it. Today it is a rule the user has to remember and manually respect
by wiring generators at the right frequencies in the right order.

Frequency Split makes it a visible wiring pattern:

- Input: a field. Parameter: a cutoff in world metres.
- **Two outputs**: `low` (the form) and `high` (the detail residual).

The canonical erosion graph then reads out loud: split the terrain, erode the low band,
add the high band back. Detail survives erosion instead of being smeared by it, and the
user can see why. It also subsumes a family: height-above-local-base (the mask wanted for
snow that respects valleys rather than a flat altitude line) is just the `high` output of
a coarse split. No new node required.

Implementation is nearly free. `low = blur(in, cutoff)`, `high = in − low`, and Blur
already exists. It becomes the first multi-output non-endpoint node outside erosion, which
is a seam worth exercising early rather than late.

This is the cheapest node in the document and probably the most valuable. Build it first.

### 2. Field-driven parameters (core) — largest change, largest payoff

Not in the inventory in any form. Every surveyed tool has it, Instant Terra markets it as
its headline ("masks on every parameter"), and Gaea's Selective Processing (modulate a
parameter everywhere, versus confine an effect to a region) is the distinction that makes
erosion directable rather than merely maskable. Ymir has masking. It does not have
modulation.

**Mechanism: promotion, not static declaration.** A scalar parameter is a constant by
default. The user promotes it, which adds a pin to that node instance. The promotion set
is per-instance state, serialized alongside params and connections. Static declaration (an
optional input per modulatable param) would grow twelve pins on a twelve-parameter erosion
node and make the canvas unreadable.

**Semantics: an explicit lerp.**

```
value(cell) = lerp(low, high, clamp(field(cell), 0.0, 1.0))
```

`low` and `high` appear as sub-parameters when the parameter is promoted, defaulting to
its declared range. This makes modulate-versus-confine fall out without a second concept
(set `low` to the unmodified value to modulate; set it to zero to confine), and avoids
inventing a per-parameter normalisation convention nobody can remember.

**Consequences to design in deliberately:**

- *Arity.* Node kind derives from arity, and a promoted parameter adds a pin. A promoted
  generator would reclassify as a modifier, which is wrong. Distinguish **data inputs**
  (declared in `NodeSpec.inputs`) from **modulation inputs** (derived from the instance's
  promotion set); kind derives from data inputs only. Modulation inputs still participate
  fully in DAG validation and evaluation. Note that Expression already bends this rule (it
  runs as a generator when unwired), so the arity model has precedent for needing care
  here.
- *Cache key.* Must include the promotion set and the hashes of the modulation inputs.
- *Operator signature.* The evaluator resolves each parameter to a constant or a field and
  hands the operator an accessor. Operator code should never branch on whether a parameter
  was promoted.
- *`ParamSpec` gains `modulatable`.* A seed cannot vary per cell. Nor can an iteration
  count, a path, or an enum. Say so in the schema rather than discovering it in a bug.

This should land with the `NodeSpec` category and i18n change the GUI doc calls for, so
core is disturbed once rather than twice.

### 3. The eikonal solve (substrate, not a node) — three consumers, one solver

Not a node. A library function in `ymir-nodes`, with three callers already:

1. **The flow-map flat-resolution fix.** The residual ±22.5° crease artifacts are chamfer
   metric anisotropy; the fix is a fast-sweeping solve of `|∇T| = 1` for true isotropic
   geodesic distance. Already diagnosed.
2. **Spline / path guide** (proposed, generator). Its entire job is distance-from-path
   falloff. Ranges, rivers, faults as linear features.
3. **A Distance selector.** Distance from any `[0,1]` mask, in world metres. Distance to
   coast (which the Coastal shaper wants), to river (which texturing wants), to ridge.
   (Built as `modifier.distance`, #137, on an eikonal solve. The remaining substrate work is
   generalizing that solve so the flow-map fix and Spline guide reuse it.)

Three consumers is not speculative abstraction; it is the threshold at which building it
once and properly is obviously correct. Right now it sits buried inside the flow-map issue
as an implementation detail, which risks it being solved twice and badly. Lift it out and
make it visible in the issue graph. Do the diagnostic step first as already planned (test
on flats-free terrain to isolate metric contribution from MFD contribution).

### 4. Occlusion (Selectors) — the missing non-local measure

The local three (slope, curvature, aspect) are all first- and second-derivative measures
with a radius of one cell. There is no measure with *extent*. Occlusion is the obvious one
and it is absent from both lists.

Sky-view factor by horizon scanning: for each cell, march a set of directions out to a
maximum distance and find the horizon angle. Output in `[0, 1]`. Parameters: ray count,
maximum distance in world metres.

It is the cavity and crevice mask; it is physically meaningful rather than an
ambient-occlusion hack; and unlike curvature it knows the difference between a narrow slot
and a broad basin, because it has a length scale. It is also the natural driver for snow
retention, moisture, and vegetation masks later. Same `{selection, measure}` mode as the
rest.

### 5. Basin (as a Stream output, not a node) — the atom nobody has

Substance's Flood Fill family (identify connected regions, then assign per-region values)
has no terrain analog in any surveyed tool. The analog is watershed labelling, and it falls
out nearly free once flow routing exists.

**Crucially, and correcting Revision 1: this must not be a standalone node.** The firm
decision that standalone flow accumulation is off the main path applies with full force and
for the same reason. Basin is therefore **an additional output of the Stream erosion node**,
which already fills depressions and routes MFD drainage. It costs a receiver-chain walk to
the terminal sink, on routing that is already computed.

- **Stream gains a `basin` output.** Value is a basin id. One extra parameter: a minimum
  basin size in world units, below which small basins merge into their neighbour, because
  raw watersheds are absurdly numerous.
- **Basin Value** (Selectors) is then a small node: input a field carrying basin ids, output
  one scalar per basin. Modes: random (seeded from `stable_id`), area, index, mean of source,
  max of source, distance to outlet.

What it buys: per-basin erodibility, so one valley system is soft rock and the next is hard
**with the boundary falling on the actual drainage divide**. Per-basin uplift, giving a
different relief regime between neighbouring watersheds. Per-basin strata rotation.
Region-aware biome masks later that respect where water actually goes.

The contrast with **Cellular Regions** is the whole point, and worth being explicit about
since that node already exists. Cellular Regions gives regional variation whose boundaries
are noise contours painted over terrain that happens to have drainage. Basin gives regional
variation that *is* the drainage structure. Geologically these are not the same object, and
the second is the one a geologist would want.

**Representation.** A basin id is categorical and `Layer` is `f32`. Two options:

- Encode as an exactly-integral `f32`. Integers to 2^24 are exact, which is 16.7M basins,
  far beyond any plausible map. Costs nothing, blocks nothing.
- Wait for the categorical layer that **Biome classify already requires**.

The second is more attractive than it was, because Basin is a *second* consumer of the same
data-model addition. Two consumers is the point at which "the one data-model addition" stops
being a biome-specific tax and becomes a general capability. If the categorical layer is
going to happen anyway, Basin should ride it rather than ship an encoding hack that later
has to be unwound. If that decision is not close, ship the `f32` encoding, document the
ceiling, and do not let it block.

### 6. Slope Blur is the LookDev node

The inventory lists Slope Blur (proposed, filter) and LookDev/fake-erosion (proposed,
filter/geology) separately, noting they overlap. They do not overlap. They are the same
node.

Slope Blur is blur pushed along the gradient of a driver field rather than isotropically.
With a `min` mode rather than a mean, it produces something remarkably like thermal
weathering at a fraction of the cost. That *is* the cheap eroded look, and it is a
deformation primitive rather than a fake physics model, which means it composes instead of
sitting in a corner labelled "not real erosion". Build Slope Blur, delete the LookDev
proposal, and if a fast preview tier is wanted later it is a wiring pattern (Slope Blur plus
Terrace plus Warp), not a node.

Inputs: field, driver. Parameters: intensity (world metres), samples, mode
`{blur, min, max}`.

---

## What survives from Revision 1 unchanged

- **Terrace** (already designed, keystone of the deferred Filters category) is Revision 1's
  Quantize. Same node, better name. Keep the existing name.
- **Strata** as a layer and hook consumed via `layer_or(RESISTANCE, 1.0)`, rather than a
  standalone node, is a better framing than Revision 1's. Adopted.
- **Erosion stays cohesive.** No carve/deposit/smooth construction kit. Settled, and this
  document does not reopen it.
- **No node per arithmetic operation.** Blend's mode enum plus Expression covers it. The
  palette should stay around forty nodes, not two hundred.
- **Spatial parameters in world units**, which the built set already does consistently
  (radii in metres, angles in degrees). Keep holding it.

---

## Revised build plan

Ordered by dependency and value, not by appeal.

**A. Frequency Split.** Two outputs, cutoff in metres, built on the existing Blur. Cheap,
high value, exercises the multi-output seam outside erosion, and makes the project's own
stated erosion principle expressible in the graph. First.

**B. Measure mode on the selectors.** `output {selection, measure}` on Height, Slope and
Curvature, and on Aspect (#68) as it lands. One enum, one branch each. Includes the
decision on raw versus RMS-normalized curvature.

**C. Histogram Scan.** Justified by B. Position, width, falloff shape, invert. Plus a
two-sided Range variant if the one-sided version proves annoying, which it will.

**D. Aspect (#68).** Already designed. Lands in the same shape as B.

**E. Field-driven parameters (core).** `ParamSpec.modulatable`, the promotion set, the
data-input versus modulation-input distinction, the cache-key change, the resolved-parameter
accessor. Sequence with the `NodeSpec` category and i18n change.

**F. Terrace.** Already designed. Trivially better once E lands (a modulated step count).

**G. Slope Blur.** Absorbs the LookDev proposal.

**H. Eikonal solve (substrate).** Diagnostic first, then fast sweeping. Unblocks the
flow-map fix, the Spline guide, and the Distance selector. (First landed with the Distance
selector, #137; the remaining work is lifting it into a shared primitive.)

**I. Distance selector.** Built as `modifier.distance` (#137).

**J. Occlusion selector.**

**K. Basin.** Stream gains a `basin` output; Basin Value as a small selector. Gated on the
categorical-layer decision, or shipped on the `f32` encoding if that decision is not close.

**L. Strata.** Already designed and flagged highest value. Sequence with the erosion
workstreams that consume it, not with this document's.

---

## Open questions

- **Measure mode: an enum on the existing selectors, or a second output?** A second output
  (band and raw from the same node) means no mode switching and no greyed-out params, and
  the multi-output convention already exists for erosion. It also means every selector
  always pays for both. Leaning enum, but the multi-output version is arguably more
  Ymir-shaped.
- **Raw curvature units.** If the RMS normalization is load-bearing rather than a
  convenience, that needs understanding before B, not after.
- **Categorical layer: now or later?** Two consumers (Biome, Basin) rather than one. Does
  that move it, or is Biome still far enough out that Basin should ship the `f32` encoding?
- **Does Basin belong to Stream, or to a routing substrate that Stream and Basin both
  call?** The former is cheaper and honours "read erosion's flow". The latter is cleaner if
  Precipitation or a future rivers node ever wants routing without incision.
- **Promotion semantics: `lerp(low, high, field)`, or a mode (replace / multiply / add)?**
  Lerp is more explicit and covers modulate-versus-confine in one concept. A mode is more
  compact on the panel.
- **Does Frequency Split's `high` output want a gain parameter**, or does that belong to the
  Blend that puts it back? The latter, probably. Resist parameters that exist because they
  were convenient at the time.
