---
title: Fields and layers
status: draft
---

# Fields and layers

One kind of data flows along every connection in Ymir: a field. A field is a grid of values covering the world, and every node takes fields in and hands fields out. Because the type is always the same, a node drops in anywhere a field is available, and you are never forced into a fixed build order.

A field carries named **layers**. The `height` layer is the terrain itself, and it is the one almost every node reads and writes. Other layers ride alongside it: a `mask` that scopes where an effect applies, or the `flow`, `wear`, and `deposition` that erosion produces as it runs. A node that changes the height passes every other layer through untouched, so inserting it never loses the work upstream of it.

## The height convention

Height is a plain number in the working range `[0, 1]`: 0 is the lowest ground, 1 the highest. This is a convention, not a wall. A node is free to push values above 1 or below 0, and the range is only resolved to real elevation at the end, by [World height](../reference/world-settings.md) on export and by the display when you look at the terrain.

Two habits follow from this. Judge a terrain by its shape and its relative heights, not by an absolute number, because the mapping to metres happens later. And when a value leaves the range, do not reach for a clamp out of reflex: the range is meant to be exceeded in passing, and clamping throws away detail an export or a later node could have used.
