---
status: draft
---

## Purpose

Parallel bands swept across the terrain: dunes, ripples, corrugated ridges. Raw material to start a terrain from, not a finished landform. Shape it with a Curve, break it up with a Warp, and mask it into place.

## Behaviour

The output rises straight to each crest and falls straight away again, so the height tells you where you are within the wave. That is what makes it shapeable. A Curve after it maps position to height, which is exactly what a wave profile is: draw an S and you have a sine, draw a step and you have terraces, draw a dune and you have dunes.

For that reason the node has no waveform choice and no crest control. Building a sine in would hand you a profile already bent by someone else's curve, and a square would throw the position away entirely, leaving every cell at 0 or 1 with nothing in between for a Curve to work with.

Skew is the exception, and it is here because nothing downstream can do it. It moves the crest within the wavelength so the two slopes are different lengths. A Curve changes height, so it treats both slopes of a wave the same however it bends them; only moving the crest makes one slope long and the other short. That is the difference between a ripple and a dune, so the windward slope and the slip face are this node's business while the shape of either one is not.

Positive skew puts the crest late, giving a long rise and a short drop, which is the way round a dune runs.

Height comes out in the usual `[0, 1]`, so Levels sets how tall the result is, as with the other shape generators.

A dune field is this node, then a Curve to round the windward slope and sharpen the crest, then a Warp to stop the crests running dead straight.

## Recipes

Take care at extreme skew. Past about 0.9 one side of the wave becomes a vertical face one cell wide, and a face that thin steps sideways along the grid rather than running straight. That reads as notches on the slope once the terrain is triangulated in a game engine. A small Blur before export spreads the drop over several cells and clears it.
