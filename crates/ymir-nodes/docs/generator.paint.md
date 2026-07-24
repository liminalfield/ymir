---
status: draft
---

## Purpose

A selection you paint by hand, for scoping effects to regions no measured selector would find. Brush on the 2D map or the 3D surface and feed the result into an effect's mask input, and the effect applies only where you painted.

## Behaviour

Feed an existing selection into the mask input to hand-correct it, where paint adds and erase removes, or leave the input empty to paint a fresh one. The strokes are stored as editable vectors and rasterized at build resolution, so the mask stays crisp at any size.
