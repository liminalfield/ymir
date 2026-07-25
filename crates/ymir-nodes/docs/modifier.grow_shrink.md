---
status: draft
---

## Purpose

Grows or shrinks a selection by a distance in metres. Reach for it to widen a mask that came out too tight (a thin ridgeline from a low Curvature strength) or to pull one in from an edge, without the blur-then-rethreshold dance.

## Behaviour

It reads the input as a region bounded by its half-way contour and offsets that boundary: a positive Amount moves it outward (grow), a negative Amount inward (shrink), over a Softness-wide soft edge. The offset is a true isotropic distance from the shared eikonal solver, so a grown circle stays a circle, and the result is byte-identical on every machine. Working from the contour means a soft selection comes back solid with a clean edge, so an Amount of zero tidies a fuzzy mask.
