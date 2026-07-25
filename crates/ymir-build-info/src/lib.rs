//! Build and version provenance for Ymir's binaries.
//!
//! Exposes the crate version (the single workspace version, via `CARGO_PKG_VERSION`)
//! together with the git commit the binary was built from, stamped at compile time by
//! `build.rs`. [`version_string`] formats them for an About box or a `--version` flag.
//!
//! This is a leaf crate that only the binaries (`ymir-cli`, `ymir-gui`) depend on, so
//! the engine crates and their golden tests never see build metadata.

/// The SemVer version, for example `"0.2.0"`. This is the one workspace version (see the
/// root `Cargo.toml`), shared by every crate, and is distinct from the save-format
/// versions, which track the serialized schema.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short git commit hash the binary was built from, for example `"a1b2c3d"`, or
/// `"unknown"` when built outside a git checkout.
pub const GIT_SHA: &str = env!("YMIR_GIT_SHA");

/// Whether the working tree had uncommitted changes at build time: `"1"` or `"0"`.
pub const GIT_DIRTY: &str = env!("YMIR_GIT_DIRTY");

/// The commit date as `YYYY-MM-DD`, or empty when built outside a git checkout.
pub const COMMIT_DATE: &str = env!("YMIR_COMMIT_DATE");

/// Whether the working tree was modified at build time.
#[must_use]
pub fn is_dirty() -> bool {
    GIT_DIRTY == "1"
}

/// A human-facing version line: the SemVer version, then the commit hash (with a
/// `-dirty` suffix when the tree was modified) and the commit date when known.
///
/// Examples: `"0.2.0 (a1b2c3d, 2026-07-21)"`, `"0.2.0 (a1b2c3d-dirty)"`, or a bare
/// `"0.2.0"` when built without a git checkout.
#[must_use]
pub fn version_string() -> String {
    if GIT_SHA == "unknown" {
        return VERSION.to_owned();
    }
    let dirty = if is_dirty() { "-dirty" } else { "" };
    if COMMIT_DATE.is_empty() {
        format!("{VERSION} ({GIT_SHA}{dirty})")
    } else {
        format!("{VERSION} ({GIT_SHA}{dirty}, {COMMIT_DATE})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_equals_the_cargo_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn version_string_begins_with_the_semver_version() {
        assert!(version_string().starts_with(VERSION));
    }

    #[test]
    fn version_string_is_bare_only_when_the_sha_is_unknown() {
        let s = version_string();
        if GIT_SHA == "unknown" {
            assert_eq!(s, VERSION);
        } else {
            // In a normal checkout the commit is stamped and shown in parentheses.
            assert!(s.contains(GIT_SHA), "{s} should carry the commit hash");
            assert!(
                s.contains('(') && s.ends_with(')'),
                "{s} should parenthesize it"
            );
        }
    }

    #[test]
    fn dirty_flag_is_one_of_two_known_values() {
        assert!(
            GIT_DIRTY == "0" || GIT_DIRTY == "1",
            "unexpected: {GIT_DIRTY}"
        );
    }
}
