//! Build script: stamps git provenance into the binary as compile-time env vars.
//!
//! Reads the short commit hash, a dirty flag, and the commit date from git and emits
//! them as `cargo:rustc-env`, so `env!` in `lib.rs` resolves them at compile time. Git
//! being absent (a source tarball with no repository, or git not installed) is not an
//! error: the values fall back to `"unknown"`/empty and the version string degrades to
//! the bare SemVer number. A `YMIR_GIT_SHA_OVERRIDE` env var takes precedence, so a
//! build outside a checkout can still be stamped by whoever drives it (a release CI).

use std::path::Path;
use std::process::Command;

/// Runs `git` with `args`, returning trimmed stdout, or `None` if git is missing, the
/// command fails, or the output is empty.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn main() {
    // An explicit override wins, then the working checkout, then a clear fallback.
    let sha = std::env::var("YMIR_GIT_SHA_OVERRIDE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    // Dirty when the working tree has any uncommitted change (tracked or staged).
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    // Commit date as YYYY-MM-DD; empty when built outside a checkout.
    let date = git(&["log", "-1", "--format=%cd", "--date=short"]).unwrap_or_default();

    println!("cargo:rustc-env=YMIR_GIT_SHA={sha}");
    println!("cargo:rustc-env=YMIR_GIT_DIRTY={}", u8::from(dirty));
    println!("cargo:rustc-env=YMIR_COMMIT_DATE={date}");

    // Re-run when HEAD moves so the stamp tracks new commits and branch switches. These
    // sit at the workspace root, two levels up from this crate's manifest dir. Absent in
    // a tarball build, in which case the script simply runs once.
    for rel in ["../../.git/HEAD", "../../.git/logs/HEAD"] {
        if Path::new(rel).exists() {
            println!("cargo:rerun-if-changed={rel}");
        }
    }
    println!("cargo:rerun-if-env-changed=YMIR_GIT_SHA_OVERRIDE");
}
