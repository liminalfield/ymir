---
title: Stream Erosion
status: draft
---

# Stream Erosion

`modifier.stream_erosion` · Geology · Mask-aware

Carves drainage networks from flow accumulation; outputs the river/flow map.

## Purpose

*Not yet written.*

## Inputs

- `in`
- `mask` (optional)

## Outputs

- `heightfield`
- `flow`
- `wear`
- `deposition`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Strength (`strength`) | float | [0, 1] | 0.2 |  |  | no |
| Diffusion (`diffusion`) | float | [0, 1] | 0.5 |  |  | no |
| Iterations (`iterations`) | int | [0, 500] | 30 |  |  | no |
| Concavity (`concavity`) | float | [0.1, 2] | 0.5 |  |  | no |
| Concentration (`concentration`) | float | [1, 6] | 1.5 |  |  | no |
| Fill (`fill`) | float | [0, 1] | 0.05 |  |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.

Emits `flow`, `wear`, `deposition` alongside the height layer.
