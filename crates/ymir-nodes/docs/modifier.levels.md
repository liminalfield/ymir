---
status: draft
---

## Purpose

Rescales the height range: stretch an input window to full, bias the midtones, and map into an output window. Reach for it to set contrast and amplitude, or to bring a field into range by hand where Normalize is too blunt.

## Behaviour

The five parameters are one control, and the inspector draws them as one picture. The incoming distribution runs along the bottom, the transfer curve crosses the plot, and each window bound is a line on the axis it acts along: the input bounds are vertical, since they cut the input, and the output bounds are horizontal, since they place the result. The curve is drawn by the same transfer the node applies, so what is drawn is what happens.

The horizontal axis is the incoming data's own range, not a fixed `[0, 1]`. A field carrying metres from a contour shows its real spread, so the input window can be set against where the data actually is rather than by guessing.

Neither axis moves when a bound does, so the distribution stays put while you aim at it. Dragging therefore reaches as far as the axis; a bound beyond it is typed in its row and parks its marker at that edge. Use the picture to place a bound against the data and the rows to set one exactly.
