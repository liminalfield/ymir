---
title: Rectangle
status: draft
---

# Rectangle

`generator.rect` · Generators

A flat-topped rectangular footprint with soft, rounded flanks: the envelope for a plateau, mesa, or rectangular landmass. Turn it with rotation.

## Purpose

*Not yet written.*

## Inputs

This node takes no inputs.

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Extent X (`extent_x`) | float | [0, 100000] | 500 | m |  | no |
| Extent Y (`extent_y`) | float | [0, 100000] | 300 | m |  | no |
| Falloff (`falloff`) | float | [0, 100000] | 120 | m |  | no |
| Rotation (`rotation`) | float | [0, 360] | 0 | ° |  | no |
| Center X (`center_x`) | float | [0, 1] | 0.5 |  |  | no |
| Center Y (`center_y`) | float | [0, 1] | 0.5 |  |  | no |

## Layer contract

Reads and writes the height layer.
