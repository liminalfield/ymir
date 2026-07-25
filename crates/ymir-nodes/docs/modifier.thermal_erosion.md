---
status: draft
resolution_dependent: true
---

## Purpose

Settles steep, sharp terrain into weathered slopes by letting material shed downhill until it rests
at a natural angle. Reach for it to turn raw noise or hard-edged shapes into ground that looks like
it has stood for a while.

## Behaviour

Thermal erosion is an iterative simulation, so it is resolution-dependent: the same settings at
preview and build resolutions give related but slightly different terrain, so treat the preview as
representative. Talus sets the steepest slope that survives; gentler ground is left alone. With no
mask the whole field settles; a mask confines the effect to the region you paint or select, and the
protected ground keeps its original height.

## See also

- [Hydraulic Erosion](modifier.hydraulic_erosion.md)
- [Stream Erosion](modifier.stream_erosion.md)
