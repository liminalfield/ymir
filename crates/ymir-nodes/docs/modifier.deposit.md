---
status: draft
---

## Purpose

Rains material onto the terrain and lets it settle: snow across a range, or sand burying all but the peaks of a desertifying landscape. The material arrives from outside the terrain rather than being moved around by it, so this is not erosion.

## Behaviour

Deciding where material lands is not what this node is for. Every rule you might want already exists as a selector: Curvature picks out hollows, Aspect the lee side, Slope the flats, Height an elevation band, Occlusion the sheltered ground. Wire any of them into the mask.

What a mask cannot do is settle, and that is the difference between a covering and a coat of paint. Add a masked constant and you get an even thickness that follows every bump underneath. Real snow fills a hollow because its top is level, thick in the middle and thin at the edges, and no mask said any of that. Sand buries a landscape and leaves the peaks standing because the sand found a level, not because something drew around the peaks.

Repose is the main dial and it covers a wide range. Near zero the material behaves like a liquid: it finds a level, fills every hollow, and drowns the terrain up to a line. Around 34 degrees it behaves like sand, and around 38 like snow, draping the ground, holding the flat places and sliding off anything steep.

Ground steeper than the repose angle holds nothing at all, so cliffs come out bare without needing to be masked out.

Elevation gives a snow line, with the falloff setting how sharp it is. Wind piles material on the sheltered side of ridges, which is where cornices and dunes build.

The terrain underneath is never moved. Material slides down it and off it, but the rock stays where it was, so the height output is never lower than the input.

The cover output reports how deep the material lies, and rides on the heightfield as a layer too, so a Material or a mask downstream can tell covered ground from bare rock without working it out again.

## Recipes

Settling is iterative, so it is resolution-dependent in the way the erosion nodes are: a low-resolution preview approximates the build rather than matching it. Passes scale with resolution, so the preview stays representative.

For snow, keep repose near 38, set an elevation for the snow line, and add a little wind bias to build the lee-side drifts. Thirty passes is plenty: snow does not travel far before it comes to rest.

For sand, drop repose to a few degrees and raise the depth until only the peaks remain. **Raise the passes a long way too.** Material moves one cell per pass, so at a low repose it has to travel a long way to find its level, and too few passes leaves it stranded part-way as tongues and fronts that look steeper than the ground they landed on. Measured on a 256 grid, sixty passes made the covered ground *rougher* than the bare rock, while two hundred brought it down to 41% of the original slope. If a deposit looks lumpy rather than level, the passes are the first thing to raise.
