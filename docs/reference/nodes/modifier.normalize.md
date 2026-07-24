---
title: Normalize
status: draft
---

# Normalize

`modifier.normalize` · Adjust · Mask-aware

Fits the height layer's actual min-max to [0, 1] (the one-click companion to Levels): pulls a raw measure or out-of-range height back into the working greyscale. A flat field passes through. Mask-aware.

## Purpose

*Not yet written.*

## Inputs

- `in`

## Outputs

- `out`

## Parameters

This node has no parameters.

## Layer contract

Honours a mask on its input, applying everywhere the mask is absent.
