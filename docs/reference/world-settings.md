---
title: World settings
status: draft
---

# World settings

World settings are project-wide values that describe the world a graph builds. They travel with the project and are edited in the World panel.

## Seed

The global random seed. Every generator derives its randomness from the seed and the node's own stable identity, so changing the seed regenerates the whole world, while editing one part of a graph leaves the unrelated nodes producing the same result.

## World extent

The world's physical width in metres, across the full canvas. Cells are kept square, so the depth follows from the grid's aspect. A length given in metres, such as an erosion or blur radius, is measured against this extent, so the same graph holds its proportions when the world grows or shrinks. Default: 1 m.

## World height

The real elevation in metres that a normalized height of 1.0 represents: the vertical counterpart to World extent. Two things read it. Slope-aware nodes (thermal erosion's talus angle, the Slope selector) combine it with the horizontal cell size to work in real degrees, and export bakes it into an absolute-metre heightmap as height times World height. Default: 1 m.

## Sea level

The sea or base level, as a normalized height in the working range `[0, 1]`. The 3D viewport draws its water plane here, and the nodes that need a base level read it: the Coastal shaper bevels the shore down to it, and Stream erosion cuts its channels toward it. Default: 0.

## Viewport exaggeration

A display control for the 3D viewport. It exaggerates vertical relief so subtle height changes, such as fine erosion detail, stay legible in the view. It changes only what you see, leaving the field data and every export untouched.

## Water

How the 3D viewport draws the water surface, in the World panel's Water section. These are display settings: they travel with the project, and they change nothing a node computes and nothing an export writes. Sea level is the one water value the terrain itself reads, and it is described above.

The surface is drawn wherever the terrain sits below Sea level, so the water controls do nothing until Show water is on and some terrain is below that level.

**Water colour** is the tint the surface takes. Default: a deep blue.

Each group below has its own toggle. Turning one off drops that effect and leaves the rest.

### Depth

Beer-Lambert extinction: the deeper the water over the seabed, the more it tints toward its deep shade and the more opaque it becomes. Off, the surface is one flat tint at a fixed translucency.

**Falloff** sets how fast that happens, so a higher value clears to opaque in shallower water. Range 1 to 30, default 5.

### Gerstner waves

Trochoidal waves that displace the surface geometry, so crests sharpen and troughs broaden. The surface normal comes from the same wave sum, which is what the reflection then shades. Wave height is damped to nothing at the shoreline, so crests never break through the terrain.

| Control | What it does | Range | Default |
|---|---|---|---|
| Speed | Animation rate for the wave surface and the foam wash. At 0 both hold still. | 0 to 2 | 0.4 |
| Amplitude | Overall wave height. | 0 to 1 | 0.5 |
| Steepness | Crest sharpness. Held below the point where a wave would fold over itself. | 0 to 1 | 0.6 |
| Wavelength | Multiplier on the wave sizes, so higher is longer and lower in frequency. | 0.3 to 3 | 1 |
| Direction | The bearing the swell travels: 0° toward the right of the map, 90° toward the bottom. Rolls, so scrubbing past 360° carries on from 0°. | a full turn | 14° |
| Spread | How widely the wave components fan around Direction. At 0 they are parallel and read as one swell; at 1 they cross by up to 105° and read as chop. | 0 to 1 | 1 |

### Reflection

A sky reflection that strengthens at grazing angles, plus a specular highlight from the sun. It toggles separately from the waves, so the surface can be flat and mirrored, or wavy and matte.

| Control | What it does | Range | Default |
|---|---|---|---|
| Reflectivity | How much sky the surface reflects. | 0 to 1 | 0.6 |
| Specular | Strength of the sun's highlight. | 0 to 1 | 0.5 |

### Foam

A band hugging the waterline, solid at the water's edge and breaking into patches toward its outer edge, washing in and out.

| Control | What it does | Range | Default |
|---|---|---|---|
| Amount | How strongly the foam reads. | 0 to 1 | 0.5 |
| Width | How far out the band reaches, in depth below the waterline. | 0 to 0.05 | 0.015 |

### Wet shore

Darkens the land just above the waterline, the way a beach reads wet between waves.

| Control | What it does | Range | Default |
|---|---|---|---|
| Strength | How dark the wet band goes. | 0 to 1 | 0.35 |
| Width | How far up the shore it reaches, in height above the waterline. | 0 to 0.1 | 0.03 |

## Resolution

Two square resolutions travel with the project: the resolution the interactive preview evaluates at, and the resolution a full build evaluates at. Iterative simulations such as erosion are resolution-dependent, so a preview is a representative approximation of the build. See [Preview and build](../concepts/preview-and-build.md).
