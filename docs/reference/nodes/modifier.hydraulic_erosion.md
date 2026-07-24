---
title: Hydraulic Erosion
status: draft
---

# Hydraulic Erosion

`modifier.hydraulic_erosion` · Geology · Mask-aware

Water carving the terrain, simulated as rain droplets that run downhill, pick up and drop sediment, and cut rills while depositing fans and filling hollows. The deposition is what reads as weathered. Taps wear, deposition, and flow.

## Purpose

*Not yet written.*

## Inputs

- `in`
- `mask` (optional)

## Outputs

- `heightfield`
- `wear`
- `deposition`
- `flow`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Density (`density`) | float | [0, 16] | 3 |  |  | no |
| Inertia (`inertia`) | float | [0, 1] | 0.05 |  |  | no |
| Capacity (`capacity`) | float | [0.1, 16] | 4 |  |  | no |
| Erosion (`erosion`) | float | [0, 1] | 0.3 |  |  | no |
| Deposition (`deposition`) | float | [0, 1] | 0.3 |  |  | no |
| Evaporation (`evaporation`) | float | [0, 0.2] | 0.02 |  |  | no |
| Radius (`radius`) | int | [1, 8] | 3 |  |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.

Emits `wear`, `deposition`, `flow` alongside the height layer.
