---
status: draft
---

## Purpose

Names a selection as a material and gives it a colour, so it can be shown on the terrain and arranged with other materials. Reach for it once the form is right and you want to say which parts are rock, grass, snow, or sand.

## Behaviour

Wire a selection into it: a Slope selector for rock on steep ground, a Height selector for snow above a line, a Constant at 1 for a material that covers everything. The selection's values become the material's weight, clamped to the range a weight is defined over, and that is what comes out. Tap it to preview that material's coverage, or run it into an export node to write the weight map to disk.

It takes no terrain. A material says where something is, not what the ground does. Which materials are shown together, in what order, and which are muted is a material set, and that lives in the Materials panel rather than in the graph.

The name and the colour are read by the panel. The colour is for previewing: it is never exported, and no engine reads it.
