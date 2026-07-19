# Handoff: Ymir viewport display controls — redesign

## Overview
Redesign of the **viewport display controls** — the settings that govern how the previewed
node's terrain field is rendered in the viewport (the pane below the node-graph canvas).
Today these live in a floating top-left HUD plus two top-right overlays, with several settings
duplicated between the viewport and the node inspector. This redesign consolidates them into a
single **Display flyout** launched from a compact on-render cluster, in the shipped frost theme
(IBM Plex Sans/Mono, translucent dark chrome, single cyan accent) — consistent with the
node-inspector and World Settings redesigns.

**Locked direction: 3a — Display flyout (no rail).**

- `mockup_3a_display_flyout.png` — the spec image: viewport in 3D mode, Display flyout open.
- `Ymir Viewport Controls.dc.html` — interactive doc. Turn 3 (top) is the locked design (3a);
  turns 2a/2b below are the earlier explorations, kept dimmed for rationale.
- `current_state_viewport.png` — screenshot of today's HUD.

Open the HTML in a browser to inspect exact spacing, colors, and states.

---

## Decisions captured (from review)

1. **2D and 3D keep separate state.** Not a unified control; each projection owns its own
   settings (Fixed/Auto, sun, etc.). The flyout shows the active projection's set and swaps
   on toggle.
2. **Light is per-projection.** The 3D sun and the 2D hillshade sun are independent. The
   LIGHT section header is tagged with which sun it edits (`3D sun` / `2D sun`).
3. **Output picker lives in both places, kept in sync.** It appears at the top of the flyout
   *and* stays on the inspector thumbnail; changing either updates both (a sync affordance is
   shown under the picker).
4. **On-render essentials:** projection toggle (3D/2D) + fidelity readout ("Showing:
   preview/build") + the Display button — all in one top-left cluster. Everything else
   (Output, Height scale, Exaggeration, Light) is inside the flyout.
5. **Layout switcher removed from the viewport.** Split / maximize-graph / maximize-preview is
   *workspace* chrome, independent of 2D/3D — it moves to the workspace pane divider (the
   maximize/minimize control from the workspace-panes redesign). It is no longer part of the
   viewport control cluster.
6. **No left-edge rail (for now).** The standing design intent was a left-edge vertical
   toolbar, but with only three houseable items (Output, Scale, Light) a persistent rail isn't
   justified — a rail earns its space at ~5+ frequently-toggled tools or when tools must be
   one-click-always; these are set-and-forget. A single Display button carries it. **If the
   toolset grows** (extra lights, masks, measurement, layers), promote this same flyout to a
   left rail — the flyout content is unchanged, only its trigger/anchor moves.

---

## The design (3a)

### On-render cluster — top-left, always visible
A single row of frosted-dark overlay controls:
- **Projection toggle** — `[3D | 2D]` segmented pills (active = cyan fill).
- **Fidelity readout** — muted mono text "Showing: preview" / "Showing: build". Not
  interactive; states whether the viewport shows full Build-resolution quality or the live
  coarse preview.
- **Display button** — cyan-outlined; opens/closes the flyout. (Only control that opens a
  panel.)

Nothing sits top-right anymore. The HUD's old always-visible sliders are gone into the flyout,
so a short viewport no longer needs to hide the HUD — the cluster is one compact row.

### Display flyout — opens under the Display button
Frost panel (solid `--c1` body), World-Settings-style rows. Contents, top to bottom:

- **Output** (picker) — `Height ▾` dropdown; for multi-output nodes (e.g. erosion:
  height / flow / wear / deposition) chooses which tapped field the viewport shows. Sync note
  below it ("synced with inspector thumbnail"). Set on a subtly accent-tinted header block
  because it's the "what," not the "how."
- **Height scale** — `[Fixed | Auto]` pills. Fixed = true amplitude (clips out of range);
  Auto = normalize to fill the relief.
- **Exaggeration** — slider, 0.25×–8×, logarithmic, shown as `1.00x`. 1× = real-world
  proportion (World height ÷ World extent).
- **LIGHT** — collapsible section (collapsed by default in-app to stay compact; shown open in
  the mockup). 2-column grid of sliders: Azimuth 0–360°, Elevation 0–90°, Intensity 0–2,
  Ambient 0–1. Header tagged `3D sun`.

### Mode differences
- **3D mode** (mockup): Height scale + Exaggeration + Light (3D sun).
- **2D mode:** the top-left cluster gains a `[Height | Relief]` shading toggle. Height =
  greyscale field value (best for data maps: flow, masks, wear); Relief = hillshade (best for
  reading shape). Height scale (Fixed/Auto) only applies in Height shading. In Relief, the 2D
  hillshade **sun dial** appears top-right (drag to steer; azimuth°·altitude° readout) — its
  own state, separate from the 3D sun. The flyout's LIGHT section is tagged `2D sun` in this
  mode.

---

## Spec

**Frost tokens**
```
--c0        oklch(0.255 0.019 250)   input / track fill
--c1        oklch(0.305 0.017 250)   flyout panel background
--c2        oklch(0.345 0.017 248)
--c3        oklch(0.385 0.018 248)   pill active fill
--line      oklch(0.46 0.02 250)     borders
--line-soft oklch(0.40 0.018 250)    dividers, input outlines
--ink-hi    oklch(0.94 0.012 235)    values, active labels, titles
--ink-mid   oklch(0.77 0.016 240)    param labels
--ink-lo    oklch(0.62 0.016 240)    hints, readouts
--accent    oklch(0.70 0.13 205)     active pills, sliders, Display button, tint
```

**On-render overlay chrome** (controls floating directly on the render — must hold contrast
over both the dark 3D relief and the light 2D map):
```
background        oklch(0.20 0.018 250 / 0.82)
backdrop-filter   blur(10px)
border            1px solid oklch(0.5 0.02 250 / 0.4)
border-radius     6px
```
Active projection pill: `--accent` fill, `oklch(0.18 0.02 250)` text. Display button:
accent-tinted fill + `--accent` border. Fidelity text: `oklch(0.78 0.02 245 / 0.85)` with a
`text-shadow: 0 1px 3px rgba(0,0,0,0.6)` for legibility on light backgrounds.

**Flyout**
- Width 250px; solid `--c1`; 8px radius; shadow `0 20px 44px -18px rgba(0,0,0,0.7)`; small
  pointer triangle at the top toward the Display button.
- Output header block: accent-tinted gradient, bottom `--line-soft` divider.
- Param rows: label (fixed ~78px) + control + mono value right-aligned; slider track 4px, knob
  10–11px with a 2px ring in the panel bg. Pills: `--c0` track, `--c3` active with a subtle
  inset shadow.
- Section header: 34px, chevron + uppercase label (11px, 0.1em tracking), right-aligned
  sun-scope tag.
- Light grid: 2 columns, per-cell label+value row above a slider.

**Interaction**
- Display toggles the flyout; click-away closes it. Projection toggle swaps the flyout's
  mode-specific contents and the active sun.
- Output picker two-way-binds with the inspector thumbnail's picker.
- Sliders click-to-type / drag-to-scrub (consistent with node inspector + World Settings).
- Camera affordances are unchanged (3D: Alt+drag tumble/pan/dolly, scroll zoom; 2D: drag pan,
  scroll zoom, double-click fit) — no on-screen buttons.

## Out of scope
- The workspace layout switcher itself — see the workspace-panes handoff; it moves to the pane
  divider.
- The inspector thumbnail's own layout (only its Output picker is referenced, for sync).
- Terrain rendering / shading math; only the control surface changes.
- Placeholder render backgrounds in the mockup are CSS gradients, not real terrain.
