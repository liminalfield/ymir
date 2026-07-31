> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Ymir: from using nodes to authoring them

Status: **session record, superseded as a plan by
[`subgraphs-as-authored-nodes.md`](subgraphs-as-authored-nodes.md).** Kept for the measurements in
§1 and §2, which are evidence rather than reasoning, and for the survey in §3.3. The strategy
document takes positions on everything §4 leaves open; where the two differ, the strategy governs.
The body below is as written on 2026-07-31 and is not updated as work lands. Direct quotations are
reproduced verbatim.

A handoff for strategy work. Written 2026-07-31, at the end of a session that started as a
complaint about a beach and ended somewhere structural.

Nothing here needs repository access. Where a fact came from measurement rather than reasoning, it
says so, because several conclusions in this session were reached only after a theory was checked
and found wrong.

---

## 0. What Ymir is, in one paragraph

An open-source, native-Linux, node-based procedural terrain generator in Rust. GPL-3.0, personal,
non-commercial, but built to a standard that would survive review by an experienced Rust
developer. One universal data type (`Field`: a 2D grid with named scalar layers plus a small bag
of scalar globals) flows on every edge, so the engine never needs to know what any node does. The
governing architectural rule is that **nothing in the application may ask "which node is this?"**,
so everything either dispatches polymorphically or reads the node's own declared schema. Adding a
node touches only its own new file.

The stated philosophy prefers **many small single-purpose nodes over few multi-purpose ones**, on
the grounds that a graph should be readable from its wiring rather than from parameters buried
inside nodes. That principle is directly load-bearing in what follows.

---

## 1. How the session got here

It began with a specific frustration: *"I am trying to create an actual beach — literally, a ring
of sand, flattish, around the islands. But I am totally unable to accomplish this."* Strength
"just squashes down the terrain until it's flat", and berm height was "acting really strangely, I
don't even know how to describe it."

That was diagnosed by measurement rather than by reading the code, and it produced three findings,
the third of which turned out to matter far beyond coasts.

### 1.1 A scale mismatch the tool did nothing to surface

The default graph's islands are about **30 m across on a 1000 m world**. The Coastal node's
default beach width is **60 m**, wider than the island. Every cell was inside the beach zone from
both sides, so the whole island flattened. Measured cross-sections:

| beach width | profile inland, metres above sea |
|---|---|
| 60 m (default) | 0.0 0.1 0.1 0.2 0.3 0.3, island gone |
| 20 m | 0.1 0.2 0.4 0.6 0.8 1.0 1.2 |
| 8 m | 0.2 0.9 1.7 2.6 3.7 4.8 6.4, island survives |

The user's own reaction: *"again, it's a matter of not understanding the scale that I'm working
with."* This is the **second** time in the session that scale literacy was the real problem (the
first being noise wavelengths). Worth treating as a recurring theme rather than two incidents.

### 1.2 A parameter named for something it does not do

The node computes the beach face as `berm_height / beach_width`, so `berm_height` sets the
*grade*, not a crest height. And because the node only ever cuts down (`min(terrain, envelope)`),
raising `berm_height` raises the envelope and therefore cuts **less**, so the terrain comes back.
Measured:

| berm_height @ 20 m width | profile |
|---|---|
| 0.5 m | 0.0 0.1 0.1 0.2 0.2 0.3 0.3 |
| 20 m | 0.0 3.9 5.8 7.7 9.7 11.6 |

The parameter is named for a thing it cannot do, because the node is subtractive only. It can
bevel a hillside; it can never deposit sand. On an already-gentle coast it does nothing at all.

### 1.3 The model itself

Ymir has a thorough internal design document for coastal erosion (~1100 lines), written before the
shipped node. Its own thesis is the user's complaint: *"the coast is a bevel applied to a contour,
and it reads that way."* The shipped node is deliberately that document's "lean v0".

The document's answer is **wave exposure**: keep the geometry, but let computed wave energy decide
how much reshaping happens where, so headlands erode into cliffs while bays accrete beaches. That
gives variety instead of a uniform ring, and it is cheap, needing directional sweeps and a
distance field, no fluid simulation.

**Input from another model (Claude Fable) sharpened this usefully.** Two points survived scrutiny:

- The commercial field has not moved. World Machine's Coastal Erosion is described by its own
  developer as "nothing but a quick approximation, done without simulation"; Gaea 2's Sea node is
  a nicer bevel with better plumbing. Nobody ships exposure, fetch, or an alongshore sediment
  budget.
- **The design document does not fix the artifact the user is objecting to.** Its stage 8 still
  builds the profile analytically as `min(berm_height, beach_face_tan * φ)`, the same hard crease
  as the shipped v0. Exposure changes *where* beaches appear and *how big*; it never changes the
  cross-section's shape.

Fable's proposal: make the beach cross-section an **artist-editable curve** parameterised by
signed shoreline distance, with the physical profiles (Dean's equilibrium curve, grain-size slope
tables) becoming *presets* on that curve rather than the mechanism.

That proposal is more consistent with the design document than the document is with itself. The
document's own priority order is (1) does it look good, (2) is it loosely plausible, (3) is it
physically defensible, with item 3 explicitly "a means, not an end". Yet it then specifies that
the profile constants are "derived from grain size and wave climate, not exposed as free sliders
by default." That is physics as mechanism, which its own section 1.1 forbids.

**Decision: `modifier.coastal` is parked.** Not deleted; it produces something with one slider.
But no further work goes into the bevel.

---

## 2. The turn: compose the coast instead of building a bigger node

If the profile should be drawn rather than derived, the coast stops being a monolithic node and
becomes a graph. That aligns with Ymir's stated preference for many small nodes, and it means the
pieces pay for themselves elsewhere:

- **Shore distance.** The signed distance field is the contour's own coordinate system. Its
  gradient is the shore normal and its level sets run parallel to the shore, so any "distance from
  the shore" effect becomes a one-dimensional transfer function with no direction maths.
- **Exposure.** The same directional-sweep machinery serves sun, wind and snow. An exposure field
  is immediately useful for wet-rock, vegetation and weathering masks with no coastal node
  present.
- **Profile shaping.** A curve widget already exists for height shaping.

The design document explicitly rejects this ("erosion nodes are cohesive models, not a
construction kit"), but that principle was imported from the *erosion* roadmap, where it is right
because the sub-steps are physically coupled: you cannot run deposition without the transport that
fed it. A coast is not like that. Distance, exposure and profile are genuinely separable, and two
of the three are wanted independently.

### 2.1 What was built

Signed distance was shipped this session. The `Distance` node now emits the measurement itself in
world metres (sign preserved), alongside its existing `[0,1]` proximity band, and gained a choice
of contour: a height you name, or **the world's sea level**, where "sea" is not merely
`level = sea_level`, because only water connected to the map edge counts, so an enclosed hollow
measures its distance to the real coast instead of seeding a shore of its own.

One dependency had to be fixed to make that output consumable: the `Levels` node's input window
was clamped to ±4, which cannot window a field carrying metres. Widened. Its own documentation
calls it the tool for normalising out-of-range values, so a window stopping at 4 was a limitation
rather than a missing feature.

### 2.2 What was learned, which is the important part

The user tried to build the beach from these parts and **gave up**: *"I have no idea what
parameters are required to get something usable there. I tried and I give up."*

The chain was then built and measured, and the assistant's own suggested wiring **was wrong**.
Adding a profile to the terrain produces a swelling on a cliff, not a beach: the coast rises
74 to 127 m across the relevant cells and a 3 m profile adds nothing readable. A beach requires
*replacing* the terrain near the water with a profile anchored at sea level, which is a different
and larger graph:

```
terrain ─┬─→ Distance(sea) ─[distance]→ Levels(0..W m) → Curve → Levels(sea .. sea+A) ─┐
         ├─→ Distance(sea, range=W, side=outside) ──────────────────────────[mask]─┐   │
         └────────────────────────────────────────────────────────────[base]─→ Blend(normal)
```

Six nodes. And critically: **the beach width `W` appears in two of them and must match.** The
window that shapes the profile and the mask that places it are separate numbers with no
connection; if they disagree the profile is cut off mid-slope or the mask feathers over nothing.
Nothing in the graph communicates this. The amplitude is also still entered as a raw fraction
(`3 m / 256 m world` = `0.0117`).

So the experiment answered a question, but not the one it was set: the problem is **not** that
parameters are scattered. It is that one *conceptual* parameter is duplicated across several
nodes, with an invisible constraint between them, and that some parameters are entered in units
nobody thinks in.

That is the finding the strategy has to absorb. **Composition alone does not scale to authorable
tools.**

---

## 3. The structural turn: subgraphs as authored nodes

### 3.0 What a Ymir subgraph is today

There is already a shipped subgraph feature with its own design document, and the strategy has to
build on it rather than beside it. The mechanics:

- **A container node holding an inner graph**, evaluated by recursion, not a flat paste. Chosen so
  it can be bypassed, previewed and wired as a unit.
- **Ports are explicit marker nodes inside.** An `Input` marker names a field fed from outside, an
  `Output` marker names one exposed outside, and the container's ports are derived from them. This
  was described at the time as "the real new capability", because every other node has a fixed
  schema per *type* while a subgraph's ports vary per *instance*.
- **It carries its own seed**, used as the *absolute* global seed for the inner graph rather than
  an offset from the host world's seed. That self-containment is deliberate: it is what lets a
  shared subgraph reproduce the same terrain in any project.
- **Library entries are standalone git-friendly JSON** (`.ymirsub`) at `~/.config/ymir/subgraphs/`,
  carrying a format version, display name, free-text category, description, and **per-port
  documentation** (each port's index, name and a human description).
- **Built-ins ship inside the app; user files stay portable.**

So a meaningful part of "make it feel like a node" already exists: a name, a category, a
description, documented ports, and a stable identity. **The missing piece is the parameter
interface.** The container's schema currently hardcodes exactly one parameter, `seed`.

Three things were already recorded as deferred: bypass semantics for a multi-port container,
editing a library entry in place, and nested subgraphs.

### 3.0.1 One existing decision is now in tension

The subgraph design states, as a decision with a reason:

> **Library-drop is a copy with no link back**, for "no definition-versioning or fork-on-edit
> rabbit hole."

That was right for templates. It is questionable the moment a subgraph is a *custom node*. If you
author a coastal node, use it in four projects, and then fix a bug in it, the copy model means
four manual replacements and no way to tell which instances are stale. The node model implies a
**definition plus instances**, which is exactly the versioning problem that decision was taken to
avoid.

This is a genuine fork and the strategy should take a position on it explicitly rather than
letting it be settled by whichever gets built first. Houdini's answer (HDAs with
definition/instance separation, versioned type names, "allow editing of contents" per instance) is
the mature form and is also, by common consent, one of the more confusing parts of Houdini to
learn.

### 3.1 What a parameter interface actually requires

Concretely, beyond "expose some parameters":

- **A declaration list** on the subgraph: name, kind, range, default, unit, description. This is
  the same shape as the schema a native node declares, which is a point in favour of the two
  converging (see §4.1).
- **Ordering and grouping**, because a node with fifteen parameters and no structure is the sprawl
  problem in a smaller box. Every mature tool has folders or tabs here.
- **Storage in two places**: the interface belongs to the subgraph *definition* (the `.ymirsub`
  file); the values belong to each *instance* (the project file). That split is the same one the
  definition/instance question above turns on.
- **Rendering**, which is close to free: Ymir's inspector already builds parameter UI by
  introspecting the declared schema rather than by per-node widget code, so a subgraph's declared
  parameters would render like any other node's.
- **Referencing**, which is the part that does not exist. See §3.3 and §3.4.

Adjacent capabilities that were raised as "maybe some other functionality as well", none of them
designed:

- Conditional visibility (show or disable a parameter depending on another).
- Presets: named sets of interface values, which is where the *physical* profile tables from the
  coastal document would naturally live (Dean's curve, grain-size slope tables) rather than being
  baked in as mechanism.
- An icon, and whether authored nodes appear in the palette alongside built-ins.
- Whether the internals can be locked, and whether an instance may diverge from its definition.

The user's objection to a subgraph, stated before any of this was built, was exactly right in
advance:

> *"the problem with that is the number of parameters across nodes that might need to be adjusted
> to get something that looks good and works well, or is easy to customize, vs. a 'coastal
> erosion' node that has optimal values built in or set as defaults, with reasonable ranges for
> tweaking values."*

**A Ymir subgraph currently exposes exactly one parameter: `seed`.** There is no promotion, no
interface, no defaults, no ranges. So the comparison was never fair: the thing that would make a
subgraph a real alternative does not exist.

The user's response on hearing that: *"I hadn't thought about building subgraphs with that
capability — but I like the idea an awful lot. That would really let me build subgraphs that were
like custom nodes."*

**This is the strategic shift, and it was not planned.** Ymir becomes a tool where users author
nodes, not only use them.

### 3.2 A proposal that was made and then withdrawn

The first design was: a promoted parameter binds to *several* inner parameters, and carries a
*unit conversion* (metres in, normalised out).

The user pushed back on instinct: *"Unit conversion starts to make things even more complicated.
It's starting to feel like a band-aid type solution... I want to make sure that the functionality
that we're building into enhanced subgraphs is industry standard... and not stuff that we're
MacGyvering to make this work."*

That instinct was correct, and checking the field confirmed it. **No tool that does this well has
"promotion with a conversion."** They have two separate things:

1. A subgraph exposes a **parameter interface**: named, typed, with ranges, defaults and units.
2. Inner parameters get their values by **referencing** it.

Houdini references by channel expression (`ch("../beach_width")`). Blender wires a Group Input
socket into inner node inputs. Substance exposes a parameter and computes derived values in a
function graph.

Multi-binding and unit conversion are not features in any of them. They **fall out**. Two inner
parameters referencing the same interface parameter is multi-binding.
`ch("../berm_height") / ch("../world_height")` is unit conversion. The proposed bespoke mechanism
was a weaker version of something settled decades ago, and it was withdrawn.

### 3.3 Expressions or wires, and the answer from tools that shipped both

The user asked whether any tool implemented *both* and left the choice to the author. Several did:

- **Maya.** An attribute can be driven by a DG connection or by an expression, author's choice.
- **Nuke.** Knobs can be linked or given expression text.
- **Blender.** A value can be wired from a Group Input socket, *or* the property can carry a
  driver (an expression referencing other properties). Same value, two ways, decided per property.
- **Houdini** has both but split by *context* rather than choice: SOP networks use channel
  references, VOP networks are wires. The network type decides, not the author.
- **Substance** resolved it the other way entirely: exposed parameters plus *function graphs*, so
  its expressions are themselves wire graphs. It unified by making the expression graphical.

What the tools that ship both learned is the useful part: **they are not competitors, they do
different jobs.** A wire is right when the relationship is part of what the graph *is* and a
reader must see it. An expression is right when it is arithmetic glue nobody should have to look
at. Maya's production guidance drifted toward connections for anything others maintain,
expressions for local one-offs.

They also all solved discoverability the same way: **an unmissable visual state on the parameter
itself** (Maya colours driven attributes, Blender turns driven properties purple, Nuke marks
expression knobs). So "expressions are less discoverable" is a real cost with a known, cheap fix.

The user's preference: *"I like expressions, they are much cleaner, and reduce the amount of
wiring needed."*

### 3.4 Where it landed

Ymir does not have to choose, because it was already going to have both. A separate planned epic
("Directability") wants parameters driven by **fields**: spatial, per-cell, and inherently wired
since a field arrives on a connection. Expression-driven scalars are the other case.

So the division falls out along a line the roadmap already draws:

> **A parameter is a literal, an expression (computed, scalar), or a field (spatial, wired).**
> One seam, three sources. Spatial variation is wired; computed scalars are expressions.

This was filed as a design issue. Key constraints recorded in it:

- Ymir already has a real expression compiler (bytecode, fixed variable environment, unknown
  identifiers are compile errors rather than silent zeros, hand-rolled specifically so its numeric
  behaviour is byte-stable). It currently only runs per-cell inside one node. Running it once per
  node against a scalar environment is the same engine with a different variable set. It would
  need to move out of the node crate, since the thing resolving a parameter cannot depend on it.
- The **resolved value**, not the expression text, goes into the memoisation cache key. Otherwise
  an expression whose referenced parameter changed would hit a stale entry.
- Resolution happens **before** a node evaluates, so no node learns that expressions exist. This
  preserves the "never ask which node this is" invariant.
- A cycle in parameter references must be detected and reported, never hang.
- The computed-value visual state cannot rely on a red/green distinction (the maintainer is
  red/green colourblind).
- The unit problem dissolves: a subgraph parameter declared in metres is a metre value, and an
  inner parameter wanting normalised height writes `berm_height / world_height`. Arithmetic in the
  reference, no machinery.

---

## 4. Open strategic questions the session did not answer

These are the ones a strategy document probably has to take a position on. They were surfaced but
deliberately not decided. All are answered in
[`subgraphs-as-authored-nodes.md`](subgraphs-as-authored-nodes.md).

**4.1 Does the built-in node set and the authored node set converge?** If a subgraph can declare a
parameter interface with defaults, ranges and units, it is a node in every respect a user cares
about. Does a shipped node eventually become "a subgraph that happens to ship"? If not, what stays
uniquely native, and why? There is an existing "one new file per node" invariant that assumes
native nodes; it is unclear whether authored nodes live alongside or eventually subsume that.

**4.1b Definition-and-instance, or copy?** The current design explicitly chose
copy-with-no-link-back to avoid versioning. Authored nodes push toward definition-plus-instances.
Whichever is chosen has consequences for editing, for distribution (§4.2), and for what happens to
existing graphs when an interface changes. This is probably the single highest-consequence open
question in the set, because it is very expensive to reverse once projects exist that depend on
either behaviour.

**4.2 What is the distribution story?** There is a standing intent to import node networks from a
git repo into a local library, and the project format is deliberately JSON and git-diffable. That
intent becomes far more valuable once networks have interfaces: it turns into a package ecosystem,
with everything that implies (versioning, breaking changes to an interface, dependency on built-in
node behaviour). Currently unplanned.

**4.3 Does "erosion nodes are cohesive models, not a construction kit" survive?** It was
reaffirmed for erosion and rejected for coasts in the same session. What is the actual test for
which side of that line a capability falls on? A candidate: *are the sub-steps physically coupled,
or merely sequential?* Untested as a general rule.

**4.4 Scale literacy.** Twice in one session the real problem was the user not having a feel for
the scale they were working at, first noise wavelengths, then beach width against island size.
Both were diagnosed only by measuring cross-sections. Is there a tooling answer (a readout, a
scale reference, a measuring affordance in the viewport), or is it a documentation answer, or is
it simply the cost of learning the domain? A cross-section/profile tool is already an open
request, which may be the same need surfacing a third time.

**4.5 Does the coastal design document get rewritten or annotated?** Its thesis holds and its
survey is still accurate. But its stage 8 contradicts its own priority order, its architectural
prerequisite list is stale (the eikonal solver it treats as the pivot is built and shipped; one
prerequisite was solved a different way), and its node decomposition assumes a monolith that has
now been rejected. Leaving it as-is risks someone building from a plan that has been partly
superseded.

**4.6 What is the acceptance test for "authorable"?** The coastal subgraph is currently the
motivating case, but it is a hard one. A strategy might want an easier first target to validate
the interface design before committing to it.

---

## 5. State of play

As of 2026-07-31, when this was written. Not updated since; see the strategy document's Phase 0
for what has landed.

**Shipped this session (merged):** noise sized and positioned in world units throughout (feature
size, cell size and pan all in metres; the world centred on the noise field); a coordinate-hash
collision fixed that had been merging Worley cells and mirroring simplex gradients; a noise-space
explorer (pull back from the world, see the surrounding pattern, drag it under a fixed viewfinder,
resize the world by its outline).

**Built, unpushed:** the signed-distance output and sea-level contour choice on `Distance`, plus
the `Levels` input-window widening.

**Open and relevant:**

- The value-source seam (literal / expression / field), the design issue from §3.4.
- Directability epic (field-driven parameters), pre-existing, now converging with the above.
- An `Exposure` node. Curvature term first (the Laplacian of the distance field, needing no new
  machinery), fetch sweeps second. Deliberately promoted from "deferred utility" to a node in its
  own right, because it is the one piece with no cheap fake and it serves sun, wind and snow too.
- A `Levels` editor. It is five bare sliders where the companion `Curve` node got a visual widget,
  despite a stated project principle that shaping controls need one. The histogram plumbing
  already exists and feeds the curve editor a few lines away.
- `modifier.coastal`, parked, not deleted.

**Not started:** anything in §4.
