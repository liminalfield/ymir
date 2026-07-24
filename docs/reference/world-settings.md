---
title: World settings
status: draft
---

# World settings

World settings are project-wide values that describe the world a graph builds. They travel with the project and are edited in the World panel.

## Seed

The global random seed. Every generator derives its randomness from the seed and the node's own stable identity, so changing the seed regenerates the whole world, while editing one part of a graph leaves the unrelated nodes producing the same result.

## World extent

The world's physical width in metres, across the full canvas. Cells are kept square, so the depth follows from the grid's aspect. A length given in metres, such as an erosion or blur radius, is measured against this extent, so the same graph holds its proportions when the world grows or shrinks. Default: 1 m.

## World height

The real elevation in metres that a normalized height of 1.0 represents: the vertical counterpart to World extent. Two things read it. Slope-aware nodes (thermal erosion's talus angle, the Slope selector) combine it with the horizontal cell size to work in real degrees, and export bakes it into an absolute-metre heightmap as height times World height. Default: 1 m.

## Sea level

The sea or base level, as a normalized height in the working range `[0, 1]`. The 3D viewport draws its water plane here, and the nodes that need a base level read it: the Coastal shaper bevels the shore down to it, and Stream erosion cuts its channels toward it. Default: 0.

## Viewport exaggeration

A display control for the 3D viewport. It exaggerates vertical relief so subtle height changes, such as fine erosion detail, stay legible in the view. It changes only what you see, leaving the field data and every export untouched.

## Resolution

Two square resolutions travel with the project: the resolution the interactive preview evaluates at, and the resolution a full build evaluates at. Iterative simulations such as erosion are resolution-dependent, so a preview is a representative approximation of the build. See [Preview and build](../concepts/index.md).
