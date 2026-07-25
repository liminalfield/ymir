> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Ymir erosion strategy and design

This is the design brief for Ymir's erosion subsystem. It is to erosion what
`ymir-gui-DESIGN.md` is to the GUI: the locked decisions plus a stepwise build plan.
It is a draft, written to be reviewed and revised, and then decomposed into issues.

It supersedes the ad hoc erosion work to date. The intent is a reset: pick the right
set of erosion models, understand why each one exists, and build them well, rather than
accreting more models or chasing a standalone flow node that was reproducing what an
erosion node should already do.

Revision 2 incorporates a review by the Claude Code instance in the repo and the
maintainer's decision to target GPU compute now. The strategic spine is unchanged from
revision 1; what moved is the hydraulic model's discretization (now a GPU grid model,
not a CPU droplet model), the determinism stance (now visual-equivalence within
tolerance, not a full drop), and the order of the build plan's front half.

When settled, this file lives at `design/ymir-erosion-DESIGN.md`, and the determinism
section below is lifted into `CLAUDE.md` to replace the current hard requirement.

## Purpose and scope

The goal is terrain that looks good. Specifically it should reach the weathered,
settled look of a tool like World Machine, not the incised, chopped look the current
stream node produces on raw noise. Aesthetics outrank physical purity and outrank
byte-level reproducibility. Where a geomorphologically honest model also looks good,
that is the happy case, but the tie-breaker is the eye.

This document covers the erosion models Ymir commits to, the reasoning that selects
them, the byproduct layers they emit, the GPU-compute foundation they now sit on, and
the order to build them. The external landscape survey (`ymir-erosion-survey.md`) is the
input; this is the synthesis.

## What the engine already decides

These are inherited from `CLAUDE.md` and are not reopened here:

- One universal `Field` on every edge. Every layer (height, flow, wear, and so on) is a
  named scalar layer, interchangeable and passed through untouched when a node does not
  write it. A blur applied to a flow layer is the same operation as a blur applied to
  height.
- Erosion nodes are cohesive geological models, not a carve/smooth/deposit primitive
  kit wired together by the user. The user wires complete models.
- Soft layer contracts: a node reads a mask if present and applies everywhere if not,
  via `layer_or`. Nothing gates a connection.
- Iterative erosion is resolution-dependent physics. A low-resolution preview
  approximates the target-resolution build; it is not identical, and nothing promises
  otherwise. The build is the source of truth.
- Nodes that compute useful intermediate fields write them as layers rather than
  discarding them.
- Heavy dependencies belong with the nodes that use them (`ymir-nodes`), never in the
  engine (`ymir-core`). This principle now governs where the GPU dependency lives.

## The framing that selects the set: three transport drivers

Every erosion algorithm in the survey is a rule for moving material downhill. What
distinguishes them is what drives the transport, and there are only three physically
distinct drivers. This is the spine of the whole strategy, because "do we have the
right set" reduces to "is each driver represented, in the right formulation."

1. **Local slope.** Move material where the local slope exceeds the material's angle of
   repose, until the slope relaxes below it. No water. This is thermal, talus, or
   mass-wasting. It rounds ridges, builds scree, and imposes a maximum slope.

2. **Local flow velocity (transport capacity).** Simulate water actually moving across
   the surface; carry sediment up to a capacity set by speed, water, and slope; erode
   where under capacity and deposit where over. This is the hydraulic family. It
   produces rills, gullies, banks, granular deposition, and a settled surface. It has
   two discretizations: Eulerian grid cells exchanging water through virtual pipes, and
   Lagrangian particles (droplets) carrying water along their paths. Same driver, same
   physics, different bookkeeping. The choice between them is settled below and it turns
   on the hardware target.

3. **Drainage area.** Route flow to compute, at each cell, the upstream area that
   funnels water through it, then incise in proportion to that area (stream power,
   `E = K * A^m * S^n`). Drainage area is the only driver with a self-reinforcing
   feedback: a deeper channel draws more flow, which deepens it further. That feedback
   is what grows coherent dendritic networks and watersheds. No other driver can
   produce macro-scale river structure, because no other driver knows about upstream
   area.

The drivers are complementary, not competing, and they cannot substitute for one
another. Each owns a signature the others structurally cannot produce. Local rules
cannot fake drainage-area structure (this is why the standalone flow-node attempts
failed: they tried to select a river network on terrain that has none). Stream power
cannot produce fine granular deposition (it pushes the bed toward a target elevation
and has no grains). The hydraulic model does not build grand watersheds (it is a local
process with no area feedback).

Coastal and glacial are out of scope. Coastal is a geometric approximation, not
erosion, and is cheap to add later for high visual payoff. Glacial is niche. Neither is
a driver the target terrains need.

## The central lesson: deposition is what makes terrain look weathered

The current stream node looks like a dull machete chopping and slicing. That look has a
single cause: erosion that only removes material and never puts it back.

- The thin, deep cuts are detachment-limited incision with no erosion radius: material
  is removed and vanishes from the world.
- The hard rims and abrupt edges are the absence of deposition: nothing fills, nothing
  settles, so every cut keeps a sharp lip instead of a softened bank.
- The dullness between the cuts is the interfluves never being worked: only the
  drainage lines were carved, and everything between them stayed raw noise.

In a real landscape the material that leaves a slope has to go somewhere, and where it
piles up is half of what the eye reads as natural. A model that only incises always
looks violent. Transport plus deposition is what looks weathered. This single fact
drives the model selection and the build order below. The grid hydraulic model chosen as
the flagship has deposition as a native step in its core loop, not a bolt-on, which is
the main reason it can reach the target look.

## The committed set and the build priority

One cohesive model per driver. The revision-2 change is which discretization of the
hydraulic driver is the flagship, and it follows from the GPU target.

- **Thermal erosion (local slope). Keep as-is.** It is mature, stable, mask-aware, and
  the one member that is already the right thing in the right form. It has genuine
  standalone use (weathering any surface) and it is the coupling partner for the other
  two (see composition). Its diffusion math is a stencil, so it is GPU-friendly and its
  update is reusable substrate for the interleaved passes.

- **Hydraulic erosion, grid/pipe formulation on GPU (local flow velocity). Build first
  after the GPU foundation, and build well.** This is the Mei virtual-pipe model: cells
  exchange water through virtual pipes, a velocity field is derived from the flux, and a
  sediment transport-capacity step erodes and, crucially, deposits. It is the flagship
  for the look-good goal. On a GPU it is the natural citizen: a double-buffered stencil
  update, no atomics, order-independent within a pass, which makes it fast and keeps it
  deterministic per device. Its deposition step is native. World Machine's erosion is a
  pipe model, which is direct evidence that this discretization reaches the target
  aesthetic. It replaces the current rough pipe node, reusing `water_sim.rs` as the
  starting point rather than parking it.

- **Stream-power fluvial erosion (drainage area). Build second, on CPU, as the
  structural differentiator.** This is what produces coherent dendritic river networks
  and watersheds, which the hydraulic model cannot. It is Ymir's genuine differentiator
  versus commercial tools (Gaea fakes dendritic networks; Houdini ships no stream-power
  node). It stays on CPU: the Braun-Willett solve is a serial drainage-tree traversal
  and is GPU-hostile, and it is not interactivity-critical. It must be rebuilt correctly
  (see below), and the current detachment-limited node stays usable until the rebuild
  lands so the structural model does not go dark mid-rebuild.

- **Droplet hydraulic (local flow velocity, Lagrangian). Optional, as a CPU aesthetic
  yardstick, not a committed model.** The droplet's brush-radius erosion and
  deposit-to-four-cells produce a proven weathered look with little code, but on a GPU it
  fights the hardware (atomic scatter, inherently non-deterministic). Its only remaining
  value is as a fast CPU reference to get unstuck and to set the visual bar the grid
  model must match, built while the GPU foundation is under construction. Build it only
  if that fast-unstuck value is worth carrying a second implementation; otherwise skip
  straight to the grid model. It is not in the committed set either way.

Why the flagship flipped from revision 1: revision 1 reasoned the pipe-versus-droplet
choice in a CPU frame, where the droplet's simplicity and proven look won and its
determinism-hostility was the reason determinism was dropped. On a GPU target the
ergonomics invert. The grid model is what the hardware is built for, it stays essentially
deterministic because its update is order-independent, it is already seeded in
`water_sim.rs`, and its aesthetic case is backed by World Machine using exactly this
family. The droplet's advantages (simplicity, a ready recipe) shrink to a bring-up
convenience, and its disadvantages (atomics, non-determinism) become permanent costs.

## GPU compute: the foundation erosion justifies

GPU compute is adopted now, and erosion is its justifying application. The foundation is
reusable: once headless compute, Field-to-buffer marshalling, and an eval-time device
handle exist, other per-cell operators (noise, blurs, filters) can move onto the GPU
too. Erosion forces the investment; the whole tool amortizes it.

The seams, stated so they can be de-risked deliberately (in the spirit of the GUI
reconciliation spike), not so they are fully specified here:

- **Headless wgpu compute context.** wgpu creates a device and queue without a surface,
  so compute needs no window. The GUI already holds a wgpu device for the 3D viewport;
  sharing one device is preferable to creating a second. The context is created by the
  application (`ymir-gui` or `ymir-cli`) and passed into evaluation, never created by the
  engine.

- **Where the dependency lives.** Per the engine-purity principle, wgpu is a node-side
  dependency (`ymir-nodes`, or a small dedicated `ymir-gpu` helper crate), never a
  `ymir-core` dependency. `ymir-core` stays free of GPU types. This keeps the dependency
  arrow pointing the right way and matches how heavy node dependencies are already
  scoped.

- **How the device reaches an operator.** The operator-facing `EvalContext` carries an
  optional compute-device handle, threaded through evaluation the same way seed and
  resolution already are. A CPU-only operator ignores it; a GPU-capable operator uses it
  when present and falls back to a CPU path when absent (so headless CPU-only runs, and
  golden tests, still work). The exact form of the handle (a trait defined in
  `ymir-core` and implemented in the GPU crate, a small shared crate holding the handle
  type, or a type-erased handle) is an open design question below, because it decides
  whether any GPU type can reach `ymir-core` and that must be gotten right.

- **Field to buffer and back.** A `Field` layer is row-major `f32` on CPU behind an
  `Arc`. GPU erosion uploads the height layer (and any mask) to a storage buffer, runs N
  double-buffered compute passes ping-ponging state, then reads the final height and
  byproduct layers back and wraps them as new `Layer`s. Upload and readback are O(cells);
  the iteration is O(cells times iterations); so the transfer is amortized and the
  speedup is large. Readback is mandatory because downstream nodes consume the `Field` on
  CPU. Displaying a preview directly from the GPU buffer without readback (the viewport
  is already wgpu) is a possible later optimization, not a requirement.

- **Determinism on GPU.** Within one device a double-buffered stencil is order-independent
  and deterministic. Across devices and drivers, floating-point results can differ, which
  is the visual-equivalence rung, not byte-identity. See the determinism section.

## Determinism: the revised stance

This section replaces the "Determinism (hard requirement)" section of `CLAUDE.md`.

Byte-identical output across machines and GPUs is not a requirement. It is given up
deliberately, because different hardware, drivers, and shader compilation produce
slightly different floating-point results, and chasing byte-identity across GPUs is not
worth it for a tool whose bar is the eye. What is kept is stronger than revision 1's full
drop, because the chosen grid model is far more determinism-friendly than the droplet
would have been:

- **Deterministic seeding stays.** Per-node seeds still derive from
  `mix(global_seed, stable_id)`. Where randomness is used (rain distribution, any jitter),
  this pins where it lands, so the macro form of the terrain is set by seed plus
  parameters and stays stable across runs and reloads. Reloading a project and re-cooking
  yields the same landscape, not a different one.

- **Per-device determinism from the order-independent update.** The grid model's
  double-buffered stencil does not depend on iteration order or thread count, so on a
  given device it is deterministic under any parallelism. This is a real guarantee the
  droplet model could not have offered.

- **A CPU reference path for exact golden tests.** The grid model is a stencil, so it has
  a natural CPU implementation. Keep one as the golden-test oracle: it is byte-exact and
  single-threaded, so a refactor that silently changes output still fails a test. The GPU
  production path is validated against the CPU reference within a tolerance. Whether the
  CPU reference is kept permanently or only during bring-up is an open question below.

- **Golden tests move to tolerance where a CPU reference is not maintained.** Exact-byte
  comparison is replaced by within-tolerance comparison, which the "determinism is a
  means, not an end" stance already permits.

The principle, stated positively: determinism is surgical. It is guaranteed where it is
cheap and load-bearing (seeding, the per-device update, the CPU golden oracle) and
released where it is expensive and invisible (cross-GPU float differences). This is a
scoping of the old rule, not an abandonment of care.

## The hydraulic (grid/pipe) model: build specification

The model is the Mei-Decaudin-Hu virtual-pipe shallow-water simulation with sediment
transport. Each cell holds terrain height, water depth, suspended sediment, and outflow
flux to its neighbours. The reference is Mei et al. (2007); `water_sim.rs` is the
starting point. State is double-buffered so every pass is order-independent.

Per-iteration steps, in order:

- **Water input.** Add rain uniformly, or from a precipitation layer when present
  (`layer_or`). This is the seam a precipitation input later uses.
- **Flux update.** For each cell, compute outflow to each neighbour driven by the
  difference in height-plus-water (hydrostatic pressure), scaled by pipe cross-section
  and time step, then scale the outflows so a cell never drains more water than it holds.
  This scaling clamp is the main stability guard.
- **Water-surface update.** Apply the net of inflow and outflow flux to each cell's water
  depth.
- **Velocity field.** Derive a per-cell velocity from the flux.
- **Erosion and deposition.** Compute sediment transport capacity from velocity and local
  tilt. Where capacity exceeds current suspended sediment, erode terrain into suspension;
  where it falls short, deposit suspended sediment back onto terrain. Deposition is native
  here; it is not a separate mechanism. Modulate the erosion term by
  `layer_or(layers::MASK, 1.0)` so a hardness or erodibility layer works with no
  special-casing. This is the same hook a strata field later feeds.
- **Sediment transport.** Advect suspended sediment along the velocity field
  (semi-Lagrangian).
- **Evaporation.** Reduce water depth by an evaporation fraction.

Tuning is the known hard part and the survey flags it explicitly: capacity, erosion rate,
deposition rate, evaporation, rain, pipe geometry, and time step all interact, and the
result is sensitive to the time step. The build plan therefore makes reaching the look an
explicit acceptance gate (see step C2), not an afterthought.

Artifacts to handle, not discover late:

- **Grid-aligned channels.** Four-neighbour flux biases channels onto the axes. Options:
  eight-neighbour flux, a small anisotropy or jitter, or leaning on an interleaved thermal
  pass to relax the bias. This is the grid analogue of the droplet's one-cell-ravine
  problem. Pick an approach at the tuning step.
- **Stability.** Too large a time step diverges. Enforce the flux clamp above and a
  conservative time step; express iteration count in resolution-aware terms per
  `CLAUDE.md`.
- **Boundaries.** Decide reflective versus absorbing edges for water leaving the map, the
  analogue of the droplet drain-valley effect.

Byproduct layers this model emits (see the vocabulary section):

- **water** and **sediment**, the live simulation fields, which are this model's unique
  contribution and which neither thermal nor stream produces.
- **wear** and **deposition**, from the tracked erosion and deposition terms, more
  accurate than an after-the-fact height difference.
- **flow**, accumulated flux or velocity magnitude over the simulation: an honest "where
  water runs" map computed on terrain the model is actively carving. It is the flow map
  the failed standalone nodes were reaching for, produced correctly and for free.

Optional CPU droplet yardstick: if built (see the committed set), it follows Beyer's
method as implemented by Lague: one-cell-unit steps regardless of speed, inertia-blended
direction, bilinear height and gradient, a precomputed brush-radius erode, and
deposit-to-four-cells to fill pits precisely. Its purpose is to produce a target look
quickly and to exercise the layer vocabulary and hooks, not to ship.

## Thermal: keep, and couple

Thermal stays as it is for standalone use. Its second role is as the coupling partner: a
recommended remedy for thin hydraulic channels is to let talus slippage at the angle of
repose relax them, which is exactly thermal diffusion. So thermal is both a node the user
can wire and a diffusion pass the other models can interleave (see composition). The
diffusion is a stencil, so it lives as reusable substrate that the thermal node, the
interleaved passes, and a GPU implementation all call.

## Stream-power fluvial: the rebuild

The current stream node is the right driver in the wrong formulation, run in isolation.
It stays on CPU (serial drainage-tree solve) and stays usable until the rebuild lands.
Rebuild it with three corrections:

- **Transport-limited, not detachment-limited.** The current node (straight
  Braun-Willett) only incises; eroded material vanishes and it cannot deposit, which is
  half of why it looks chopped. Rebuild it to track sediment flux and deposit where the
  flow is over capacity (the Davy-Lague transport-limited form, or the hybrid that
  incises bedrock and deposits sediment in one mass balance). This makes deposition a
  native output, not a faked height difference.
- **Coupled hillslope diffusion.** Interleave a diffusion pass per iteration (or per few
  iterations) so that as channels incise, the slopes above them relax and the interfluves
  resolve. Incision alone slots narrow channels into untouched noise; coupling is what
  turns slots into valleys with cross-sectional form. Reuse the thermal diffusion
  substrate.
- **Optional relief or uplift control field.** Feeding relief back in is what lets stream
  power reach a satisfying steady state rather than living on the knife-edge between
  barely biting and washing flat. The planned mountain-primitive node is the natural
  source of that field. Hold this as an optional control input, not a requirement.

Byproduct layers: **flow** (drainage accumulation, which the model computes anyway),
optionally **flowdir** (the receiver graph as a direction field), and **wear** and
**deposition** from the transport-limited mass balance.

## The layer vocabulary

Because every layer is interchangeable, the discipline is a canonical named set plus a
rule for which node emits which. Define these as layer-name constants (a typo is a
compile error) and provide a small shared helper that any erosion node calls on exit to
emit the universal pair consistently.

| Layer        | Meaning                     | Source                                          | Notes |
|--------------|-----------------------------|-------------------------------------------------|-------|
| `height`     | the eroded surface          | all                                             | the primary layer |
| `wear`       | where the bed was stripped  | all                                             | from tracked erode events where available, else `max(0, original - eroded)` |
| `deposition` | where material settled      | all                                             | native and physically real in the grid hydraulic and transport-limited stream models; a height-difference fallback elsewhere |
| `flow`       | where water runs            | grid hydraulic (flux/velocity), stream (accum)  | a byproduct of erosion on carved terrain, never computed on raw noise |
| `flowdir`    | flow direction vector field | stream (optional)                               | emit when a downstream consumer wants it |
| `water`      | live water depth surface    | grid hydraulic                                  | now a first-class output of the flagship, not a parked concern |
| `sediment`   | suspended load              | grid hydraulic                                  | likewise |

The universal pair (`wear`, `deposition`) is the nearly free realization: it is the
signed difference between input and output height, split into positive and negative, and
every erosion node can emit it. Where a model tracks material explicitly (the grid
model's erode and deposit terms, the stream model's sediment flux), emit the tracked
quantity instead. Emitting these consistently gives most of the texturing currency the
survey describes with no new simulation.

Note that `water` and `sediment` are promoted relative to revision 1: because the
flagship is now the grid model, its live water surface and suspended-sediment fields are
first-class outputs of the primary erosion node, not a GPU-era someday.

One naming decision to settle: thermal currently emits `debris` for its dry talus
accumulation, which is a kind of deposition. Keep `debris` distinct as dry mass-wasting
talus, separate from fluvial `deposition`: they are genuinely different processes and the
maintainer has the geology background to use the distinction. Caveat flagged by the
review: the old roadmap marked thermal's `debris` output as possibly broken, so verify it
is actually correct before it becomes the first consumer of the shared byproduct helper
in build step A1.

## Directability: the real roadmap after the core

Once the core looks good, the gap versus commercial tools is directability and
deposition control, not more models. In priority order:

- **Per-parameter mask inputs across all models.** The mask seam (`layer_or`) is already
  the hook; extend it so individual parameters (erosion strength, deposition rate,
  precipitation) can each take a modulating layer. The useful distinction from the survey
  is modulate versus confine: a mask that scales a parameter while the process still runs
  everywhere, as opposed to hard-confining the effect to a region.
- **Strata and erodibility fields.** Depth-varying hardness is what produces terracing,
  mesas, and buttes. This rides the same per-cell hardness hook the grid model already
  has, extended to vary with depth below a reference. Houdini's strata ramp is the
  reference.
- **Precipitation as an input layer.** A rainfall layer feeding the water-input step of
  the grid model and the stream model, so the user can direct where water enters.

Deposition is not a separate roadmap item any more. Choosing transport-capacity
formulations makes it a native output of both the grid hydraulic and the rebuilt stream
model.

## Multi-scale workflow

Running erosion at a chosen feature size, then again at a finer size, is a workflow
wrapper, not a new algorithm, and it is the assumed practice in every serious tool. It is
cheap to add once the core models are good: run the same model at decreasing feature
sizes, optionally distorting between passes. Build it after the core is excellent.

## How the models compose

Compose between cohesive models by layering order, masks, and control fields. A typical
pipeline is: regional form, then stream power for macro drainage structure, then grid
hydraulic weathering for the surface look, then thermal for talus cleanup, then
high-frequency detail added last. The user wires complete models and directs them with
masks.

The one exception, and it is deliberate: the tight feedback between fluvial incision and
hillslope diffusion is strong enough that it belongs inside the stream node as
interleaved passes, not expressed as two nodes wired in sequence. Sequential-after is not
the same as interleaved-during; the feedback only exists in the latter. Likewise the grid
hydraulic model may interleave a light thermal pass to relax grid-aligned channels. Keep
tight physical coupling internal to the model that needs it; keep loose composition
between models in the graph.

## The fork decisions, resolved

1. **Realism versus art-direction versus both.** Both, sequenced. Get each model looking
   right first, because directability layered over a model that looks wrong just yields
   controllable wrongness. Masks and control fields are the second layer.

2. **Which phenomena beyond fluvial and talus matter.** Deposition (native, via the
   transport-capacity formulations) and strata/erodibility (depth-varying hardness).
   Those two unlock the recognizable landforms. Everything else is extra.

3. **How models compose.** Between cohesive models via layering order, masks, and control
   fields, with tight fluvial-hillslope and hydraulic-thermal coupling kept internal to
   the model that needs it.

4. **GPU target, and does it flip the flagship (revision 2).** Yes. GPU compute is
   adopted now, erosion is its justifying application, and on a GPU target the flagship is
   the grid/pipe model, not the droplet, for the reasons in the committed-set section.
   Determinism softens to per-device determinism plus cross-machine visual equivalence
   rather than a full drop.

## Build plan

Each step is reviewable and runnable, following the incremental discipline in `CLAUDE.md`:
small single-purpose steps, each ending compiling, tested, clippy- and fmt-clean, with a
checkpoint for review before the next. The steps are grouped into workstreams so the
dependencies are legible; within a workstream they are ordered. Workstream A has no GPU
dependency and can start immediately.

**Workstream A: the layer foundation (CPU, start now)**

A1. **Layer vocabulary and the shared byproduct helper.** Define the canonical
layer-name constants (`wear`, `deposition`, `flow`, `flowdir`, `water`, `sediment`).
Provide the shared helper that emits `wear` and `deposition` from a before/after height
pair. Before making the existing thermal node the first consumer, verify thermal's
`debris` output actually works (the old roadmap flagged it as possibly broken); fix it if
not. Then have thermal emit `wear`/`deposition` (keeping `debris` distinct). Golden test
on the emitted layers.

**Workstream B: the GPU compute foundation (the dependency erosion justifies)**

B1. **Headless wgpu compute and the Field-to-buffer path.** Stand up a headless wgpu
compute context, ideally sharing the viewport's device. Settle where the wgpu dependency
lives (a `ymir-gpu` helper crate or `ymir-nodes`, never `ymir-core`) and how the device
handle is threaded through `EvalContext` with a CPU fallback. Implement Field-layer upload
to a storage buffer and readback to a `Layer`. Prove the whole path with one trivial
compute shader (for example a scalar multiply or a box blur) round-tripping a `Field`, and
a test that the GPU result matches a CPU reference within tolerance. This establishes the
reusable foundation; the erosion model is the first real user of it.

**Workstream C: the flagship hydraulic model (GPU)**

C1. **Grid/pipe (Mei) hydraulic model on GPU.** Implement the double-buffered
seven-step iteration (water input, flux, water-surface, velocity, erode/deposit, sediment
transport, evaporation) as compute passes over height/water/sediment/flux state, starting
from `water_sim.rs`. Keep a CPU reference implementation of the same stencil as the golden
oracle. Emit `height` plus `water`, `sediment`, `wear`, `deposition`. Test the GPU path
against the CPU reference within tolerance.

C2. **Look-matching and tuning checkpoint.** The make-or-break step. Tune capacity,
erosion and deposition rates, evaporation, rain, pipe geometry, and time step to reach the
deposition-rich weathered look on raw fBm at build resolution. Choose the grid-alignment
mitigation (eight-neighbour flux, jitter, or interleaved thermal) and the boundary
behaviour. Do not proceed until the output reaches the bar. The acceptance criterion is
the maintainer's eye; the optional CPU droplet yardstick (workstream O) can serve as the
concrete bar to match.

C3. **Mask hook, flow output, and artifact handling.** Wire the erosion term through
`layer_or(layers::MASK, 1.0)`. Add the accumulated `flow` layer. Finalize boundary
handling and the stability clamp. Tests for the mask hook and the edge behaviour.

C4. **Retire the rough pipe node.** Replace the current `modifier.hydraulic_erosion`
node with the GPU grid model as the user-facing Hydraulic Erosion node (the discretization
is an implementation detail the graph does not leak). Confirm the registry and the GUI
palette pick up the replacement.

**Workstream D: stream-power rebuild (CPU, second)**

D1. **Transport-limited core.** Rebuild the stream node to track sediment flux and
deposit over capacity (Davy-Lague), so it incises and deposits from one mass balance,
while keeping the current detachment-limited node usable until this lands. Emit `flow`,
`wear`, `deposition`. Checkpoint: valleys with fill and softened banks, not bare slots.

D2. **Coupled hillslope diffusion.** Interleave diffusion per iteration so interfluves
resolve and valleys gain cross-sectional form. Reuse the thermal diffusion substrate.
Checkpoint against D1 output.

D3. **Optional relief control field.** Add an optional uplift/relief input layer so the
model can reach a steady state, degrading gracefully when absent. This is the seam the
mountain primitive later feeds.

**Workstream E: the contract**

E1. **Determinism section update in `CLAUDE.md`.** Lift the revised stance from this
document into `CLAUDE.md`, replacing the current hard requirement, so the contract matches
the code.

**Workstream O: optional aesthetic yardstick**

O1. **CPU droplet spike.** Build the Beyer/Lague droplet on CPU as a throwaway or side
reference to get unstuck fast and set the look bar for C2, only if the fast-unstuck value
justifies a second implementation. Not shipped, not in the committed set.

Later, beyond this core: per-parameter masks and the modulate-versus-confine distinction,
strata/erodibility with depth-varying hardness, precipitation input layers, the
multi-scale pass wrapper, and moving other per-cell operators (noise, blurs) onto the GPU
foundation from workstream B. None are built in this plan, but the seams above keep them
contained.

## Decisions locked

- Erosion is understood as three transport drivers (local slope, local flow velocity,
  drainage area). The committed set is one cohesive model per driver.
- GPU compute is adopted now. Erosion is its justifying application; the foundation
  (headless wgpu, Field-to-buffer marshalling, an `EvalContext` device handle) is reusable
  across the tool.
- The wgpu dependency lives with the nodes (`ymir-nodes` or a small `ymir-gpu` crate),
  never in `ymir-core`. The engine stays GPU-type-free; the device is threaded through
  `EvalContext` with a CPU fallback.
- Thermal is kept as-is and also serves as GPU-friendly coupling substrate.
- The flagship hydraulic model is the grid/pipe (Mei) model on GPU, deposition-aware,
  built from `water_sim.rs`. It is built first after the GPU foundation.
- The droplet model is optional, at most a CPU aesthetic yardstick, not committed.
- Stream power is rebuilt second, on CPU, transport-limited and coupled with hillslope
  diffusion, and the current node stays usable until the rebuild lands.
- Determinism is not byte-identical across machines. Kept: deterministic seeding,
  per-device determinism from the order-independent stencil, a CPU reference path for
  exact golden tests, tolerance comparison otherwise.
- Every erosion node emits `wear` and `deposition`; `flow`, `water`, and `sediment` come
  only from erosion on carved terrain, never from raw noise.
- Deposition is achieved through transport-capacity formulations, not treated as a
  separate later feature.
- `debris` (dry talus) stays distinct from `deposition` (fluvial fill).
- Standalone flow-selector nodes are off the main path.

## Open questions for review

- The GPU device-handle seam: a trait defined in `ymir-core` and implemented in the GPU
  crate, a small shared crate holding the handle type, or a type-erased handle? This
  decides whether any GPU type can reach `ymir-core`.
- Keep a full CPU reference of the grid model permanently (as golden oracle and
  correctness check), or only during bring-up?
- Four-neighbour versus eight-neighbour flux for the pipe model (the grid-alignment
  tradeoff), decided at C2.
- Boundary behaviour for water leaving the map: reflective or absorbing?
- Build the optional CPU droplet yardstick (workstream O), or go straight to the GPU grid
  model?
- Preview path: display from the GPU buffer directly to the viewport (no readback), or
  always read back to a `Field` and let the normal preview handle it? The Field model
  needs readback for downstream nodes regardless; this is only about the preview.

## References

- Mei, Decaudin, Hu (2007), Fast Hydraulic Erosion Simulation and Visualization on GPU.
  The virtual-pipe model behind the flagship, and the reason it is GPU-native.
- Stava et al. (2008), Interactive Terrain Modeling Using Hydraulic Erosion.
- Beyer, Hans Theobald, Implementation of a method for hydraulic erosion (TU Munich), and
  Lague, Sebastian, Hydraulic-Erosion (GitHub, MIT). The droplet method for the optional
  CPU yardstick.
- Cordonnier et al. (2016), Large Scale Terrain Generation from Tectonic Uplift and
  Fluvial Erosion. The stream-power-plus-uplift recipe behind the relief control field.
- Braun and Willett, implicit O(n) stream-power solver (serial; the reason stream stays
  on CPU).
- The Ymir erosion survey (`ymir-erosion-survey.md`) for the full external landscape,
  including the feature matrix showing World Machine's erosion as a pipe model.
