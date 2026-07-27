---
title: Command line
status: draft
---

# Command line

`ymir-cli` is the headless runner. Run it with `cargo run -p ymir-cli`, or from the built binary at `target/release/ymir-cli` after `cargo build --release`.

On Windows it is `ymir-cli.exe`, either from that same build or from the `ymir-windows-x86_64.zip` attached to a [release](https://github.com/liminalfield/ymir/releases). Everything below applies unchanged; the examples write `\` paths where a Linux one would write `/`.

## render

`ymir-cli render PROJECT` builds a saved project.

```
ymir-cli render coast.ymir --out height.png
```

The seed, world extent, world height, sea level, and build resolution all come from the project file, so a render reproduces what the editor shows rather than the same node network under different settings. Erosion runs on the GPU when a device is reachable, and falls back to the CPU otherwise.

What gets built is the project's result nodes: the ones whose output nothing else reads. That is what Build means in the editor too.

### Where the output goes

A project that ends in export nodes needs no `--out`. Each export node writes the path it carries, and an export switched off for builds stays off here.

```
ymir-cli render coast.ymir
built 2 outputs at 4096x4096
```

A project that ends in an ordinary node has no path of its own, which is the usual shape for a project saved while working in the editor. Name a file with `--out`, and the format follows its extension: `.png` and `.r16` are 16-bit, `.exr` is 32-bit float. See [Export formats](export-formats.md).

Giving `--out` to a project whose export nodes already name their paths is an error rather than a silent override.

### Options

`--res N` builds at N cells square instead of the project's own build resolution. Erosion is resolution-dependent physics, so a smaller render is an approximation of the full build, not the same terrain scaled down.

`--node NAME` builds one node instead of every result node. It takes the node's name from the editor, matched without regard to case, or its stable id. A name shared by several nodes is an error listing their ids.

`--out FILE` writes the result to `FILE`.

A render that cannot do what was asked exits non-zero and writes nothing.

## Render the sample

With no arguments, the runner builds a sample graph (fBm noise through thermal erosion into a PNG export), saves the project to `out/project.json`, reloads it, and writes `out/heightmap.png` from the reloaded project. It exercises the full save-and-reload path end to end.

## docs

`ymir-cli docs --format json` prints the node reference as JSON: every registered node with its ports, parameters, defaults, layer contract, and resolved display strings. It is the input the documentation generator consumes, so the reference always matches the running build.

## Version

`ymir-cli --version` (or `-V`) prints the build-stamped version and exits.

## Help

`ymir-cli --help` (or `-h`) prints the usage: `render` and its options, what the runner does with
no arguments, the `docs` command, and these two flags.

An argument the runner does not recognise is an error. It prints the usage to standard error and
exits with status 2, so a mistyped command cannot be mistaken for a request to render.
