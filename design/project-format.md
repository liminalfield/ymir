> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Project file format

How a Ymir project is saved and loaded. This is the contributor-facing companion to
the `project` module in `ymir-core`.

There are two nested things, each with its own version:

- The **project file** (`ProjectFile`, `PROJECT_FILE_VERSION`), the whole `.ymir` file: the
  world settings, the graph, and an editor's view state.
- The **graph document** (`ProjectDocument`, `FORMAT_VERSION`) nested inside it: the nodes,
  their parameters, and their wiring.

They version separately because they change for different reasons. Adding a node parameter
touches the graph schema; moving a setting between the world and the view touches only the
envelope.

## Goals

- **Deterministic.** Saving the same graph twice produces byte-identical output, and a
  loaded project evaluates byte-identically to the one that was saved. The format never
  carries anything that would change evaluation.
- **Git-friendly.** Files are pretty-printed JSON with a stable element order, so a
  project diffs cleanly and node networks are practical to share and review in version
  control.
- **Stable.** The schema is decoupled from the runtime types and carries a format
  version, so the engine can evolve without orphaning saved projects.

## What is stored

A project document mirrors only the persistent state of a `Graph`. It deliberately does
not store the live operators, and it never stores the runtime `NodeId` (a generational
slotmap key that changes across runs).

Per node:

- `stable_id` — the node's persistent identity, the only node identity serialized. The
  per-node seed derives from it, which is why a reload reproduces identical output.
- `type_id` — the registered operator id. On load the operator is rebuilt through the
  registry, so the format never names a concrete node type and adding a node touches no
  central list.
- `name` — the optional display-name override. Omitted when unset.
- `params` — the instance's parameters, a name-keyed map (sorted). Omitted when empty,
  so a node left on its defaults writes no `params` at all. Values are self-typed
  (`float`, `int`, `bool`, `text`, `curve`); a `curve` is its list of `[x, y]` control
  points and is re-sanitized through `Curve::new` on load.
- `connections` — the node's input wiring, one entry per connected input port, sorted by
  port. Each names its source by the source node's `stable_id` (never a `NodeId`).
  Omitted when the node has no inputs wired.

At the top level the document carries `format_version`, `next_stable_id` (so ids
assigned after a load cannot collide with loaded ones), and the nodes in ascending
`stable_id` order.

### Example

```json
{
  "format_version": 1,
  "next_stable_id": 3,
  "nodes": [
    { "stable_id": 0, "type_id": "generator.fbm" },
    {
      "stable_id": 1,
      "type_id": "modifier.thermal_erosion",
      "connections": [ { "input": 0, "source": 0, "output": 0 } ]
    },
    {
      "stable_id": 2,
      "type_id": "endpoint.export",
      "params": { "path": { "text": "out/heightmap.png" } },
      "connections": [ { "input": 0, "source": 1, "output": 0 } ]
    }
  ]
}
```

## API

`Graph::to_document` / `Graph::from_document` convert between a graph and the
serializable `ProjectDocument`. `Graph::save` / `Graph::load` (and the
`save_to_writer` / `load_from_reader` primitives) handle the JSON file layer. Loading
reports a typed error for each failure mode: an unsupported format version, an unknown
node type, a duplicate stable id, a dangling connection, or malformed JSON.

## The envelope

A `.ymir` file is a `ProjectFile`: the world the graph builds, the graph itself, and whatever
an editor wants to remember about showing it.

```json
{
  "format_version": 2,
  "world": {
    "seed": 0,
    "world_extent": 1024.0,
    "world_height": 256.0,
    "sea_level": 0.3,
    "build_res": 4096
  },
  "graph": { "...this document..." },
  "view": {
    "settings": { "preview_res": 1024, "show_water": true, "water": { "...": null } },
    "canvas": { "nodes": { "0": [40.0, 40.0] }, "camera": { "...": null }, "frames": [] }
  }
}
```

**`world` is what the graph builds.** Every field in it reaches an operator: the first four
through `EvalRequest` into each node's `EvalContext`, and `build_res` as the request's grid size.
That is the test for belonging there. The consequence is what matters: a project's terrain is
reproducible from `world` plus `graph`, with nothing else needed, so a headless render produces
what the editor shows rather than the same node network under invented settings.

**`view` is how an editor shows it.** Node positions, the canvas camera, frames, the preview
resolution, the water rendering look, the node pane's ordering. None of it reaches evaluation.

The `view` section is a type parameter on `ProjectFile<V>` rather than opaque JSON. An editor
keeps its own typed state, comparable and cheap to clone; anything headless writes
`ProjectFile` (defaulting to `serde_json::Value`) and ignores it. The typing is not incidental:
the GUI uses a `ProjectFile` as its per-settled-frame undo snapshot and compares snapshots for
equality, and an opaque blob would push a JSON serialization into that comparison.

Inside `view`, only `canvas` nests. A subgraph has its own node positions and camera, so
`canvas.subgraphs` recurses; it has no preview resolution or pane ordering of its own, so
`settings` sits above the recursion. Writing settings into every subgraph would be both noise in
the diff and a claim that is not true.

A graph-only file is valid: `view` defaults, so a file written by a headless tool, or a fragment
shared without layout, opens with nodes cascaded onto the canvas.

## Versioning and migration

Both versions start at 1 and a loader rejects a version it does not understand rather than
guessing. `ProjectFile::check_version` is the explicit check on the envelope, so an older project
reports what it is instead of surfacing as a field-level parse error. `Graph::from_document` is
the equivalent seam for the graph document, and where a migration hook goes.

The envelope is at version 2. Version 1 kept the world settings in the GUI's own envelope, where
nothing headless could reach them; the file said what the node network was but not what world it
described, so a CLI render could only invent a seed and a resolution. That is not a shape a
migration can rescue meaningfully, and Ymir was at 0.2 with no external users, so version 1 is a
documented break rather than a migration: it is rejected on load with
`Error::UnsupportedFormatVersion`.

That is the exception, not the pattern. Saved projects are something to preserve, and a break
needs both a reason the old shape cannot express and a version young enough to afford it.

## Default startup graph

The GUI opens a fresh session with a built-in starter chain (a generator feeding
erosion feeding an export endpoint) rather than a blank canvas. A user can override it
with their own: "File > Save as Default Startup Graph" writes the current session, in
the same envelope format above, to `$XDG_CONFIG_HOME/ymir/default.ymir` (falling back to
`$HOME/.config/ymir/default.ymir`). On launch that file, if present, opens in place of
the built-in starter. It is loaded as a template, not bound as the session's save
target, so the first `Save` still prompts for a location and does not overwrite the
default. A missing default is the normal first-run case; a corrupt one is reported and
the built-in starter stands.
