---
title: Occlusion
status: draft
---

# Occlusion

`modifier.occlusion` · Selectors

Ambient-occlusion / sky-view measure: high in crevices and valley floors hemmed in by higher ground, low on open peaks and flats. Ray count and world-unit radius set the sampling. Picks sheltered terrain (catchment, moisture, shadow).

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Radius (`radius`) | float | [1, 100000] | 100 | m |  | no |
| Rays (`rays`) | int | [4, 64] | 16 |  |  | no |

## Layer contract

Reads and writes the height layer.
