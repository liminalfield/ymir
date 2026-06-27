//! Content-addressed disk store for cached node outputs: the warm tier of the evaluation
//! cache (see `docs/design/evaluation-cache.md`).
//!
//! Each entry is a node's output (`Vec<Field>`) serialized with [`crate::field_cache`] to a
//! file named by its content-hash key, in a single cache directory. The store survives
//! restarts and is shared across projects, since the key is the computation, not the project.
//! Bounded by a total-bytes budget with least-recently-used eviction (read access bumps a
//! file's modification time). Every operation is best-effort: a missing directory, a corrupt
//! file, or a write failure degrades to a cache miss, never a panic, so a full or read-only
//! disk can never break a build.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::field::Field;
use crate::field_cache::{read_fields, write_fields};

/// Extension for cache blobs, so eviction only ever considers our own files.
const EXTENSION: &str = "ymfc";

/// A content-addressed, byte-bounded disk cache of node outputs.
pub struct FieldStore {
    dir: PathBuf,
    budget_bytes: u64,
}

impl FieldStore {
    /// Opens (creating if needed) a store rooted at `dir`, bounded to `budget_bytes` total.
    /// Returns `None` if the directory cannot be created, which disables the disk tier for the
    /// session while the memory tier keeps working.
    #[must_use]
    pub fn open(dir: PathBuf, budget_bytes: u64) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        Some(Self { dir, budget_bytes })
    }

    /// The default cache directory, `<cache>/ymir/fields`, reading the real environment.
    /// `None` if neither `XDG_CACHE_HOME` nor `HOME` is set.
    #[must_use]
    pub fn default_dir() -> Option<PathBuf> {
        cache_dir_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
    }

    fn path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.{EXTENSION}"))
    }

    /// Loads the cached output for `key`, or `None` on any miss (absent, corrupt, unreadable).
    /// On a hit, best-effort bumps the file's modification time so eviction is least-recently-
    /// *used*, not merely least-recently-written.
    #[must_use]
    pub fn load(&self, key: u64) -> Option<Vec<Field>> {
        let path = self.path(key);
        let bytes = fs::read(&path).ok()?;
        let fields = read_fields(&bytes).ok()?;
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now()); // shortcut-ok: best-effort LRU touch
        }
        Some(fields)
    }

    /// Writes `fields` under `key` (write-through), then trims the directory to the budget.
    /// Best-effort: a serialization that cannot be written is skipped, never propagated.
    pub fn store(&self, key: u64, fields: &[Field]) {
        let bytes = write_fields(fields);
        let path = self.path(key);
        // Write to a temp file then rename, so a concurrent reader never sees a partial blob.
        let tmp = self.dir.join(format!(".{key:016x}.tmp"));
        if fs::write(&tmp, &bytes).is_err() {
            let _ = fs::remove_file(&tmp); // shortcut-ok: clean up a failed temp write
            return;
        }
        if fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp); // shortcut-ok: clean up after a failed rename
            return;
        }
        self.evict();
    }

    /// Trims the directory to `budget_bytes` by deleting the least-recently-used (oldest
    /// modification time) blobs first. Best-effort; unreadable entries are skipped.
    fn evict(&self) {
        let Ok(read_dir) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut blobs: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue; // skip temp files and anything not ours
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            total += meta.len();
            blobs.push((path, meta.len(), mtime));
        }
        if total <= self.budget_bytes {
            return;
        }
        blobs.sort_by_key(|(_, _, mtime)| *mtime); // oldest first
        for (path, size, _) in blobs {
            if total <= self.budget_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
    }
}

/// Pure resolver for the cache directory, testable without touching the real environment:
/// prefer `$XDG_CACHE_HOME`, else `$HOME/.cache`, then `ymir/fields` under it. Empty values
/// are treated as unset.
fn cache_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(xdg) = xdg.filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(xdg).join("ymir").join("fields"));
    }
    let home = home.filter(|s| !s.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".cache")
            .join("ymir")
            .join("fields"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::layer::Layer;
    use crate::layers;
    use crate::region::Region;

    /// A unique scratch directory under the OS temp dir, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ymir-fieldstore-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir); // shortcut-ok: pre-clean any stale dir
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0); // shortcut-ok: best-effort test cleanup
        }
    }

    fn field(value: f32) -> Vec<Field> {
        vec![
            Field::new(10, 10, Region::UNIT)
                .with_layer(layers::HEIGHT, Arc::new(Layer::filled(10, 10, value))),
        ]
    }

    /// Forces a file's modification time, so eviction order is deterministic in tests.
    fn set_mtime(path: &PathBuf, time: SystemTime) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open blob");
        file.set_modified(time).expect("set mtime");
    }

    #[test]
    fn store_then_load_round_trips() {
        let scratch = Scratch::new("roundtrip");
        let store = FieldStore::open(scratch.0.clone(), 1 << 20).expect("store opens");
        let original = field(0.42);
        store.store(7, &original);
        let loaded = store.load(7).expect("hit after store");
        assert_eq!(loaded[0].content_hash(), original[0].content_hash());
    }

    #[test]
    fn missing_and_corrupt_keys_miss() {
        let scratch = Scratch::new("miss");
        let store = FieldStore::open(scratch.0.clone(), 1 << 20).expect("store opens");
        assert!(store.load(123).is_none(), "absent key is a miss");

        // A corrupt blob at a valid key path is a miss, not a panic.
        fs::write(store.path(99), b"not a field blob").expect("write garbage");
        assert!(store.load(99).is_none(), "corrupt blob is a miss");
    }

    #[test]
    fn eviction_trims_to_budget_oldest_first() {
        let scratch = Scratch::new("evict");
        // Each 10x10 single-layer blob is well under 600 bytes; the budget holds one, not two.
        let store = FieldStore::open(scratch.0.clone(), 600).expect("store opens");

        store.store(1, &field(0.1));
        // Pin entry 1 firmly in the past so it is unambiguously the eviction victim.
        set_mtime(
            &store.path(1),
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
        );

        store.store(2, &field(0.2)); // pushes total over budget; evicts the oldest (1)

        assert!(
            store.load(1).is_none(),
            "oldest blob evicted under budget pressure"
        );
        assert!(store.load(2).is_some(), "newest blob retained");
    }

    #[test]
    fn cache_dir_resolution() {
        // XDG set wins.
        let xdg = cache_dir_from(
            Some(OsString::from("/x/cache")),
            Some(OsString::from("/home/u")),
        );
        assert_eq!(xdg, Some(PathBuf::from("/x/cache/ymir/fields")));
        // Empty XDG falls back to HOME/.cache.
        let home = cache_dir_from(Some(OsString::new()), Some(OsString::from("/home/u")));
        assert_eq!(home, Some(PathBuf::from("/home/u/.cache/ymir/fields")));
        // Neither set: no directory.
        assert_eq!(cache_dir_from(None, None), None);
    }
}
