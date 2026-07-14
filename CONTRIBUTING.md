# Contributing to Ymir

Thanks for your interest. Ymir is an open-source (GPL-3.0-only) procedural terrain
generator, built to a deliberately high bar: the code should hold up to scrutiny from
experienced Rust developers. Contributions are welcome as long as they hold that line.

This is early-stage software with moving internals. If you plan a substantial change,
please open an issue first so we can agree on the approach before you invest the work.

## Getting set up

You need:

- A Rust toolchain via [rustup](https://rustup.rs). The pinned compiler version lives
  in `rust-toolchain.toml` and is fetched automatically.
- For the GUI, a Vulkan-capable GPU and drivers (the viewport uses wgpu); the editor
  targets Wayland and X11.

Enable the repository git hooks once, so the shortcut scanner runs before each commit:

```bash
git config core.hooksPath .githooks
```

## Building and testing

```bash
cargo build --workspace
cargo test --workspace
cargo run -p ymir-gui        # the node editor
cargo run -p ymir-cli        # a headless sample render into out/
```

## The quality gate

Every change must pass, and CI enforces, the same checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
sh scripts/check-shortcuts.sh --worktree
```

Clippy warnings are treated as errors. Public items carry `///` docs and `cargo doc`
should build clean. `Cargo.lock` is committed (Ymir is an application), so dependency
changes show up in review.

## No shortcuts

The one rule that matters most: **fix causes, not symptoms.** A green build is
necessary, not sufficient. The failure to avoid is making a symptom disappear instead
of fixing what caused it. When the correct solution is hard, do the correct solution;
if it is large or needs a decision, say so in the issue or PR rather than shipping a
workaround.

The following are mechanically blocked by `scripts/check-shortcuts.sh` (run by the
pre-commit hook, a Stop hook, and CI):

- Arbitrary timers, sleeps, or polling to dodge a race or ordering problem.
- `unwrap`, `expect`, or panics on expected conditions in library code.
- `#[allow(...)]` to silence a lint instead of fixing the underlying code.
- `todo!`, `unimplemented!`, or stub bodies in committed code.
- Tests that assert nothing, are `#[ignore]`d, or are weakened to pass.
- Swallowing an error (`let _ =`, `.ok()`, an empty `Err` arm) to quiet a failure.

A genuinely justified exception is annotated inline with `// shortcut-ok: <reason>`,
which should be rare and visible in review. Conceptual shortcuts that a scanner cannot
catch are the job of review and tests.

There is no `unsafe` and no `async` (this is compute, not I/O bound). The graph is
represented with id/index keys, not nodes holding references to other nodes.

## Working style

- Small, single-purpose commits with clear messages. The git history is meant to read
  as a record of how the project was built.
- Every commit leaves the tree compiling, tested, and clippy and fmt clean.
- Write tests as part of the change, not after. A change is not done until its tests
  pass and the gate is clean.
- Match the surrounding code: its naming, comment density, and idiom.
- Prose in docs and comments is honest and non-performative. Avoid em dashes and
  marketing language.

## Adding a node

Adding a terrain node touches only its own new file plus one registration. See the
"Adding a node" section of [`ARCHITECTURE.md`](ARCHITECTURE.md), and use an existing
node in `crates/ymir-nodes/src/` as a template. Update the registry smoke test's
expected set so the new node is accounted for.

## Submitting a pull request

1. Branch from `main`.
2. Make the change in small commits, each passing the gate above.
3. Ensure `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test
   --workspace`, and the shortcut scan are all clean.
4. Open the PR describing what changed and why. Link the issue if there is one.

By contributing, you agree that your contributions are licensed under the project's
GPL-3.0-only license.
