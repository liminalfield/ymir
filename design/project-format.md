# Project file format

How a Ymir graph is saved and loaded. This is the contributor-facing companion to
the `project` module in `ymir-core`.

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

## Versioning and migration

`format_version` starts at 1. A loader rejects a version it does not understand rather
than guessing. When a breaking schema change lands, bump the version and add a migration
that recognizes the older shape and upgrades it before rebuild; the version check in
`from_document` is the seam where that hook goes. Saved projects are something to
preserve, not to silently break.

## What is not stored (and where it will live)

Canvas positions, pan/zoom, and the preview pin are GUI view-state, not engine truth, so
they are absent from this document and `ymir-core` stays headless. The GUI save layer
(issue #75) wraps this document in a single self-contained file with two sections:

```json
{ "graph": { ...this document... }, "view": { "nodes": { "0": [x, y] }, "pan": [...], "zoom": 1.0 } }
```

The `view` section is keyed by `stable_id` and kept last so layout-only edits localize
their diff. The headless CLI reads only `graph`; a graph-only file (no `view`, such as
one the CLI wrote or a fragment imported from a git repo) opens in the GUI and
auto-lays-out.

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
