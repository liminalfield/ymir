---
status: draft
---

## Purpose

Flat, discrete cells, for plates or zones you can shape or scatter one region at a time. Reach for it as a control field that varies in patches rather than smoothly.

## Behaviour

Left unwired, every cell takes a random value of its own. Neighbouring cells are unrelated, so the field is patchy at every scale, and a cell that lands high has no neighbours near it.

Wire a field into `values` and each cell reads a single value from it instead, taken at that cell's own position. The cell stays flat, but cells near each other now take nearby values.

Two things this is for:

Feed it a low-frequency noise and the variation between cells becomes gradual across the map, so no cell stands alone. This is what jointed rock looks like: groups of blocks at similar heights, not a bed of spikes.

Feed it a gradient and the cells step down across the terrain while each one stays flat. Blending a gradient in afterwards instead would tilt every cell top, and stepping the gradient would cut bands straight across the cells; sampling once per cell does neither.

Frequency sets the cell size and jitter their regularity: 0 is a square grid, 1 fully organic.
