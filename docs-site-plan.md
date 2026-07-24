# Ymir documentation site: work plan

Plan of record for building the published documentation site. Item ids are stable so they
can be referenced when handing work to Claude Code.

Owners: **O** = Oluf, **CC** = Claude Code, **C** = Claude (chat).

Settled context: the site is user-facing only. Design records are not published as site
pages. The published information architecture is Tutorial, How-to, Reference, Concepts,
which maps 1:1 onto Diátaxis (learning, task, information, understanding). Docs source
lives in the Ymir repo, in `docs/`, and everything in `docs/` publishes.

## Revision record

**Revision 3.** D6 and D7 settled. No open decisions remain; every item is assignable and
S3 is ungated.

**Revision 2.** Investigations I1 to I3 answered from code. D1, D2, D3, D4 and D5 settled.
S1 struck as already done (PR #207). D5 reshaped to a generic-with-override key scheme with
a one-sentence tooltip limit, plus a resolution-level guard added to G6. G3 narrowed to
emitted layers and a mask-aware flag. G6's design-link rule scoped to intra-site links.
Two new decisions opened, D6 and D7.

Revision 1 called `strings.rs` and the Fluent commitment in `ymir-gui/DESIGN.md` a
contradiction requiring resolution. That was wrong. DESIGN.md defers Fluent behind the
`tr(key)` seam rather than committing to it now, so the `tr` map is both the current
reality and the v1 plan, and there was nothing to reconcile.

---

## Phase 0. Decisions

**D1. Site generator. SETTLED: mkdocs-material.**
Nav explicit in `mkdocs.yml`. Add `mike` for versioning when the first release is cut, not
before.

**D2. Where long node prose lives. SETTLED: migrate to fragments.**
Stated as a single rule, which is cleaner than the original framing: **the string catalog
holds short strings only (display names and one-line tooltips); all long prose lives in
per-node docs fragments.** This governs D5 as well, so parameter descriptions cannot
reintroduce multi-paragraph text into the catalog by the back door.

**D3. Localization. SETTLED: keep the `tr` map, change nothing.**
Fluent stays a documented future behind the `tr(key)` seam. This has no bearing on the docs
generator, because G1 emits already-resolved strings from the running binary through `tr`,
making it agnostic to the backend.

**D4. IA and page kinds. SETTLED.**
Four sections, `status: stable | draft` frontmatter, and no documentation of unimplemented
features anywhere except a managed roadmap page. Page kinds line up 1:1 with the sections.
The generated node reference and the mechanical pages (world settings, formats, CLI,
keyboard) all sit under Reference.

**D5. Parameter strings. SETTLED: do it, with three constraints.**
No `param-*` keys exist today and the GUI has no parameter tooltips at all; labels are
derived by running the snake_case name through `prettify_param_name`. Adding the catalog
entries fixes a real application gap and supplies the reference table column that explains
what a parameter does.

1. **One sentence per description.** `param-<...>-desc` is a single sentence serving both
   the GUI tooltip and the reference table cell. Longer parameter discussion goes in the
   node's fragment under Behaviour. Follows directly from D2.
2. **Generic with override.** Resolve `param-<type_id>-<name>`, then `param-<name>`, then
   `prettify(name)`, matching the echo-fallback pattern `tr` already uses. Cuts roughly 200
   naive entries to 60 to 80 and removes the copy-paste maintenance burden. A node adds an
   override only where its parameter genuinely differs.
3. **Shared entries are allowlisted, and enums never share.** This is the guard the fallback
   chain needs. `param-<name>` is permitted only for parameters whose meaning is invariant
   across every node that uses them (`frequency`, `octaves`, `lacunarity`, `gain`, `seed`,
   `offset_x`, `offset_y` and similar). Contextual names must not share: Warp's `amount` in
   metres of domain warp and a blend `amount` as opacity are different parameters with the
   same name, and `mode` on Blend and on Curvature are unrelated enums. A shared entry that
   is subtly wrong is worse than no entry, because a silent fallback produces a plausible
   tooltip that nobody notices is false. When in doubt, override. See the resolution-level
   check in G6, which makes the fallback visible in CI rather than silent.

**D6. `docs/index.html`. SETTLED: mkdocs owns the whole Pages site.**
`docs/index.md` becomes the documentation home and absorbs the durable parts of the
conference field briefing (what Ymir is, a screenshot or two, links to install and the
tutorial). The briefing itself moves out of `docs/`, either as a repo file or folded into the
README. One build, one artifact, no path composition. If a distinct marketing landing is
wanted later, mkdocs-material's home page can be styled to do that job without a second
toolchain.

One caveat for S3: if the briefing's URL has already been printed on a slide, linked from a
talk description, or posted anywhere, do not break it. Either keep the briefing reachable at
its existing path or leave a redirect stub there. Check this before deleting anything.

**D7. Design records page. SETTLED: do not build it.**
The site carries a single line in the footer or on the home page pointing at the repository.
No enumerated design-records page. The design documents have already drifted behind the code
(the capability list names nodes the design docs never mention), and an index on the
documentation site would lend stale records the site's authority. `ARCHITECTURE.md` and
`CONTRIBUTING.md` in the repo serve the contributor who arrives from the repo, which is where
that audience arrives from anyway.

G6's link rule stands as written: intra-site links into `design/` always fail the lint, and
absolute GitHub URLs remain the only sanctioned way to reference a design record. No page now
needs that exception, but the rule stays correct.

## Investigations: answered

**I1. Node and parameter strings.** ANSWERED. `strings.rs` is a single hand-rolled
`tr(key) -> &str` match, keys by convention (`node-<type_id>`, `-desc`, `category-<id>`).
Parameter labels are derived via `prettify_param_name`, not catalogued. No `param-*` keys and
no parameter tooltips exist; the only `on_hover` in the inspector is on the reset button.
Adding `param-*` is greenfield: catalog entries, `param_label` looking them up with a
`prettify` fallback, and an `on_hover_text` for the description. Feeds D5.

**I2. Layer contract in `NodeSpec`.** ANSWERED: not present. `NodeSpec` is
`{ type_id, category, inputs, outputs, params }`. Soft contracts live only in each node's
`eval` code and in prose. G3 is a real core extension, confirmed, and batches with G2.

**I3. Central keymap.** ANSWERED: none. Shortcuts are hand-coded in three places
(`OrbitCamera::handle_input`, `main.rs` shortcut and key handling, canvas click handling).
W4 is hand-maintained for now. A keymap registry is a separate feature and is not worth
blocking documentation on. See the mitigation in W4.

---

## Phase 1. Scaffolding

Goal: an almost empty site that publishes. Do this before any content.

**S1. Move design records out of `docs/`.** DONE, PR #207.

**S2. Add a one-line header to every file in `design/`.** *(CC)*
States that the file is a design record, not user documentation, and points at the site. The
repo is public, so someone will arrive there from a search engine.

**S3. Create the `docs/` tree with stub pages.** *(CC)*
`index.md`, `install.md`, `tutorial/`, `how-to/`, `reference/`, `concepts/`, `roadmap.md`,
`assets/`. Every stub carries `status: draft`. Relocate `index.html` per D6 in this step,
honouring the existing-URL caveat.

**S4. Add `mkdocs.yml` and the nav.** *(CC)*
Explicit nav, status badge rendered from frontmatter, and the D7 footer link to the
repository.

**S5. Add the deploy workflow.** *(CC)*
GitHub Actions Pages artifact flow, no `gh-pages` branch. Build then deploy.

**S6. Confirm the site publishes.** *(O, 5 min)*
The Phase 1 acceptance test.

---

## Phase 2. The generated reference

Sequenced as normal Ymir steps: small, reviewable, tested, one commit each.

**G1. `ymir-cli docs --format json`.** *(CC)*
Iterates `inventory` and serializes real `NodeSpec` values: type_id, category, resolved
display name and description, ports, and per-parameter kind, range, default and unit. Emit
from the running binary, never by parsing source. The documentation dump demonstrates why:
44 descriptions truncated to `=> {`, and `crate::selector::output_param()` left unresolved.

**G2 + G3 + D5: one core-touching change.** *(CC)*
Core is disturbed once, not three times.

- **G2.** `param-*` catalog entries and wiring, per D5's three constraints. Emit the
  resolution level (override, shared, or prettify) alongside each parameter in G1's JSON so
  G6 can check it.
- **G3.** Minimal `NodeSpec` extension: **declared emitted layers** (the byproducts: wear,
  flow, deposition, shore) and a **mask-aware flag**. Both mechanical and tabular, and both
  feed the byproduct line and the header flag G4 wants. Absent-layer behaviour stays prose
  in the fragment; it is explanatory rather than tabular, and schematising it would be
  over-engineering.

**G4. The page generator.** *(CC)*
JSON in, markdown pages plus a category index out. Fixed section order: display name,
`type_id`, category, status, one-line description, Purpose, Inputs, Outputs, Parameters,
Layer contract, Behaviour, Recipes, See also. Generated sections are mechanical; Purpose,
Behaviour, Recipes and See also merge from the prose fragment. Header flags for mask-aware
and resolution-dependent. Parameter table columns: name, type, range, default, unit,
description, modulatable, the last populated as "no" everywhere so the table shape does not
change when field-driven parameters land.

**G5. Prose fragments live in `ymir-nodes`.** *(CC)*
`ymir-nodes/docs/<type_id>.md`, flat filenames keyed by `type_id`, so adding a node still
touches only its own directory. Frontmatter carries `status`.

**G6. The docs lint in `xtask`.** *(CC)*
Fails on: a registry node with no fragment; a fragment with no registry node; an invalid
heading set; a `stable` page with an empty Purpose or containing TODO; a figure reference
with no file; a link to a nonexistent page; **any parameter on a `stable` node page whose
label or description resolved at the `prettify` level** (this is what keeps D5's silent
fallback honest); the strings "open question", "TBD" or "proposed" in any published page;
and **any relative or intra-site link into `design/`**, which is always a bug because
`design/` is not part of the built site. Absolute GitHub URLs to `design/` are permitted and
are the only sanctioned way to reference a design record from the site.

**G7. Commit generated pages, mark them generated.** *(CC)*
`linguist-generated=true` in `.gitattributes`, a do-not-edit README in each generated
directory, and `git diff --exit-code` after regeneration in CI so drift is a build failure.
A parameter default change then shows its user-visible consequence in the diff.

**G8. Run the generator and lint in the clippy CI job.** *(CC)*

**G9. Add a docs clause to `CLAUDE.md`.** *(CC, O ratifies)*
A step is not done until user-visible changes have their page updated. Name the shortcuts
page explicitly, per W4.

---

## Phase 3. Written content

**W1. `DOCS.md`, the docs style guide.** *(C drafts, O ratifies)*
Voice seeded from `CLAUDE.md`: honest, non-performative, no em dashes, no marketing
fragments, no emoji. Node-page prohibitions: do not restate the parameter table in prose, do
not walk parameters one by one, state negative space, no "simply" or "powerful", every
behavioural claim true of the current build. Hard rule: no published page may reference an
open question, a rejected approach, a revision history, or a future intention. Harvest rule:
`design/` is reading material for understanding, never source to copy, and its voice must not
transplant into user documentation. Also records the D2 rule and the D5 allowlist, since both
are authoring guidance as much as code constraints.

**W2. Concepts pages.** *(C drafts, O verifies claims)*
From dump sections 2, 4, 7, 9 and 16, rewritten for someone trying to make terrain.
Candidates: everything is a Field; the `[0,1]` height convention and what it means for a
graph; masks and selections; preview is approximation and build is truth; erosion carves into
existing form; macro form rather than surface texture, and why that is scale-relative;
landforms are composed rather than bespoke nodes; design principles.

**W3. Mechanical reference pages.** *(CC)*
World settings, export formats, project format, CLI. Straight from code.

**W4. Keyboard and interaction reference.** *(CC drafts from dump section 14, O verifies)*
Hand-maintained, per I3. **Every modifier is Ctrl, never Cmd**: the code uses
`Modifiers::COMMAND`, which egui maps to Ctrl on Linux, and Ymir is Linux-only. Drop the
`Cmd/` prefixes the dump carried. Because no lint can guard this page, G9's `CLAUDE.md`
clause names it specifically: a change to input handling in `OrbitCamera`, `main.rs` or
canvas click handling is not done until this page is updated. A keymap registry that would
let the page be generated is a reasonable future issue, filed separately.

**W5. `install.md`.** *(CC drafts, O verifies on a clean machine)*
With no binary releases, building from source is user documentation. Written for someone who
does not know Rust. Verify from a clean checkout on midgard or fensalir.

**W6. Node Purpose lines, all 46.** *(CC extracts, C edits, O reviews by category)*
CC recovers the full existing descriptions from source, C edits them into Purpose prose
against W1, O reviews in category-sized batches rather than all at once. This is also where
D2's migration physically happens.

**W7. The tutorial.** *(O captures, C writes)*
The only item that genuinely requires using the application. Cheapest shape: O builds one
terrain start to finish and dumps rough notes or a screen recording, C turns it into the
guaranteed-to-work path. Do this before the how-tos; walking a real first-run path is the
fastest way to find UX holes.

**W8. How-tos.** *(deferred)*
Write them as presets land, since presets are already the acceptance test.

---

## Phase 4. Figures

**F1. The canonical reference terrain.** *(O)*
One committed `.ymir` project at a fixed resolution, doubling as a test fixture. Every node
figure in the reference is a before and after pair rendered from it with the same ramp,
camera and window size, so figures are comparable across pages.

**F2. The capture recipe and script.** *(CC)*
Fixed window size, theme, ramp, naming and output path, so regenerating all figures after a
UI change is a scripted job.

**F3. Tutorial figures only, for now.** *(O)*
No GUI chrome screenshots in node pages. Those belong to the tutorial and the interface
reference, the only places worth paying the churn cost.

---

## Minimum publishable site

The smallest thing worth linking from the README: `index.md`, `install.md`, the tutorial, the
generated node reference, and the keyboard reference. Concepts pages can land one at a time
afterwards without the site looking unfinished.

## What only Oluf can do

No decisions remain open. What is left: the existing-URL check in D6, S6, W7's capture pass,
W5's clean-machine verification, F1, and claim verification on W2, W4 and W6. Everything else
is assignable.
