# CLAUDE.md

Guidance for Claude Code working in this repository. Read it fully before making changes.

## What Ymir is

Ymir is an open-source (GPL-3.0), native-Linux, node-based procedural terrain
generator. It is a personal, non-commercial project, and it is meant to be a serious
one: the architecture, code quality, and structure should hold up to scrutiny from
experienced Rust developers. Nothing here should read as a weekend hack. The goal is a
tool that is genuinely pleasant to compose with, not a clone of Gaea or World Machine.

## How to build it (working style)

This is the most important section. Build incrementally and legibly, never
fire-and-forget:

- Work in small, single-purpose steps. One component or concept per step. Never
  "build the engine" in one pass.
- Every step ends compiling, tested, and runnable. Never leave the tree broken
  between steps.
- Before a step, state in plain language what you are about to do and why. After it,
  state what changed. The maintainer should follow the reasoning, not reverse-engineer
  a diff.
- Stop at each checkpoint for review. Do not proceed to the next component until the
  maintainer approves.
- One small, focused commit per step, with a clear message. The git history is meant
  to be a readable record of how the project was built.
- Get visible output as early as possible (a generated heightmap in the first steps),
  so progress can be inspected, not just trusted.
- Write tests as part of each step, not afterward. A step is not done until its tests
  pass and clippy and fmt are clean.

The maintainer will typically hand you one step of the build plan at a time. Respect
that pace even if you can see further ahead.

## No shortcuts (fix causes, not symptoms)

The maintainer is not in a hurry. Correctness and doing this exceedingly well always
outrank doing it quickly. The failure to avoid is making a symptom disappear instead of
fixing its cause. The canonical example from other work: adding a timer or sleep to mask
a race rather than fixing the dependency that caused it.

When the correct solution is hard, do the correct solution. If it is large, unclear, or
needs a decision, stop at the checkpoint and say so. Never ship a hack to keep the step
moving. Asking is always better than guessing.

Concretely forbidden as symptom-hiding shortcuts:

- Arbitrary timers, sleeps, or polling to dodge a race or ordering problem.
- `unwrap`, `expect`, or panics on expected conditions (also required elsewhere).
- `#[allow(...)]` to silence a clippy lint instead of fixing the underlying code.
- `todo!`, `unimplemented!`, or stub bodies left in committed code.
- Tests that assert nothing, are `#[ignore]`d, or are weakened to pass.
- Hardcoding a value purely to make a test or check go green.
- Swallowing an error (`let _ =`, `.ok()`, an empty `Err` arm) to quiet a failure.

A green build is necessary, not sufficient. The bar is that an experienced Rust reviewer
would find no shortcut, not merely that `cargo test` passes. A scanner (`.githooks` +
a Stop hook, see `scripts/check-shortcuts.sh`) blocks the mechanical shapes above at
commit time; the conceptual shortcuts are caught by the per-step review and the tests.
A deliberate, justified exception is annotated inline with `// shortcut-ok: <reason>`,
which should be rare and visible in review.

## Core philosophy

Everything is data. One universal type flows on every edge of the node graph, so the
engine never needs to know what any node does, and the user is never forced into a
fixed build order. This is modelled on Houdini's heightfield workflow.

Adopt Houdini's attribute philosophy, not its full element model. Ymir has one
element, the 2D grid, plus a small bag of scalar globals. Do not build a
points/vertices/primitives schema. Resisting that over-generalization is what keeps
Ymir a terrain tool rather than a general dataflow engine. Hold this line actively
against scope creep.

Prefer many small, single-purpose nodes over few multi-purpose ones. A graph should
be readable from its node structure, not from parameters buried inside nodes:
someone reading the wiring should see what it does. When a node accretes several
behaviors, split it into focused nodes (an Invert node, not a remap with its output
range swapped); reserve parameters for genuine intra-node configuration. This is the
Unix philosophy applied to terrain graphs, and it is what makes the additive-node
invariant pay off in practice. A corollary: shaping controls (levels, curves) need a
visual widget, since bare sliders for a transfer function are not controllable.

## The data model

The single type on every edge is a `Field`:

- `width`, `height`: grid resolution.
- `region`: normalized bounds, for resolution and region independence.
- `layers`: a map of named scalar layers (`Arc<Layer>`, so pass-through is cheap).
  `"height"` is primary by convention; nodes may create arbitrary layers (`mask`,
  `flow`, `water`, `sediment`, `wear`, etc.).
- `detail`: a small map of scalar globals (seed, world bounds, vertical scale). The
  only non-grid data.

Conventions:

- **Height values are nominally normalized.** The `height` layer works in `[0, 1]`,
  with world vertical scale applied at export. It is NOT hard-clamped: intermediate
  operations may exceed the range and export maps the actual range. Treat `[0, 1]` as
  the working convention, not an invariant enforced on every value. (If the maintainer
  prefers metric heights, this is the one convention to flip before step 1.)
- **Layer names are constants, not bare strings.** Define canonical names (e.g.
  `layers::HEIGHT`, `layers::MASK`) so a typo is a compile error, while still allowing
  arbitrary custom layer names.
- A modifier that touches only `height` returns the field with that layer replaced and
  all other layers passed through untouched (cheap via `Arc`). This pass-through is
  what makes nodes insertable anywhere.

## Soft layer contracts

Nodes declare the layers they would like, degrade gracefully when a layer is absent,
and never gate a connection. Erosion reads a mask if present and applies everywhere if
not, via `field.layer_or(layers::MASK, 1.0)`. Hold this on every node.

## Nodes

- `Operator` is a trait: stateless behavior plus a `NodeSpec` schema. The engine
  depends only on this trait and holds `Box<dyn Operator>`. Signature:
  `eval(&self, inputs: &[&Field], params: &Params, ctx: &EvalContext) -> Result<Vec<Field>, Error>`.
- Inputs are an ordered, named list from the start. `eval` receives multiple inputs,
  so combine and blend nodes with two or more inputs are first-class, never a retrofit.
  The single-input modifier is just the common case, not a constraint.
- A graph node instance stores `(stable_id, type_id, params, connections)`. The
  `stable_id` is a persistent identity assigned at creation and serialized, distinct
  from the runtime slotmap `NodeId` used only for wiring and lookup. Seeding and any
  per-node identity derive from `stable_id`, never from the slotmap key, so a saved
  project reloads to identical output. Behavior lives in the operator, per-instance
  config in the graph; this separation enables clean memoization.
- `NodeSpec` declares `type_id`, a palette `category` id, search `tags`, `inputs`,
  `outputs`, and a `params` schema (name, type, range, default). Schema only, never GUI
  widgets, and only ids/keys, never display prose: the human name and description are
  resolved by convention from `type_id` through a downstream `tr(key)` layer, so the
  engine stays free of localization.
- A node's kind (`NodeKind`: generator, modifier, endpoint) is derived from arity, never
  a hard-coded enum the engine branches on. No inputs => generator; no outputs =>
  endpoint; both => modifier. "Generators only at the head" enforces itself, since a
  generator has no input socket. This kind is engine structure, distinct from the
  presentation `category` id above.

## The invariant that keeps nodes additive

Nothing in the application may ask "which node is this?" Everything either dispatches
polymorphically or reads the node's own spec. Adding a node touches only its own new
file. Enforced in four places:

1. The evaluator dispatches through `dyn Operator::eval`, never matching on node kind.
   This is also structural: the evaluator lives in `ymir-core`, which does not depend on
   `ymir-nodes`, so it has no concrete node type to match on even by accident.
2. The node palette is generated by iterating the registry (`inventory`), not a
   hand-kept list.
3. The parameter UI is built by introspecting `ParamSpec`, not per-node widget code.
4. Save/load stores `type_id` plus params and rebuilds via the registry. No central
   enum to extend.

A new node = one new file implementing `Operator` + one `inventory::submit!`.

Registration gotcha: `inventory`'s register-before-main can be dropped by the linker
when an operator's module is otherwise unreferenced (under `--gc-sections` and in some
test configs), making a node silently vanish from the registry. Ensure node modules sit
on a referenced path, prefer `linkme` if stripping shows up, and keep a CI smoke test
that asserts the expected node count so a missing node fails fast.

## Engine behavior

- Headless core: no GUI dependencies in `ymir-core`. Keep it testable and usable in
  batch mode.
- The operator-facing `EvalContext` carries resolution, region, and seed, threaded
  through evaluation together so `eval`'s signature stays stable as the engine grows.
  The target endpoint is the evaluator's argument, not part of the context: which node
  an evaluation was requested for is the evaluator's concern, not an operator's. Do not
  pass the context fields as loose arguments.
- Pull-based, memoized evaluation. Validate the graph is a DAG first, evaluate from the
  requested endpoint, and recompute only downstream of a change.
- Bounded cache. A full-resolution `Field` per node will exhaust memory on a large
  graph at build resolution. The cache is bounded by policy (cache along the active
  evaluation path plus a small LRU for reuse), not unbounded memoization of every node.
  This is a designed-in seam, not a later patch.
- Resolution behavior is two distinct things, and conflating them is a trap:
  - Sampled operations (noise, anything evaluated per cell from continuous
    coordinates) are resolution-independent: the same world coordinates yield the same
    value at any resolution.
  - Iterative simulations (erosion) are resolution-dependent physics, not sampled
    functions. Running N iterations at 512 squared and at 2048 squared produces
    genuinely different terrain. A low-resolution preview is therefore an approximation
    of the full build, not an identical result. Do not promise otherwise anywhere.
  - What we do promise: determinism at a given resolution; a tiled build matches an
    untiled build at the same resolution (halo overlap handles seams); and erosion
    parameters are expressed in resolution-aware terms (strength and scale in world
    units, iteration counts scaled with cell count) so the preview is representative
    rather than misleading. The target-resolution build is the source of truth.

## Determinism (hard requirement)

Same seed and same graph must produce byte-identical output, on any machine,
regardless of thread count. This is a core promise of a procedural tool. Watch the
Rust footguns:

- `HashMap` iteration order is nondeterministic. Never let output depend on it; use
  ordered iteration wherever layer or node order can affect results.
- `rayon` reductions must be order-independent.
- A node's seed is derived from the global seed (carried in `EvalContext`) and the
  node's persistent `stable_id`, never from the runtime slotmap key, the clock, or a
  thread id. So a node yields identical output across reloads and graph edits, while
  changing the global seed reseeds the whole world and each node stays internally stable.

Every node and the evaluator get a determinism test: run twice, assert identical bytes.

## Hashing and identity

Memoization keys, golden snapshot tests, and save/load all depend on canonical,
deterministic hashing. Decide it once, early:

- `ParamValue` has defined `Eq` and a canonical `hash_into` (folding into the same
  FNV-1a as `Field`/`Layer`), not a `std::hash::Hash`: it is only ever a map value, never
  a hash key, and that ambiguous "Hash" is exactly what could be mistaken for the
  canonical one. Floats are normalized for both equality and hashing (every NaN to one
  pattern, `-0.0` to `+0.0`), so equal params always produce equal keys. (`Params`, the
  value a node receives, is the ordered `name -> ParamValue` map; keep the names
  consistent in code.)
- Hashing a `Field` or layer uses a canonical order: layers in sorted name order, cells
  row-major, `f32` by bits. Never hash in `HashMap` iteration order.
- A node's cache key is its `type_id`, its canonical param hash, the hashes of its
  upstream inputs, and the `EvalContext` fields it depends on (seed, resolution,
  region). Including the context in every key is the simplest correct rule: a generator
  has no upstream inputs, so without it, two builds at different seed or resolution
  collide on `type_id` plus params and the cache returns a stale field. Propagating
  context transitively through input hashes is an optimization on top, never a
  substitute. A node may later declare which context fields it actually depends on (via
  `NodeSpec`) to avoid over-invalidating, for instance a node that ignores the seed.

Small, but it underpins determinism, the memo cache, and golden tests at once, so it is
its own decision, not an incidental detail.

## Error model

- One crate error type (`thiserror`).
- The engine surfaces per-node failures as values and never panics. A failing node is
  reported (shown "red" in a future GUI) while the rest of the graph still evaluates.
- A graph cycle is detected before evaluation and reported as a graph error, never a
  panic or infinite recursion.
- No `unwrap`, `expect`, or panics in library code on expected conditions.

## Testing and regression strategy

- Unit tests per operator.
- Property tests (`proptest`) for invariants: pass-through leaves untouched layers
  identical; evaluation is deterministic; `layer_or` returns the default for missing
  layers.
- Golden snapshot tests: hash a generated heightmap (canonical order, per above) so a
  refactor that silently changes output fails the test.
- `criterion` benchmarks on hot paths (noise, erosion) so performance regressions
  surface.

## Rust conventions and code-quality bar

This project should look idiomatic and deliberate to any Rust reviewer.

- Follow the Rust API Guidelines for public items.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check` must
  pass. Treat clippy warnings as errors.
- Doc comments (`///`) on all public items; `cargo doc` should build clean.
- Keep the public API surface intentional; default to private, expose deliberately.
- Represent the graph with ID/index keys (a slotmap or `Vec` keyed by `NodeId`), not
  nodes holding references to other nodes. Edges store `NodeId`s.
- `Arc<Layer>` for cheap pass-through. `rayon` for per-cell parallel work.
- No `unsafe`. No `async` (this is compute, not I/O bound).
- `Cargo.lock` is committed; this is an application.

## Dependency hygiene

Every dependency is a liability for maintenance, build time, and review. Keep the set
small and justified, and note the reason for each addition in its commit. Prefer the
standard library and a few well-chosen crates over many.

## File-format stability

Serialized graphs carry a format version field. Evolving the node or param schema must
not orphan existing project files; provide a migration path or explicit, documented
breaking changes. Treat users' saved projects as something to preserve.

## Keeping seams clean (forward-compatibility)

Extension comes from clean seams, not speculative abstraction. Do not add machinery for
a feature before it exists. Two cheap habits keep future work easy:

- Access `Layer` data through methods, never raw `Vec<f32>` indexing scattered across
  the codebase. If a typed or categorical layer is ever genuinely needed, that keeps it
  a contained change rather than a global rewrite.
- Nodes that compute useful intermediate fields (for example erosion's flow, water, and
  sediment) write them as layers rather than discarding them. Downstream nodes and later
  features will consume them.

## Workspace layout

```
ymir/
  Cargo.toml            # workspace
  rust-toolchain.toml   # pinned compiler
  crates/
    ymir-core/          # headless engine: Field, Operator trait, NodeSpec, registry
                        #   mechanism, evaluator, format I/O. No concrete nodes.
    ymir-nodes/         # concrete operators (the nodes): generators, modifiers,
                        #   endpoints, and their terrain math. Depends on ymir-core.
    ymir-cli/           # headless batch runner (exists)
    ymir-gui/           # later: egui + wgpu viewport and node editor
```

Concrete nodes live in `ymir-nodes`, not `ymir-core`, on purpose: the engine crate
cannot name any concrete operator because the dependency arrow points the other way, so
the "never ask which node is this" invariant below is enforced by the compiler, not
discipline. `ymir-core` keeps mechanism and reusable, non-terrain-semantic I/O (the PNG
encoder); terrain math (noise, erosion) lives beside the operators that use it in
`ymir-nodes`. A node's potentially heavy dependencies (a scripting engine for a future
wrangler node, say) therefore never reach the engine.

Keep a human-facing `ARCHITECTURE.md` alongside this file once the core lands, so
contributors can orient quickly. CLAUDE.md is the agent's brief; ARCHITECTURE.md is the
contributor's.

## Commands

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo doc --no-deps
cargo bench        # once benchmarks exist
```

## Build plan (MVP)

Each step is a reviewable, runnable commit. Do them one at a time, pausing for review.

1. `Field`, `Layer`, `layer_or`, the layer-name constants, and the canonical
   `Field`/layer hashing, with unit tests for pass-through, default behavior, and
   stable hashing. (`ParamValue` and `EvalContext` are designed now in this doc but
   built when first needed, at steps 4 and 5, to keep this step small.)
2. 16-bit PNG export of a field's height layer, plus a `main` that fills a gradient, so
   `cargo run` writes a viewable heightmap.
3. A Perlin/fBm generator writing the height layer, plus the determinism test (same
   seed, identical bytes).
4. The `Operator` trait, `NodeSpec`, and the `inventory` registry (with the missing-node
   smoke test), the noise generator refactored into the first operator.
5. The graph type and the pull-based, DAG-validated, bounded-cache evaluator, wiring
   generator into endpoint, with tests for evaluation, memoization, cycle handling, and
   determinism.
6. Export becomes an endpoint operator; a mask-aware thermal erosion modifier joins,
   giving a real three-node graph, with a golden snapshot test on the output.

No node editor and no 3D viewport until the engine works.

## Writing style for docs and comments

Honest, non-performative prose. Avoid em dashes. No marketing fragments.
