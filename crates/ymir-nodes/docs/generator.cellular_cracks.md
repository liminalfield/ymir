---
status: draft
---

## Purpose

A network of cell-edge ridges, for cracks, fractures, dried mud, and rocky cell walls. Reach for it to break a surface into angular plates.

## Behaviour

Placement sets where the cells start from. `square` puts one point per square of a grid, so at jitter 0 the cells are squares. `hex` uses a triangular lattice, where every point is the same distance from its six nearest neighbours, so at jitter 0 the cells are regular hexagons.

Reach for `hex` when the pattern is cracking rather than tiling. Rock cools and mud dries by contracting, and contraction joints meet at 120 degrees, which makes hexagons. Columnar basalt is the clearest case. Raise jitter from there and you get irregular but still six-sided-on-average cells, which is what real jointing looks like.

Cell size is a real width in world units, so the crack spacing stays what you asked for when the world extent changes and a larger world simply holds more cells.
