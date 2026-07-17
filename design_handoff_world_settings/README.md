# Handoff: Ymir World Settings — panel reorganization

## Overview
Reorganization of the **World Settings** tab in the left panel. Today it is one long,
undifferentiated scroll (seed → world size → sea level → a big block of water-rendering
params → build/preview resolution → outputs). This redesign pins the essentials at the top
and groups everything else into collapsible sections, using the shipped frost theme
(IBM Plex Sans/Mono, dark slate chrome, cyan accent) — consistent with the node-inspector
and workspace-pane redesigns.

**Locked direction: 1c — Clustered water (collapsible sections).**

- `mockup_1c_world_settings.png` — the spec image: the full panel, all sections expanded.
- `Ymir World Settings 1c.dc.html` — interactive final panel (open in a browser).
- `Ymir World Settings.dc.html` — the exploration doc: all three options (1a / 1b / 1c)
  side by side with the rationale, for reference on why 1c was chosen.
- `current_state_world_settings.png` — screenshot of today's flat panel.

Open either HTML in a browser to inspect exact spacing, colors, and states.

---

## What's wrong today
1. **No hierarchy** — 15+ controls in one flat column; the four settings a user touches most
   (extent, height, sea level, show water) sit level with rarely-touched foam-width tuning.
2. **The water block dominates** — nine water-rendering params (color, three Effects
   checkboxes, speed, depth falloff, waves, reflectivity, specular, foam, foam width) are the
   bulk of the panel but are only relevant when water is shown.
3. **"Effects" checkboxes are orphaned** — Depth / Surface / Foam are three master switches
   floating above a flat list; nothing visually ties each switch to the params it governs.
4. **Build & Outputs are buried** — they live at the very bottom of the water scroll.

---

## The reorganization

### Top level — a pinned **World** block (always visible)
The world's identity and the settings touched most often. Never collapses.
- **Seed** (with a re-roll affordance)
- **World extent** — field + derived `≈ 4.000 m/cell at build` readout
- **World height** — field + derived `0.50× footprint at full height` readout
- **Sea level** — slider + value, with the **Show water** toggle inline in its header, and a
  derived `≈ 348 m elevation` readout below

Set on a subtly accent-tinted background and divided from the sections below with a solid
`--line` rule, so it reads as the fixed header of the panel.

### Collapsible sections (below the pinned block)
Standard accordion — a chevron + uppercase label header per section, each opens/closes
independently. Order:

**WATER** (open by default)
- **Water color** (base swatch, always shown)
- Three **clustered sub-groups**, each a bordered card with the group name and an **enable
  toggle** in its header — these toggles *are* the old "Effects" checkboxes, now owning the
  params they gate:
  - **Surface** → Speed, Waves, Reflectivity, Specular
  - **Depth** → Falloff
  - **Foam** → Amount, Width
- When a cluster's toggle is **off**, its body greys out (`opacity:0.4`,
  `pointer-events:none`) — params stay visible for context but read as inactive. (Shown on
  Foam in the mockup.)
- The whole WATER section can also be dimmed/disabled when **Show water** is off upstream.

**BUILD & PREVIEW**
- Build resolution — numeric field + preset dropdown
- Preview resolution — numeric field

**OUTPUTS**
- Intro line "Endpoints a Build will write; tick to include."
- Export PNG (checkbox + path `out/stream_hf.png`)
- Shoreline as EXR (checkbox)
- Header shows an active-count badge (`2`).

---

## Why 1c (and what we rejected)
Three presentations were explored (see `Ymir World Settings.dc.html`):

- **1a — Segmented sub-tabs** (Water / Build / Output). Most compact, one group in focus,
  no scroll. **Rejected on extensibility:** a segmented control in a ~280px panel fits 3,
  tolerates 4, and breaks at 5+ (label truncation / overflow strip). New groups fight for
  horizontal space.
- **1b — Plain collapsible sections.** Extensible vertically, but leaves the nine-item water
  list flat inside its section — doesn't solve the orphaned-Effects problem.
- **1c — Collapsible sections + clustered water.** ← chosen. Accordions scale vertically
  without limit (adding a future "Climate" or "Erosion defaults" group is just one more
  header), and the Surface/Depth/Foam clustering tames the heavy water list while giving the
  Effects toggles a clear home.

**Extensibility note / trick in reserve:** if any single section later grows past ~10 params,
split *its interior* into tabs (accordion outside, tabs inside that one heavy section) —
vertical extensibility between groups, compact tabs within a group. Water is the likely first
candidate.

---

## Spec

**Frost tokens used**
```
--c0        oklch(0.255 0.019 250)   input / track fill
--c1        oklch(0.305 0.017 250)   panel background
--c2        oklch(0.345 0.017 248)   cluster header fill
--c3        oklch(0.385 0.018 248)   secondary control fill
--line      oklch(0.46 0.02 250)     borders, section dividers
--line-soft oklch(0.40 0.018 250)    subtle borders, input outlines
--ink-hi    oklch(0.94 0.012 235)    values, active labels, section titles
--ink-mid   oklch(0.77 0.016 240)    param labels
--ink-lo    oklch(0.62 0.016 240)    derived readouts, hints, badges
--accent    oklch(0.70 0.13 205)     sliders, active toggles, tinted world block
```

**Metrics**
- Panel width 280px (was ~208–300; 280 gives room for label + slider + value without wrap).
- Param row height ~28–30px; slider track 4px, knob 11–12px with a 2px ring in the row's bg
  color. Labels 11–11.5px, values 11.5–12px IBM Plex Mono, right-aligned in a fixed column.
- Section header height 36px, chevron + uppercase label (11px, 0.1em tracking).
- Cluster card: 1px `--line-soft` border, 6px radius, `--c2` header (30px), body padded
  8/10/10. Cluster toggle 30×16; top-level toggles 32×17.
- Pinned World block: accent-tinted gradient background, solid `--line` bottom divider.

**Interaction**
- Section headers toggle open/closed; state persists per session.
- Cluster enable toggles gate their body (grey + disable when off).
- Derived readouts (m/cell, footprint, elevation) recompute live from their inputs.
- Values are click-to-type / drag-to-scrub (consistent with the node inspector).

## Out of scope
- The other left-panel tabs (Subgraph Library, etc.) — this covers the World Settings tab only.
- The actual water-rendering math; only the control grouping changes.
- Final copy for the preset dropdown contents.
