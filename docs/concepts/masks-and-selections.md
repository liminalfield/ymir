---
title: Masks and selections
status: draft
---

# Masks and selections

Ymir scopes an effect to part of the terrain with a grayscale field: 1 where the effect applies fully, 0 where it does not, and the soft values between for a gradual edge. The same values do two jobs, and keeping the two words apart keeps a graph readable.

A **selection** is what a selector produces. The Slope, Height, Curvature, and Aspect selectors each measure the terrain and turn it into a `[0, 1]` field: high on the steep ground, or the high ground, or the ridgelines, or the sun-facing slopes. You can also paint a selection by hand with the Paint node.

A **mask** is the input that a node reads to know where to act. Erosion, Blur, Levels, and the rest take a mask and confine themselves to it. A node that reads a mask and finds none applies everywhere, so leaving the input empty is the same as selecting the whole terrain.

The two connect in the obvious way: a selection feeds a mask. Measure the steep ground with a Slope selector, feed it into an erosion node's mask, and the erosion works only on the slopes. To combine conditions, blend two selections before the mask (steep **and** high, steep **or** near the coast). Reach for this whenever an effect should be placed rather than applied everywhere.
