---
title: Blur
status: draft
---

# Blur

`modifier.blur` · Filters · Mask-aware

Gaussian-blurs the height layer by a world-unit radius (the scale knob for derived selectors, and feathers masks).

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Radius (`radius`) | float | [0, 100000] | 8 | m |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.
