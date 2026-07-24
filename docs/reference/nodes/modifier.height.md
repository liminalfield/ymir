---
title: Height
status: draft
---

# Height

`modifier.height` · Selectors

Selects a band of elevation: high where the normalized height is within min..max, softening over the falloff.

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Min (`min`) | float | [0, 1] | 0.4 |  |  | no |
| Max (`max`) | float | [0, 1] | 0.7 |  |  | no |
| Falloff (`falloff`) | float | [0, 1] | 0.1 |  |  | no |
| Output (`output`) | enum | selection, measure | selection |  |  | no |

## Layer contract

Reads and writes the height layer.
