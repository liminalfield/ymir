---
title: Blend
status: draft
---

# Blend

`modifier.blend` · Combine · Mask-aware

Composites two fields by a mode (normal, add, subtract, multiply, max, min, difference) eased in by opacity.

## Purpose

*Not yet written.*

## Inputs

- `base`
- `overlay`
- `mask` (optional)

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Mode (`mode`) | enum | normal, add, subtract, multiply, max, min, difference | normal |  | How the overlay is combined with the base field. | no |
| Opacity (`opacity`) | float | [0, 1] | 1 |  |  | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.
