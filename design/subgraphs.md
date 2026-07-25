> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: subgraphs (container nodes)

Status: **design agreed, not yet built (#106, mechanism from #79).** This supersedes the
earlier "catalogue of pasteable fragments" sketch. The decision below is to build the
dive-in container directly, which #79 had deferred behind a cheap flat-paste form. We are
reordering: build the container, skip the flat form. The container still follows #79's
locked mechanism (template instantiation: copy on use, no link back to a definition).

## What a subgraph is

A subgraph is **a node that contains a graph.** It shows input and output ports derived
from what is wired inside it, and you treat it like any other node on the canvas. You
**dive into** it to edit the insides, and **pop out** to wire it into the rest of the
graph.

That one sentence is the whole idea. Everything below is how you make one, how the ports
come about, and how a shared subgraph reproduces the same terrain for everyone.

## How you use it (the three flows)

**Build one from scratch (bottom-up).**
1. Drop an empty Subgraph node.
2. Dive in. Add Input and Output nodes for the pins you want to expose, and build the
   graph between them.
3. Pop out. The node now has those ports. Wire it up like anything else.

**Make one from existing nodes (top-down).**
1. Select a set of nodes on the canvas.
2. Right-click, "Create subgraph".
3. The selected nodes move inside a new Subgraph node. The engine adds Input and Output
   nodes for every wire that crossed the selection boundary, so the surrounding graph
   stays connected, now to the new node's ports.

**Reuse one from the library.**
1. Open the library, pick a subgraph (name and thumbnail).
2. It drops in as a fresh container, a copy you can dive into and edit freely. No link
   back to the library entry.

## How the ports come about

Ports are **explicit**: each one is an Input or Output node living inside the subgraph.
We chose explicit over inferring ports from "unconnected pins" because optional inputs
make inference ambiguous (an unwired optional input could be a port or just unused).
Explicit removes the guess.

When you "Create subgraph" from a selection, the engine generates those boundary nodes
from the wires that cross the boundary:

- **One input port per internal input pin wired to a node outside the selection.**
- **One output port per internal output pin that feeds anything outside.** Fan-out stays
  a single port (one inside pin feeding three outside nodes is one Output node, fanning
  out on the outside).
- **An unwired optional input gets no port.** To expose it later, dive in and add an
  Input node by hand.

```
Before, selecting {Warp, Erode}:

   Noise ──▶ Warp ──▶ Erode ──┬──▶ Export
                              └──▶ Slope

After "Create subgraph":

   Noise ──▶ ┌ Subgraph ───────────────────────┐ ──▶ Export
             │  In ─▶ Warp ─▶ Erode ─▶ Out      │ ──▶ Slope
             └─────────────────────────────────┘
```

Ports take their names from the boundary nodes (auto-named at first, renamable by diving
in), and you can reorder them inside.

## The same Mount Fuji everywhere

If you dial in a subgraph that looks like Mount Fuji and share it, everyone who uses it
gets that same mountain. This is what the engine's determinism is for, with one precise
boundary:

- The **pure parts** (noise, shapes, anything sampled per cell) come out **bit-for-bit
  identical** on any machine.
- The **iterative parts** (erosion) come out **visually the same**, with possibly a few
  least-significant bits differing across machines or core counts.

So "a mountain that looks just like Mount Fuji" is the exact claim. Bit-identical files
across machines is not promised and not needed.

For this to hold, **a subgraph carries its own seed, baked into the file.** A node's
randomness normally derives from the project's global seed plus the node's id; if a
dropped subgraph re-derived from the host project's seed and the fresh ids it gets on
drop, it would come out a different mountain. So a subgraph establishes its own seed
world that travels with it. (This is #79's "composable seed" seam, now load-bearing.)

That same mechanism is a switch the author controls:

- **Keep the captured seed** and everyone gets *the* Fuji.
- **Reseed an instance** (a reseed action, or dive in and change it) and you get a
  different mountain from the same graph, which is what you want for a generic "mountain"
  or a "dune field" you scatter around.

## It is a node, so most of it already exists

Because a subgraph is just a node, every node interaction applies to it for free, with no
extra design:

- drop-on-wire splice (#124), click-to-wire and wire-to-create (#50, #123)
- duplicate, bypass, pin-preview
- marquee select, delete, frame grouping (#94)

The only edge: drop-on-wire can splice a subgraph only if it has at least one input and
one output port, same as any node.

## The decisions, and why

| Decision | Why |
|---|---|
| A subgraph is a container node, not a flat paste | A container can be bypassed, previewed, and wired as a unit; a flat paste cannot |
| Build the container directly; skip the flat-paste feature | The flat form was a stepping stone we do not need as a product surface |
| Keep one piece of the flat form: copy-a-subgraph (node subset + internal wiring + fresh ids) | Create-from-selection, library-drop, and duplicate all need it; it is the first engine step |
| Ports are explicit Input/Output nodes inside | Inferring from unconnected pins is ambiguous with optional inputs |
| Create-from-selection derives ports from boundary-crossing wires | Matches "ports come from the internal structure" without manual setup |
| A subgraph carries its own seed | So a shared subgraph reproduces the same terrain everywhere |
| Library-drop is a copy with no link back | #79's template instantiation: no definition-versioning or fork-on-edit rabbit hole |
| Built-ins ship inside the app; user library at `~/.config/ymir/subgraphs/` | Built-ins cannot be clobbered; user files stay portable and git-friendly |

## What this asks of the engine

This is the honest cost. The container is the nesting engine #79 said arrives "once the
engine can nest", so it is more than a GUI feature:

1. **A node that holds an inner graph and evaluates it.** Recognising "this node contains
   a graph, recurse into it" is a structural distinction at topology level, not a check on
   operator identity, so it does not break the "nothing asks which node is this"
   invariant.
2. **Per-instance, derived ports.** Today a node's ports are a fixed schema per type. A
   subgraph's ports vary per instance, computed from its insides. This is the real new
   capability.
3. **Input/Output boundary node types** that mark where the inner graph meets the
   container's ports.
4. **A self-contained seed** for the inner graph, captured in the file (the composable
   seed seam).
5. **Nested serialization.** The project and subgraph file formats hold a graph that
   contains graphs. Both already carry a format version, so this is a forward-compatible
   evolution, not a break.

## Build order

In dependency order. Each step ends compiling, tested, and runnable, per the house rules.

1. **Copy-a-subgraph primitive**: copy a node subset with its internal wiring, minting
   fresh ids. Shared by everything below.
2. **Engine nesting**: a container node that holds and evaluates an inner graph, with the
   Input/Output boundary node types.
3. **Self-contained seed**: nested evaluation seeds from the subgraph's captured seed.
4. **Dive in / pop out** navigation in the editor (an inner canvas and a breadcrumb).
5. **Create-from-selection** (top-down), using the port rule and the copy primitive.
6. **Bottom-up authoring**: empty Subgraph node plus adding Input/Output nodes inside.
7. **Save to library / export to file**, and the library panel to browse and drop
   (the original #106 surface, now secondary to the in-graph container).
8. **Reseed action** and **bypass semantics** for a multi-port container.

## Deferred or open

- **Bypass semantics for a multi-port container.** What "bypass" routes when a node has
  several inputs and outputs is not obvious; settled in step 8, not now.
- **Editing a library entry in place.** Drops are copies with no link back, so "update
  the library" means re-saving. Whether to offer an explicit "open library entry, edit,
  save back" flow is a later question.
- **Nested subgraphs (a subgraph inside a subgraph).** The recursion should fall out of
  the engine nesting naturally; we will confirm it works rather than special-case it, and
  not chase arbitrary depth until something needs it.
