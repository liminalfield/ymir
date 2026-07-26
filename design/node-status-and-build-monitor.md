> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: the node status pane

Status: settled in review, not yet built. Supersedes the open questions in #44 (per-node
stale status on the canvas) and #45 (build monitor pane), which this consolidates.

The visual record is [`node-status-pane.html`](node-status-pane.html), beside this file. Open
it from a local checkout: it carries the real palette and typefaces, and every decision below is
drawn there rather than described.

## The idea in one line

> One dock pane listing every node in the graph and what each one is doing, whose columns follow
> the state of the graph while its rows never move.

## What it is for

Three questions, asked at different moments, that the editor cannot currently answer without
clicking through the canvas node by node:

1. **What is out of date?** After an edit, which parts of a long graph will recompute.
2. **What is happening, and how far along?** During a build that takes minutes, which node is
   running and whether it is stuck.
3. **What can I look at in full quality?** After a build, which nodes hold a build-resolution
   result the viewport can show.

## The consolidation

#44 and #45 read as two features and are not. They are two *renderings of one status model*.
Building the model once, then drawing it as a pip on a node header and as a row in the pane, is
what makes this worth doing as a single piece of work.

It also removes #135's cause for the right reason. Thumbnails flash status frames today because
thumbnail evaluation drives the status colour; an explicit model separates "what state is this
node in" from "something is being recomputed for the canvas".

## Two renderings, two fidelities

- **The canvas** carries the dot alone, on every node header, so the graph stays readable at a
  glance.
- **The pane** carries the glyph, the word, and the detail.

Neither is the other's legend, and only the pane is meant to be read.

## The row

The rule the whole layout rests on: **a row's left half is identical in every state, and only
the trailing slot changes.** Nothing moves under your hands when a build starts.

Left half: the state stripe, the glyph cell, the name, and the type id.

Trailing slot, capped at **two marks**, in precedence order:

1. A **status word** when one applies (`stale`, `queued`, `cached`, `written`, `no input`,
   `bypassed`, `skipped`, `excluded`, or an elapsed time).
2. The **view chip**, shown only when a *build* result is held, and naming the resolution it was
   built at. A preview result is not worth a chip: every evaluated node has one, so it would
   repeat what the state glyph already says. A build result is not implied by anything else on
   the row, and a build covers only what fed its targets, so a side branch will not have one.
3. The dashed **build mirror**, shown only when the status word is not already speaking about
   the build. `written` and `excluded` suppress it; `stale` does not.

**The pin takes the glyph cell.** It is an identity flag rather than a status, and it is the more
specific fact about that row. Because it displaces the glyph, **a pinned row always spells its
state out in the trailing slot**, as a word, or as the progress bar and its number while
building. No row is ever left resting on stripe colour alone.

### Colour never carries a state by itself

Every state reads three ways: a stripe, a glyph, and a word. This is not a courtesy. `theme.rs`
already commits to it, and a dense status list is where that commitment is tested hardest. A 3px
stripe is the weakest possible sample of a hue, so it reinforces and never informs alone.

## State vocabulary

| State | Means | Source |
|---|---|---|
| `●` current | Evaluated at the key it would evaluate at now. | `Graph::cache_status` |
| `◐` stale | An edit upstream changed its key; it recomputes on the next pull. | `Graph::cache_status` |
| `○` not evaluated | Never computed at this resolution. | `Graph::cache_status` |
| `▲` failed / no input | Errored, or a required input is unwired. | `graph.input_source`, `evaluate` |
| `⏸` bypassed | Passing its input through untouched. | `Graph::is_bypassed` |
| `◗` active | Computing now, with a percentage where the node reports one. | evaluator progress sink |
| `·` queued | In this build, not started. | evaluator progress sink |
| cached | Skipped: the memo cache already held it. | evaluator progress sink |
| excluded | An endpoint left out of this build. | endpoint `build` param |
| `2048` | A build result is held at that resolution, so the viewport can show this node at build quality. | `FieldStore`, `output_key` |
| `build` | Included in the next build; a mirror, dashed. | endpoint `build` param |
| `⊙` pinned | The display flag, in the glyph cell. | `preview_pin` |

`cached` matters more than it looks. Memoization means most nodes finish instantly, and a
monitor that flickers them past reads as broken. Naming the skip is more honest than animating
it.

## Order, grouping, filtering, density

- **Dependency order, always, in every state.** An explicit user sort offers canvas order,
  alphabetical, and stale first. Sorting is a choice the user makes, never one the state makes
  for them.
- **Subgraphs group, and collapse.** A collapsed header reports its **worst** state as pip and
  stripe, then the count in that state, then the total. Precedence: failed, stale, active, not
  evaluated, current. An all-current group shows the total alone, so a phrase appearing is
  itself the signal.
- **Names disambiguate on collision only.** A dimmed type suffix (`Smooth ·blur`) appears on the
  colliding rows alone, computed over the rows currently visible so it follows the filter. Two
  nodes sharing a name *and* a type take a trailing ordinal in canvas order, since a type suffix
  cannot separate them.
- **A filter field with stale, failed and endpoint quick chips**, and a compact one-line density
  for large graphs.
- **Sort persists with the project; the filter does not.** A filter is a momentary question, and
  restoring one means opening a project to a list that hides most of the graph with no memory of
  why. While a filter is active, the summary states the hidden count and offers a way out, so a
  filtered list is never mistakable for the whole graph.

## The build picture, and what it will not promise

**Elapsed time only, plus a per-node percentage where the operator reports one.** No build-level
estimate: it would be extrapolation over nodes whose costs differ by orders of magnitude, and it
would be most wrong exactly when it matters.

A node that reports no fraction shows an indeterminate track and its elapsed time. Motion is not
load-bearing there: a counter changing every second is data, so under a reduced-motion
preference the hatch fills the whole track and stops moving, which reads as unknown extent
rather than 38 per cent done. Determinate bars are untouched, because they move only when the
value moves.

## Two layers, updated at different rates

The pane shows two kinds of fact, and they must not share a mechanism.

**The model** carries what changes when the *graph* changes: identity, dependency order, wiring,
bypass, build inclusion, preview freshness. It is derived by walking and keying the graph, which
is `O(nodes × depth)`, so it is rebuilt only when the graph, the worker's report or the pin
moves. During a build none of those move, so it rebuilds about twice per build rather than
continuously.

**The progress overlay** is a separate map from node to `Queued`, `Active { fraction, started }`,
`Done { duration }`, `Cached` or `Skipped`. The UI drains the evaluator's progress channel each
frame and updates that map. Its cost is proportional to the number of events, never to the size
of the graph.

A row draws its left half from the model and its trailing slot from the overlay when the node
appears there. That is the mechanical reason the left half is fixed and only the trailing slot
changes: the two halves come from sources that update at completely different rates, and keeping
them apart is what lets a build tick along without touching the graph walk.

**Elapsed time needs no updates at all.** Store an `Instant` when a node goes active and subtract
at draw time. It is arithmetic per visible row, and it is why the reduced-motion answer holds:
the counter is live because time passes, not because anything is being recomputed.

### Three things that would undo this

All three are tempting, and the first has already been made twice during the pane's own
construction:

1. **Do not put progress into the model's cache key.** That rebuilds the graph walk on every
   event, which is exactly the per-frame derivation this design exists to avoid.
2. **Do not recompute `cache_status` during a build.** The worker's report describes the
   *preview*; the build has its own cache and reports its own events.
3. **Do not let progress reorder rows.** Order comes from the model and stays put, whatever the
   overlay says.

Open, for the build-states step: whether progress events drive repaints directly, or the pane
repaints on a fixed cadence while a build runs. A percentage bar does not need two hundred frames
a second, and the build already repaints continuously for its own spinner.

## Where the data comes from

**Available today, no engine change.** `Graph::cache_status` returns current-versus-stale for
every node in a pull without evaluating anything. Bypass, pinning, wiring and build inclusion
are readable from the graph. Build availability is the same field-store lookup the viewport's
source toggle already performs. That covers the idle and after-build states, which is most of
the pane.

**Needs an engine seam.** Queued, active and done per node need an optional **progress sink** on
the evaluator, notified as each node starts and finishes. It is purely observational, so
determinism is untouched.

**Needs operators to opt in.** A percentage inside a node needs the operator to report
fractional progress, such as erosion's iteration `i / N`. Nodes that do not report show elapsed
time, never an invented percentage.

**Small plumbing.** The rail icon is one `DockPane` registration at order 20, after World (0) and
the Subgraph Library (10). The saved sort is a `ViewState` field with a serde default, the same
additive pattern `frames` and `camera` used, so no format bump. Reduced motion has no query
through egui, so it reads an application preference.

## The performance rule

Status is computed **when something changes, never per frame**. A pane listing every node, each
recomputing a key every frame, is O(nodes × depth) of avoidable work, and #254 was exactly that
class of bug: `Layer::content_hash` re-hashed every frame until the viewport crawled. The worker
reports cache state after each evaluation; the UI holds it until the next report.

## Outputs stays the source of truth

The World panel's Outputs section owns build inclusion. The pane's build chip **mirrors** it, on
the same two-controls-one-value pattern the viewport's output picker already uses. One value,
two places to see it, no third opinion.

## Phasing

Each step is a reviewable commit that leaves the tree runnable.

1. **The status model.** One place that derives a per-node status from the graph, the worker's
   cache report, the pin, and the field store, computed on change. Unit-tested without egui.
2. **The pane, idle.** Dock registration, rows in dependency order, the state vocabulary, the
   trailing-slot rules, expanded subgraph groups.
3. **Sort, filter, density.** The sort control and its persistence, the filter field and quick
   chips, the compact density, collapsed groups with their worst-state summary.
4. **The canvas dots** for every node, reading the same model, which decouples the status colour
   from thumbnail evaluation and closes #135.
5. **The evaluator progress sink** in `ymir-core`, with the determinism tests to prove it is
   observational.
6. **Operator sub-progress**, with the erosion models opting in first.
7. **The build states in the pane**: queued, active, cached, done, elapsed, per-node percentage,
   cancel, and the reduced-motion preference.

Steps 1 to 4 need no engine change and deliver most of the value. Steps 5 to 7 are the live
build picture.

## Relation to existing design

- **#44** (per-node stale status on the canvas) is step 4 of this note.
- **#45** (build monitor pane) is steps 5 to 7.
- **#135** (thumbnail toggle flashes status frames) has its cause removed by step 4.
- **[Subgraphs](subgraphs.md)**: the grouping follows the container structure the canvas already
  navigates.
- **#37** (settings infrastructure): the reduced-motion preference is one more setting on the
  existing `preferences.rs` seam, not a blocker on the umbrella.
