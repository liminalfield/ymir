---
title: Install
status: draft
---

# Install

Ymir runs on Linux and Windows. Download a released binary, or build it from source.

## Download a binary

Take the newest from the [releases page](https://github.com/liminalfield/ymir/releases).

### Linux

Each release attaches a `ymir-gui` binary for x86_64, alongside the headless `ymir-cli` and a
`SHA256SUMS` file covering every asset. Make it executable and run it:

```bash
chmod +x ymir-gui-linux-x86_64
./ymir-gui-linux-x86_64
```

### Windows

Each release attaches `ymir-windows-x86_64.zip`, holding `ymir-gui.exe`, `ymir-cli.exe`, and a
readme. Unzip it anywhere and run `ymir-gui.exe`.

The binaries are unsigned, so Windows SmartScreen warns the first time you run one. Choose **More
info** and then **Run anyway**. The warning says that the file carries no code-signing
certificate, which is true; it is not a report of anything found in the file. Signing costs a
recurring annual fee, and Ymir does not pay it yet.

Ymir keeps its settings in `%APPDATA%\Ymir` and its build cache in `%LOCALAPPDATA%\Ymir`.
Deleting either is safe: the cache only costs rebuild time.

The requirements below still apply. A released binary does not remove the need for working GPU
drivers.

Build from source instead if you are on another architecture, want to follow `main`, or intend to
change something.

## Requirements

- Linux on Wayland or X11, or Windows 10 or later, on x86_64.
- A GPU with working drivers: Vulkan on Linux, and Vulkan or DX12 on Windows. The 3D viewport
  runs through wgpu, and the editor will not start without one.
- [git](https://git-scm.com/), to fetch the source.
- [rustup](https://rustup.rs), which installs the Rust toolchain. rustup reads the exact compiler
  version the project pins and fetches it for you, so building Ymir does not call for knowing
  Rust.

On Windows, building from source also needs the Visual Studio build tools for the MSVC linker.
The rustup installer offers to fetch them.

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

The headless runner builds a saved project. The seed, world size, and build resolution come from
the project file, so the result matches what the editor shows:

```bash
cargo run -p ymir-cli --release -- render examples/terraced_beach.ymir --out beach.png
```

See the [command line reference](reference/cli.md) for the rest of what it does.

## If the build fails

On Linux, the system packages that the Wayland and X11 backends need vary between distributions.
If the build stops on a missing library, [open an
issue](https://github.com/liminalfield/ymir/issues) with the error, your distribution, and your
Vulkan driver.

On Windows, a build that stops at the link step usually means the Visual Studio build tools are
missing. Install them and open a new terminal.
