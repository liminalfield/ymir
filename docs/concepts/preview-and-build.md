---
title: Preview and build
status: draft
---

# Preview and build

Ymir works at two resolutions. The preview evaluates at a lower resolution so the terrain updates while you compose. The build evaluates at the full resolution you export. The build is the source of truth; the preview is a fast approximation of it.

For most nodes the two agree exactly. Noise, shapes, and anything sampled from continuous coordinates give the same value at any resolution, so the preview shows precisely what the build will produce, only coarser.

Erosion is the exception, and it matters. Erosion is an iterative simulation, not a sampled function: it runs many small steps, each reading the last, and a finer grid carves different detail than a coarse one. So an eroded preview is representative of the build, not identical to it. The valleys land in the same places and the character holds, but the fine drainage differs.

What this means in practice: compose and place erosion against the preview, trusting its overall form, and read the fine result from a build. The erosion controls are expressed in world terms (strengths and distances in metres, iteration counts that scale with resolution) so the preview stays a fair guide rather than a misleading one. Two builds of the same project at the same resolution on the same machine always match.
