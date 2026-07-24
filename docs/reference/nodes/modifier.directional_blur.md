---
title: Directional Blur
status: draft
---

# Directional Blur

`modifier.directional_blur` · Filters · Mask-aware

Smooths the height layer along (or across) a guide direction, not isotropically: steer by the slope (fall line, or a distance field's shore normal) or a flow field. Along combs valleys and smears downslope; across softens a cross-profile while keeping the guide crest crisp. Optional guide input; degrades gracefully.

## Purpose

*Not yet written.*

## Inputs

- `in`
- `guide` (optional)
- `mask` (optional)

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Radius (`radius`) | float | [0, 100000] | 8 | m |  | no |
| Guide (`guide`) | enum | slope, flow | slope |  |  | no |
| Direction (`direction`) | enum | along, across | along |  | Whether smoothing runs along the guide direction or across it. | no |

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.
