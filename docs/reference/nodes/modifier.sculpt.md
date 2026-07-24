---
title: Sculpt
status: draft
---

# Sculpt

`modifier.sculpt` · Adjust

Sculpt terrain by brushing height onto it: paint raises, erase lowers, and overlapping strokes build up pass by pass. Wire a terrain in to sculpt it, or leave the input empty to build form from scratch. Strength sets how hard each pass bites; the height is not clamped. Stored as editable vector strokes.

## Purpose

*Not yet written.*

## Inputs

- `in` (optional)

## Outputs

- `out`

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Strokes (`strokes`) | strokes |  |  |  |  | no |

## Layer contract

Reads and writes the height layer.
