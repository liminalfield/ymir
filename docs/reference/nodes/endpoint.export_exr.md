---
title: Export EXR
status: draft
---

# Export EXR

`endpoint.export_exr` · Outputs

Writes the height layer to a 32-bit float EXR: lossless, and can bake absolute elevation in meters (height x world height) so the file is self-describing.

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

This node produces no outputs.

## Parameters

| Parameter | Type | Range | Default | Unit | Description | Field-driven |
|---|---|---|---|---|---|---|
| Path (`path`) | text |  | out/heightmap.exr |  |  | no |
| Build (`build`) | bool |  | true |  |  | no |
| Height Units (`height_units`) | enum | normalized, meters | normalized |  | Whether height is written as the normalized [0, 1] value or scaled to absolute metres by World Height. | no |

## Layer contract

Reads and writes the height layer.
