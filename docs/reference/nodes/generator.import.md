---
title: Import
status: draft
---

# Import

`generator.import` · Generators

Loads a heightmap PNG as a field, resampled to the build resolution and placed by offset, rotation, and scale. Set the file path; an empty path is a flat field. The edge policy fills where the placement maps outside the image.

## Purpose

*Not yet written.*

## Inputs

This node takes no inputs.

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Path (`path`) | path |  |  |  |  | no |
| Offset X (`offset_x`) | float | [-1, 1] | 0 |  | Pans the noise pattern along the X axis without changing its shape. | no |
| Offset Y (`offset_y`) | float | [-1, 1] | 0 |  | Pans the noise pattern along the Y axis without changing its shape. | no |
| Rotation (`rotation`) | float | [0, 360] | 0 | ° |  | no |
| Scale (`scale`) | float | [0.05, 8] | 1 |  |  | no |
| Edge (`edge`) | enum | extend, zero, wrap | extend |  | How the field is filled where the placement maps outside the source image. | no |

## Layer contract

Reads and writes the height layer.
