//! Where this platform keeps an application's files (#310).
//!
//! Ymir stores a handful of things outside any project: the user's preferences, their recent
//! projects, their default startup graph, their subgraph library, the session log, and the
//! on-disk field cache. Every platform puts those in a different place, and until this module
//! existed Ymir only knew the XDG answer. On Windows none of those variables are set, so every
//! lookup returned `None` and each feature silently did nothing, which is a worse failure than an
//! error: the editor forgets everything between sessions and never says why.
//!
//! Three kinds, deliberately distinct:
//!
//! - **Config** is what the user set up. Preferences, the recent list, the default startup graph.
//! - **Data** is what the user made. The subgraph library.
//! - **Cache** is what can be thrown away and rebuilt. The field store.
//!
//! The distinction earns its keep on Windows, where roaming and local profiles are separate:
//! config and data follow a user to another machine, and a cache of build results, which is
//! large and machine-specific, must not.
//!
//! # Testability
//!
//! The resolution is a pure function over a platform and an environment lookup, so every
//! platform's rules are exercised from any host. A Linux CI machine proves the Windows and macOS
//! precedence, which is the whole reason this does not use a `dirs`-family crate: the dependency
//! would trade tested code for untested code, and add a transitive tree for one lookup.
//!
//! Nothing in Ymir branches on `cfg(target_os)`, here or elsewhere: this module reads
//! `std::env::consts::OS` once, and every other platform difference follows from what it returns.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Which class of application directory is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// What the user configured, and would be annoyed to lose.
    Config,
    /// What the user authored, and would be upset to lose.
    Data,
    /// What can be regenerated, and costs only time to lose.
    Cache,
}

/// Which platform's conventions to follow. A parameter rather than a compile-time branch, so a
/// test on one host covers all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    /// XDG Base Directory conventions.
    Unix,
    /// Windows known folders, via their environment variables.
    Windows,
    /// macOS `~/Library` conventions.
    Macos,
}

impl Platform {
    /// The application's directory name within this platform's base directory.
    ///
    /// Lowercase on Unix, where the convention is lowercase and, more to the point, where
    /// existing installations already have `~/.config/ymir`. Capitalized on Windows and macOS,
    /// whose conventions are a display-cased application folder.
    ///
    /// A method on the platform rather than a `cfg` constant, so a test that asks for the
    /// Windows rules from a Linux host gets the Windows answer. As a constant this was the one
    /// part of the resolution that ignored the platform parameter, which made the Windows and
    /// macOS tests silently assert the Linux name.
    const fn app_dir(self) -> &'static str {
        match self {
            Self::Unix => "ymir",
            Self::Windows | Self::Macos => "Ymir",
        }
    }

    /// The platform this build targets.
    ///
    /// Reads `std::env::consts::OS` rather than branching on `cfg`. It is a compile-time
    /// constant either way, so the match folds away, but this form constructs every variant in
    /// ordinary code. Under `cfg` the other two are unreachable on any given host and read as
    /// dead, which is a warning that can only be silenced by suppressing it. The test below
    /// checks this against `cfg`, since a typo'd string would otherwise fall quietly to `Unix`.
    fn current() -> Self {
        match std::env::consts::OS {
            "windows" => Self::Windows,
            "macos" => Self::Macos,
            _ => Self::Unix,
        }
    }
}

/// Reads an environment variable, treating an empty value as unset.
///
/// An empty `XDG_CONFIG_HOME` is common enough (an exported-but-unset shell variable) that
/// honouring it would resolve paths against the filesystem root.
fn env(get: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    get(name).filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// The base directory for `kind` under `platform`, before the application's own folder is added.
///
/// Pure: every environment read goes through `get`, so a test supplies its own environment
/// without touching the process's.
fn base(platform: Platform, kind: Kind, get: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match platform {
        Platform::Unix => {
            let (var, fallback) = match kind {
                Kind::Config => ("XDG_CONFIG_HOME", ".config"),
                Kind::Data => ("XDG_DATA_HOME", ".local/share"),
                Kind::Cache => ("XDG_CACHE_HOME", ".cache"),
            };
            env(get, var).or_else(|| Some(env(get, "HOME")?.join(fallback)))
        }
        Platform::Windows => {
            // Roaming for what the user set up and made, Local for what can be rebuilt: a field
            // cache of build results is large and machine-specific, and must not follow a user
            // onto another machine.
            //
            // The %USERPROFILE% fallbacks matter because these are ordinary environment
            // variables, not the known-folder API, and a stripped or service environment can be
            // missing them while still having a profile.
            let (var, under) = match kind {
                Kind::Config | Kind::Data => ("APPDATA", "AppData/Roaming"),
                Kind::Cache => ("LOCALAPPDATA", "AppData/Local"),
            };
            env(get, var).or_else(|| Some(env(get, "USERPROFILE")?.join(under)))
        }
        Platform::Macos => {
            let under = match kind {
                Kind::Config | Kind::Data => "Library/Application Support",
                Kind::Cache => "Library/Caches",
            };
            Some(env(get, "HOME")?.join(under))
        }
    }
}

/// Resolves `rel` under the application's own folder in the `kind` base.
fn resolve(
    platform: Platform,
    kind: Kind,
    get: &dyn Fn(&str) -> Option<OsString>,
    rel: &Path,
) -> Option<PathBuf> {
    Some(
        base(platform, kind, get)?
            .join(platform.app_dir())
            .join(rel),
    )
}

/// Resolves `rel` against the real environment for this platform.
fn resolve_here(kind: Kind, rel: &Path) -> Option<PathBuf> {
    // Reading environment variables is safe in edition 2024; only `set_var` is the unsafe one,
    // which nothing here needs.
    resolve(
        Platform::current(),
        kind,
        &|name| std::env::var_os(name),
        rel,
    )
}

/// A path under the user's Ymir *configuration* directory, for what they set up: preferences,
/// the recent-projects list, the default startup graph.
///
/// `None` when the platform's base cannot be resolved, in which case the caller should degrade
/// (settings for this session only) rather than treat it as an error.
///
/// | Platform | Location |
/// | --- | --- |
/// | Linux | `$XDG_CONFIG_HOME/ymir`, else `~/.config/ymir` |
/// | Windows | `%APPDATA%\Ymir` |
/// | macOS | `~/Library/Application Support/Ymir` |
#[must_use]
pub fn config_path(rel: impl AsRef<Path>) -> Option<PathBuf> {
    resolve_here(Kind::Config, rel.as_ref())
}

/// A path under the user's Ymir *data* directory, for what they authored and would not want
/// swept away as cache: the subgraph library.
///
/// | Platform | Location |
/// | --- | --- |
/// | Linux | `$XDG_DATA_HOME/ymir`, else `~/.local/share/ymir` |
/// | Windows | `%APPDATA%\Ymir` |
/// | macOS | `~/Library/Application Support/Ymir` |
#[must_use]
pub fn data_path(rel: impl AsRef<Path>) -> Option<PathBuf> {
    resolve_here(Kind::Data, rel.as_ref())
}

/// A path under the user's Ymir *cache* directory, for what can be regenerated: the field store.
///
/// | Platform | Location |
/// | --- | --- |
/// | Linux | `$XDG_CACHE_HOME/ymir`, else `~/.cache/ymir` |
/// | Windows | `%LOCALAPPDATA%\Ymir` |
/// | macOS | `~/Library/Caches/Ymir` |
#[must_use]
pub fn cache_path(rel: impl AsRef<Path>) -> Option<PathBuf> {
    resolve_here(Kind::Cache, rel.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment built from pairs, for a test that needs no process environment.
    fn envs(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    /// Resolves with `/` separators regardless of host, so an expectation reads the same
    /// everywhere. `Path::join` accepts forward slashes on Windows too.
    fn at(platform: Platform, kind: Kind, pairs: &[(&str, &str)], rel: &str) -> Option<String> {
        resolve(platform, kind, &envs(pairs), Path::new(rel))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    #[test]
    fn the_detected_platform_agrees_with_the_compiler() {
        // `current` matches on a string, so a typo would fall silently through to `Unix` and this
        // host would quietly use the wrong conventions. `cfg` is the authority it is checked
        // against.
        let expected = if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Unix
        };
        assert_eq!(Platform::current(), expected);
    }

    #[test]
    fn the_public_functions_are_wired_to_this_host() {
        // Wiring rather than rules. Everything else here drives the pure resolver directly, so a
        // mistake in what the public functions pass it (the wrong `Kind`, or an environment
        // lookup that never happens) would go unnoticed. CI runs this on Linux and on Windows,
        // which is where such a mismatch would actually differ.
        let Some(config) = config_path("preferences.json") else {
            // A host with none of this platform's variables set has nowhere to put files. That
            // is the documented degradation, not a failure.
            return;
        };
        assert!(config.ends_with("preferences.json"));
        assert!(
            config
                .to_string_lossy()
                .contains(Platform::current().app_dir()),
            "resolved outside the application folder: {}",
            config.display()
        );
        if let Some(cache) = cache_path("fields") {
            assert_ne!(
                config.parent(),
                cache.parent(),
                "clearing the cache would take the user's preferences with it"
            );
        }
    }

    #[test]
    fn linux_prefers_xdg_then_falls_back_to_home() {
        let xdg = [
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_DATA_HOME", "/xdg/data"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("HOME", "/home/u"),
        ];
        assert_eq!(
            at(Platform::Unix, Kind::Config, &xdg, "preferences.json").as_deref(),
            Some("/xdg/config/ymir/preferences.json")
        );
        assert_eq!(
            at(Platform::Unix, Kind::Data, &xdg, "subgraphs").as_deref(),
            Some("/xdg/data/ymir/subgraphs")
        );
        assert_eq!(
            at(Platform::Unix, Kind::Cache, &xdg, "fields").as_deref(),
            Some("/xdg/cache/ymir/fields")
        );

        let home = [("HOME", "/home/u")];
        assert_eq!(
            at(Platform::Unix, Kind::Config, &home, "preferences.json").as_deref(),
            Some("/home/u/.config/ymir/preferences.json")
        );
        assert_eq!(
            at(Platform::Unix, Kind::Data, &home, "subgraphs").as_deref(),
            Some("/home/u/.local/share/ymir/subgraphs")
        );
        assert_eq!(
            at(Platform::Unix, Kind::Cache, &home, "fields").as_deref(),
            Some("/home/u/.cache/ymir/fields")
        );
    }

    #[test]
    fn windows_splits_roaming_from_local() {
        // The distinction that matters on Windows: what the user set up and made follows them to
        // another machine, and a cache of build results, which is large and machine-specific,
        // does not.
        let e = [
            ("APPDATA", "C:/Users/u/AppData/Roaming"),
            ("LOCALAPPDATA", "C:/Users/u/AppData/Local"),
        ];
        assert_eq!(
            at(Platform::Windows, Kind::Config, &e, "preferences.json").as_deref(),
            Some("C:/Users/u/AppData/Roaming/Ymir/preferences.json")
        );
        assert_eq!(
            at(Platform::Windows, Kind::Data, &e, "subgraphs").as_deref(),
            Some("C:/Users/u/AppData/Roaming/Ymir/subgraphs")
        );
        assert_eq!(
            at(Platform::Windows, Kind::Cache, &e, "fields").as_deref(),
            Some("C:/Users/u/AppData/Local/Ymir/fields"),
            "a rebuildable cache belongs in the local profile, not the roaming one"
        );
    }

    #[test]
    fn windows_falls_back_to_the_user_profile() {
        // These are ordinary environment variables, not the known-folder API, so a stripped or
        // service environment can lack them while still having a profile.
        let e = [("USERPROFILE", "C:/Users/u")];
        assert_eq!(
            at(Platform::Windows, Kind::Config, &e, "recent.json").as_deref(),
            Some("C:/Users/u/AppData/Roaming/Ymir/recent.json")
        );
        assert_eq!(
            at(Platform::Windows, Kind::Cache, &e, "fields").as_deref(),
            Some("C:/Users/u/AppData/Local/Ymir/fields")
        );
    }

    #[test]
    fn macos_uses_its_library_conventions() {
        let e = [("HOME", "/Users/u")];
        assert_eq!(
            at(Platform::Macos, Kind::Config, &e, "preferences.json").as_deref(),
            Some("/Users/u/Library/Application Support/Ymir/preferences.json")
        );
        assert_eq!(
            at(Platform::Macos, Kind::Cache, &e, "fields").as_deref(),
            Some("/Users/u/Library/Caches/Ymir/fields")
        );
    }

    #[test]
    fn an_empty_variable_is_treated_as_unset() {
        // An exported-but-unset shell variable is common, and honouring it would resolve every
        // path against the filesystem root.
        let e = [("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")];
        assert_eq!(
            at(Platform::Unix, Kind::Config, &e, "x.json").as_deref(),
            Some("/home/u/.config/ymir/x.json")
        );
        let win = [("APPDATA", ""), ("USERPROFILE", "C:/Users/u")];
        assert_eq!(
            at(Platform::Windows, Kind::Config, &win, "x.json").as_deref(),
            Some("C:/Users/u/AppData/Roaming/Ymir/x.json")
        );
    }

    #[test]
    fn nothing_resolves_without_an_environment() {
        // The caller degrades (this session only) rather than failing, so an unusual environment
        // costs a feature and not a launch.
        for platform in [Platform::Unix, Platform::Windows, Platform::Macos] {
            for kind in [Kind::Config, Kind::Data, Kind::Cache] {
                assert_eq!(
                    at(platform, kind, &[], "x"),
                    None,
                    "{platform:?}/{kind:?} invented a path from nothing"
                );
            }
        }
    }

    #[test]
    fn every_platform_separates_config_from_cache() {
        // The property each platform's table above is meant to satisfy, asserted once rather than
        // re-read out of three sets of literals: a cache is never placed inside the config
        // directory, or clearing it would take the user's preferences with it.
        let e = [
            ("HOME", "/home/u"),
            ("USERPROFILE", "C:/Users/u"),
            ("APPDATA", "C:/Users/u/AppData/Roaming"),
            ("LOCALAPPDATA", "C:/Users/u/AppData/Local"),
        ];
        for platform in [Platform::Unix, Platform::Windows, Platform::Macos] {
            let config = at(platform, Kind::Config, &e, "").expect("config");
            let cache = at(platform, Kind::Cache, &e, "").expect("cache");
            assert_ne!(
                config, cache,
                "{platform:?} put its cache in its config dir"
            );
        }
    }
}
