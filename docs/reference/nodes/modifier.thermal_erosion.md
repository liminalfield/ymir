---
title: Thermal Erosion
status: draft
---

# Thermal Erosion

`modifier.thermal_erosion` · Geology · Mask-aware · Resolution-dependent

Relaxes slopes steeper than the talus angle toward repose.

## Purpose

Settles steep, sharp terrain into weathered slopes by letting material shed downhill until it rests
at a natural angle. Reach for it to turn raw noise or hard-edged shapes into ground that looks like
it has stood for a while.

## Inputs

- `in`
- `mask` (optional)

## Outputs

- `heightfield`
- `wear`
- `debris`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Talus (`talus`) | float | [0, 90] | 35 | ° |  | no |
| Strength (`strength`) | float | [0, 1] | 0.5 |  |  | no |
| Iterations (`iterations`) | int | [0, 1000] | 35 |  |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.

Emits `wear`, `debris` alongside the height layer.

## Behaviour

Thermal erosion is an iterative simulation, so it is resolution-dependent: the same settings at
preview and build resolutions give related but slightly different terrain, so treat the preview as
representative. Talus sets the steepest slope that survives; gentler ground is left alone. With no
mask the whole field settles; a mask confines the effect to the region you paint or select, and the
protected ground keeps its original height.

## See also

- [Hydraulic Erosion](modifier.hydraulic_erosion.md)
- [Stream Erosion](modifier.stream_erosion.md)
