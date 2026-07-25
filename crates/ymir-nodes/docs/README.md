# Node prose fragments

One file per node, named by its `type_id` (`modifier.warp.md`). Each holds the hand-written prose
that `cargo xtask docs-gen` merges into the node's generated reference page under
`docs/reference/nodes/`: the Purpose, Behaviour, Recipes, and See also sections. The mechanical
sections (metadata, inputs, outputs, the parameter table, the layer contract) come from the node's
`NodeSpec` and cannot be edited here.

Adding a node touches only its own fragment. A node with no fragment still gets a page, with an
empty Purpose flagged for writing.

## Format

```
---
status: draft                # draft | stable
resolution_dependent: true   # optional; flags an iterative simulation in the page header
---

## Purpose

One or two sentences: what the node is for and when a reader would reach for it. Required for a
stable page.

## Behaviour

Optional. Failure modes and surprises.

## Recipes

Optional. Two or three short pointers to how the node is combined in practice.

## See also

Optional. Up to three curated links.
```

Only those four headings are allowed; any other is an error. `DOCS.md` at the repository root is the
authoritative writing guide. `modifier.warp.md` and `modifier.thermal_erosion.md` are worked
examples.
