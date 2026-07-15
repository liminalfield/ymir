# HANDOFF — resume here (water plane / coastal work)

**Written:** 2026-07-15, before a laptop switch. **Delete this file when you resume**
(it lives at the repo root only so it's impossible to miss; it must not merge to `main`).

**Branch:** `feature/coastal-erosion`. Read `CLAUDE.md` first — it is the working brief
(small steps, checkpoint per step, one commit per step, maintainer runs the commits,
no shortcuts). This file is just "where we are and what's next."

---

## First thing on the new laptop

```bash
git fetch && git switch feature/coastal-erosion && git pull
cargo build && cargo test        # sanity
```

If the branch isn't on the remote yet, it was never pushed from the other machine —
push it from there first with `git push -u origin feature/coastal-erosion`.

---

## What just shipped (this session, all committed)

Three commits landed, in order:

1. `a95f1b1` feat(core): thread `sea_level` through `EvalContext` and the cache key.
   A world global (normalized height, default `0.0`) alongside `world_height`; keyed
   into the memo cache; threaded into subgraph evaluation. **No node reads it yet** —
   pure plumbing for coastal/stream base level and the viewport water.
2. `5d1cc3c` feat(gui): water plane at sea level, anchored to a stable world datum.
   - A translucent quad in the 3D viewport (`crates/ymir-gui/src/viewport.rs`), generated
     in the vertex shader, drawn **after** the terrain, depth-tested but **not** depth-written,
     so the terrain clips it cleanly at the waterline. `sea_level` maps through the same
     height transform the terrain uses (Fixed passes through; Auto rides the range remap).
   - **Baseline fix (important):** the terrain block's base is now anchored to a fixed world
     datum (height 0), *not* the previewed field's own minimum. Before, switching to a node
     with a higher minimum (e.g. after a Levels node) floated the whole terrain up in world
     space and left the water stranded below it. Now the terrain keeps a stable vertical
     position across node selection and the water (height ≥ 0) can never sit below the block.
     Elevated terrain shows a taller plinth — that's the honest, stable result.
   - Controls: **Sea level** slider + **Show water** toggle in the World settings panel.
     Moving the slider only updates a uniform (no re-mesh).
3. `d8fddf2` feat(gui): persist sea level and water toggle with the project.
   `sea_level` + `show_water` added to `project_file::WorldSettings` (serde-defaulted, **no
   format bump**; old files load with water off). They flow through `world_settings()` /
   `snapshot()`, so Save writes them, Open restores them, and changing either now marks the
   project modified (so Close prompts to save). `install_fresh` was refactored to take a
   `WorldSettings` bundle; as a side effect New/Close/open-default now also reset/restore
   `build_res` (previously left stale).

Working tree is clean. Gate is green (`cargo fmt`, `clippy -D warnings`, `cargo test`,
`scripts/check-shortcuts.sh`).

---

## Verify visually when you sit down (GUI can't run headless)

1. Launch `cargo run -p ymir-gui`. Flip between the raw **fBm** node and the post-**Levels**
   erosion node: the terrain should **no longer jump vertically**, and the water should stay
   at/above the block base in both.
2. Move the **Sea level** slider / toggle **Show water**, then try to **close** → you should
   now be **prompted to save**. Save, reopen, confirm the level + toggle come back.

If any of that is off, that's the first thing to fix.

---

## Open decision waiting on you

Toggling **Show water** currently marks the project dirty (it's saved project state now).
If you'd rather the visibility toggle *not* count as an unsaved change while the sea-level
value still does, that's a small split — decide and say so.

---

## Where this is heading (coastal roadmap)

Issues on the "Coastal Erosion (roadmap)" column of the Ymir project board:

- **#96** — sea-level World-panel slider + 2D/relief water overlay. (3D slider + toggle are
  done; the 2D/relief map overlay is not.)
- **#136** — sea_level plumbing. **Step 1 done** (the core commit above). **Step 2 deferred:**
  stream-power grading rivers to sea level as base level — we concluded it's low-value for now
  (a river mouth's base level is underwater and visually irrelevant; deltas are the real
  feature, and that's a separate piece).
- **#137** — eikonal distance solver (fast sweeping, |∇φ|=1). Shared by 4 consumers: coastal
  distance, a Distance selector, foam, and the flow-map fix. Build once.
- **#138** — offscreen viewport render fork (own colour+depth targets, `register_native_texture`).
  **Prerequisite for "fancy" water** (Tier 2+ refraction, MSAA). The basic plane we shipped did
  NOT need it. See `docs/design/viewport-water.md`.
- **#139** — Coastal v0 node (lean, artist-first — a cheap coastal bevel, ~5 dials + a preset,
  NOT the physics brief). See `docs/design/coastal-erosion.md` (kept as "where it could go").
- **#140 / #141** — water shader tiers 0 (Beer-Lambert) / 1 (normals + Fresnel + specular).
  Stop at Tier 0+1 for a terrain tool.

Suggested next concrete step (small, visible): **#96's 2D/relief water overlay**, or start the
**#139 Coastal v0** node. Confirm with the maintainer before diving in (checkpoint rule).

---

## Design decisions to respect (don't relitigate)

- **Sea level is a world setting, never a node.** It lives in `EvalContext` (like `world_height`),
  not as a Field layer or a node output. No "Sea" node, no sea/shore/water layers in v0.
- **The layer test:** a layer earns its place only if it carries info not recoverable from the
  heightfield + globals. `wear`/`deposition` (erosion history) are real layers; shore/water/
  exposure are derivable *selections*, not layers. You can already select beaches today by
  combining selectors with an erosion node's `deposition` output (byproducts are separate output
  ports, each a Field carrying data on the `height` layer).
- **Water rendering is presentation-only** — the determinism contract never reaches a viewport
  shader, so be as aesthetic as you like. "Fancy water" is wanted, but take the offscreen fork
  (#138) before going past a basic plane.
- **Viewport water design** is fully written up in `docs/design/viewport-water.md` (two-source /
  one-shader, tiers 0–3, eikonal foam). Coastal model in `docs/design/coastal-erosion.md` and
  `docs/design/node-taxonomy.md`.
- User is **red/green colourblind** — design all UI colour for it (no red-vs-green alone), but
  don't let it constrain application functionality.

---

## Gate before every commit

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test && sh scripts/check-shortcuts.sh --worktree
```

Maintainer runs the commits; Claude stages + drafts the message. One focused commit per step,
pause at each checkpoint for review, get visible output early. Fix causes, not symptoms.
