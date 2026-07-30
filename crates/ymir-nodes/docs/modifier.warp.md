---
status: draft
---

## Purpose

Adds meandering irregularity to features that look too regular or machine-made. Warp displaces the
terrain sideways rather than up, so it changes the shape of features without changing their height.

## Behaviour

Wire the optional mask input to confine the warp to a selection: the displacement scales by the
mask, so a Curvature selection of the ridgelines jitters only the ridges while the rest holds still.
Where the mask is zero the terrain passes through unwarped, and a partial mask eases the warp in.

Wavelength is the warp field's own feature size, in world units: how far apart the swirls sit, as against amount, which is how far each one pushes. Both are real sizes, so a warp keeps its character when the world extent changes.
