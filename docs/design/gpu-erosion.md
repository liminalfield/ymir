# Design note: GPU-accelerated erosion (optional path, CPU reference retained)

Status: design only, not scheduled. Captures the reasoning so the idea is on record with
its tradeoffs, without committing to it now. This is the deliberate, scoped version of
"GPU support", and it is intentionally *not* "evaluate the whole node graph on the GPU."

## Why it exists

Erosion is the one node whose cost scales as roughly resolution cubed: passes scale
linearly with resolution and cells scale as resolution squared. On the CPU an 8K build is
many minutes (observed at 15+), and parallelizing the passes (#113) only buys a constant
factor against a cubic. The GPU is the lever that actually changes the order: erosion is a
stencil over a grid, the textbook compute-shader workload (ping-pong textures, one pass per
dispatch), and on a GPU the same build is seconds. This is how Gaea, World Creator, and
similar tools make high-resolution erosion usable. So GPU erosion is not a nice-to-have, it
is the real answer to the cubic, far more than CPU data parallelism.

## Why it is scoped to erosion, not the whole graph

Running the entire node graph on the GPU is a rearchitecture, not an additive feature, and
the cost is high:

- `ymir-core` is deliberately headless and pure CPU. `Field` / `Layer` are CPU buffers; the
  `Operator` trait and evaluator are CPU. General GPU evaluation means nodes become compute
  shaders (WGSL), which breaks the "a new node = one Rust file" invariant and raises the bar
  to author one enormously.
- The CPU path has to exist regardless: headless batch, CI, machines without a capable GPU,
  and the deterministic reference. So general GPU support is two implementations per node to
  keep in sync, or a GPU-only path that breaks headless use.
- Mixed graphs (a CPU-only node such as Import's PNG decode feeding GPU nodes) need buffer
  upload and download orchestration: real complexity about what runs where and when.
- It is premature. The node set and the engine are still young and changing; committing to
  GPU now ossifies them before they have settled.

Erosion sidesteps most of this because it is one node with a self-contained, stencil-shaped
computation. Accelerate just it, keep everything else on CPU, and the blast radius is one
operator plus a buffer round trip, not the whole engine.

## Determinism is not a blocker

GPU floating point is not bit-identical across vendors and drivers, but it is deterministic
on a fixed machine, and cross-machine "visually equivalent, not bit-identical" is exactly
the policy in CLAUDE.md's determinism section. So the determinism promise does not stand in
the way here. (An earlier framing treated byte-identity as a hard blocker; that was the old
dogma, since relaxed.)

## Shape of the work, when it is taken

- A GPU erosion path behind the same `Operator`, selected at runtime (GPU when a device is
  available and the resolution is worth the upload, else CPU). wgpu is already a dependency
  via eframe, so the device is present; this adds compute usage, not a new dependency.
- The CPU erosion stays as the reference implementation and the headless/CI path. The GPU
  result must be visually equivalent to it (within tolerance), not bit-identical.
- The two-phase gather (#113) maps cleanly to a compute shader: each cell reads neighbours
  and writes only its own cell, which is already how a stencil shader is written, so the CPU
  reformulation is also the GPU design.
- Cost to weigh at the time: the CPU<->GPU transfer per build, keeping two implementations of
  the erosion math in agreement, and a fallback when no suitable device is present.

## When

After the engine and node library have stabilized, and when erosion speed is the thing
gating real use (heavy high-resolution builds as a routine step, not an occasional bake).
Until then the CPU path plus the cancellable Build (#114) and authoring at preview
resolution are the workflow. Not now, but not never, and with eyes open about the cost.
