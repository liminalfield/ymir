# <img src="ymir-icon-512.png" alt="" height="30" align="middle"> Ymir

Ymir is an open-source, node-based procedural terrain generator for Linux. Everything
in it is a layered field, and every node is something that transforms one. You compose
terrain by wiring small, single-purpose nodes into a graph, where each node reads the
fields coming into it and passes on what it has changed.

It is named for the primordial giant of Norse myth, whose body the world is shaped from.

<!-- Replace with a real screenshot before launch (see docs/images/). -->
![The Ymir node editor and 3D viewport](docs/images/ymir-editor.png)

## Status

Ymir is in early development and is already usable. It has a working node editor, a 3D
terrain viewport, 32 nodes covering noise, shapes, selectors, filters, and three erosion
models, along with subgraphs and export to 16-bit PNG, raw R16, and 32-bit EXR. The
internals are still changing and there are rough edges, so feedback and issues are
welcome.

This is a personal, non-commercial project, held to a high bar: the architecture and the
code should stand up to scrutiny from experienced Rust developers.

## What is inside

A single `Field` type flows on every edge of the graph. A field is a grid of named
scalar layers (`height`, `mask`, `flow`, `water`, `sediment`, and any others a node
cares to create) together with a few scalar globals. Because the engine never needs to
know what a node does with those layers, nodes are insertable anywhere and the graph
imposes no fixed build order.

The node set favours many small operators over a few configurable ones, so a graph's
structure is visible in its wiring. There are generators (fBm, ridged, billow, hybrid,
flow, cellular, and shape primitives), selectors that read height, slope, and curvature,
shapers for curve, levels, invert, blend, warp, and blur, and three erosion models:
thermal, hydraulic, and stream. The full list, with what each node does, is in
[`design/node-inventory.md`](design/node-inventory.md).

The erosion models write out their byproducts as layers rather than discarding them.
`flow`, `water`, `wear`, and `deposition` all come back on the field, where downstream
nodes and a future texturing stage can consume them.

Results are reproducible. The same seed and the same graph produce the same terrain on
the same machine, every time, which content-hash memoization and a pinned toolchain
between them make possible.

## Building and running

Ymir is a native Linux application. You will need a Rust toolchain via
[rustup](https://rustup.rs), which fetches the pinned compiler version recorded in
`rust-toolchain.toml` automatically, and a Vulkan-capable GPU with working drivers for
the 3D viewport, since the GUI is built on wgpu. The editor targets both Wayland and
X11.

A release build of the whole workspace is the usual starting point:

```bash
cargo build --release
```

The node editor is the `ymir-gui` binary:

```bash
cargo run -p ymir-gui --release
```

The CLI renders a sample terrain headlessly, running fBm through thermal erosion into a
PNG export and writing the result to `out/heightmap.png`:

```bash
cargo run -p ymir-cli
```

If the build fails on your distribution, please open an issue with the error and the
distro you are on. The exact system packages needed for the Wayland and X11 backends
vary between them.

## Documentation

[`ARCHITECTURE.md`](ARCHITECTURE.md) explains how the engine and the editor fit
together, and [`design/`](design/) holds the design notes behind the data
model, the node taxonomy, erosion, and subgraphs. For the Expression node there is a
set of worked recipes in
[`design/expression-cookbook.md`](design/expression-cookbook.md). [`CLAUDE.md`](CLAUDE.md)
records the working brief and the quality bar the project is held to.

## Contributing

Contributions are welcome. [`CONTRIBUTING.md`](CONTRIBUTING.md) covers how to build,
test, and run the quality gates, and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) sets
out community expectations. The short version is that every change leaves the tree
compiling, tested, and clippy and fmt clean, and that a fix addresses the cause of a
problem at its source.

## License

Ymir is licensed under the GNU General Public License v3.0 only (GPL-3.0-only); see
[`LICENSE`](LICENSE). The bundled IBM Plex fonts are licensed separately under the SIL
Open Font License 1.1, recorded in
[`crates/ymir-gui/assets/fonts/OFL.txt`](crates/ymir-gui/assets/fonts/OFL.txt), and the
vendored `egui-snarl` under `vendor/` is MIT OR Apache-2.0.
