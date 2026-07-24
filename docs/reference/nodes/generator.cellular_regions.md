---
title: Cellular Regions
status: draft
---

# Cellular Regions

`generator.cellular_regions` · Generators

Worley cells as flat, discrete regions (plates, zones): a control field to shape or scatter per region. Frequency sets the region count, jitter shape.

## Purpose

*Not yet written.*

## Inputs

This node takes no inputs.

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Frequency (`frequency`) | float | [0, 64] | 8 |  | Sets the feature size of the noise; higher values pack in smaller, denser features. | no |
| Jitter (`jitter`) | float | [0, 1] | 1 |  |  | no |
| Seed (`seed`) | int | [0, 2147483647] | 0 |  | The random seed; changing it regenerates a different variation of the same pattern. | no |
| Offset X (`offset_x`) | int | [-10000, 10000] | 0 |  | Pans the noise pattern along the X axis without changing its shape. | no |
| Offset Y (`offset_y`) | int | [-10000, 10000] | 0 |  | Pans the noise pattern along the Y axis without changing its shape. | no |

## Layer contract

Reads and writes the height layer.
