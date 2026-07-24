# DOCS.md

Guidance for writing Ymir's published documentation. Read it fully before writing or
editing anything under `docs/` or in `ymir-nodes/docs/`.

`CLAUDE.md` is the contract for the code. This is the contract for the documentation. It
inherits `CLAUDE.md`'s working style wholesale: small single-purpose changes, a checkpoint
for review, no shortcuts, one focused commit per step.

## What the documentation is for

Someone who wants to make terrain with Ymir. That reader is the only audience. They are
capable and unhurried, they may not know Rust, and they have not read the source.

The site is user-facing. It is a manual, and it carries no record of how Ymir was built.
Development history, architectural reasoning, and design deliberation live in `design/`,
`ARCHITECTURE.md` and `CONTRIBUTING.md`, and none of that is published.

## The four page kinds

Every page is exactly one of these. A page that is two of them should be two pages.

**Tutorial** (`docs/tutorial/`). One learning path, followed start to finish, which works.
Exactly one exists. Every step is verified against the current build. No alternatives, no
asides, no "you could also".

**How-to** (`docs/how-to/`). One task, achieved. Goal in a sentence, prerequisites,
numbered steps, expected result, variations. Roughly one screen. A how-to that needs more
than that is usually a concepts page in disguise.

**Reference** (`docs/reference/`). Complete and dry. Node pages, shortcuts, world settings,
formats, CLI. It answers "what does this do", never "how do I achieve X". No teaching, no
tutorials, no opinions.

**Concepts** (`docs/concepts/`). Understanding. Why a preview does not match a build, what
the `[0, 1]` height convention means for a graph, why erosion carves into existing form,
why landforms are composed from primitives. Written for someone trying to make terrain, so
every concepts page must be usable rather than merely interesting. If a page cannot say
what the reader will do differently having read it, it does not belong.

## Frontmatter

```yaml
---
title: Slope
status: draft   # draft | stable
---
```

`draft` renders a badge. `stable` asserts that every claim on the page is true of the
current build and that the page is complete for what it covers. A page becomes `stable`
only after someone has verified it against a running Ymir.

## Voice

Honest and non-performative. Plain language. Treat the reader as capable.

Second person, present tense. Imperative for steps ("Add a Blur node", never "You should
now add a Blur node" and never "We add a Blur node").

Length is not a virtue. If a paragraph can do its job in two sentences, two sentences.
Every paragraph should have a job you could state in one sentence.

Closing paragraphs are usually unnecessary. End on the last substantive point.

## Forbidden

**Em dashes.** Use commas, parentheses, semicolons, or a new sentence.

**Defining by negation.** "Ymir is not a simulator, it is an art tool." State the positive
claim on its own. This includes the variants "rather than", "instead of X, Y", and "while X
is not Y".

**Minimising adverbs.** simply, just, easily, obviously, of course, merely. They tell a
stuck reader that their problem is their own fault.

**Marketing vocabulary.** powerful, seamless, intuitive, robust, cutting-edge, world-class,
rich, flexible, leverage as a verb. Also no exclamation marks and no emoji.

**Hedges.** "may produce", "can sometimes", "should generally". Either the behaviour is
known, in which case state it, or it is unknown, in which case leave it out and flag it for
verification. Hedged prose is how documentation stops being trustworthy.

**Meta-commentary.** "This page will explain", "as mentioned above", "in this section".
Headings already tell the reader where they are.

**Mechanical parallelism.** Three sentences of identical shape in a row.

**Addressing the reader's state of mind.** "You may be wondering", "don't worry".

## Claims discipline

Every behavioural claim must be true of the current build. Not the design document, not the
intended behaviour, not what the code appears to do on reading.

If a claim cannot be verified, leave it out. Do not soften it into a hedge. When a claim
matters and verification requires running the application, write the page without it and
list the claim in the pull request description for verification.

Never document a feature that does not exist. Roadmap material lives on `docs/roadmap.md`
and nowhere else. A page that describes a planned capability, even accurately, will be read
as describing the current build.

## Terminology

One word per concept, used everywhere. Where the table below disagrees with a label on
screen, the on-screen label wins and the table is wrong; correct it rather than
contradicting the application.

| Use | Not |
|---|---|
| node | operator, module, block |
| graph | network, tree, pipeline |
| canvas | editor, workspace, node view |
| input, output | port, pin, socket, slot |
| connect, connection | wire, link, edge |
| parameter | setting, option, property, slider, knob |
| field | data, signal |
| layer | channel, band |
| height (the layer) | heightmap, elevation channel |
| selection | mask, when produced by a Selector |
| mask (the input) | matte, stencil |
| preview | viewport render, low-res |
| build | full-res export, final render |
| world extent, world height | terrain size, scale |

Selection and mask are worth care, because they are the same values doing two jobs. A
Selector produces a **selection**. A selection fed into another node's **mask** input
restricts where that node acts. Use the words that way and the distinction stays legible.

Node display names are capitalised as they appear in the palette (Stream Erosion, Histogram
Scan). Parameter names in prose use their display label (Talus Angle), never the snake_case
identifier, which belongs only in the generated table and in expression syntax.

## Units and numbers

State units on every quantity. Metres for world dimensions, degrees for angles, cells for
resolution-relative distances.

Height is a unitless value in the working range `[0, 1]`, mapped to metres by World Height.
Use one phrasing for this everywhere and link the concepts page on the first mention in any
page.

Describe controls by the observable thing they change, not by the quantity they compute.
This is a design principle of the application and it governs the documentation too. A
parameter named for a landform feature is explained in terms of that feature, with the
underlying quantity mentioned only where the reader needs it to predict behaviour.

Never restate a default or a range in prose. Both are in the generated parameter table, and
a prose copy will be wrong within a release.

## Single source of truth

Each fact has exactly one home. Cross-link, never copy.

| Fact | Home |
|---|---|
| Node display name, one-line description | the string catalog |
| Parameter label, one-line description | the string catalog |
| Parameter kind, range, default, unit | generated from `NodeSpec` |
| Emitted layers, mask awareness | generated from `NodeSpec` |
| What a node is for, how it behaves | `ymir-nodes/docs/<type_id>.md` |
| Keyboard and mouse bindings | `docs/reference/shortcuts.md` |
| Planned work | `docs/roadmap.md` |

## Node pages

The generated sections are not editable. Prose goes in `ymir-nodes/docs/<type_id>.md` under
a fixed heading set.

**Purpose** (required for `stable`). One or two sentences: what the node is for and when a
user would reach for it. Not what it computes. Where a reader is likely to expect something
the node does not do, say so, because negative space prevents more confusion than
description does.

> Good. Adds meandering irregularity to features that look too regular or machine-made.
> Warp displaces the terrain sideways rather than up, so it changes the shape of features
> without changing their height.
>
> Bad. Warp is a powerful fBm domain warp node that leverages noise to seamlessly perturb
> the input field's sample coordinates, producing organic results.

The bad version names the algorithm, sells the node, and leaves the reader no clearer about
when to use it.

**Behaviour** (optional). Failure modes and surprises. What happens at extreme parameter
values, how the node behaves when an optional mask is absent, whether it is
resolution-dependent and what that means for preview against build, and how it interacts
with nodes commonly placed near it. Do not walk the parameters one at a time; that is what
the table is for.

**Recipes** (optional). Two or three short pointers to how the node is combined in practice.
A recipe that needs its own walkthrough is a how-to, and should be one.

**See also** (optional). Three links at most, curated. Not a category dump.

Omit optional sections rather than filling them. Forty-six nodes each with a dutiful
Behaviour section produces thirty pages of padding, and Null and Invert have nothing to say
beyond their one-liner.

## The string catalog

The catalog holds short strings only: display names and one-sentence descriptions. All
longer prose lives in fragments. A multi-paragraph description in the catalog is a bug, and
the reason is that translation catalogs are miserable places to edit prose and this one will
never be translated.

A shared `param-<name>` entry is permitted only where the parameter means the same thing in
every node that uses it. Where the meaning is contextual, write a
`param-<type_id>-<name>` override. Enum parameters always get an override. A shared entry
that is subtly wrong is worse than none, because the fallback is silent and the tooltip
looks plausible.

The documentation is English only. The catalog is localizable; the pages are not.

## Figures

Every node figure is a before and after pair rendered from the canonical reference terrain,
at the same resolution, camera, and colour ramp as every other figure in the reference, so
that figures are comparable across pages.

No application chrome in node pages. Interface screenshots belong to the tutorial and the
interface reference, which are the only places worth paying the churn cost while the GUI is
moving.

Alt text on every image, describing what the figure shows rather than naming the file.

## Links

Link the first mention of a node in a page to its reference page, once, not on every
mention.

Never link into `design/` with a relative path; it is not part of the built site. Where a
design record genuinely needs referencing, use an absolute GitHub URL.

## Harvesting from `design/`

The design records are reading material. Read them to understand a subsystem, then write
fresh prose for someone who wants to make terrain. Never copy their text, reproduce their
structure, or carry over their voice.

Those documents deliberately state disagreements, record what was excluded and why, and
preserve superseded proposals. That voice is correct for a design record and wrong for a
manual: transplanted into user documentation it reads as an unfinished product. No published
page may reference an open question, a rejected approach, a revision history, or a future
intention.

The design records also lag the code in places. Where a record and the code disagree, the
code is right.

## What the lint cannot check

`cargo xtask docs-lint` catches missing fragments, orphaned fragments, invalid heading sets,
empty Purpose on a `stable` page, TODO markers, broken links and figures, parameters
resolving to a prettified fallback, and intra-site links into `design/`.

It cannot tell whether a claim is true, whether the terminology is consistent, whether a
figure matches the terrain it claims to show, or whether a Purpose line explains anything.
Those are review, and they are the reason node prose is reviewed in category-sized batches
rather than all at once.
