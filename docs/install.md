---
title: Install
status: draft
---

# Install

Ymir is a native Linux application that you build from source. There are no binary releases yet.

## Requirements

- Linux, on Wayland or X11.
- A Vulkan-capable GPU with working drivers. The 3D viewport runs on Vulkan through wgpu, and the editor will not start without it.
- [git](https://git-scm.com/), to fetch the source.
- [rustup](https://rustup.rs), which installs the Rust toolchain. rustup reads the exact compiler version the project pins and fetches it for you, so building Ymir does not call for knowing Rust.

## Install the Rust toolchain

Follow the one-line instructions at [rustup.rs](https://rustup.rs) and accept the defaults. When it finishes, open a new terminal so the `cargo` command is on your path.

## Get the source

```bash
git clone https://github.com/liminalfield/ymir
cd ymir
```

## Build

```bash
cargo build --release
```

The first build compiles the whole dependency tree, including the wgpu and egui graphics stack, so it takes several minutes. Later builds recompile only what changed and are much faster.

## Run

Start the node editor:

```bash
cargo run -p ymir-gui --release
```

The headless command-line renderer produces a sample terrain (fBm noise through thermal erosion) and writes `out/heightmap.png`:

```bash
cargo run -p ymir-cli
```

## If the build fails

The system packages that the Wayland and X11 backends need vary between distributions. If the build stops on a missing library, [open an issue](https://github.com/liminalfield/ymir/issues) with the error, your distribution, and your Vulkan driver.
