> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Strategy: subgraphs as authored nodes

Status: **strategy ratified, tracked as [#373](https://github.com/liminalfield/ymir/issues/373).**
Written 2026-07-31 from the authoring handoff ([`authoring-handoff.md`](authoring-handoff.md)),
the shipped subgraph feature, and the existing design corpus ([`subgraphs.md`](subgraphs.md),
[`node-taxonomy.md`](node-taxonomy.md) item E, [`project-format.md`](project-format.md),
[`coastal-erosion.md`](coastal-erosion.md)). Where this document supersedes a prior decision, it
says so by name; the superseded documents get status annotations, not silent edits.

## 0. Thesis

Ymir becomes a tool where users author nodes, not only use them. The mechanism is the subgraph
feature that already shipped, extended with a **parameter interface**: a declaration list (name,
kind, range, default, unit, description) on the subgraph definition, with inner parameters taking
their values by **referencing** that interface through expressions. An authored node then has
everything a native node has that a user can see (ports, docs, category, parameters with defaults
and ranges, presets) while native code keeps everything a user cannot see.

This was not planned. It fell out of a session that started with a beach and found the limits of
composition (handoff §1 and §2). The strategy's job is to absorb that finding and take positions
on the questions the handoff deliberately left open (§4.1 to §4.6), because the
highest-consequence one (definition versus copy) gets settled by whichever gets built first if
nobody decides it.

## 1. The finding this strategy absorbs

The beach experiment (handoff §2.2) established that the missing capability is not encapsulation
and not parameter exposure in the abstract. It is that **one conceptual parameter must be written
once and referenced many times, with arithmetic at the reference site.** Beach width `W` appeared
in two nodes with an invisible must-match constraint between them; amplitude had to be entered as
a raw fraction of world height. Six correct nodes, unusable interface.

The consequence for sequencing: expression-driven parameters (the literal / expression / field
seam, handoff §3.4) are the **prerequisite** for the parameter interface, not an adjacent
convenience. An interface without referencing is a fancier way to have two disconnected `W`s.
Every phase below hangs off this ordering.

## 2. The value-source model

> **A parameter is a literal, an expression (computed, scalar), or a field (spatial, wired).**
> One seam, three sources.

This line, from the handoff, is adopted as the governing model. The three sources map to three
workstreams:

- **Literal** is what exists today. Unchanged.
- **Expression** is a scalar computed once per node evaluation from a scalar environment. New;
  the subject of Phase 1. The engine is the existing expression compiler (bytecode, fixed variable
  environment, unknown identifiers are compile errors), run once per node instead of once per cell.
- **Field** is a per-cell spatial value arriving on a wire via promotion. Pre-existing design
  ([`node-taxonomy.md`](node-taxonomy.md) item E, the Directability epic). Parallel track, not a
  dependency of the authoring work; the two share the resolve-before-eval philosophy and nothing
  else structural.

The engine-side constraints recorded in the handoff are adopted as-is: the compiler moves out of
the node crate (the resolver cannot depend on it); resolution happens **before** a node evaluates,
so no node learns that expressions exist and the "never ask which node this is" invariant holds;
the **resolved value**, not the expression text, enters the memoization cache key; parameter
reference cycles are detected and reported, never hung (see §3.1a for how small that check turns
out to be); and the computed-value visual state does not rely on a red/green distinction.

## 3. Settled decisions

Each subsection is a position this document commits to, with the reasoning. The table in §3.10
summarizes them in the house format.

### 3.1 Reference scope: no reach into a parent graph

An expression on a parameter may reference exactly three things: **its enclosing subgraph's
declared interface**, **world globals** (`world_height`, `sea_level`, world extent), and **the
node's own other parameters**. Nothing else. Not sibling nodes' parameters, not anything in a
parent graph, not parameters two containers up, no path syntax.

The governing principle is portability, and it is sharper than "keep the scope small". A subgraph
that reads something from the graph around it carries an invisible requirement about where it can
be used: drop it into a project that does not supply that thing and it breaks. So anything a
subgraph needs from outside is **declared**, not reached for. For fields that means a wire, since
a field arrives on a connection. For scalars it means an interface parameter, which is the
declared entry point a scalar equivalent of a wire would be.

Note what this does *not* forbid. An inner node reading `beach_width` off its enclosing interface
is not reaching into a parent graph: `beach_width` is declared on the subgraph the node is part
of, and the two travel together into any project. Nothing in the surrounding graph has to supply
it.

Maya and Nuke allow any parameter to reference any other parameter anywhere, and the result is
dependency spaghetti the graph does not show, which is precisely what "a graph should be readable
from its wiring" exists to prevent. One-level lexical scope keeps cache keys local and shrinks the
discoverability problem to a single visual state meaning "computed". Inside a subgraph, interface
parameters are bare identifiers in the expression environment, the same as the per-cell
environment today; no `ch("../x")` machinery is ever needed.

Nested subgraphs pass values down the same way, one level at a time: the outer interface drives
the inner container's parameter via an expression, and the inner interface drives its own
contents. Values flow down visibly, never reach across.

**World globals are the deliberate exception**, and they are safe because they are universal.
Every Ymir project has a sea level, a world height and a world extent, so depending on them
constrains nothing about where a subgraph can be used. They are also load-bearing: the unit
problem that motivated this work dissolves only because an inner parameter can write
`amplitude / world_height`. The accepted consequence is that the same authored node behaves
differently in a 1000 m world than in a 256 m one, which is the intended behaviour for anything
sized in metres rather than a side effect.

User-defined project globals ([#374](https://github.com/liminalfield/ymir/issues/374), idea
capture) do **not** get this exemption, because a user global may be absent in the next project.
A subgraph that wants one declares an interface parameter, and whoever places the node fills it
in, writing the global's name there. Same rule, applied one level up.

Multi-binding and unit conversion fall out, as the handoff's survey found they do in every mature
tool: two inner parameters referencing `beach_width` is multi-binding; `amplitude / world_height`
at the reference site is unit conversion. Neither is a feature.

### 3.1a Cycles are possible, and the check is local

Allowing a parameter to reference another parameter **on the same node** is what makes a loop
reachable at all: write `in_low = in_high - 5` and `in_high = in_low + 5` and each needs the other
resolved first. Nothing else in §3.1's scope can cycle, because an interface parameter and a world
global are values that point at nothing.

That case is worth having. Its motivating use is not inside a subgraph but at the **call site** of
one. An authored Coastal node exposes `beach_width` and `amplitude`, and those two want to be
related: a 20 m beach rising 3 m is a different beach from one rising 12 m, and the ratio is the
quantity an author has a feel for. Without same-node references you type two numbers and keep them
in step by hand, which is the duplicated-`W` problem of §1 reappearing one level further out, on
the very node this strategy exists to make authorable. `amplitude = beach_width * 0.15` closes it.

Because loops are confined to a single node's own parameter set, the detection is correspondingly
small: a topological sort over that node's parameters, reported as a **parameter error on that
node**, never a graph error and never a hang. This is not the graph-wide traversal the phrase
"parameter reference cycles" would otherwise imply.

Two consequences follow for the implementation:

- **Resolution order within a node is declaration order**, never map iteration order. Ordering was
  already required to be deterministic; it is now load-bearing in a new place, so it is written
  down here rather than discovered later.
- **A declared default is a literal; an instance's value may be an expression.** A default is part
  of the definition and travels into projects that know nothing about it, so it cannot depend on a
  name resolved elsewhere. So `amplitude` defaults to `3` in the declaration, while a particular
  Coastal node on a canvas may carry `beach_width * 0.15`. Definitions stay portable, call sites
  stay expressive.

### 3.2 The parameter interface

A subgraph definition carries a declaration list with the same shape a native node declares: name,
`ParamKind`, range, default, unit, description. Deliberately the existing `ParamSpec` shape,
because the inspector already renders parameter UI by introspecting a declared schema. An authored
node's parameters render through the same path as a native node's, with no new widget code.

Ordering and grouping are part of the interface from the start (a fifteen-parameter node with no
structure is the sprawl problem in a smaller box; every mature tool has folders or tabs here).
Conditional visibility is explicitly deferred (§7).

**Storage splits in two**: the interface belongs to the *definition*; the values belong to each
*instance*. That split is the same one §3.3 turns on. Handoff §3.1 sketches this split as
definition-in-the-`.ymirsub`-file and values-in-the-project-file; under §3.3 below both live in
the project file, separated by section rather than by file, with the `.ymirsub` serving
distribution. The split itself is unchanged; only where the definition rests differs.

The seed design is unchanged and falls out of the split cleanly: the captured seed belongs to the
definition (it is what makes the shared Fuji the same Fuji everywhere); the reseed override is
instance state.

### 3.3 Definition model: embed-with-provenance

This is the handoff's §4.1b, flagged there as the highest-consequence open question. It is posed
as a binary (copy with no link back, versus definition-plus-instances) and it is not one. The
position: **embed-with-provenance**, the model Blender node groups demonstrate.

- A project **embeds the full definition** of every subgraph type it uses, once. Instances within
  the project reference the embedded definition and carry only their own parameter values and seed
  override.
- Each embedded definition records its **origin**: a stable definition id, a version, and a
  content hash of the definition.
- The library is a **distribution channel, not a runtime dependency**. Dropping from the library
  copies the definition into the project.

Why this and not the alternatives:

- **Pure copy** (the shipped behaviour, per [`subgraphs.md`](subgraphs.md)'s locked mechanism) was
  right for templates and is preserved *in spirit*: the copy still happens, at definition
  granularity instead of instance granularity, so a project file remains fully self-contained and
  reproduces the same terrain on any machine with no live dependency on anyone's library
  directory. What pure copy cannot do is the authored-node case: fix a bug in your coastal node
  used four times in a project, and the copy model means four manual replacements with no way to
  see which instances are stale.
- **Live reference to the library** (the Houdini HDA shape) breaks self-containment. HDA's
  learning-curve pain comes almost entirely from the definition living outside the hip file: scan
  paths, missing-asset errors, version shadowing. Ymir does not import that.

Under embed-with-provenance, a fix to a definition updates every instance *in that project* at
once, because the project holds one definition. Across projects, the content hash makes staleness
**detectable**: on open, if the library holds a definition with the same id and a different hash
or newer version, the app offers "the library has a newer Coastal, update this project's copy?" as
an explicit, user-initiated, git-diffable act. Divergence (this project's embedded copy differs
from the library) is a normal, visible state, not an error and not a fork-on-edit rabbit hole.
Silent propagation never happens.

The model contains both alternatives: ignore the provenance and it degrades to pure copy; the
ecosystem could grow toward live references later without a file-format break, because the
provenance triple is already in the file.

**This supersedes** the [`subgraphs.md`](subgraphs.md) decision "library-drop is a copy with no
link back", and it **answers** that document's deferred item "editing a library entry in place"
(edit the definition wherever it lives; the hash tells every project holding a copy that an update
exists).

### 3.3a Authored nodes need an identity the program can see

The model above assumes something that does not exist yet. Today every subgraph reports the same
`type_id`, and what distinguishes one from another is the inner graph stored in that particular
instance (`SubgraphNode` holds its own `inner: Graph`). There is no shared definition for
instances to point at and no name the program knows them by.

Two commitments in this document require one. §3.3 stores one definition per subgraph *type*,
which presumes the program can say "these four instances are the same type". §3.5 puts authored
nodes in the palette under their own category, which presumes they have a category and a name of
their own.

So the **stable definition id lands in Phase 2, alongside the interface**, rather than waiting for
the format work in Phase 3. The interface work already touches the definition, and identity must
exist before the project format freezes around it; getting it wrong in Phase 3 means migrating a
format that real projects are already saved in.

The palette consequence is separate and belongs to Phase 4. The palette is generated by iterating
the `inventory` registry, which is a hard invariant. Authored nodes are files on disk and can
never be `inventory` entries, so either the palette learns to merge a second source or the
registry learns to accept entries added at runtime. Neither is difficult; both are undesigned, and
the choice is noted here so it is not discovered during Phase 4.

### 3.4 Distribution: inline on export, no dependency resolution, ever

The standing intent (import node networks from a git repo into a local library) becomes far more
valuable once networks carry interfaces, and it stays exactly as simple: importing is copying
`.ymirsub` files into the library directory. What must be decided now, because it is cheap now and
expensive later:

1. The `.ymirsub` format carries the **provenance triple** (stable id, version, content hash), the
   same fields the embed model needs, so one metadata design serves both. The shipped format
   already carries a format version, display name, category, description, and per-port docs; this
   extends an existing block rather than introducing one.
2. **An exported `.ymirsub` inlines any nested subgraph definitions it uses.** A distributed
   subgraph is always self-contained. This single rule forecloses dependency resolution, version
   solving, and lockfiles permanently; the "package ecosystem" of handoff §4.2 can grow on top of
   flat, self-contained files or not at all.
3. A subgraph's only external dependency is the **built-in node set**. The definition records the
   app version it was authored against; the loader already reports unknown-`type_id` as a typed
   error, which becomes the "authored against a newer Ymir" warning for free.

Everything else about the ecosystem (registries, discovery, curation) is explicitly unplanned.

### 3.5 Convergence: schemas converge completely, implementations never do

Handoff §4.1 asks whether the built-in and authored node sets converge. Both halves have a
definite answer.

**At the schema level, total convergence.** Parameters, ports, docs, category, description,
presets: an authored node declares what a native node declares, renders through the same
introspection, and appears in the same palette (category from its metadata, by the mechanism
§3.3a leaves to Phase 4). From the user's chair there is one kind of node. This is the moment the
strategic shift becomes visible in the product, and it is deliberate.

**At the implementation level, none.** Native nodes remain native wherever the computation is a
coupled algorithm, per-cell code, or a future GPU path. Erosion never becomes a subgraph. A
Mountain macro probably always should be one, and this is worth stating plainly: the existing
principle "Mountain is a thin opinionated composition on top of reusable substrate, not a
mega-node" is a description of an authored node, written before the mechanism existed. Enhanced
subgraphs are the mechanism.

The "one new file per node" invariant generalizes rather than breaks: a native node is one new
source file; a shipped composition is one new `.ymirsub` data file bundled with the app. Nothing
about the invariant assumed the file was Rust.

### 3.6 The cohesion test

Handoff §4.3: "erosion nodes are cohesive models, not a construction kit" was reaffirmed for
erosion and rejected for coasts in one session, and needs a general test.

> **Does the computation feed back on itself?** Coupling through iteration forces cohesion;
> feed-forward pipelines invite decomposition.

Erosion's transport, deposition and slope relaxation are steps in a loop whose state at iteration
`n` depends on iteration `n-1`. You cannot lift deposition out and run it, because what it
deposits is what transport carried, which is what the previous pass eroded. Therefore erosion is
cohesive. Shore distance, exposure and profile shaping are a feed-forward chain: each is computed
once, from the terrain, without reference to the others' results. Therefore the coast decomposes.

A useful corollary, checkable by inspection: **do the intermediate results stand alone as useful
fields?** Signed shore distance is useful with no coast in sight; exposure serves sun, wind and
snow. Treat this as a corroborating signal rather than the test, because it does not survive on
its own. Erosion writes flow, water and sediment as layers precisely so downstream nodes can
consume them, so by the letter of the standalone question erosion would decompose, which is the
wrong answer. The iteration criterion gets both cases right without a qualifier.

The two principles reconcile rather than conflict: the cohesion test governs where the
**native-code boundary** sits; the many-small-nodes preference governs what is built above that
boundary. A cohesive native erosion node is still *wrapped* by authored compositions that preset,
mask, and blend it. The session did not reject the erosion principle for coasts; it discovered the
coast was never on erosion's side of the line.

### 3.7 Acceptance: machinery and look are separate tests on the same artifact

Handoff §4.6 worries the coastal subgraph is a hard first validation target and suggests finding
an easier one. The risk evaporates once the two reasons it is hard are separated into two
acceptance tests:

1. **Machinery acceptance** (gates Phase 2). The beach subgraph declares `beach_width` (metres),
   `amplitude` (metres), and a profile curve. `W` is entered exactly once and reaches both the
   Levels window and the mask via references. Amplitude reaches the inner parameter as
   `amplitude / world_height`, written at the reference site. Defaults and ranges render in the
   inspector like any native node's. Checkable without ever judging whether it looks like a beach,
   and it exercises precisely the three capabilities that matter: multi-binding,
   arithmetic-at-reference, interface rendering. The duplicated-`W` problem, closed.
2. **Look acceptance** (belongs to the coastal workstream). The beach reads as a beach across
   coast types. Depends on the Exposure node and profile-curve presets; does not gate the
   authoring machinery and is not gated by it.

Conflating these would judge the interface design by the hardest aesthetic problem in the queue.
Split, the machinery test *is* the easy first target, on the motivating artifact.

### 3.8 Scale literacy is a tooling problem

Handoff §4.4, surfaced three times (noise wavelengths, beach width against island size, the
standing cross-section request). Three answers in cost order, all adopted:

1. **World-relative annotation on every `Meters` parameter.** A 60 m beach-width slider on a
   1000 m world shows "6% of world" for nearly nothing. This alone would have caught the flattened
   island before evaluation did.
2. **The cross-section/profile tool.** Its third surfacing, and it was also the diagnostic
   instrument the whole handoff session ran on. Build the thing the debugging already proved out.
3. **World-aware defaults at instantiation.** When a node is dropped, length defaults are computed
   as a sane fraction of the current world extent rather than fixed constants. An initial
   parameter value only, so no determinism implications. The 60 m default was not wrong in
   general; it was wrong *for that world*, and the node had the information to know it.

Items 1 and 3 apply to authored nodes automatically, because their parameters carry the same
`Unit` metadata (§3.2).

### 3.9 What this document does not settle

Recorded so the gap is visible rather than assumed closed: **bypass semantics for a multi-port
container**, deferred since the original subgraph design and untouched here. It gets more pressing
once subgraphs are the primary authoring surface, since bypassing an authored node is the natural
way to A/B its contribution.

### 3.10 Decision table

| Decision | Why |
|---|---|
| Expressions are the prerequisite for the interface, and sequence first | An interface without referencing reproduces the duplicated-`W` failure |
| Expression scope: enclosing interface, world globals, the node's own parameters | Nothing a subgraph needs from outside is reached for, so it carries no hidden requirement about where it can be used |
| World globals are exempt; user-defined globals are not | Built-ins exist in every project; a user global may be absent in the next one |
| Cycles are per-node, checked by a local topological sort | Same-node references are the only reachable loop, and they earn their place at an authored node's call site |
| Declared defaults are literals; instance values may be expressions | A default travels into projects that know nothing about it |
| Interface declarations use the native `ParamSpec` shape | Inspector introspection renders authored and native parameters through one path |
| Definition model: embed-with-provenance | Self-contained projects (copy's virtue) + one definition per project (instance's virtue) + detectable staleness via content hash |
| The definition id lands in Phase 2, not Phase 3 | Authored-node identity must exist before the project format freezes around it |
| Library is a distribution channel, never a runtime dependency | The HDA scan-path failure class is excluded by construction |
| Exported `.ymirsub` inlines nested definitions | Distribution never grows dependency resolution |
| Schemas converge; implementations do not | One kind of node for users; native code where computation is coupled, per-cell, or GPU |
| Cohesion test: does the computation feed back on itself? | Gets erosion and the coast right without a qualifier, unlike the standalone-intermediates form |
| Machinery and look acceptance are separate tests | The interface design is not judged by the hardest aesthetic problem in the queue |
| Scale literacy gets tooling: annotation, cross-section, world-aware defaults | Third surfacing of the same need; documentation would not have caught the flattened island |

## 4. Negative space

Deliberately excluded, with reasons, so absence reads as decision rather than omission:

- **Cross-node parameter references** (Maya/Nuke style, any parameter to any parameter). The
  invisible-spaghetti failure mode; excluded by the scope rule in §3.1.
- **Reaching into a parent graph from inside a subgraph.** Excluded for portability: it would make
  a subgraph work only where the surrounding graph happened to supply the right thing.
- **Path syntax in expressions** (`ch("../../x")`). One-level scope makes it unnecessary; its
  absence keeps the resolver and the mental model small.
- **Promotion-with-unit-conversion as a mechanism.** Proposed and withdrawn in the handoff session
  (§3.2 there); the survey confirmed no mature tool has it. Arithmetic at the reference site does
  the job.
- **Live library references.** Breaks project self-containment; the HDA pain imported wholesale.
- **Dependency resolution for distributed subgraphs.** Foreclosed by inline-on-export.
- **A package registry or ecosystem tooling.** Unplanned. The file-level affordances (provenance
  triple, inlining) are the full commitment.
- **Conditional parameter visibility and instance-divergence locks.** Deferred until something
  needs them, not designed now.
- **Erosion decomposition.** The cohesion test keeps erosion native and whole; nothing in this
  strategy reopens it.

## 5. Consequences for existing documents and decisions

- [`subgraphs.md`](subgraphs.md): status annotation. The "copy with no link back" decision row is
  superseded by §3.3; the "editing a library entry in place" deferred item is answered by it; the
  status header ("not yet built") is stale against the shipped feature and should point at the
  handoff's §3.0 as the accurate current-state description.
- [`coastal-erosion.md`](coastal-erosion.md): annotate, do not rewrite. Thesis and survey stand.
  Stage 8's analytic profile is superseded by the artist-editable curve with physical presets; the
  monolithic node decomposition is superseded by composition; the eikonal prerequisite is shipped.
  Re-ground the document only when the composed coast ships.
- [`node-taxonomy.md`](node-taxonomy.md) item E (field-driven parameters): unchanged, runs as the
  parallel track. The three-source model (§2) is the shared frame; the workstreams stay decoupled.
- [`project-format.md`](project-format.md): gains the embedded-definitions section when Phase 3
  lands; the format version and migration seam already anticipate it.
- `modifier.coastal`: remains parked, per the handoff. The composed coast is the path; the presets
  that would have been baked into the node become interface presets on the authored version (the
  coastal doc's profile tables, landing as data).

## 6. Phasing

The dependency spine: expressions, then interface, then definition model, then visibility.
Directability runs beside it, not inside it. Each phase decomposes into issues under
[#373](https://github.com/liminalfield/ymir/issues/373); each step ends compiling, tested, and
`fmt`/`clippy`-clean per the house rules.

**Phase 0, land the session.** The signed-distance output and sea-level contour on `Distance`,
plus the `Levels` input-window widening: shipped in
[#372](https://github.com/liminalfield/ymir/pull/372), closing
[#146](https://github.com/liminalfield/ymir/issues/146). Remaining: the `Levels` editor
([#369](https://github.com/liminalfield/ymir/issues/369), a stated-principle violation with the
histogram plumbing already feeding the curve editor a few lines away), and the status annotations
in §5.

**Phase 1, the value-source seam.** Expression-driven scalar parameters, engine side
([#371](https://github.com/liminalfield/ymir/issues/371)). The compiler moves out of the node
crate. Resolve-before-eval with the resolved value in the cache key. Per-node cycle detection
(§3.1a) with a reported parameter error. The scope rule implemented as the variable environment,
built as a name lookup that can grow rather than a fixed set, since user-defined globals
([#374](https://github.com/liminalfield/ymir/issues/374)) would otherwise mean rebuilding it. The
computed-value visual state, CVD-safe. This phase is useful standalone (`sea_level + 2` on any
node's parameter, before subgraphs gain anything) which gives it its own acceptance independent of
the phases behind it.

**Phase 2, the parameter interface.** The declaration list on the definition (native `ParamSpec`
shape, plus ordering and grouping). The definition/instance storage split. The stable definition
id (§3.3a). Inspector rendering by introspection. Inner-parameter referencing against the
interface. Gate: **machinery acceptance on the beach subgraph** (§3.7, criterion 1).

**Phase 3, embed-with-provenance.** The embedded-definition project format (one definition per
subgraph type per project; instances hold values and seed overrides). The provenance triple in
`.ymirsub` and in the embedded copy. Staleness detection on open and the explicit, user-initiated
update flow. Inline-on-export for nested definitions. Format-version bump with migration from the
shipped per-instance form.

**Phase 4, authored nodes become visible.** Palette integration, by whichever mechanism §3.3a's
open choice resolves to. Interface presets (where Dean's curve and the grain-size slope tables
land, as data on the authored coast). Thumbnails and icons as the library panel already implies.
Deferred items stay deferred.

**Parallel track, directability** ([`node-taxonomy.md`](node-taxonomy.md) item E). Promotion,
`ParamSpec.modulatable`, the data-input/modulation-input arity distinction, the cache-key change,
the resolved-parameter accessor. Core-side work with its own spec; shares the resolve-before-eval
philosophy with Phase 1 and nothing else structural. Forcing it into the authoring epic would
couple a GUI/format workstream to a core arity/cache workstream for no gain.

**Woven through, not a phase: scale literacy** (§3.8). The `Meters` annotation is small enough to
land opportunistically; the cross-section tool is its own issue; world-aware defaults land
per-node as nodes are touched.

## 7. Open questions

Genuinely open, as opposed to decided-and-recorded above:

- **Expression entry affordance.** How a parameter widget switches from literal to expression (a
  prefix character, a context action, a mode toggle) and how the computed-value state renders
  within the Frost theme's CVD constraints. GUI design, not architecture; settle in Phase 1's GUI
  step.
- **Interface editing surface.** Where the author edits the declaration list: a panel inside the
  dived-in view, a properties dialog on the container, or promotion gestures from inner parameters
  ("expose this") that build the interface incrementally. Likely all three eventually; which ships
  first is open.
- **Palette source for authored nodes.** Whether the palette merges a second source or the
  registry accepts runtime entries (§3.3a). Phase 4, but worth deciding earlier if it constrains
  the definition id's shape.
- **Version field semantics on definitions.** A bare integer, or semver-shaped? The content hash
  does the correctness work either way; the version is for humans. Leaning integer.
- **Grouping representation.** Folders, tabs, or labeled separators in the declaration list.
  Cosmetic, but it is in the definition format, so it should be decided before Phase 3 freezes the
  embedded shape.
- **What the update flow shows.** "Library has a newer version" needs a diff the user can
  evaluate. A parameter-level diff is cheap (the declaration lists are data); an inner-graph diff
  is a graph-diff problem and probably starts as "open both and look".
- **Nested-subgraph depth.** The recursion should fall out (per [`subgraphs.md`](subgraphs.md));
  confirm rather than special-case, and do not chase arbitrary depth until something needs it.
  Unchanged from the prior document, restated here because inline-on-export (§3.4) depends on
  nesting existing.

## Revision history

- **2026-07-31**: ratified. Reference scope re-grounded on portability and widened to include the
  node's own parameters (§3.1, §3.1a), with the cycle check scoped to one node and the
  default-versus-value distinction recorded. Authored-node identity pulled forward to Phase 2
  (§3.3a). Cohesion test restated on the iteration criterion, since the standalone-intermediates
  form gives the wrong answer for erosion (§3.6). Phase 0 updated for #372. Tracked as #373.
- **2026-07-31**: initial strategy, from the authoring handoff and the design corpus. Positions
  taken on handoff §4.1 (schemas converge, implementations do not), §4.1b (embed-with-provenance),
  §4.2 (inline-on-export, no ecosystem), §4.3 (the standalone-intermediates test), §4.4 (tooling,
  three tiers), §4.5 (annotate), §4.6 (machinery/look split). Supersedes `subgraphs.md`'s
  copy-with-no-link-back decision.
