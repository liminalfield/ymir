---
title: Clamp
status: draft
---

# Clamp

`modifier.clamp` · Adjust · Mask-aware

Hard-clamps the height layer into [min, max]: caps overshoots, floors basins, or bounds a value before it feeds something range-sensitive. Mask-aware.

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Min (`min`) | float | [-4, 4] | 0 |  |  | no |
| Max (`max`) | float | [-4, 4] | 1 |  |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.
