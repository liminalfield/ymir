> **Design record, not user documentation.** A design or decision note captured at a point in time; it may lag the current build. To learn how to use Ymir, see the documentation site (linked from the [README](../README.md)).

# Design note: the tiered evaluation cache (memory + disk)

Status: **designed, ready to build in steps.** Captures the design so the hard decisions
(format, keying, bounds, failure handling) are settled before code. Motivated by a concrete
problem: re-running a Build that changed nothing recomputes the entire graph, because the build
creates a fresh `EvalCache` each time and drops it. This note makes evaluation results survive,
in memory and on disk, so an unchanged rebuild is near-instant and the memory ceiling at build
resolution goes away.

Related: [erosion roadmap](erosion-roadmap.md) (this is a Phase 0 companion), the
build-result-feeds-viewport memory (this is its foundation), CLAUDE.md "Bounded cache" and
"Hashing and identity".

## 1. Why

- A second Build of an unchanged graph takes the full time. `run()` in the GUI build path makes
  a fresh `EvalCache` per build and discards it, so nothing is reused across builds even though
  the cache is content-hash keyed and would hit on every node.
- Persisting results purely in memory hits a wall: a full-resolution `Field` per node is large
  (a 4096-squared f32 layer is 64 MB), so holding a whole graph at build resolution can reach
  gigabytes. CLAUDE.md already calls this out as a designed-in seam.
- Disk changes the tradeoff. For the nodes that make builds slow (erosion: seconds per node),
  reading a raw-byte field back from an SSD (tens of milliseconds) is 100 to 1000 times faster
  than recomputing, and it sidesteps the RAM ceiling and survives app restarts.

## 2. The design: three tiers

Lookup order on a pull, fastest first; write-through back up on a recompute.

1. **Memory (hot).** The existing in-memory `EvalCache`, kept small and bounded by an approximate
   memory budget (not an entry count). Fastest: a hit is an `Arc` clone.
2. **Disk (warm).** Content-hash-named raw-byte files in a cache directory, bounded by a disk
   budget with least-recently-used eviction. Survives restarts and is shared across projects
   (the key is the computation, not the project). A hit loads the file and promotes it into the
   memory tier.
3. **Recompute (cold).** Only when both miss. On the way back, the result is written through to
   the disk tier and inserted into the memory tier.

A useful consequence: with a persistent disk tier, the build worker does **not** need to
round-trip its in-memory cache across the thread boundary. Each build can use a fresh in-memory
tier over the shared disk tier; an unchanged rebuild simply misses memory and hits disk. The
disk tier provides the persistence; the memory tier is just a hot accelerator.

## 3. Keying and correctness

- The cache key is the existing **content hash** (the `u64` the evaluator already computes per
  node: `type_id` + canonical param hash + upstream input hashes + the `EvalContext` fields it
  depends on, including seed, resolution, and region). It is input-derived and deterministic.
- The **disk key is that hash alone** (the runtime `NodeId` is not portable across restarts or
  projects). The filename is the hash in hex. Two nodes with the same key compute the same
  output, so sharing one file is correct, not a collision bug.
- Correctness is automatic: change a param or anything upstream and the key changes, so the
  lookup misses and the node (and only its downstream) recomputes and rewrites. There is no
  stale-result risk; a stale file just becomes unreferenced and is evicted later.
- Determinism: same-machine repeatability (a hard Ymir promise) means a recompute on this
  machine would produce the same bytes the cache holds, so reuse is sound. For the few
  order-sensitive nodes that are only visually-equivalent across machines, reuse is actually
  preferable, since it keeps a build consistent with its own earlier passes. Resolution is part
  of the key, so build-resolution and preview-resolution results never collide.

## 4. The binary field format

One cache entry is a node's full output, a `Vec<Field>` (multi-output nodes). `BTreeMap` iteration
is sorted, so the byte layout is canonical and deterministic. All multi-byte values are
little-endian.

```
Entry:
  magic        : [u8; 4]  = b"YMFC"   (Ymir field cache)
  version      : u16
  field_count  : u32
  Field × field_count:
    width        : u32
    height       : u32
    region       : f64 × 4            (min_x, min_y, max_x, max_y)
    detail_count : u32
    detail entry × detail_count       (BTreeMap order, sorted by name):
      name_len   : u32
      name       : u8 × name_len      (utf-8)
      value      : f64
    layer_count  : u32
    layer × layer_count               (BTreeMap order, sorted by name):
      name_len   : u32
      name       : u8 × name_len      (utf-8)
      data       : f32 × (width*height)   (row-major, raw little-endian)
```

This is an internal cache format, not the project format, and carries its own `version` so it
can evolve freely; an unrecognised magic or version is treated as a miss (recompute), never an
error. Raw f32 bytes (no serde, no compression to start) keep read and write at I/O speed, which
is the entire point. Layer data reconstructs through `Layer::from_vec`. The format lives in
`ymir-core` beside the PNG encoder (core already owns reusable, non-terrain-semantic I/O).

## 5. Bounds and eviction

- **Memory tier:** bound by approximate bytes (`sum over layers of width*height*4`, plus small
  overhead), not entry count. Default budget auto-sized to a sane fraction of system RAM with a
  hard cap (for example, the smaller of 2 GB and a quarter of RAM), no user knob to start. LRU
  eviction, deterministic tie-break (as today).
- **Disk tier:** bound by a total-bytes budget over the cache directory (a few GB default),
  LRU by file access or modification time. Eviction runs opportunistically (on write, trim the
  oldest until under budget). Cheap to clear: the directory is disposable.
- **Location:** the XDG cache directory (`$XDG_CACHE_HOME/ymir/fields/`, falling back to
  `$HOME/.cache/ymir/fields/`), so it survives restarts, is user-clearable, and never pollutes
  the project. A single global directory, since keys are content-addressed.
- **Planned: user settings for both budgets.** The memory and disk budgets will become exposed
  settings (so users can tune RAM and disk usage to their machine). Built with sensible
  auto-sized/fixed defaults first; the settings UI is a deliberate later addition, not part of
  the initial cache work.

## 6. Failure handling

Every disk interaction degrades to recompute, never panics and never corrupts a result:

- Unreadable or corrupt file, wrong magic or version, short read: treat as a miss.
- Write failure (disk full, permission): log once, skip the write, keep the in-memory result.
- Missing or uncreatable cache directory: disable the disk tier for the session, memory tier
  still works.

No `unwrap`/`expect` on these expected conditions, per the project bar.

## 7. Scope of who uses it

- **Build and the future viewport read:** use the full tiered cache. This is the immediate win.
- **Preview:** stays in-memory only for now. Preview resolution is small and churns rapidly;
  disk-caching tiny short-lived fields is not obviously worth the I/O. Revisit if switching
  between heavy nodes at preview resolution proves slow.
- **CLI:** inherits the tiered cache for free, since it lives in core.

## 8. Implementation steps (each a reviewable commit)

1. **Binary field (de)serialization in `ymir-core`**: `write_fields`/`read_fields` for
   `&[Field]`, with a round-trip golden test (serialize, deserialize, assert byte-identical
   content hash) and malformed-input tests (truncated, bad magic, bad version all return a clean
   miss/error, never panic).
2. **Memory tier becomes byte-bounded**: change `EvalCache` eviction from entry count to an
   approximate-bytes budget with an auto-sized default; tests for eviction order and the budget.
3. **Disk tier**: a content-addressed field store (read by hash, write-through, dir size cap with
   LRU eviction, all failures degrade to miss). Wire it under `EvalCache` so `get` falls through
   memory then disk, and a recompute writes through to both. Tests with a temp directory.
4. **Build path uses the persistent disk tier**: drop the fresh-per-build discard; give the build
   worker a tiered cache pointed at the cache directory. Verify (manually, GUI cannot run
   headless) that an unchanged rebuild is near-instant and a one-node edit recomputes only that
   node and its downstream.
5. **Viewport reads build-quality fields** from the cache for the shown node, falling back to the
   preview field when the build result is absent or stale (the build-result-feeds-viewport
   memory). This is the payoff step and can follow once 1 to 4 land.

## 9. Open questions

- **Disk budget default**: a few GB is a guess. Auto-size against free disk, or a fixed default
  with a setting later? Leaning fixed default now, revisit.
- **Compression**: raw bytes first (speed). If disk footprint becomes a problem, a fast codec
  (LZ4-class) is a later, contained addition; measure before adding.
- **Cross-session safety**: the format `version` guards format changes, but if the *hashing*
  scheme ever changes, old keys silently never match (harmless: they evict as unreferenced). Note
  it so a hash change is understood to invalidate the disk cache.
