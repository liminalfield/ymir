---
title: Keyboard and mouse
status: draft
---

# Keyboard and mouse

Every modifier is Ctrl. Ymir is a Linux application and uses Ctrl throughout, never a Cmd key.

## Canvas

The node graph.

| Action | Input |
|---|---|
| Add a node | Right-click the canvas, press Space, or drag a connection into empty space |
| Move a node | Drag it |
| Connect two nodes | Drag from an output to an input |
| Select a node | Click it |
| Select all | Ctrl + A |
| Delete the selection | Delete or Backspace |
| Undo | Ctrl + Z |
| Redo | Ctrl + Shift + Z, or Ctrl + Y |
| Pan | Drag empty space |
| Zoom | Scroll |

## Parameters

In the inspector, for the selected node.

| Action | Input |
|---|---|
| Scrub a value | Drag the value box |
| Type a value | Click the value box, then type |
| Step an integer by one | The − and + buttons beside it |
| Reset a number to its default | The revert arrow, which appears once the value differs from it |

A scrub keeps going while you drag, so the pointer reaching the edge of the screen does not end it. Values stay within the range the node declares, so a scrub or a typed number stops at the limit. A direction is the exception: it rolls, so scrubbing past 360° carries on from 0°.

## Nodes pane

The left dock's list of every node in the graph, in dependency order.

| Action | Input |
|---|---|
| Select a node | Click its row |
| Include or exclude an output from the next build | Click its `build` chip |
| Narrow the list | Type in the filter, or click a `stale`, `failed` or `endpoints` chip |
| Clear the filter | Clear, beside the count |

Each row states what its node is doing as a stripe, a glyph and a word, so no state is told apart by colour alone. Every node on the canvas carries the same state as a dot on its header, so the graph reads at a glance and the pane spells out whichever row you are reading.

The list is in dependency order unless you change the sort, and the sort you choose is saved with the project. A filter is not: opening a project always shows the whole graph.

## 3D viewport

The viewport has two navigation modes.

| Action | Input |
|---|---|
| Orbit (tumble) | Alt + left-drag |
| Track (pan) | Alt + middle-drag |
| Dolly (zoom) | Alt + right-drag, or scroll |
| Fly | Hold the right button; the mouse looks around |
| Move while flying | W, A, S, D, with E up and Q down |
| Fly faster | Hold Shift while flying |

## 2D preview

| Action | Input |
|---|---|
| Pan | Drag |
| Zoom | Scroll |

## Painting

For the Sculpt and Paint nodes, with the paint tool active.

| Action | Input |
|---|---|
| Paint | Drag on the 2D map or the 3D surface |
| Brush size | Ctrl + scroll |
| Brush hardness | Shift + scroll |
| Invert the stroke | Hold Ctrl while painting (Raise becomes Lower, Paint becomes Erase) |
