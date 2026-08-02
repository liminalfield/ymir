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
| Compute it instead | Press `=` in the value box |
| Abandon a change and put the value back | Escape, while still holding |
| Step an integer by one | The − and + buttons beside it |
| Reset a number to its default | The revert arrow, which appears once the value differs from it |

Holding the pointer down on a value box opens a ruler above it, with a column per magnitude: 1000, 100, 10, 1, 0.1, 0.01, 0.001. A click is shorter than that hold, so clicking into a box to type its value does not bring the ruler up. The column under the pointer is the one you are changing, and moving sideways picks another. Columns the value cannot reach are faded: an integer has nothing below the 1s, and a parameter that tops out at 64 has nothing above the 10s.

Choosing a column and changing the value are the same gesture, so they happen in that order. Nothing moves while you are still crossing the ruler; a vertical stroke starts the change, and the step reads bright once it does. Drag up to raise the value and down to lower it, one step per short pull, for as long as you hold. Moving sideways again goes back to choosing.

Above and below the number sit the value one step up and one step down, so you can see where a pull lands before it does.

Every number is a value box. There is no slider: a slider cannot show a unit, cannot reach a value finer than one pixel, and lets its range decide its precision. The box takes an exact number, and the ruler covers the range.

Values stay within the range the node declares, so a change or a typed number stops at the limit. A direction is the exception: it rolls, so passing 360° carries on from 0°.

### Computing a value instead of typing one

A number can be worked out rather than entered. Press `=` in a value box to open an expression field, write one, and the parameter is computed from then on.

| Action | Input |
|---|---|
| Compute a value | Click the value box and press `=` |
| Edit an expression | Its own field, on the line beneath the label |
| See what it works out to | The number beside the `=` |
| Go back to a plain number | Type the number on its own, then Enter |
| Go back to the default | Clear the field, or the revert arrow |
| Abandon an edit | Escape, before you leave the field |

An expression can read the world settings, `sea_level`, `world_height` and `world_extent`, and any other numeric parameter on the same node by its name. It cannot read another node. Anything a subgraph needs from outside is wired in or declared on it, so that a subgraph does not stop working when it is used somewhere else.

They are not all in the same units, and the rule is the one Ymir uses throughout: heights are normalized, horizontal lengths are metres.

| Name | Unit |
|---|---|
| `sea_level` | A height in `0` to `1`, the same as any height parameter |
| `world_height` | Metres: the elevation a height of `1` represents |
| `world_extent` | Metres across the map |

So `sea_level * world_height` is the sea in metres, and `sea_level + 0.05` is a little above the water. `sea_level + 2` is two whole world heights up, which is almost certainly not what you meant.

The row shows the number it currently computes, with `=` beside it. `=!` means the expression does not resolve, and hovering says why; the node cannot run until it does. Parameters that reference each other in a loop are reported the same way rather than hanging.

Pressing `=` opens the field straight away and puts the caret in it, so asking for an expression and writing one are a single gesture. It opens empty and stores nothing: the parameter keeps the value it had until you commit something, so opening the field and changing your mind costs nothing.

A computed parameter gets its own full-width line, since an expression does not fit in a value box.

While the field has focus it says what you have typed so far works out to, or why it does not: an unknown name, a syntax error, or a loop of parameters referencing each other. The message is the one the node would report, so what you see while typing is what you will get.

Nothing commits until you press Enter or leave the field, so a half-finished expression is never applied and the preview does not rebuild on every keystroke.

### Declaring a subgraph's own parameters

Select a subgraph and its inspector ends with an Interface list: the parameters that node exposes. Add one with `+`, and anything inside the subgraph can read it by name in an expression.

| Action | Input |
|---|---|
| Declare a parameter | `+` beside Interface |
| Rename it | Its name field |
| Change its type | The dropdown beside the name |
| Declare it a length in metres | The `m` box |
| Remove it | `−` at the end of its row |

The name is what an expression inside writes, so it is an identifier: letters, digits and `_`, not starting with a digit. A name that could not be written in an expression, or one already taken, is reported under the row.

The list belongs to the node itself, not to the copy you are looking at, so a parameter added here appears on every instance. The values are per instance and are set in the rows above, like any other node's.

Renaming or removing a parameter does not rewrite the expressions inside that read it. Those report the old name as unknown until you edit them.

### The Levels editor

Levels draws its five parameters as one picture, with the incoming distribution along the bottom and the transfer curve across it. The window bounds are draggable on the axis each one acts along.

| Action | Input |
|---|---|
| Move an input bound | Drag its marker along the bottom edge |
| Move an output bound | Drag its marker along the left edge |
| Set a bound exactly | Its row beneath the picture, as any other value |

The horizontal axis is the incoming data's own range, and the vertical one is the `[0, 1]` a height works in. Neither moves when you drag a bound, so the distribution you are aiming at stays where it is and a handle goes where you put it.

Dragging reaches as far as the axis. To put a bound beyond the data, or an output bound outside `[0, 1]`, type it in its row. A bound already out there parks its marker at that edge, drawn hollow, since the axis does not stretch to reach it.

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
| Move your world | Drag the pattern, anywhere except a corner handle |
| Resize your world | Drag a corner handle |
| Abandon a resize | Escape, while still holding |

The outline is your world, and it stays in the middle of the view. You do not move it: you drag the pattern underneath it, the way you would drag a map under a viewfinder, until the part you want is inside. That is also why a drag through the middle of the outline does the same thing as one outside it.

Resizing the outline changes the world extent, and the size it will become is shown above it while you drag. Nothing is written until you release, so the pattern holds still while you aim.

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
