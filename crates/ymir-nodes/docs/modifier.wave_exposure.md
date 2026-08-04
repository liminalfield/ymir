---
status: draft
---

## Purpose

Selects the coast the sea works hardest: high on headlands that jut into the water, low in sheltered bays. Reach for it when a coastline reads as uniform, with the same beach in an inlet as on an open cape.

Wire it into a mask and the difference appears. Inverted, it places beaches where the water is calm; used directly, it places cliffs, wet rock, and wear where the water is not.

## Behaviour

Waves bend toward shallow water, which gathers their energy onto ground that sticks out and spreads it around the inside of a bay. That is why headlands erode back while bays fill in, and it is what this node measures, from the shape of the shoreline in plan rather than from any simulation.

The output is centred on 0.5. A straight coast reads 0.5, headlands read above it, bays below. Nothing is switched off at the midpoint, so invert it or window it with Levels to get a mask.

It reads the sea level from the world settings, so there is no waterline setting here. An enclosed below-sea basin counts as land and grows no coast of its own, the same rule the Distance node follows.

The reading is a coastal one, so it fades back to 0.5 away from the water over the same feature radius. Out in open water and far inland the field is simply neutral.

**Feature radius is the control that matters.** Set it before anything else, to the size of the headlands you care about. It is a length, so its right value depends entirely on how big your coast is, and there is no default that suits every world.

Curvature is a second derivative, so it magnifies whatever detail is present. On a real coastline the fine wiggle of the shore is far sharper than the broad shape of the cape it sits on, and left alone the node reads the wiggle. On a test island with fine shoreline detail, three headlands in five came out reading as bays; setting the radius to the scale of the landform fixed every one of them.

Both errors have a look. Too small reads as noise, and a headland you can see plainly comes out sheltered. Too large erases the coast: the field goes flat and near-empty, because the smoothing has removed the very features being measured. If it looks blank, halve it.

**Refraction gain** is how hard headlands gain and bays lose, as contrast around the midpoint. Zero gives a flat field.

Wave direction is not included yet. Every direction is currently treated as equally open, so a bay facing away from the prevailing swell is judged by its shape alone rather than by being in the lee. Shape is the larger part of the effect, and direction is the other half of the model still to come.

## Recipes

**Beaches in the bays, bare capes.** Wave Exposure into Invert, multiplied against a shore band from Distance, into the mask of the blend that applies your beach. The beach then fills the sheltered water and thins out where the sea is working.

**Wet rock and wear.** Wave Exposure straight into a Levels to window the top of its range, as the selection for a material or an erosion mask along the worked coast.
