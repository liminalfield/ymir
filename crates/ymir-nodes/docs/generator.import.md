---
status: draft
---

## Purpose

Brings an existing heightmap into the graph as a field, so you can build on terrain made elsewhere. Reach for it to start from a real-world heightmap or a rough sketch, then shape and erode it like any generated terrain.

## Behaviour

The image is resampled to the build resolution and placed by offset, rotation, and scale. An empty path gives a flat field, and the edge policy fills anywhere the placement maps outside the source image.
