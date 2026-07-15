# HANDOFF — resume here (coastal / distance work)

**Written:** 2026-07-15, before a laptop switch. **Delete this file when you resume**
(it lives at the repo root only so it's impossible to miss; it must not merge to `main`).

**Branch:** `feature/coastal-erosion`. Read `CLAUDE.md` first (the working brief: small
steps, checkpoint per step, one commit per step, maintainer runs the commits, no
shortcuts). This file is just "where we are and what's next."

---

## First thing on the new laptop

```bash
git fetch && git switch feature/coastal-erosion && git pull
git switch main && git pull        # main also moved this session (dock fix)
git switch feature/coastal-erosion
cargo build && cargo test          # sanity
```

Everything below is already committed **and pushed** on both `feature/coastal-erosion`
and (for the dock fix) `main`. Nothing local is stranded on the Starbucks laptop.

---

## What shipped this session (all committed + pushed)

On `main` (and merged into coastal):

- `fix/dock-default-world` (merge `64295a0`) — the left dock now opens to **World
  settings** by default and its icon leads the rail. Done via an explicit `order` field
  on `DockPane` instead of id-sort. Fast-forwarded onto `main`; merged into coastal.

On `feature/coastal-erosion`, in order:

1. `22869d4` feat(gui): **#96 water overlay on the 2D preview and map.** A blue,
   depth-shaded tint below the waterline in both the preview pane and the 2D map mode,
   in Height and Relief. Driven by the existing **Sea level** slider + **Show water**
   toggle (one control set now drives the 3D plane and both 2D surfaces). Appearance is
   isolated in a `WaterStyle` value in `shade.rs` so a future "Water" section in the
   World panel can drive it. Presentation only, no eval impact, no graph re-eval on
   slider move.
2. `f035dc3` feat(gui): **#96 steerable relief sun for the 2D map.** Extracted the
   preview pane's sun dial into a shared `sun` module (`sun.rs`); the 2D map now has its
   own relief light, shown as a compact dial **top-right, only in relief mode**. Angle
   readout is fixed-width so the popup does not jitter while dragging. Light is ephemeral
   per-surface view state (not persisted), independent of the preview and 3D lights.
3. `c2f7bd9` feat(nodes): **#137 eikonal distance selector.** A fast-sweeping eikonal
   solver (`|∇φ| = 1`, Zhao 2005) with sub-cell contour seeding, plus a **`Distance`**
   selector node: a `[0, 1]` band around a height contour by true isotropic distance.
   Verified in the viewport — even band width all around a coastline, **no eight-lobed
   star** (that visual check is the acceptance test). Lives in `crates/ymir-nodes/src/
   distance.rs` as shared substrate (`signed_distance_to_contour` / `eikonal_solve`).

Working tree is clean. Gate is green (`cargo fmt`, `clippy -D warnings`, `cargo test`,
`scripts/check-shortcuts.sh`).

---

## Where this is heading — next concrete step: #139 Coastal v0

**#139 is now unblocked by #137** and should be the *real distance-based bevel*, not the
elevation-band compromise we nearly built. The key realization this session: a good
coastal bevel is parameterised by **distance from the shoreline**, which is exactly what
the `Distance` node's substrate (`signed_distance_to_contour`) now provides. Coastal v0
will call it with `sea_level` as the level, then apply a geometric beach-and-bluff
profile as a function of that signed distance.

The full brief is `docs/design/coastal-erosion.md` (the ambitious exposure-driven model,
phases 1-2). **v0 is deliberately lean** (handoff intent): a cheap bevel, ~5 dials + a
preset, NOT the physics brief (no exposure sweeps, no longshore transport, no connected
components). Now that distance-from-shore exists, v0 can honestly offer metric beach
width and a non-terraced profile.

Suggested v0 shape (confirm at the checkpoint before coding):

- New `Coastal` modifier in `ymir-nodes`, category `geology`. Reads `height` + `mask`
  (soft) + `ctx.sea_level()` (first real consumer of the threaded global).
- Reshapes land near the shore by signed distance; emits `water` depth + a new `shore`
  mask layer. Multi-output like `thermal` (heightfield / water / shore).
- ~5 dials + a `coast_type` preset, all driven off the distance field.
- Per-cell over the distance field, so byte-deterministic. Golden cone-island test
  (the isotropy canary passes for free).

---

## Open threads / decisions

- **Eikonal consolidation (deferred, your call).** `hydrology.rs` already has its own
  fast-sweeping eikonal solver (for the drainage flat-resolution fix). `#137`'s solver
  is a **deliberately separate** general-grid variant with fractional sub-cell seeding;
  the two were kept apart on your instruction to not touch/risk the working erosion path.
  You said you'd consider sharing "once everything's verified working." It now is. The
  shared piece is only the ~6-line Godunov update; see the note at the top of
  `distance.rs`. Revisit only if you want to, it is not blocking anything.
- **#136 step 2** (stream-power grading rivers to sea level) stays deferred — low value
  (a river mouth's base level is underwater). Deltas are the real feature, separate work.
- **Show water toggle** marking the project dirty: **settled — keep as is** (both the
  sea-level value and the toggle dirty the project; turning water on and finding it off
  on reload would read as a bug). No code change.

---

## Coastal roadmap (project board)

- **#96** — sea-level slider + 2D/relief water overlay. **Done** (3D plane, 2D overlay,
  and the relief sun all shipped).
- **#136** — sea_level plumbing. Step 1 done (`a95f1b1`). Step 2 deferred (see above).
- **#137** — eikonal distance solver. **Done** (`c2f7bd9`).
- **#138** — offscreen viewport render fork. Prerequisite for "fancy" water (Tier 2+).
  Not needed for anything shipped. See `docs/design/viewport-water.md`.
- **#139** — Coastal v0 node. **Next.** Unblocked by #137. See above.
- **#140 / #141** — water shader tiers 0 / 1. Stop at Tier 0+1 for a terrain tool.

---

## Design decisions to respect (don't relitigate)

- **Sea level is a world setting, never a node.** It lives in `EvalContext`, not a Field
  layer. Coastal reads `ctx.sea_level()`; no per-node sea-level param in v0.
- **The layer test:** a layer earns its place only if it carries info not recoverable
  from the heightfield + globals. Distance-from-a-contour is *derivable*, so the
  `Distance` node computes it on demand rather than it being a stored layer.
- **True eikonal, never chamfer/BFS.** The whole point of #137: a chamfer metric stars
  at ±22.5°. Hold this bar for any distance work.
- **Water rendering is presentation-only** — no determinism contract in a viewport
  shader, so be as aesthetic as you like. `WaterStyle` in `shade.rs` is the tuning seam.
- User is **red/green colourblind** — design UI colour for it (no red-vs-green alone).

---

## Gate before every commit

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test && sh scripts/check-shortcuts.sh --worktree
```

Maintainer runs the commits; Claude stages + drafts the message. One focused commit per
step, pause at each checkpoint for review, get visible output early. Fix causes, not
symptoms.
