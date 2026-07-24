---
title: Landforms are composed, not built in
status: draft
---

# Landforms are composed, not built in

Ymir has no crater node, no volcano node, no caldera node. A landform like that is a shape you compose from a few general nodes, not a single node with a landform baked inside it. This is deliberate, and it is what makes the node set small and a graph readable.

A crater is a good example. A [Radial Falloff](../reference/nodes/generator.falloff.md) gives a clean distance ramp from a centre. A [Curve](../reference/nodes/modifier.curve.md) reshapes that ramp into any radial cross-section you draw: a rim that rises then drops into a bowl is a crater, a rim with a flat floor is a caldera, a cone is a hill. To place many of them, a Scatter distributes the shape across the terrain. The same three moves, with a different curve, give a different landform.

The payoff is that the graph shows what it does. Someone reading the wiring sees a falloff shaped by a curve and scattered, which is a description of the terrain, where a single "crater" node would hide its behaviour inside parameters. Common compositions ship as example subgraphs you can drop in and open up, so you start from a worked landform and adjust it rather than wiring it from scratch.

When you want a feature Ymir has no node for, this is the move: find the general shape underneath it (a distance, a band, a direction), draw its profile with a curve, and place it. Reserve new nodes for genuinely new operations, not for shapes you can compose.
