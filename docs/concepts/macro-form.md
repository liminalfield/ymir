---
title: Macro form and surface texture
status: draft
---

# Macro form and surface texture

Ymir shapes the large form of terrain: the mountains, valleys, ridgelines, coasts, and drainage that read at the scale of a landscape. It works the heightfield, the elevation across the world, and it is built to do that well.

The fine surface, the pebble-scale roughness and material grain you see from a metre away, is a different job, and one a dedicated texturing tool in a game engine or DCC package does better. Ymir hands off a heightmap (and, if you want them, the erosion byproduct layers) for that stage to work from.

Knowing which scale you are working at keeps a graph honest. Choose terrain that lives on macro form: an eroded range, a coastline, a fold of hills, a river basin. Judge the result at the scale of the whole world, not zoomed in on a single face.

The distinction is relative to the world, not absolute. A frequency that is coarse detail on a hundred-kilometre continent is the whole landform on a one-kilometre island. Because Ymir measures lengths against [World extent](../reference/world-settings.md), the same graph holds its proportions as the world grows or shrinks, and "macro" always means the same fraction of the terrain you are building.
