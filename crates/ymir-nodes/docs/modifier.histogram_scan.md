---
status: draft
---

## Purpose

Windows a range of input values into a crisp `[0, 1]` mask, set by a position, a width, and a soft edge. Reach for it to turn a selector's raw measure, such as slope in degrees or curvature, into a clean selection.

## Behaviour

Auto range scans the input's actual min-max, so it reshapes a raw measure directly; fixed range works in absolute `[0, 1]`.
