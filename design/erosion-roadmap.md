> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: the erosion roadmap

Status: **superseded** by [the erosion strategy and design brief](ymir-erosion-DESIGN.md)
(the three-transport-drivers framing, the GPU grid-hydraulic flagship, the revised determinism
stance, and the build plan). Kept for the competitive/technical survey synthesis it records;
the live plan is the brief.

Original status: **direction, being worked through.** Synthesised from a competitive and technical
survey of erosion in other tools (Gaea, World Machine, Houdini, World Creator, Hesiod/HighMap,
Instant Terra), plus what Ymir already learned building Thermal, Stream, and Hydraulic. This is
the map we prioritise against; individual items still get their own design note and review
before code.

Related notes: [mask & selection model](mask-and-selection-model.md) (the directability
mechanism, largely built), [control fields & directability](control-fields-and-directability.md)
(where direction is going), [GPU erosion](gpu-erosion.md) (optional acceleration path),
[node categories](node-categories.md).

## 1. What the field agrees on

Across every tool surveyed and the algorithm literature, the same handful of ideas recur. These
are the load-bearing themes, ranked by how universal they were.

1. **Named byproduct layers are the real product, and they exist for texturing.** Every serious
   tool emits `flow`, `flowdir`, `wear`, `deposition`/`sediment`, `water`, `debris` as
   first-class layers, and the texturing pipeline runs on them. Houdini binds them by name
   (height, bedrock, debris, sediment, water, flow, flowdir); Gaea and World Machine expose
   Flow/Wear/Deposits as extra outputs. Artist caution worth heeding (stated by Gaea): wear and
   slope texture convincingly, raw flow alone reads as fake. Hydro `sediment` (fine) and thermal
   `debris` (broken) are kept distinct, coupled by a conversion rate.

2. **Masks modulate, they do not gate, and they are everywhere.** The primary direction
   mechanism in every tool is a per-parameter mask. Gaea draws the useful distinction between
   *confine* (plain mask, effect only inside) and *modulate* (Selective Processing: the sim runs
   everywhere but a parameter like rock softness or precipitation varies by the mask). This is
   exactly Ymir's soft-layer contract.

3. **Erosion nodes are cohesive models, not a carve/smooth/deposit construction kit.** Users
   wire complete models. None of the tools expose erosion as primitive operators to assemble.

4. **Multi-pass stacking at decreasing feature size is the assumed workflow, not a trick.**
   Houdini's idiom is Noise then Erode then Distort then Erode at finer scale, never resampling
   between passes. Gaea stacks erosion passes where the first lays down flow structure the next
   respects. "One pass never looks right at every scale" is universal.

5. **Hardness / bedrock / strata.** Variable erodibility plus a bedrock floor that bounds
   incision and exposes rock structure. Houdini's strata erodibility ramp is how you get
   terracing, mesas, and buttes. The literature flags uniform-solubility as the standard
   unrealism. Present, in some form, in every tool.

6. **Precipitation is an input, not an assumption.** Houdini splits Precip into its own node
   that only writes the `water` layer; erosion reads it. Gaea adds orographic rain-shadow. This
   makes rainfall directable and reusable.

7. **Resolution honesty: preview as approximation, build as truth, tiled build with halo
   overlap.** World Machine and Houdini both confirm Ymir's existing stance. Controls expressed
   in world units / feature size, not raw iteration counts.

8. **Shared structural primitives.** Priority-flood depression filling (Barnes et al.) before
   flow accumulation; flow routing as D8 / D-infinity / multiple-flow-direction. HighMap factors
   these as reusable functions.

## 2. The algorithm families (and which Ymir should bet on)

| Family | What it produces | Determinism | Ymir status |
|---|---|---|---|
| Thermal / talus (diffusive relaxation) | Scree, rounded ridges, talus slopes | Easy: order-independent with double buffering | Built (Thermal) |
| Cell pipe-model hydraulic (Mei et al.) | Full water surface, channels, pooling, deposition | Deterministic if state arrays double-buffered | Built, rough (Hydraulic) |
| Particle / droplet hydraulic | Rills, local detail, cheap on large maps | **Problem child**: scattered read-modify-write, order-dependent | Not built, and we should not |
| Stream-power / drainage fluvial (Braun-Willett) | Grand dendritic networks, watersheds, valley profiles | Deterministic with stable tie-breaks | Built, our flagship (Stream) |
| Coastal / sea / lake | Beach-and-bluff, water surface | Geometric, trivially deterministic | Not built |
| Glacial | U-valleys, cirques, fjords | n/a | Not built, niche |

The pivotal finding: **genuinely physical stream-power fluvial erosion is rare in commercial
tools.** Gaea fakes the dendritic look, World Machine approximates it with an uplift mechanism,
and Houdini does not ship a stream-power node at all (its hydro is transport-capacity only).
Ymir's Stream node (real Braun-Willett FastScape with multiple-flow-direction routing) is
therefore a genuine differentiator, not a catch-up feature.

Determinism cuts the same way. Thermal, the pipe model, and stream-power are all
deterministic-friendly. Particle/droplet is the one family that fights byte-stability (Gaea
ships a single-core "Deterministic" toggle precisely because its parallel core is not
order-stable). Since same-machine repeatability is a hard Ymir promise, **we skip particle as a
core model.** Stream plus the pipe model cover its use cases deterministically.

## 3. Ymir's current position

What we have: Thermal (solid), Stream (the flagship, just rebuilt as iterative stream-power with
MFD routing, depression filling, base level), Hydraulic (pipe model, committed but rough and
needing tuning).

Validated by the survey:
- The Stream investment was right: it is the capability the reference tools lack.
- Cohesive erosion models is the correct grain (every tool agrees).
- Resolution honesty (preview vs build, halo overlap, world-unit controls) matches Houdini and
  World Machine exactly.
- The masking direction ([mask & selection model](mask-and-selection-model.md)) matches the
  universal "masks modulate, do not gate" theme; Slope and Height selectors already exist.

Gaps the survey exposes:
- No formal canonical set of erosion byproduct layers (the texturing currency).
- Masking is per-some-nodes, not yet a uniform per-parameter convention on erosion.
- Depression filling and flow accumulation live inline in Stream, not as shared primitives.
- No bedrock / hardness / strata layer; erosion treats terrain as uniformly soluble.
- No precipitation input; rainfall is assumed uniform.
- No deposition-as-a-feature beyond what each sim does internally.

## 4. Architectural decisions

These are the forks the roadmap rests on. Recommendations stated; to be confirmed.

- **North star: physically grounded, directable, deterministic.** Lead with the real Stream
  model (the differentiator), make every model directable through masks (match the commercial
  strength), keep it deterministic (our promise, and a differentiator versus Gaea's single-core
  toggle), and add a fast non-simulation "look" tier for speed and preview. This positions Ymir
  as the tool with real fluvial structure plus art direction, not a Gaea clone.

- **Node decomposition: Houdini's model.** Decomposed cohesive primitives (Stream, Hydraulic,
  Thermal) plus, over time, a convenience aggregate for the common case. Push masking,
  precipitation, selection, and deposition out into composable nodes. Avoid World Machine's fat
  monolith and Gaea's duplicate engines. This is also Ymir's existing "many small nodes"
  philosophy.

- **Stream and Hydraulic have distinct, documented roles.** Stream owns macro dendritic fluvial
  structure (drainage-area driven incision, watersheds, valley networks). Hydraulic owns local
  water behaviour (channel cross-sections, pooling, fine deposition, the water surface). They
  compose; they are not competitors. Documenting this resolves the confusion that drove the last
  session.

- **The workflow principle: form, then erode, then detail.** Erosion carves drainage into an
  existing regional form; it does not invent form, and it cannot create detail that is not in
  the input. The pipeline is large-scale form, then erosion, then high-frequency detail layered
  on top. The roadmap encodes this through example graphs and the control-fields direction, not
  through trying to make erosion do everything.

- **Skip particle erosion** (determinism cost, and Stream plus pipe cover it).

## 5. The roadmap

Phased by dependency and leverage. Earlier phases unblock later ones.

### Phase 0: foundations (unblock everything)
- **Canonical erosion layer vocabulary.** Define `layers::` constants for the byproduct set:
  `flow`, `flowdir`, `water`, `sediment`, `debris`, `wear`, `deposition`, plus `erodibility` and
  `bedrock` as inputs. This is the highest-leverage move: it is the texturing currency and the
  inter-node chaining glue.
- **Shared terrain-math primitives.** Extract priority-flood depression filling and flow
  accumulation (MFD) from Stream into reusable functions, so future drainage nodes share them.
- **Per-parameter mask convention on erosion.** Formalise optional mask inputs that modulate
  (soft-layer contract), with Gaea's modulate-vs-confine distinction in mind.
- **Real slope: the vertical-to-horizontal ratio in eval.** Today heights are normalised [0, 1]
  and `world_height` (the vertical scale) is display/export only, deliberately kept out of
  `EvalContext` to avoid cache churn (see the world-height-display-not-eval decision). Erosion is
  the trigger we said would force this in. Real slope is `rise / run = (Δh · world_height) / Δx`;
  without the vertical scale, slope is in normalised units. For stream-power this folds into the
  `K` constant (pattern unchanged, only magnitude), but for **thermal it is essential**: a talus
  *angle* (35 degrees) is only meaningful against a real slope, and the Slope and Curvature
  selectors have the same need. Thread the vertical:horizontal ratio into `EvalContext` so every
  slope-based node (thermal talus, slope/curvature selectors, stream slope) is scale-honest and
  consistent. This is the highest-value deferred item for erosion, and it belongs in the
  foundations rather than being retrofitted under each model.

### Phase 1: directability and the texturing currency
- **Selector node family**: extend beyond Slope/Height to Curvature and Flow selectors (the
  mask & selection note already plans this), so erosion can be steered and its outputs textured.
- **Erosion nodes emit the canonical data layers** so the planned Texture/TextureSet work has
  real inputs. Heed the "wear and slope texture well, raw flow does not" caution.

### Phase 2: mature the core models
- **Stream (flagship):** add discharge/momentum accumulation for meandering and coherent rivers
  (nickmcd SimpleHydrology), a bedrock floor plus hardness, deposition, and optionally a tectonic
  uplift field (Cordonnier et al.) for steady-state landscapes.
- **Hydraulic:** finish and tune the pipe model, confirm it is deterministic via double-buffered
  state, and lock its role (local water sim and deposition).
- **Thermal:** add an erodibility mask, anisotropy / grid or sunlight bias, and scale-with-
  elevation; otherwise it is solid.

### Phase 3: realism and structure
- **Bedrock / strata / depth-varying hardness** (Houdini-style ramp) for terracing, mesas,
  buttes. A differentiator, clean fit for the layer model.
- **Precipitation node** that writes the `water` layer, with orographic / rain-shadow support.
- **Multi-scale workflow**: feature-size control and the erode-distort-erode idiom; ensure a
  pass's flow layer is consumable by the next.
- **Deposition / alluvium** as an explicit model (fans, valley fill), distinct from carving.

### Phase 4: breadth (by demand, not by default)
Prioritised by research maturity (which phenomena have solid prior art) rather than by name:
- **Established, self-contained sims that fit the heightfield + layer model** (good future
  candidates): **Glacial** (Cordonnier et al. 2023, SIGGRAPH: actually carves bedrock into
  U-valleys, cirques, fjords) and **Aeolian / dunes** (Paris et al. 2019 "Desertscape":
  saltation and barchan/longitudinal dunes). **Meandering rivers** (Paris et al. 2023) for
  fluvial deposition realism (point bars, oxbows) sits with the Stream/deposition work.
- **Coastal (geometric, wanted).** Distinguish two things: a *geometric* coastal shaper (pick a
  water level, reshape adjacent land into a beach-and-bluff profile keyed to local slope, emit a
  water-depth layer) is well-established (World Machine, Gaea Sea/Lake) and produces the beaches,
  coves, and slope-varying shorelines Oluf wants. That is a real planned node. Only *physically
  simulated wave erosion* is the open research gap, and we do not need it.
- **Open research gaps with little prior art** (do not commit node time yet): physical wave
  erosion, standalone alluvial fans / deltas, and debris-flow authoring. Areas Ymir could
  eventually lead, but research, not scheduled nodes.
- **Rivers** (explicit headwater-seeded channels, maskable) and **Snow** as wanted.
- **LookDev / fast fake-erosion nodes** (slope-aware diffusion/blur and procedural "eroded look",
  per HighMap and Gaea), which double as a fast interactive preview tier.

### Cross-cutting (ongoing)
- Resolution / build model and the build-result-to-viewport cache (see
  [[build-result-feeds-viewport]] memory).
- GPU acceleration as an optional path for the parallel-friendly models (see
  [GPU erosion](gpu-erosion.md)), CPU reference retained.
- Example graphs that encode form-then-erode-then-detail so the workflow is discoverable.

## 6. Where the field is going (and why Ymir is aligned)

From the academic survey (anchored by Galin et al. 2019, "A Review of Digital Terrain
Modeling," and a research cluster, Inria-Lyon and Purdue, that uses a heightfield + layer-stack
formulation matching Ymir's `Field`/`layers` model):

- **Control-domain authoring is winning.** The direction of the field is to edit *causes*
  (tectonic uplift maps, drainage sketches, hardness) and let physical erosion produce the
  relief, rather than sculpting the result directly (Cordonnier et al. 2016, through Schott /
  Paris et al. 2023, "Large-scale Terrain Authoring through Interactive Erosion Simulation").
  This directly validates Ymir's control-fields direction; we are aimed where the research is.
- **Interactivity comes from fast drainage-area approximation**, not brute-force iteration
  (Schott 2023; Tzathas et al. 2024 analytical erosion). Relevant to our preview tier.
- **Amplification / super-resolution as a first-class stage** (coarse simulation, then learned or
  procedural high-frequency detail) mirrors our preview-versus-build resolution split, and the
  form-then-erode-then-detail principle.
- **Generative (GAN, now diffusion) terrain** (Guérin et al. 2017, through Lochner et al. 2023
  and 2025 diffusion work) is the long-horizon shift. A learned amplification stage is a
  plausible far-future item, not a near-term erosion node.

Determinism, confirmed by the literature: thermal (gather/double-buffered), the pipe model
(per-cell stencil), and stream-power (with stable tie-breaks) are all bit-exact and parallel.
Particle/droplet is the one family that is not bit-deterministic in parallel, which is why we
skip it. This is a strict improvement over Gaea, which falls back to a single core for
repeatability.

The highest-value new phenomenon to add is **stratification / hardness layers** (Benes and
Forsbach 2001): a resistance layer consumed by every existing model via the soft-layer contract
(`layer_or(RESISTANCE, 1.0)`), unlocking mesas, hoodoos, and cliff bands. It composes with
everything we already have, which is why it sits in Phase 3 rather than Phase 4.

## 7. Decisions log

Settled (2026-06-27):
- **North star:** physically grounded, directable, deterministic. (Working assumption, accepted
  by proceeding; revisit if it starts to bite.)
- **Sequencing:** start with Phase 0, the foundations, regardless of the rest.
- **Hydraulic:** invest to bring the pipe model to a shipping-ready state. It fills a real need
  distinct from Stream (local water sim, pooling, deposition, the water surface). Hydraulic +
  Thermal at shipping quality matches Houdini's whole erosion surface; Stream puts us past it.
- **Scope, confirmed wanted:** coastal erosion via the geometric beach-and-bluff shaper (the
  World Machine effect Oluf liked: beaches, coves, slope-varying shorelines). Stratification is
  already high-value in Phase 3.

Still open:
- **Phenomena scope, the rest:** beyond fluvial, thermal, stratification, and coastal, do
  glacial or aeolian matter for your terrains, or are they out of scope for now?

## 8. A foundational item worth scheduling alongside Phase 0

- **Tiered evaluation cache (memory + disk)**, designed in
  [evaluation-cache.md](evaluation-cache.md). Fixes two things at once: an unchanged Build
  currently recomputes the whole graph (fresh cache discarded per build), and holding build-
  resolution results in memory hits a gigabyte ceiling. A memory-hot, disk-warm, content-hash
  keyed cache makes an unchanged rebuild near-instant, removes the memory ceiling, survives
  restarts, and is the foundation for feeding build-quality fields into the viewport (the
  build-result-feeds-viewport memory). Chosen scope: do the full tiered cache now. It speeds
  every future erosion build and sets up the viewer.
