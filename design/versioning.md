> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Versioning

How Ymir is versioned, where the version lives, and how a release is cut. Kept short on
purpose: the mechanism is small and the policy is a few rules.

## Scheme

Ymir uses [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`. It is an
application, not a published library, so the version tracks user-visible change rather
than a Rust API surface:

- **PATCH** (`0.2.x`): bug fixes and internal changes, no new capability.
- **MINOR** (`0.x.0`): new nodes, new UI, notable features. Most releases.
- **MAJOR**: reserved. `1.0.0` is the deliberate "this is real and the save format is
  stable" milestone. After 1.0, a MAJOR bump means an intentional break, most likely a
  save-format epoch that old files cannot load without migration.

Ymir is in `0.x` while it is a public preview: still evolving, no stability promise yet.
`0.2.0` is the first public baseline (`0.1.0` was pre-public development).

## The version is the app version, not the file-format version

Two independent axes, do not conflate them:

- **App version** (this document): the marketing/release number, one per release.
- **Save-format versions**: `ymir_core::project::FORMAT_VERSION` and the GUI's
  `PROJECT_FORMAT_VERSION` / `SUBGRAPH_FORMAT_VERSION`. These bump only when the
  serialized schema changes, and each change ships a migration path (see the
  file-format-stability rule in CLAUDE.md). A release can bump the app version many times
  without touching the format version, and vice versa.

## Single source of truth

The version lives once, in the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.2.0"
```

Every crate inherits it with `version.workspace = true`. Bumping that one line moves the
whole workspace. This is the Cargo analog of a single `package.json` `version`.

At compile time Cargo exposes it as `CARGO_PKG_VERSION`, so any binary reads its own
version with `env!("CARGO_PKG_VERSION")`, no plumbing.

## Build provenance (instead of a build number)

There is no hand-incremented build number. It is bookkeeping that tells you nothing a
commit hash does not. Instead, `crates/ymir-build-info` has a `build.rs` that stamps the
git short SHA, a dirty flag, and the commit date into the binary at compile time.
`ymir_build_info::version_string()` formats them:

```
0.2.0 (a1b2c3d, 2026-07-21)     # a clean checkout
0.2.0 (a1b2c3d-dirty)          # uncommitted changes at build time
0.2.0                          # built with no git checkout (a source tarball)
```

Only the binaries (`ymir-cli`, `ymir-gui`) depend on `ymir-build-info`, so the engine
crates and their golden tests never see build metadata. Git being absent is not an error;
the string degrades to the bare version.

Where it surfaces:

- `ymir-cli --version` (or `-V`).
- The GUI's **Help -> About** window.

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (and let `cargo build` refresh `Cargo.lock`).
2. Commit the bump, land it on `main` through the normal PR + CI flow.
3. Tag the merge commit and push the tag:

   ```
   git tag v0.2.0
   git push origin v0.2.0
   ```

That is it. The `Release` workflow (`.github/workflows/release.yml`) fires on the `v*`
tag and:

- Verifies the tag (minus its `v`) equals the workspace version, failing loudly on a
  mismatch so a tag can never ship a binary whose embedded version disagrees.
- Builds `--release` binaries for both crates, with the tagged commit stamped in.
- Publishes a GitHub Release with the binaries, their `SHA256SUMS`, and auto-generated
  notes.

The ordinary `CI` workflow is unchanged and still gates every PR; releasing is a separate
tag-triggered path.
