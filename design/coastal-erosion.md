> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Ymir coastal erosion: design brief

Revision 1. Companion to `ymir-erosion-DESIGN.md`. This document specs the coastal and
water-line model, the substrate it needs, and the research and decisions required before
implementation. It is written to be reviewed, revised, and then decomposed into issues.

---

## 1. Thesis

Every tool in the survey treats coastal erosion as a non-simulated geometric
approximation. World Machine's Coastal Erosion device picks a water level and reshapes
the adjacent land into a beach-and-bluff profile. Gaea's Sea and Lake nodes emit a water
surface and a depth field with a fast bevel at the margin. The survey's own summary of
the family is accurate: "pick a water level, then reshape adjacent land into a
beach-and-bluff profile and emit a water surface and depth field. Cheap, controllable,
high visual payoff, no physics."

The result in all of them is a coastline of uniform character. Beach width is the same
in a sheltered lagoon as on an ocean-facing headland. Cliffs retreat by the same amount
everywhere. Nothing distinguishes the windward from the leeward side of an island. The
coast is a bevel applied to a contour, and it reads that way.

The mechanism those tools are missing is **wave exposure**. Coastal form is driven almost
entirely by the spatial distribution of wave energy at the shoreline, and that
distribution is not uniform: it depends on fetch (how much open water the wind crosses to
reach a point), on the angle between the incoming swell and the shore normal, on wave
refraction focusing energy onto headlands and spreading it in bays, and on the nearshore
bathymetry that dissipates it.

All of these are computable on a regular grid, cheaply, with sweeps and a distance field.
None of them require a fluid simulation. Exposure is the coastal analogue of drainage
area in the fluvial model: a global, scalar, self-reinforcing driver that is expensive to
fake and cheap to compute properly.

**The design position is therefore: model exposure honestly, then apply a geometric
profile whose amplitude and character are driven by it.** The reshaping stays geometric
and non-iterative in phase 1, so it remains fast and controllable. The physics goes into
deciding *how much* reshaping happens *where*, which is the part every other tool
skips.

That single change buys, for free and without a simulation:

- Headlands that erode back while bays accrete, because refraction concentrates energy on
  convex-seaward shoreline.
- Beaches that appear in sheltered embayments and not on exposed capes, because that is
  where the sediment budget converges.
- Windward and leeward coasts that look different on the same island.
- Sea stacks and differential-erosion coastlines, if an erodibility layer is present,
  because resistant rock survives the cliff retreat that removes the surrounding platform.
- An `exposure` layer that is immediately useful downstream for texturing, wet-rock
  masks, vegetation exclusion, and snow.

Phase 2 adds longshore sediment transport and a shoreline that evolves under it, which is
what produces spits, tombolos, barrier bars, and cuspate forelands. That is a larger
piece of work and is scoped separately below.

### 1.1 The priority order

Ymir is a tool for making landscapes for game engines and other DCCs. It is not a coastal
process simulator, and it must never start behaving like one. The priority order, in
descending order and without ties:

1. **Does it look good.**
2. **Is it at least loosely plausible**, so that the result reads as a real place rather
   than a filter effect, and so that a user with geological instincts is not fighting the
   tool.
3. Is it physically defensible.

Item 3 is a means, not an end. Physics earns its place in this design **only where it is
the cheapest route to a look that is hard to fake**, and it is cut wherever a heuristic gets
within visual distance for less complexity or less tuning pain.

Applying that test to this document, the parts that clearly earn their keep:

- **Exposure.** Highest look-per-flop item in the whole design. Windward and leeward coasts
  that differ, headlands that erode while bays accrete, beaches only where they belong.
  There is no cheap fake for it, and a few grid sweeps buy all of it.
- **Alongshore redistribution.** The reason a beach appears in the bay and not on the cape.
  This is a look, not a physics result, and it happens to be produced by a physics-shaped
  equation.
- **The eikonal solver.** Justified purely by artifact avoidance. Chamfer distance produces
  a star-shaped beach. Stars look bad.
- **Sub-cell shoreline extraction.** Justified purely by artifact avoidance. Cell-quantised
  distance produces terracing parallel to the shore. Terracing looks bad.
- **Differential erosion and sea stacks.** Very high visual payoff, nearly free once an
  erodibility mask exists.

And the parts that do not, which are cut or demoted below rather than left in as decoration:
the Hallermeier closure-depth formula, the Iribarren number as a user-facing concept, wave
period, the Sunamura logarithm, and the platform's self-limiting feedback. Each is replaced
by a simpler knob that lands in the same visual place. See sections 3, 6, and 7.

---

## 2. What this model does not do

Stated up front so the scope is not litigated later.

- **No shallow-water fluid simulation.** No wave propagation, no swash, no tidal flow. The
  wave climate is a set of static parameters and the energy field is a solved scalar, not
  a simulated one.
- **No overhangs.** Ymir's element is a 2D grid and a height per cell. Wave-cut notches,
  sea caves, and arches are undercuts, which a heightfield cannot represent. The cliff can
  be at most vertical. This is a hard limitation of the data model, not a shortcoming of
  the algorithm, and the documentation should say so plainly rather than implying stacks
  and arches are coming. Sea *stacks* are representable (they are columns, not overhangs)
  and do fall out of differential erosion. Arches do not.
- **No tides as a time dimension.** Tidal range is a scalar parameter that widens the
  intertidal band and changes the platform's character. There is no tidal cycle.
- **No storm events.** The wave climate is a single representative state, not a
  distribution over time. A user who wants a storm coast raises the wave height.

---

## 3. Geomorphological brief

The mechanisms the model is actually reproducing, so the parameters have meaning rather
than being sliders.

### 3.1 The erosional side: cliff retreat and the shore platform

Waves attack the base of the cliff. Where the wave assailing force exceeds the resisting
strength of the rock, material is removed and the cliff retreats landward. Sunamura's
relation, the standard formulation for rocky coasts, gives the retreat rate as
proportional to the logarithm of the ratio of those forces:

```
dX/dt = C * ln(Fw / Fr)      (Sunamura 1992)
```

where `Fw` is the wave assailing force (a function of wave height and the nearshore
approach) and `Fr` is the rock's resisting strength. The logarithm is important: it means
retreat is not linear in wave energy, and it means there is a threshold below which
nothing happens at all. A coast whose rock is stronger than the waves is stable
indefinitely. This is why exposure gates the whole model rather than merely scaling it.

As the cliff retreats it leaves behind a **shore platform**, a near-horizontal bench cut
into the bedrock at roughly the intertidal level. Platform gradients are shallow, typically
one to four degrees seaward. Crucially, the platform's *character* depends on tidal range:

- **Microtidal coasts** (small tidal range) produce Type B platforms: sub-horizontal, cut
  at a narrow elevation band, terminating seaward in a low-tide cliff.
- **Macrotidal coasts** (large tidal range) produce Type A platforms: a sloping ramp,
  because the zone of wave attack sweeps across a wide vertical range.

This gives `tidal_range` a real job rather than being decorative: it sets the width of the
band over which the platform is cut and therefore whether the result is a bench or a ramp.

The platform widens over time, and as it widens it dissipates more wave energy before the
waves reach the cliff, which slows further retreat. This is a negative feedback that
produces a self-limiting coast. Whether to model that feedback is an open question below
(section 12).

### 3.2 The depositional side: the beach profile

Where sediment is available, it settles into a profile that is remarkably consistent and
well characterised. The **Dean equilibrium profile** describes the submerged part:

```
h(y) = A * y^(2/3)           (Dean 1977; Bruun 1954)
```

where `h` is water depth, `y` is distance seaward from the shoreline, and `A` is a
sediment scale parameter that depends on grain size (specifically on sediment fall
velocity). Approximate values, to be checked against the Shore Protection Manual /
Coastal Engineering Manual tables at implementation time:

| D50 (mm) | Description | A (m^1/3), approx |
|---|---|---|
| 0.1 | very fine sand | 0.06 |
| 0.2 | fine sand | 0.10 |
| 0.4 | medium sand | 0.14 |
| 0.8 | coarse sand | 0.18 |
| 1.0 | very coarse sand | 0.20 |

Sanity check: `A = 0.1`, 100 m offshore, gives a depth of about 2.2 m. That is a gentle
dissipative sand beach, which is correct.

The **subaerial** part of the beach has a different character. It rises from the shoreline
across a swash-zone **beach face** at a slope set by grain size and wave energy, up to a
**berm** whose crest sits above still-water level by roughly the wave run-up height.
Approximate beach-face slopes:

| Material | Beach face slope |
|---|---|
| Fine sand | 1 to 3 degrees |
| Medium sand | 3 to 6 degrees |
| Coarse sand | 6 to 10 degrees |
| Pebble | 10 to 20 degrees |
| Cobble | up to about 24 degrees |

The physical literature ties these together through the **Iribarren number**
(`xi = tan(beta) / sqrt(H / L0)`), which classifies breakers as spilling, plunging, or
surging and therefore predicts a dissipative, intermediate, or reflective beach.

**We do not need it.** Everything it buys us reduces to two monotonic tendencies that a
user can see and that can be tuned by eye:

- Coarser sediment gives a steeper beach.
- Higher wave energy gives a flatter, wider beach.

A two-input lookup or a simple product gets within visual distance of the full surf
similarity machinery, and it is far easier to tune. The Iribarren number stays out of the
code and out of the UI. It is recorded here only so that whoever tunes the lookup knows
which way the arrows point and why.

What **does** survive from this discussion is the more important structural point, and it
is not about physics at all:

> **Beach slope must be a field, not a scalar.**

Wave energy varies along the coast, so the derived slope varies along the coast. An exposed
headland gets a steep, narrow, reflective beach and a sheltered bay gets a wide, flat,
dissipative one, on the same terrain, from a single node. No slider can produce that,
because a slider is one number. This is a look that is unavailable to every tool in the
survey, and it is the actual reason not to expose a raw slope parameter. Section 7 handles
the control surface.

### 3.3 The seaward limit

The reshaping must not extend to the abyss. It needs a depth at which it fades out and
leaves the underlying bathymetry alone.

Coastal engineering has a rigorous answer (Hallermeier's **closure depth**, roughly
`2.28 Hs - 68.5 Hs^2 / (g T^2)`, about 4 m for a 2 m sea at an 8 second period). The
original draft of this document derived the parameter from that formula, on the argument
that a derived value beats an arbitrary slider.

That argument was wrong for this tool. The formula's output is a single depth, the user
cannot see the difference between 4.1 m and 4.0 m, and its only visible effect is *how far
underwater the node reaches*, which is a thing the user may well want to set directly for
compositional reasons that have nothing to do with wave physics.

**So it becomes a direct parameter, `underwater_reach`, defaulting to `2 * wave_height`.**
The default keeps it plausible and coupled to the wave climate. The slider keeps it
controllable. Nothing of visual value is lost.

This deletion cascades: closure depth was the last remaining consumer of **wave period**
(via the deep-water wavelength `L0`). Since the Iribarren number also went (3.2), wave
period now has no job in the model at all, and it was the least intuitive parameter in the
node. **It is cut.** I argued for keeping it during review, but I was defending it because
it fed two formulas that are themselves now gone. With those gone, it is a parameter that
does nothing a user can see.

### 3.4 The alongshore side: littoral drift

When waves approach the shore obliquely, they drive a longshore current and a sediment
flux along the coast. The CERC formula gives the volumetric transport rate:

```
Q ∝ Hb^(5/2) * sin(2 * alpha_b)
```

where `Hb` is the breaking wave height and `alpha_b` the angle between the wave crest and
the shoreline at breaking. Two consequences matter enormously:

1. **Where the flux converges, sediment accumulates; where it diverges, the coast erodes.**
   This is the sediment budget, and it is why beaches form in bays: the flux slows as the
   coast turns away from the waves, and the sediment drops out. Modelling deposition
   *locally* (depositing eroded material where it was eroded) is exactly backwards, because
   erosion happens on the headlands and deposition happens in the bays.

2. **The `sin(2 alpha)` term peaks at 45 degrees and then decreases.** Ashton and Murray
   (2001, 2006) showed that this non-monotonicity makes a coastline *unstable* under
   high-angle wave approach: a perturbation grows rather than smooths, producing cuspate
   forelands, capes, and flying spits. Under low-angle waves, the same equation reduces to
   a diffusion (the Pelnard-Considère one-line model) and the coast smooths. This means the
   wave approach angle relative to the regional shoreline trend switches the coast between
   two qualitatively different behaviours. That is a genuinely striking result and it is
   the payoff for phase 2.

---

## 4. Node decomposition

Ymir's stance is that erosion nodes are cohesive models, not a construction kit. That
holds here. But there are three separable concerns, only one of which is the erosion model,
and factoring them correctly matters because two of them are needed by other nodes.

### 4.1 `Sea` (a new node, small)

Sets a water level and classifies water. Does not modify height.

- Reads `height`, a `sea_level` parameter, and a connectivity mode.
- Emits the `water` layer (depth, zero on land) and a `sea` mask.
- Writes `sea_level` into the field's `detail` bag.

This exists separately because **base level is a shared concept**. The stream-power fluvial
node needs a base level to grade rivers to, and it must be the same number the coastal node
uses, or rivers will terminate above or below the shore. Putting sea level in `detail` and
having a `Sea` node set it is the clean way to share it. See section 5.3.

It is also useful standalone: a user who wants a lake or an ocean surface with no coastal
reshaping should not have to run the coastal model.

### 4.2 `Coastal` (the model)

The erosion node proper. Reads `height` and, softly, `water` / `sea` / `erodibility` /
`flow` / `sediment` if present. Computes exposure, reshapes the terrain, emits the layers.

If no `water` layer is present it computes one internally from its own `sea_level`
parameter, per the soft-contract rule (never gate a connection). This means a `Coastal`
node dropped on a bare terrain works immediately, and a `Sea -> Coastal` chain also works
and shares the level.

### 4.3 `Exposure` (a later utility node, optional)

The fetch and wave-energy field alone, with no reshaping. Deferred, but the *substrate*
that computes it lives in `ymir-nodes` as reusable terrain math from day one, so a later
`Exposure` node is a thin wrapper rather than a rewrite. The same directional-sweep
machinery also serves sun exposure, wind exposure, and snow deposition, all of which are
on the horizon.

### 4.4 Category

`Sea`, `Coastal`, and a future `Lake` / `River` / `Delta` group are the seed of a
**Hydrology** category. The taxonomy already anticipates this ("Hydrology and snow
processes to become their own tabs if they grow"). Recommend filing `Coastal` under
Geology > Erosion for now, and `Sea` under Hydrology if that tab is created, or Generators
otherwise. This is a labelling decision, not an architectural one, and can be deferred.

---

## 5. Architectural prerequisites

These are the pieces that must exist before the node can be built. Two of them are already
on the roadmap for other reasons, which is a large part of why this node is affordable now.

### 5.1 An eikonal distance solver (hard dependency, already planned)

The entire model is parameterised by **signed distance from the shoreline**. Beach width,
cliff band width, platform extent, and the Dean profile are all functions of it.

This must be a proper eikonal solve (`|∇φ| = 1`, fast sweeping, Zhao 2005), not a BFS or
chamfer distance transform. The reason is the one already diagnosed in the flow-map work:
chamfer metrics have maximum Euclidean error at half-angles, producing artifacts at
±22.5 degrees. In the flow map that showed up as creases. Here it would show up as a beach
whose width varies with compass direction, which on a circular island produces a visible
eight-lobed star. It would be immediately and fatally obvious.

**This is the same solver the flat-resolution fix needs.** Build it once, as shared
substrate in `ymir-nodes`, and both consumers benefit. This is a strong argument for
sequencing the eikonal work before either.

### 5.2 Sub-cell shoreline extraction (new)

The eikonal solve needs a boundary condition, and the boundary is the zero contour of
`height - sea_level`. If that contour is snapped to cell centres, the distance field is
quantised, and the beach width steps in whole-cell increments. On a gently sloping coast
this produces visible terracing running parallel to the shore, which is precisely the
artifact class the whole design is trying to avoid.

The fix is cheap: where `height` crosses `sea_level` between two neighbouring cells,
linearly interpolate the crossing position and initialise `φ` for those two cells to the
true sub-cell distance rather than to zero. This is a one-dimensional marching-squares
crossing test and it costs one pass.

Flag this as a required, non-obvious detail. It is the kind of thing that gets skipped and
then debugged for a day.

### 5.3 Deterministic connected components (new)

Distinguishing the ocean from inland lakes is a connected-components problem: flood from
the map border (or from a designated seed) over all cells below `sea_level`. Everything
reached is ocean; everything else is a lake.

Determinism requires care. A union-find with an arbitrary merge order can produce different
label assignments run to run. Two safe approaches:

- **Min-label propagation** (Jacobi, double-buffered): each cell takes the minimum label of
  itself and its neighbours, iterated to convergence. Order-independent, deterministic,
  parallel, and it fits the double-buffer pattern the engine already has for erosion passes.
  Converges in O(diameter) iterations, which is the cost, but it is a once-per-evaluation
  cost.
- **CPU union-find with a fixed scan order** (row-major) and deterministic tie-breaking.
  Faster, sequential.

Note that the priority-flood depression filling already implemented for stream-power is
adjacent machinery. It may be reusable, or at least the same code paths.

### 5.4 A directional sweep primitive (new, broadly reusable)

Fetch computation needs, for each of N wave directions, the distance from every cell back
to the nearest land in that direction. Naively this is O(cells × directions × ray length),
which is unacceptable.

The correct approach is a **shear-and-sweep**: for a fixed direction, traverse the grid
along rasterised parallel lines and carry a running accumulator ("distance since last
land"). Each direction costs one O(cells) pass. Total cost is O(cells × directions), with
directions typically 8 to 32.

This primitive is reusable well beyond coastal work:

- Sun exposure and shadowing (horizon sweeps use exactly this structure).
- Houdini's sunlight-biased thermal relaxation.
- Wind exposure, for snow accumulation and vegetation.
- Aspect-based masking generally.

Build it as substrate. It is the second most valuable thing in this document after the
eikonal solver.

### 5.5 `detail::SEA_LEVEL` (a `ymir-core` change)

The `detail` bag is described in `CLAUDE.md` as "a small map of scalar globals (seed, world
bounds, vertical scale). The only non-grid data." Sea level belongs there: it is a scalar
global that multiple nodes must agree on.

This means a new canonical detail-key constant in `ymir-core`, in the same spirit as the
layer-name constants (`layers::HEIGHT`). Core-touching, so it should be done deliberately.
It is small.

The knock-on: the stream-power node should read `detail::SEA_LEVEL` as its base level when
present, falling back to its own parameter when absent. Soft contract, same pattern as
layers. That coupling is what makes `Noise -> Stream Power -> Sea -> Coastal` produce
rivers that actually meet the sea.

---

## 6. The pipeline

Nine stages. Each is O(cells) or close to it, and all but the connected-components pass are
embarrassingly parallel.

### Stage 0: classify water

Threshold `height < sea_level` (or `< sea_level + tidal_range/2` for the upper intertidal
bound). Connected-components from the border per section 5.3. Emit `sea` mask.

Modes:
- **Ocean only**: only water connected to the map border. Inland depressions stay dry.
- **Ocean and lakes**: inland depressions become lakes, each filled to its own spill
  elevation (from the priority-flood machinery), not to `sea_level`.
- **All below level**: everything below the threshold is water at `sea_level`. The naive
  mode, and the one every other tool defaults to.

The `open_boundary` parameter decides whether off-map is treated as open ocean (the usual
case) or as land (for an inland scene).

### Stage 1: signed distance field `φ`

Eikonal solve from the sub-cell shoreline (5.1, 5.2). Convention: `φ > 0` on land, `φ < 0`
in water. Fast sweeping with a fixed sweep order.

### Stage 2: shoreline geometry

Because `|∇φ| = 1` by construction, two useful quantities are almost free:

- **Shore normal**: `n = ∇φ`, already unit length.
- **Shoreline curvature**: `κ = ∇·(∇φ / |∇φ|) = ∇²φ`, which is just the Laplacian of `φ`.
  One five-point stencil.

Curvature is the refraction proxy. Convex-seaward shoreline (a headland) has one sign;
concave (a bay) has the other. The sign convention must be pinned explicitly at
implementation and asserted in a test, because getting it backwards silently produces a
coast where bays erode and headlands accrete, which looks wrong but not *obviously* wrong.

### Stage 3: exposure

For each direction `θ_i` in a set spanning `wave_direction ± directional_spread`:

1. Sweep the grid along `θ_i` (5.4), computing fetch `F_i(cell)`, capped at `fetch_limit`.
2. Convert fetch to a wave height factor. Fetch-limited wave growth (SMB / JONSWAP) gives
   `H ∝ sqrt(F)`, so use `sqrt(F_i / fetch_limit)`, clamped to 1.

Then, at each shoreline cell, integrate:

```
E = Σ_i  w(θ_i) * sqrt(F_i / F_max) * max(0, cos(θ_i - normal_angle))^p
```

- `w(θ_i)` is the directional weight from the swell distribution (a cosine-power spread
  about `wave_direction`).
- The `max(0, cos(...))` term is essential: waves cannot strike the back of a coast. A
  leeward shore has near-zero exposure even if the fetch there is large.
- `p ≈ 1 to 2` controls how sharply energy falls off with obliquity.

Then modulate:

- **Refraction**: `E *= (1 + refraction_gain * κ_normalized)`. Headlands gain, bays lose.
  This is the cheap proxy for wave-ray convergence and it is the single most visually
  important term in the whole model.
- **Nearshore dissipation** (phase 1.5, optional): sample the bathymetry seaward along
  `-n` and reduce `E` where the approach is a long shallow ramp. A steep-to coast takes
  the full hit; a wide shelf attenuates. This is where the platform's negative feedback
  would live if it is modelled.

Emit as the `exposure` layer, normalised to `[0, 1]`.

### Stage 4: cliff retreat (erosional)

This is where the level-set formulation pays off elegantly.

The retreat distance at each shoreline point is:

```
R = cliff_retreat * f(E) * (1 - rock_hardness)
```

The response curve `f` matters, and specifically it needs a **threshold**: below some
energy, nothing happens at all. That is what makes some stretches of coast retreat hard
while others stay entirely intact, and that contrast is a large part of what makes the
result read as rock rather than as a blur.

Sunamura's relation gives the physical form (`dX/dt = C ln(Fw / Fr)`), and the logarithm is
where the threshold comes from. We do not need the logarithm. A `smoothstep` between
`erosion_threshold` and 1.0 produces the same shape, is easier to tune, and gives the user a
knob (the threshold position) that directly controls how much of the coast is hard rock
versus soft. Keep the behaviour, drop the formula.

**Advancing the shoreline landward by `R` is literally `φ ← φ - R`**, because `φ` is a
signed distance field with `|∇φ| = 1`. Subtracting a distance from a distance field moves
the zero contour by that distance along the normal. No contour tracing, no advection, no
iteration. One subtraction.

(The subtlety: `R` varies spatially, so the result is no longer an exact distance field.
For small `R` relative to feature size this is fine. For large retreat, reinitialise `φ`
with a second eikonal solve. Budget for that.)

### Stage 5: cut the platform and the cliff face

With the new `φ`, both cuts are per-cell clamps. No iteration.

**Platform** (seaward of the new shoreline, `φ < 0`, out to `underwater_reach`):

```
h_platform = sea_level - platform_slope_tan * |φ|
h ← min(h, h_platform)          // only cut, never fill
```

Blend the clamp out to nothing as the depth approaches `underwater_reach` so the node does
not touch deep bathymetry.

Tidal range widens the band over which this is applied and, at large tidal range, replaces
the sub-horizontal bench with a ramp (section 3.1).

**Cliff face** (landward, `φ > 0`, within the cliff band):

```
h_cliff = sea_level + berm_height + cliff_slope_tan * φ
h ← min(h, h_cliff)             // only cut
```

This is a single per-cell max-slope clamp measured from the shoreline, which is cheap and
exactly right: it produces a cliff of the specified angle rising from the shore, and it
leaves any terrain already below that envelope untouched.

**Cliff-top rounding**: a short thermal-style talus relaxation confined to the cliff band,
weighted by proximity to the cliff top. Reuse the existing thermal erosion pass with a
mask rather than writing new code.

Accumulate everything removed into the `wear` layer.

### Stage 6: the sediment budget

Total sediment available:

```
S = (volume removed in stages 4-5) * beach_amount
  + (incoming `sediment` layer, if present)
  + (delta contribution from `flow` at river mouths, if `flow` present)
```

The delta term is the interoperability payoff of the shared layer vocabulary: if a
stream-power node upstream has written `flow`, then cells where a high-accumulation channel
meets the shoreline are river mouths, and they inject sediment. That produces deltas and
alluvial fans at river mouths for almost no extra code, and it is exactly the kind of
cross-model coupling that justifies the canonical layer vocabulary in the first place.

### Stage 7: alongshore redistribution (the part that matters)

Depositing sediment where it was eroded is wrong (section 3.4). It must be transported
alongshore and deposited where the transport capacity drops.

**Phase 1 (cheap, defensible):** Transport capacity along the shore is
`Q ∝ E^(5/2) * sin(2 * alpha)`, where `alpha` is the angle between the wave direction and
the shore normal. Both are already computed. Sediment then diffuses along the shoreline
tangent `t = perp(∇φ)` down the gradient of `Q`, and deposits where `∇·Q < 0` (convergence).

Implement as an anisotropic diffusion of the sediment budget with the diffusion tensor
aligned to `t`, restricted to a narrow band around the shoreline. A handful of Jacobi
iterations, double-buffered, order-independent, deterministic. This is the same numerical
pattern the engine already uses for thermal erosion.

This is enough to move sediment out of exposed headlands and into sheltered bays, which is
the behaviour that makes the result read as a real coast.

**Phase 2 (the big feature, deferred):** Let the shoreline itself evolve under the
transport divergence, rather than only redistributing sediment on a static shoreline. In
level-set terms:

```
∂φ/∂t + F |∇φ| = 0,    F = a * (∇·Q) - b * κ + c * (erosion rate)
```

Evolve `φ` for N steps, reinitialising periodically, then rebuild the height from stages
5 and 8 against the evolved shoreline. This is what grows spits across bay mouths, builds
tombolos behind islands, and produces the Ashton-Murray cuspate instability under
high-angle waves. It is a substantial piece of work and should be its own workstream, but
the phase 1 design does not foreclose it: the same `φ`, `E`, `κ`, and `Q` fields are the
inputs.

### Stage 8: build the beach (depositional)

With a per-shoreline-segment sediment budget from stage 7, construct the target profile
and fill up to it, capped by the available budget.

**Seaward** (`φ < 0`), the Dean profile:

```
h_target = sea_level - dean_A * |φ|^(2/3)
h ← max(h, h_target)            // only fill, never cut
```

blended out at `underwater_reach`.

**Landward** (`φ > 0`), the beach face and berm:

```
h_target = sea_level + min(berm_height, beach_face_tan * φ)
h ← max(h, h_target)
```

so the profile rises across the swash zone at the beach-face slope and then flattens at the
berm crest.

`dean_A`, `beach_face_tan`, and `berm_height` are all derived from grain size and wave
climate (section 3.2), not exposed as free sliders by default.

**The budget cap is what makes this look right.** Where the sediment budget is small, the
fill only reaches a fraction of the target profile, producing a thin fringing beach or a
bare rocky shore. Where the budget is large (a sheltered bay receiving drift from two
headlands), the full profile builds out and the shoreline actually prograded seaward. That
contrast, beach here and bare rock there, is the thing no other tool produces.

Accumulate thickness added into the `deposition` layer.

### Stage 9: emit water and masks

- `water`: depth, recomputed against the *final* height. Zero on land.
- `shore`: the intertidal plus surf band, a soft `[0, 1]` mask derived from `φ` and
  `tidal_range`. This is the highest-value texturing output and should be smooth, not
  binary.
- `exposure`, `wear`, `deposition`, `sea` as above.

---

## 7. Parameter schema

The control surface is designed against section 1.1: **the primary controls are the things
that change how it looks**, named after things a user has seen rather than things a
sedimentologist has measured. Physics lives under the hood, in the shape of the response
curves, not on the front of the panel.

Defaults are placeholders pending tuning by eye.

### 7.1 The two rules

**Rule 1: no raw slope sliders on things that should be fields.**

Beach slope, berm height, and the underwater profile curve all vary with local wave energy,
which varies along the coast. Exposing any of them as a scalar collapses that variation and
silently downgrades the node to World Machine's bevel. But *deriving* them and hiding the
control is equally bad: the user then has no way to say "steeper, please", which is the
first thing they will want.

The answer is the distinction the survey already isolated in Gaea's Selective Processing:
**modulate, do not confine.** Every derived field gets a **bias multiplier**, default 1.0.
Turn `beach_slope_bias` up and every beach gets steeper, but the exposed headland stays
steeper than the sheltered bay. The look survives; the control is direct and obvious.

A hard uniform override still exists for the user who wants one specific slope everywhere,
but it is an explicit mode with an honest label (`uniform_beach_slope`), not the natural
next click after the bias slider.

**Rule 2: name inputs after things people have walked on.**

`D50 = 0.4 mm` is a measurement. "Shingle" is a memory. Anyone who has walked on a shingle
beach and then a sand beach already knows one is steeper, without ever having heard the
word Iribarren. Grain size is therefore an enum of materials, with the millimetres as an
advanced readout rather than the primary control.

### 7.2 Presets

A `coast_type` enum at the top of the panel that expands into the parameters below. This is
Gaea's Wizard idea and it costs almost nothing: a table of parameter sets.

`Rocky Atlantic`, `Chalk Cliffs`, `Tropical Sand`, `Shingle Coast`, `Sheltered Bay`,
`Fjord`, `Storm Coast`, `Custom`.

The user starts here, gets something that looks like a coast, and then tunes. Nobody should
have to build a coast from eighteen sliders on first contact.

### 7.3 Water

| Param | Type | Default | Notes |
|---|---|---|---|
| `sea_level` | float | 0.35 | Normalized height. Reads `detail::SEA_LEVEL` if set upstream |
| `water_mode` | enum | Ocean | Ocean / Ocean and lakes / All below level |
| `open_boundary` | bool | true | Is off-map open ocean, or land |
| `tidal_range` | float (world) | 2 m | Widens the intertidal band. Large values give a sloping ramp instead of a bench |

### 7.4 Waves (the master energy controls)

| Param | Type | Default | Notes |
|---|---|---|---|
| `wave_height` | float (world) | 2 m | The master energy dial. Scales retreat, berm, underwater reach, beach flatness. Kept in metres because it is intuitive and it grounds the world-unit derivations |
| `wave_direction` | float (deg) | 270 | Dominant swell azimuth. The single most visible parameter after sea level |
| `directional_spread` | float (deg) | 45 | 0 = one direction, 180 = waves from everywhere. Low values give strong windward/leeward asymmetry |

`wave_period` is **cut** (see 3.3). It fed only the closure depth and the Iribarren number,
both of which are gone.

### 7.5 Material

| Param | Type | Default | Notes |
|---|---|---|---|
| `sediment_type` | enum | Sand | Silt / Fine sand / Sand / Coarse sand / Shingle / Cobble. Drives beach slope and the underwater profile curve |
| `rock_hardness` | float + mask | 0.5 | Resists cliff retreat and platform cutting. **The mask is where sea stacks and differential-erosion coastlines come from**, so it is high-value, not an afterthought |

### 7.6 Shape (the look dials)

| Param | Type | Default | Notes |
|---|---|---|---|
| `cliff_retreat` | float (world) | 50 m | How far back the cliffs cut at maximum exposure. Resolution-independent |
| `cliff_angle` | float (deg) | 75 | Maximum subaerial slope near the shore |
| `cliff_rounding` | float (world) | 10 m | Talus softening at the cliff top. Turns a knife-edge into weathered rock |
| `beach_amount` | float | 0.7 | Master deposition dial. How much beach appears at all. 0 gives a bare rocky coast |
| `beach_slope_bias` | float | 1.0 | Multiplier on the derived beach-slope field (Rule 1) |
| `berm_height_bias` | float | 1.0 | Multiplier on the derived berm height |
| `platform_slope` | float (deg) | 1.5 | Seaward gradient of the shore platform |
| `underwater_reach` | float (world) | derived, `2 * wave_height` | How deep the node reaches before fading out. Directly editable |

### 7.7 Transport

| Param | Type | Default | Notes |
|---|---|---|---|
| `longshore_strength` | float | 0.5 | Alongshore redistribution. **0 deposits sediment where it was eroded, which is visually wrong.** This is the dial that puts beaches in bays |
| `delta_deposition` | float | 0.5 | River-mouth sediment injection. Active only if a `flow` layer is present |

### 7.8 Advanced

| Param | Type | Default | Notes |
|---|---|---|---|
| `refraction_gain` | float | 0.5 | Strength of the curvature focusing term. Higher values erode headlands harder. 0 disables |
| `erosion_threshold` | float | 0.15 | Energy below which nothing erodes at all. Controls how much of the coast is untouched rock |
| `fetch_limit` | float (world) | 50 km | Cap on ray length |
| `uniform_beach_slope` | optional float | off | Hard override. Collapses the beach-slope field to one value everywhere. Present, labelled honestly, not encouraged |
| `conserve_volume` | bool | false | Force `Σ wear == Σ deposition`. Costs a deterministic global reduction |
| `sediment_d50` | float (mm) | derived | Advanced readout / override on `sediment_type` |

### 7.9 Derived readouts

Small GUI ask, large discoverability payoff: show the derived values live in the panel, next
to the control that changes them.

| Readout | Driven by |
|---|---|
| Beach face slope (range across the coast) | `sediment_type`, wave energy, `beach_slope_bias` |
| Berm height | `wave_height`, `berm_height_bias` |
| Underwater reach | `wave_height`, or the override |

Dragging `sediment_type` from Sand to Shingle and watching "Beach face slope: 3° to 14°"
update in place teaches the mapping in one interaction. The user never needs the manual, and
never needs to know why. That is most of the discoverability problem solved for the cost of
a label.

This has a small implication for the GUI: `ParamSpec` currently describes inputs, and a
read-only derived value is a new kind of thing. It could be a tooltip, a new `ParamSpec`
kind, or just documentation. Minor, but it should be filed rather than discovered.

---

## 8. Layers and vocabulary

The canonical vocabulary is currently `height`, `wear`, `deposition`, `flow`, `flowdir`,
`water`, `sediment`. This node reads and writes within it and proposes three additions:

**Reads (all soft, all optional):**

| Layer | Use |
|---|---|
| `height` | Required (primary) |
| `water` | If present, use its level rather than recomputing |
| `erodibility` | Modulates cliff retreat and platform cutting. Produces sea stacks |
| `flow` | River mouths for delta deposition |
| `sediment` | Additional sediment supply into the littoral budget |
| `mask` | Standard per-node mask, per `layer_or(layers::MASK, 1.0)` |

**Writes:**

| Layer | Status | Content |
|---|---|---|
| `height` | existing | Modified |
| `water` | existing | Depth, zero on land |
| `wear` | existing | Bedrock removed |
| `deposition` | existing | Sediment thickness added |
| `exposure` | **new** | Normalized wave energy `[0, 1]` |
| `shore` | **new** | Soft intertidal and surf band mask `[0, 1]` |
| `sea` | **new** | Ocean vs. land vs. lake classification |

**Vocabulary decision required.** `exposure` is the significant one, because it generalises
beyond coastal: sun exposure, wind exposure, and snow all want an "exposure" field, and they
are not the same field. Options:

1. Name it `wave_energy`, specific and unambiguous, and let sun and wind have their own.
2. Name it `exposure` and accept that whichever node last wrote it wins.
3. Namespace it: `exposure.wave`, `exposure.sun`, `exposure.wind`. The layer map is a
   `BTreeMap<LayerId, _>` and dotted names cost nothing, but it introduces a naming
   convention the engine does not currently have.

My recommendation is (1), `wave_energy`, on the principle that the vocabulary should be
specific and non-colliding, and that a future generic-exposure concept can be introduced
deliberately rather than by accident. But this is a decision for you and it should be made
before the layer name ships, because renaming a canonical layer later breaks saved projects.

Per `CLAUDE.md`, all three are constants, not bare strings.

---

## 9. Determinism

This model is a **good determinism citizen**, which is a pleasant contrast to droplet
erosion. Under the surgical-determinism stance, here is where the constraints bind and
where they do not.

| Stage | Determinism | Notes |
|---|---|---|
| Water classification | **Must be exact** | Label assignment is discrete. A different ocean/lake classification is not a perceptual difference, it is a different terrain. Use min-label Jacobi (order-independent) or a fixed-order union-find |
| Eikonal solve | Deterministic | Fast sweeping with a fixed sweep order is deterministic by construction |
| Sub-cell shoreline | Deterministic | Pure per-cell arithmetic |
| Directional sweeps | Deterministic | Sequential along each ray, parallel across scanlines. No cross-cell reduction |
| Exposure integration | Deterministic | Per-cell sum over a fixed direction list in a fixed order. No parallel reduction, so no floating-point ordering hazard |
| Level-set retreat | Deterministic | One subtraction |
| Profile clamps | Deterministic | Pure per-cell |
| Alongshore diffusion | Deterministic | Jacobi double-buffer, order-independent per step |

The only global reduction in the whole pipeline is the **total sediment volume** when
`conserve_volume` is enabled. That is a sum over the grid, and a parallel sum is
order-dependent in floating point. Either compute it in a fixed order (single-threaded, or a
deterministic tree reduction), or accept that volume conservation is approximate to within
float epsilon and note it. The former is cheap; do that.

There is **no random number generation anywhere in this model**. It does not need a seed.
That is worth noting in the `NodeSpec` context-dependency declaration: the node can declare
that it does not depend on the seed, which avoids over-invalidating its cache entry when the
global seed changes.

---

## 10. GPU

Every stage except water classification is a natural fit for the committed wgpu compute
path, and the shapes are ones the engine already has:

- Eikonal fast sweeping: sequential in sweep direction, but the Fast Iterative Method (Jeong
  and Whitaker) is a parallel variant. Alternatively, keep it on the CPU: it is O(cells) with
  a small constant and runs once.
- Directional sweeps: parallel across scanlines, one thread per line. Very GPU-friendly.
- Exposure integration: per-cell, no communication. Trivially parallel.
- Profile clamps: per-cell. Trivially parallel.
- Alongshore diffusion: Jacobi double-buffer, exactly the pattern already committed for
  erosion passes.
- Water classification: the awkward one. Min-label propagation is parallel but converges in
  O(diameter) iterations, which on a 4K map is thousands of passes. Better to run it on the
  CPU (once, cheaply, with union-find) and upload the result.

So the GPU story is: CPU does the classification, GPU does everything else, and the whole
thing is well within the architecture already being built for the Mei pipe model. Coastal
does not force any new GPU seam.

---

## 11. The tiling hazard (important)

**Exposure is inherently non-local and it breaks the halo assumption.**

`CLAUDE.md` promises that "a tiled build matches an untiled build at the same resolution
(halo overlap handles seams)." That promise depends on every operator having a bounded
region of influence, so a halo of sufficient width makes a tile's computation exact.

Fetch does not have a bounded region of influence. A fetch ray can be tens of kilometres
long. A tile cut from the middle of the map has no idea whether the water at its edge opens
into an ocean or terminates against a landmass a hundred cells beyond the halo. The
classification pass has the same problem: whether a body of water is connected to the ocean
is a global question.

This is not a small caveat. It is the one place where this design conflicts with an existing
promise, and it needs a deliberate answer. Three options:

1. **Compute the global fields once at a coarse resolution over the whole map, then upsample
   into each tile.** This is legitimate and I think correct. Both the exposure field and the
   water classification are *smooth or piecewise-constant at large scale*: exposure varies
   over kilometres, not metres, and ocean-vs-lake is a topological fact that does not change
   with resolution. Computing them at, say, 512 squared over the full extent and bilinearly
   sampling them into a 4K tile loses nothing perceptually and preserves seam consistency
   exactly, because every tile samples the same global field. The local reshaping (stages 4,
   5, 8) then runs tiled at full resolution with an ordinary halo, because those stages *are*
   local, bounded by the cliff band plus the closure-depth extent.

2. Require the full map in memory for coastal nodes and forbid tiling on them. Simple,
   honest, and it caps the maximum coastal build size.

3. Accept seam mismatch. No.

**Recommendation: option 1**, and it implies an architectural concept the engine does not
yet have: a **global precomputed field** that is evaluated once at coarse resolution over the
full extent and made available to tiles. That is a real seam and it should be designed
deliberately, not bolted on. It is also likely to be needed again (any node that depends on
long-range visibility, sun exposure being the obvious next one).

Flag this as the single highest-risk item in the design.

---

## 12. Open questions

Ordered roughly by how much they need an answer before implementation starts.

1. **`exposure` vs `wave_energy` as the layer name.** Must be settled before the layer name
   ships, because renaming a canonical layer breaks saved projects. My recommendation is
   `wave_energy`.

2. **The global-field seam (section 11).** Which of the three tiling options? If option 1,
   the coarse-global-field concept needs designing, and it probably deserves its own issue
   ahead of the coastal work.

3. **`detail::SEA_LEVEL` in `ymir-core`.** Confirm the detail bag is the right home, and
   confirm the stream-power node should read it as base level.

4. ~~**Is the platform's negative feedback modelled?**~~ **Resolved: no, and it is cut.** A
   widening shore platform dissipates more wave energy and slows further cliff retreat, which
   is a real self-limiting mechanism. Modelling it means making stages 3 through 5 iterative
   rather than single-shot, which is a meaningful cost. Its only visible effect is *less
   retreat than the user asked for*, which the `cliff_retreat` slider already provides for
   free. This is the textbook case of physics that does not buy a look. Cut, not deferred.
   (The pipeline stays loop-friendly anyway, since nothing in stages 3 to 5 forbids it, but
   no work is done to enable it.)

5. **Phase 2 scope.** Is the level-set shoreline evolution (spits, tombolos, cuspate
   forelands) in the near roadmap or the far one? It is the most distinctive thing in this
   document and also the largest. It could be a separate `Littoral` node that consumes the
   `Coastal` node's outputs, which would keep `Coastal` shippable without it.

6. **Lake levels.** In "Ocean and lakes" mode, does each lake fill to its own spill elevation
   (correct, and the priority-flood machinery already computes it), or to `sea_level`
   (simpler, and what every other tool does)? I think per-lake spill elevation, but it is
   more work and it interacts with how the fluvial node handles depressions.

7. **Does `Coastal` also handle lakes?** A lake shore has fetch, waves, and beaches too, just
   smaller ones. The model works unchanged; the only difference is that fetch is bounded by
   the lake. Running the same node on lakes for free is appealing. The risk is that the
   default parameters (2 m waves, 50 m retreat) are absurd for a small lake and the user gets
   a mangled result. Possibly a `water_bodies` enum: Ocean only / Lakes only / Both.

8. **How does the GUI show a read-only derived readout (7.9)?** `ParamSpec` describes inputs;
   a derived value is a new kind of thing. Tooltip, new `ParamSpec` kind, or documentation.
   Small, but it touches the GUI's parameter introspection, and the teaching effect described
   in 7.9 is worth more than its cost.

9. **The preset table (7.2).** Worth agreeing the list and the values early, because presets
   are the primary interface for most users and they are also the best possible tuning
   harness: if the eight presets all look good, the model is tuned.

---

## 13. Testing

The tests here are unusually good at catching the failure modes, which is worth exploiting.

**The circular island test (the isotropy canary).** Generate a radially symmetric cone
island. Run `Coastal` with `directional_spread = 180` (omnidirectional waves), so the
physics is rotationally symmetric. Assert that the output is rotationally symmetric to
within tolerance: sample the `shore`, `wear`, and `deposition` layers on rings and assert
low angular variance.

This single test catches, immediately and unambiguously:
- chamfer-distance anisotropy in the eikonal solver (an eight-lobed star appears)
- direction-sampling bias in the fetch sweeps (an N-lobed star appears, N = direction count)
- grid-aligned bias in the alongshore diffusion

It is exactly the artifact class that burned the flow-map work, and here it is trivially
detectable. Write this test first, before the node works, and use it as the development
harness.

**Directional test.** Same cone island, `directional_spread = 0`, wave direction due west.
Assert that the western shore has high `wave_energy`, `wear`, and cliff, and the eastern
shore has near-zero exposure and an intact original slope. This is the test that catches an
inverted `cos(θ - normal)` term.

**Refraction sign test.** A terrain with one headland and one bay. Assert `wave_energy` is
higher on the headland than in the bay. This catches the curvature-sign inversion, which is
otherwise very easy to get backwards and hard to spot by eye.

**Sediment convergence test.** Same headland-and-bay terrain. Assert `deposition` in the bay
exceeds `deposition` on the headland. This is the behavioural claim the whole design rests
on, so it should be asserted, not assumed.

**Fetch bounds test.** An enclosed lagoon connected to the ocean by a narrow inlet. Assert
lagoon-interior exposure is near zero and open-coast exposure is near maximum.

**Volume conservation test.** With `conserve_volume = true`, assert `Σ wear ≈ Σ deposition`
within a stated tolerance, and assert the reduction is computed in a deterministic order.

**Monotonicity test.** Raising `sea_level` must never raise any cell's terrain. A cheap
invariant that catches sign errors in the fill/cut clamps.

**Determinism test.** Run twice, assert identical bytes. Per `CLAUDE.md`, every node gets
one.

**Golden snapshot.** Hash the output of a fixed island at fixed parameters.

**Property test.** `wear` and `deposition` are non-negative everywhere. `water` is zero
wherever `height > sea_level`. Both are the kind of invariant `proptest` is good at.

---

## 14. Workstreams

Suggested decomposition, roughly in dependency order. Lettered to match the erosion
document's convention, continuing from it.

**Prerequisites (substrate, valuable independently):**

- **P1. Eikonal distance solver.** Fast sweeping, `|∇φ| = 1`, in `ymir-nodes` terrain math.
  Shared with the flow-map flat-resolution fix, which is the other consumer. Includes the
  isotropy test on a radially symmetric input.
- **P2. Sub-cell contour extraction.** Crossing-interpolated boundary initialisation for P1.
- **P3. Deterministic connected components.** Ocean/lake classification. Check for overlap
  with the existing priority-flood implementation.
- **P4. Directional sweep primitive.** Shear-and-sweep fetch. Reusable for sun, wind, snow.

**Core changes:**

- **C1. `detail::SEA_LEVEL`.** New canonical detail key in `ymir-core`. Small, core-touching.
- **C2. Layer vocabulary additions.** `wave_energy` (or `exposure`), `shore`, `sea` as
  constants. Blocked on decision (12.1).
- **C3. Stream-power reads `detail::SEA_LEVEL` as base level.** Soft contract.

**The global-field seam:**

- **G1. Coarse global precomputed fields.** The tiling answer from section 11. Highest risk,
  and arguably should be designed before the coastal node rather than alongside it.

**The nodes:**

- **N1. `Sea` node.** Sea level, classification, `water` and `sea` layers, `detail` write.
  Depends on P3, C1, C2.
- **N2. `Coastal` phase 1.** Exposure, cliff retreat, platform, beach profile, local
  deposition. Depends on P1, P2, P4, C2, N1.
- **N3. `Coastal` alongshore redistribution.** Stage 7 phase 1. Depends on N2.
- **N4. Delta deposition from `flow`.** Stage 6 river-mouth term. Depends on N3 and a
  working stream-power node.
- **N5. Erodibility masking and sea stacks.** Depends on the general per-parameter mask work
  already on the erosion roadmap.

**Deferred:**

- **D1. `Littoral` node: level-set shoreline evolution.** Spits, tombolos, cuspate forelands.
  The Ashton-Murray instability. Large, distinctive, and probably its own node consuming
  `Coastal`'s outputs.
- **D2. `Exposure` node.** Standalone wave/sun/wind exposure field. Thin wrapper over P4.

**Cut, not deferred:**

- Platform negative feedback. Physics with no visible payoff. See open question 4.
- Wave period, the Iribarren number, the Hallermeier closure-depth formula, the Sunamura
  logarithm. Each replaced by a direct knob that lands in the same visual place.

**Tuning (not a code workstream, but the one that decides whether this succeeds):**

- **T1. The preset table (7.2).** Eight coasts that look good. This is the acceptance test
  for the whole node, and it is where the response curves actually get tuned. Budget real
  time for it, and do it with pictures, not unit tests.

---

## 15. References

**Coastal geomorphology**
- Sunamura, T. (1992). *Geomorphology of Rocky Coasts*. The standard reference for cliff
  retreat, and the source of the `dX/dt = C ln(Fw/Fr)` relation.
- Trenhaile, A. S. Shore platform development and morphology. The Type A / Type B platform
  distinction and its tidal-range control.
- Hsu, J. R. C. and Evans, C. Parabolic bay shapes. The log-spiral equilibrium planform of
  headland-bay beaches, which is the *target* form the alongshore model is converging toward.

**Beach profiles and wave parameters**
- Dean, R. G. (1977). Equilibrium beach profiles. The `h = A y^(2/3)` relation.
- Bruun, P. (1954). Coast erosion and the development of beach profiles.
- Hallermeier, R. J. (1981). Closure depth.
- Stockdon, H. F. et al. (2006). Empirical parameterization of setup, swash, and runup.
  The berm-height derivation.
- Iribarren, R. and Nogales, C. The surf similarity parameter.
- U.S. Army Corps of Engineers, *Coastal Engineering Manual* (successor to the *Shore
  Protection Manual*). The source for the Dean `A` tables, the CERC formula, and the
  fetch-limited wave growth relations. This is the single most useful practical reference
  for the parameter derivations.

**Longshore transport and shoreline instability**
- Pelnard-Considère, R. (1956). The one-line model. Shoreline diffusion.
- Ashton, A. and Murray, A. B. (2001), *Nature*. High-angle wave instability and the
  formation of capes and spits. Also Ashton and Murray (2006), JGR, for the full model.
- CERC longshore transport formula, in the Coastal Engineering Manual above.

**Numerics**
- Osher, S. and Sethian, J. A. (1988). Level set methods.
- Zhao, H. (2005). A fast sweeping method for eikonal equations. The recommended solver.
- Sethian, J. A. Fast marching methods. The alternative to fast sweeping.
- Jeong, W-K. and Whitaker, R. (2008). The Fast Iterative Method. The parallel eikonal
  variant, relevant if the solve moves to the GPU.
- Sussman, M. et al. Level-set reinitialisation.

**Already in the survey, relevant here**
- Barnes et al., priority-flood depression filling (reused for lake fill levels).
- Mei, Decaudin, Hu (2007), for the pipe model whose GPU infrastructure this node shares.
