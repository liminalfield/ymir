---
title: Ring
status: draft
---

# Ring

`generator.ring` · Generators

A smooth circular ridge (1 on the radius, 0 on each flank): the envelope for a crater rim, caldera wall, or atoll.

## Purpose

*Not yet written.*

## Inputs

This node takes no inputs.

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Radius (`radius`) | float | [0, 100000] | 300 | m |  | no |
| Width (`width`) | float | [0, 100000] | 100 | m |  | no |
| Center X (`center_x`) | float | [0, 1] | 0.5 |  |  | no |
| Center Y (`center_y`) | float | [0, 1] | 0.5 |  |  | no |

## Layer contract

Reads and writes the height layer.
