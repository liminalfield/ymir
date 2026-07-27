---
status: draft
---

## Purpose

Names a region of the terrain as a material, so it can be shown in colour while you work and exported as a weight map for a game engine. Reach for it once the form is right and you want to say which parts are rock, grass, snow, or sand.

## Behaviour

Wire a selection into the mask to say where the material goes. The selection's values become the material's weight, so a Slope selector puts rock on steep ground and a Height selector puts snow above a line. Leave the mask empty and the material covers everything, which is how you make a base material that guarantees no part of the terrain is left unclaimed.

Each Material node writes one layer and leaves the others alone. It does not take weight away from materials already on the terrain, so two materials can both claim the same ground at full strength, and the per-cell total can exceed one. That is deliberate. A game engine normalizes its landscape layers when it renders them, and takes the stacking order from its own material setup rather than from the maps you give it, so deciding the blend here would only overwrite a choice that belongs downstream.

The colour is for previewing. It is never exported, and no engine reads it.
