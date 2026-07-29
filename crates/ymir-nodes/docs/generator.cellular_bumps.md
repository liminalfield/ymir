---
status: draft
---

## Purpose

Scattered cones peaking at random points, for rock piles, bumps, and scaled surfaces. Frequency sets how densely the cones pack; reach for it to add clustered relief.

## Behaviour

Placement sets where the cells start from. `square` puts one point per square of a grid, so at jitter 0 the cells are squares. `hex` uses a triangular lattice, where every point is the same distance from its six nearest neighbours, so at jitter 0 the cells are regular hexagons.

Reach for `hex` when the pattern is cracking rather than tiling. Rock cools and mud dries by contracting, and contraction joints meet at 120 degrees, which makes hexagons. Columnar basalt is the clearest case. Raise jitter from there and you get irregular but still six-sided-on-average cells, which is what real jointing looks like.
