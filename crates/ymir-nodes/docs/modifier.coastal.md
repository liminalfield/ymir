---
status: draft
---

## Purpose

Reshapes the shore into a beach-and-bluff profile: a gentle beach face at the water, a berm crest, then a steeper backing slope that leaves the terrain behind standing as a bluff. Reach for it to turn a hard waterline into a believable coast without flattening the hills behind it.

## Behaviour

On land it cuts the terrain down to a two-slope profile measured from the shoreline. The beach face reaches `beach_width` metres inland to the berm crest at `berm_height` (so its grade is `berm_height / beach_width`), then the steeper `bluff_angle` takes over. `rounding` blends the crest between them into a shoulder over that many metres, so a long beach meets its backing as a soft break rather than a hard edge. Because the backing slope is steep it clears the terrain behind within a short run, so only the low apron near the water is carved and the hill behind is kept as a bluff; the break of slope where the profile meets the un-cut hillside is the bluff toe. The land effect self-terminates against the terrain, so its inland reach is the beach geometry itself.

Offshore it raises the seabed toward sea level, fading that lift out to `shoreface_reach`. The land and sea sides have independent extents, so widening the beach does not enlarge the underwater shelf, and the coast is not dominated by change below the waterline. It bevels by true distance from the shoreline, so the coast is even all around.

It reads the world sea level and taps three selections as layers: `shore` (a band at the waterline, for a wet edge or foam), `beach` (the whole berm slope, from the waterline up to near the crest), and `bluff` (the backing slope past the crest, which is present only where the coast is steeper than the bluff angle). You can texture each zone on its own or combine them.
