> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Windows support

Ymir has been Linux-only since it started, and says so on the README, the install page, and
every release. Most people who would try a terrain tool are on Windows, so the platform is a
distribution problem before it is a technical one.

## What the port actually costs

Less than the framing suggests. Measured on 2026-07-27 at v0.3.0:

```
rustup target add x86_64-pc-windows-msvc
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc
```

compiles clean, with no errors and no warnings, tests included. Nothing in the tree names a
Unix API: there is no `std::os::unix`, no `cfg(unix)`, and no `cfg(target_os)` anywhere. The
dependency choices already went the right way, mostly deliberately:

- **rfd** for file dialogs uses Win32 on Windows, Cocoa on macOS, and the XDG portal on Linux.
  Its Linux-only portal dependencies are target-gated by the crate and never compile elsewhere.
- **eframe/wgpu** is cross-platform, and needs no backend selection on our side. wgpu registers
  backends in the order Vulkan, Metal, DX12, GL, and `request_adapter` sorts only by device
  type with a stable sort, so among adapters of one type the enumeration order survives:
  **Vulkan wins on Windows whenever a Vulkan driver is present**, which NVIDIA, AMD and modern
  Intel all ship. The usual Windows machine therefore runs the same backend, and the same
  SPIR-V, as Linux.
- **Fonts and the window icon** are `include_bytes!`, so there is no system font or theme
  lookup to differ.
- The **eframe `x11` and `wayland` features** are enabled unconditionally in the workspace
  manifest. They forward to winit features whose dependencies are target-gated, which is why
  the Windows check passes with them on. They need no change.

So this is not a port. It is five paths, some build hygiene, CI and release plumbing, and one
genuine unknown.

## The real work: where files live

Five things resolve their location from `XDG_CONFIG_HOME` / `XDG_DATA_HOME` /
`XDG_CACHE_HOME`, falling back to `$HOME`. Windows sets none of them, so each returns `None`
and its feature silently does nothing:

| Path | What stops working |
| --- | --- |
| `default_project_path` (`main.rs`) | No default startup graph; Save as Default does nothing |
| `preferences_path` (`preferences.rs`) | Preferences never persist |
| the recent-files path (`main.rs`) | The recent list never persists |
| `library_root` (`library.rs`) | The subgraph library cannot save or load |
| `FieldStore` cache dir (`field_store.rs`) | No disk field cache, so builds lose result reuse |

Silence is the problem. Nothing errors, nothing logs, and a Windows user would find an editor
that forgets everything between sessions without being told why.

### Approach

One module, `ymir_core::app_dirs`, with three functions: `config_dir`, `data_dir`, `cache_dir`.
It lives in core because `FieldStore` is in core and the GUI needs the other two, and because
"where does this platform put application files" is mechanism, not terrain semantics.

Per platform:

| | Linux | Windows | macOS |
| --- | --- | --- | --- |
| config | `$XDG_CONFIG_HOME`, else `~/.config` | `%APPDATA%` | `~/Library/Application Support` |
| data | `$XDG_DATA_HOME`, else `~/.local/share` | `%APPDATA%` | `~/Library/Application Support` |
| cache | `$XDG_CACHE_HOME`, else `~/.cache` | `%LOCALAPPDATA%` | `~/Library/Caches` |

Windows splits roaming from local deliberately: preferences and the subgraph library are the
user's own content and belong in the roaming profile, while the field cache is a rebuildable
machine-local artifact and belongs in Local.

The existing helpers are pure functions taking the environment as arguments, with unit tests
that never touch the process environment. Keep that shape. It means each platform's precedence
is tested from any host, so the Linux CI machine proves the Windows rules too. That property is
worth more than the small duplication it costs, and it is why this does not need a
`dirs`-family crate: the dependency would replace tested code with untested code and add a
transitive tree for one lookup.

`cfg(target_os)` appears here and nowhere else. One module knows about platforms; nothing else
does.

## A gap that can be closed now

`ymir-gui/src/wgsl.rs` statically validates the viewport shaders with naga (#272), turning a
malformed edit into a test failure instead of a broken viewport at run time. The compute
kernels, `thermal.wgsl` and `scalar_multiply.wgsl`, are covered by nothing.

naga translates WGSL to HLSL with no Windows machine and no DX12 runtime involved, so a test on
a Linux host can prove both compute kernels are expressible in HLSL at all. That is the one
piece of DX12 risk that can be retired before anyone touches a Windows machine.

## Build hygiene

- **`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` on `ymir-gui`.**
  Without it a released GUI opens a console window behind itself. Gated on release so a debug
  build keeps its console and `println!` diagnostics stay visible. It must not go on
  `ymir-cli`, which is a console program.
- **`.gitattributes` needs a line-ending policy.** The repo has no `text=auto`, so a Windows
  contributor with `core.autocrlf=true` would rewrite every file it touches. Project files
  (`*.ymir`, `*.ymirsub`) should be pinned to LF specifically: they are JSON written with `\n`,
  and the whole point of the format is that it diffs cleanly, which a platform-dependent line
  ending would undo.
- The dev tooling (`scripts/check-shortcuts.sh`, `.githooks/pre-commit`) is bash. Git for
  Windows ships Git Bash, so a Windows contributor can run both. Not worth rewriting.

## The unknown: it has never been run

Everything above is verifiable without a Windows machine. This is not. Compiling is not
running, and these are unexamined:

- **The GPU path, when it falls to DX12.** Vulkan is the likely backend (above), so the DX12
  path only appears where Vulkan enumerates nothing: an older Intel iGPU, a stripped OEM driver
  install, a VM, or an explicit `WGPU_BACKEND=dx12`. There the risk is not DX12 but its shader
  compiler. wgpu 29 defaults to `Dx12Compiler::Auto`, which tries static DXC (feature off),
  then `dxcompiler.dll` on `PATH` (not shipped), then falls back to **FXC**, which wgpu's own
  documentation calls old, slow and unmaintained.

  Three things keep this small. `thermal.wgsl` is a pure gather with no atomics, barriers,
  textures, subgroup operations or matrices, so there is little for a weak HLSL backend to get
  wrong. A GPU failure already degrades to the CPU reference with a logged warning
  (`thermal.rs`), so the worst realistic outcome is a slow build rather than wrong terrain.
  And the CPU-versus-GPU agreement is guarded at `1e-4` per cell over 40 passes.

  That guard skips when no adapter is present, so whether it runs on Windows CI depends on the
  Microsoft Basic Render Driver (WARP), a software DX12 adapter usually available on
  `windows-latest`. If it enumerates, CI exercises the DX12 and FXC path for free, which is the
  best guard available; worth establishing rather than assuming.
- **File dialogs**, through rfd's Win32 backend rather than the XDG portal.
- **HiDPI scaling**, which Windows reports differently from Wayland.
- **The log file and the field store**, where Windows refuses to delete or overwrite a file
  another handle has open. Linux allows it, so any place that relies on that will only fail
  here.
- **Long paths.** Windows caps a path at 260 characters unless long-path support is on. The
  field store nests hashed directory names under the cache dir, which is the one place likely
  to get near it.

This is why the first-run pass is its own step and comes before packaging: there is no point
publishing a Windows archive before anyone has watched the editor open on Windows.

## Distribution

A `.zip` per release containing the GUI, the CLI, and a short readme, alongside the existing
Linux binaries and `SHA256SUMS`. A zip rather than a bare `.exe` because browsers and Windows
both treat a directly downloaded executable more suspiciously than an archive.

The binaries will be unsigned, so SmartScreen warns on first run. A code-signing certificate
costs money annually and is not worth it at this stage; the install page should say plainly
what the warning is and how to proceed rather than leaving someone to guess.

An installer and a winget manifest would make Ymir discoverable through
`winget install ymir`, which is how a lot of Windows users find tools at all. That is a
separate piece of work with its own tooling and a manifest review, tracked on its own.

## macOS

Designed for, not shipped. The paths table above covers macOS because doing so costs one match
arm, and a build-only CI job keeps it from rotting. There is no macOS release binary: a
downloaded, unsigned macOS application will not open without a Gatekeeper override, and
notarization needs a paid Apple Developer enrolment and Apple hardware to test on. Neither is
available, so promising macOS support would be promising something unverified.

## Sequence

1. CI builds and tests on Windows, before any Windows-specific code. It establishes the
   baseline and surfaces any test that only passes on Linux.
2. Build hygiene, which is small and verifiable from a Linux host.
3. Application directories, which is the substantive code.
4. First run on real Windows hardware, and whatever it turns up.
5. Release packaging.
6. Documentation, which stops describing Ymir as a Linux application.
