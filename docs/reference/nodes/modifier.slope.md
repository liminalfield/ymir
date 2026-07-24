---
title: Slope
status: draft
---

# Slope

`modifier.slope` · Selectors

Selects a band of steepness: high where the slope angle is within min..max degrees, softening over the falloff. Scale it with an upstream Blur.

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Min (`min`) | float | [0, 90] | 20 | ° |  | no |
| Max (`max`) | float | [0, 90] | 50 | ° |  | no |
| Falloff (`falloff`) | float | [0, 90] | 10 | ° |  | no |
| Output (`output`) | enum | selection, measure | selection |  |  | no |

## Layer contract

Reads and writes the height layer.
