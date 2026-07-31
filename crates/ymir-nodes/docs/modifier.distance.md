---
status: draft
---

## Purpose

Selects a band around a height contour, measured by true distance in metres so the band is even all around. Reach for it to place a feature that tracks a level: a shoreline, a snow line, a terrace edge.

## Behaviour

The distance is an isotropic eikonal solve, so the band width does not vary with direction. It can cover both sides of the contour or only one.

Set From to sea to track the world's sea level instead of a height you name. That reading also asks what the water is connected to: only water reaching the map edge counts as sea, so a hollow below sea level in the middle of an island measures its distance to the real coast rather than making a shore of its own.

The second output, `distance`, is the measurement itself in metres, negative on the far side of the contour and positive on the near side. The first output is a band that peaks on the contour and fades over Range, which is what a selection wants; this one keeps the sign and does not fade, which is what shaping wants. Feed it through Levels and Curve to make anything a function of how far it is from the contour: a beach profile rising from the shore, a snow line that thickens with altitude above it, a material that changes with distance from a terrace edge. The measurement is the contour's own coordinate system, so shaping along it needs no direction maths.
