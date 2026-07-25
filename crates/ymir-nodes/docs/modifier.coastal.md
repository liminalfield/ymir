---
status: draft
---

## Purpose

Reshapes the shore into a beach-and-bluff profile: a gentle beach face at the water, a berm crest, then a steeper backing slope that leaves the terrain behind standing as a bluff. Reach for it to turn a hard waterline into a believable coast without flattening the hills behind it.

## Behaviour

On land it cuts the terrain down to a two-slope profile measured from the shoreline. The beach face rises at `angle` from the waterline to the berm crest at `berm_height`, then the steeper `bluff_angle` takes over. Because that backing slope is steep it clears the terrain behind within a short run, so only the low apron near the water is carved and the hill behind is kept as a bluff; the break of slope where the profile meets the un-cut hillside is the bluff toe. Offshore it lifts the seabed toward a gentle shoreface.

It bevels by true distance from the shoreline, so the coast is even all around, and the effect fades to nothing over `width` metres. It reads the world sea level and taps the shore band as a layer. Setting `berm_height` to zero with `bluff_angle` equal to `angle` collapses the profile to a single gentle wedge, which flattens the whole band.
