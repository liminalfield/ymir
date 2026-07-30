---
title: Keyboard and mouse
status: draft
---

# Keyboard and mouse

Every modifier is Ctrl, on both Linux and Windows. There is no Cmd key anywhere.

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
| Change a value | Press and hold the value box, then drag |
| Type a value | Click the value box, then type |
| Abandon a change and put the value back | Escape, while still holding |
| Step an integer by one | The − and + buttons beside it |
| Reset a number to its default | The revert arrow, which appears once the value differs from it |

Holding the pointer down on a value box opens a ruler above it, with a column per magnitude: 1000, 100, 10, 1, 0.1, 0.01, 0.001. A click is shorter than that hold, so clicking into a box to type its value does not bring the ruler up. The column under the pointer is the one you are changing, and moving sideways picks another. Columns the value cannot reach are faded: an integer has nothing below the 1s, and a parameter that tops out at 64 has nothing above the 10s.

Choosing a column and changing the value are the same gesture, so they happen in that order. Nothing moves while you are still crossing the ruler; a vertical stroke starts the change, and the step reads bright once it does. Drag up to raise the value and down to lower it, one step per short pull, for as long as you hold. Moving sideways again goes back to choosing.

Above and below the number sit the value one step up and one step down, so you can see where a pull lands before it does.

Values stay within the range the node declares, so a change or a typed number stops at the limit. A direction is the exception: it rolls, so passing 360° carries on from 0°.

## Nodes pane

The left dock's list of every node in the graph, in dependency order.

| Action | Input |
|---|---|
| Select a node | Click its row |
| Include or exclude an output from the next build | Click its `build` chip |
| Narrow the list | Type in the filter, or click a `stale`, `failed` or `endpoints` chip |
| Clear the filter | Clear, beside the count |
| Show or hide what is inside a subgraph | Click the caret beneath its row |

Each row states what its node is doing as a stripe, a glyph and a word, so no state is told apart by colour alone. Every node on the canvas carries the same state as a dot on its header, so the graph reads at a glance and the pane spells out whichever row you are reading.

The list is in dependency order unless you change the sort, and the sort you choose is saved with the project. A filter is not: opening a project always shows the whole graph.

A subgraph's row reports the state of the subgraph as a whole, since that is what Ymir evaluates: the nodes inside it run together. Opening it lists what is inside and flags anything visibly wrong there, such as an unconnected input. To see the state of an individual node inside, dive into the subgraph, where it is an ordinary node.

While a build runs, the window title says so, so you can tell from the taskbar without switching to Ymir. When it finishes, the status beside the Build button states what happened and how long it took, and quietens after a few seconds; a failure stays until the next build.

While a build runs, each row it touches reports what the build is doing with that node: queued, how far along it is, how long it took, or that a cached result was reused. Nodes report a percentage only where they can measure one; the rest show how long they have been running.

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
| Reset to fit | Double-click |

## Exploring the field

The noise generators build from a pattern that carries on past your world, and only part of it is under the terrain. Select one and its inspector offers Explore field, which shows the surrounding pattern with your world outlined on it, so you can choose which part of it the terrain sits on.

| Action | Input |
|---|---|
| Start or stop | The Explore field button in the node's inspector |
| See more or less of the pattern | Scroll |
| Move your world | Drag inside the outline |
| Resize your world | Drag a corner handle |
| Abandon a move or resize | Escape, while still holding |

The outline is your world. Moving it changes where the generator samples the pattern; resizing it changes the world extent, and the size it will become is shown above the outline while you drag. Neither takes effect until you release, so the pattern behind the outline holds still while you aim.

Resizing keeps the surrounding pattern the same size on screen, so only the outline changes. The terrain keeps its scale either way: a 500 m hill stays 500 m, and a larger world simply holds more of them.

While exploring, the preview stays on the generator even if you select another node, so a stray click does not lose the framing you are working on. Stop exploring to return the preview to following the selection.

## Painting

For the Sculpt and Paint nodes, with the paint tool active.

| Action | Input |
|---|---|
| Paint | Drag on the 2D map or the 3D surface |
| Brush size | Ctrl + scroll |
| Brush hardness | Shift + scroll |
| Invert the stroke | Hold Ctrl while painting (Raise becomes Lower, Paint becomes Erase) |
