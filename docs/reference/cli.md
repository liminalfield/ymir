---
title: Command line
status: draft
---

# Command line

`ymir-cli` is the headless runner. Run it with `cargo run -p ymir-cli`, or from the built binary at `target/release/ymir-cli` after `cargo build --release`.

## Render the sample

With no arguments, the runner builds a sample graph (fBm noise through thermal erosion into a PNG export), saves the project to `out/project.json`, reloads it, and writes `out/heightmap.png` from the reloaded project. It exercises the full save-and-reload path end to end.

## docs

`ymir-cli docs --format json` prints the node reference as JSON: every registered node with its ports, parameters, defaults, layer contract, and resolved display strings. It is the input the documentation generator consumes, so the reference always matches the running build.

## Version

`ymir-cli --version` (or `-V`) prints the build-stamped version and exits.
