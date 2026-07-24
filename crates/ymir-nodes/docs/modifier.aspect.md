---
status: draft
---

## Purpose

Selects slopes that face a compass direction, high where the terrain looks toward it and softening away from it. Reach for it for sun-facing or wind-facing effects: poleward snow, directional weathering.

## Behaviour

Being a gradient measure, it amplifies sharp input such as thin ridges, so scale it with an upstream Blur. A slope weight suppresses the flats, where facing is meaningless.
