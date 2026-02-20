# Plan: Dual-Cache System for L2 Block Cache

## Problem

`invalidate_block_cache()` clears the **entire** thread-local LRU cache every time the
STREAMS_STREAM_ID lock is acquired. This unnecessarily evicts cached redirector blocks
belonging to user streams that no other thread could have modified. The only blocks that
genuinely need cross-thread coherency are the **Streams stream's own** redirector blocks.

### When does invalidation actually fire?

The stream descriptor's `size` field is updated **in memory** during `pyramid_write()`
(`streams_from_blocks.rs:667,722`), so individual writes to an open stream do NOT trigger
invalidation. However, every **open/close/create/delete/metadata-query** cycle does:

| Operation | Invalidates? | Line |
|-----------|-------------|------|
| `write()` to a stream | No | — |
| `close_stream()` (writer) | **Yes** | `:2080` |
| `open_stream_inner()` | **Yes** | `:1684` |
| `create_stream()` | **Yes** | `:1924` |
| `delete_stream()` | **Yes** | `:2111` |
| `stream_exists()` | **Yes** | `:2005` |
| `stream_count()` | **Yes** | `:2024` |
| `stream_ids()` | **Yes** | `:2043` |

This means **short-lived stream open/write/close cycles** are the primary pain point —
each close flushes the cached descriptor to the Streams stream and wipes the entire
per-thread LRU, evicting warm redirector blocks for every other stream that thread was
serving. Interleaved metadata queries (`stream_exists`, `stream_count`, `stream_ids`)
cause the same collateral damage.

## Design

Replace `cache: bool` with a `CacheMode` enum. Add a **shared** (Mutex-guarded) LRU cache
alongside the existing thread-local one. L3 passes `CacheMode::Shared` for Streams stream
operations and `CacheMode::ThreadLocal` for user stream redirector operations. Data blocks
continue to bypass caching entirely (`CacheMode::None`).

With write-through semantics on the shared cache, all threads see each other's Streams stream
writes immediately. `invalidate_block_cache()` is no longer needed and can be removed.

---

## Step 0: Add a targeted benchmark (on master, before cache changes)

The key insight is that cache invalidation fires on **stream lifecycle operations**
(open/close/create/delete) and **metadata queries**, not on individual reads/writes.
The most realistic scenario that suffers is **short-lived stream open/write/close
cycles interleaved with long-running I/O on other streams** — each close wipes
the calling thread's entire LRU, destroying warm redirector blocks for unrelated
streams.

**Add a `cache-pressure` benchmark** to `yak_cl/src/bench.rs` that captures this:

1. **Setup (not timed):** Create N large streams (e.g. 10 × 2 MB) so each has a
   deep-ish pyramid with many redirector blocks worth caching.
2. **Timed phase:** Launch T threads concurrently:
   - **I/O threads** (T/2): Each opens one of the pre-populated streams and performs
     many small random reads (e.g. 500 × 4 KB at random offsets). This repeatedly
     navigates the pyramid, benefiting from cached redirector blocks.
   - **Churn threads** (T/2): Each loops performing short-lived open/write/close
     cycles on throwaway streams. Every close calls `invalidate_block_cache()` on
     the churn thread itself. This is realistic — think of a workload appending
     log entries, rotating temp streams, or updating metadata streams frequently.

   To make the I/O threads *also* hit invalidation mid-flight, have them
   periodically perform a short-lived open/close of a different stream (or call
   `stream_exists()`) between batches of reads. This triggers
   `invalidate_block_cache()` **on the I/O thread**, wiping its warm user-stream
   redirector cache — the exact collateral damage this optimisation eliminates.

3. **Metric:** Wall-clock time for the I/O threads to complete all reads.

This benchmark should be committed to master first so we have a baseline, then
cherry-picked or rebased onto the working branch. After the cache changes, rerunning
the same benchmark shows the improvement.

### Why this benchmark works

- The I/O threads build up a warm thread-local cache of user stream redirector
  blocks during random reads.
- The periodic open/close cycle (or `stream_exists()` call) acquires the Streams
  lock and calls `invalidate_block_cache()`, flushing the **entire** per-thread
  LRU — including the warm redirector blocks for the I/O thread's own stream.
- After the dual-cache change, `invalidate_block_cache()` is gone. The I/O
  thread's thread-local cache survives Streams stream operations, so subsequent
  reads hit cached redirectors instead of going to L1.
- The churn threads model a realistic workload pattern (frequent short-lived
  streams) rather than an artificial stress test.

---

## Step 1: Define `CacheMode` enum

**File:** `yak/src/block_layer.rs`

Add a `CacheMode` enum:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Bypass all caches (user data blocks).
    None,
    /// Per-thread LRU cache (user stream redirector blocks).
    ThreadLocal,
    /// Cross-thread Mutex-guarded LRU cache (Streams stream redirector blocks).
    Shared,
}
```

Provide a `From<bool>` impl during transition if helpful, but ideally update all call sites
directly.

## Step 2: Update `BlockLayer` trait signatures

**File:** `yak/src/block_layer.rs`

- `read_block(... cache: bool)` → `read_block(... cache: CacheMode)`
- `write_block(... cache: bool)` → `write_block(... cache: CacheMode)`
- **Remove** `invalidate_block_cache(&self)` from the trait entirely
- Update `read_contiguous_blocks` and `write_contiguous_blocks` default implementations:
  change `false` → `CacheMode::None`

## Step 3: Update `BlocksInFile` — add shared cache

**File:** `yak/src/blocks_in_file.rs`

### 3a: Add shared cache field

Add to the `BlocksInFile` struct:

```rust
shared_cache: Mutex<LruCache<u64, Vec<u8>>>,
```

Use the same `cache_capacity` as the thread-local cache (same block size, same budget
makes sense). Could also be a separate const-generic or a fraction of the budget — for
the Streams stream the working set is small so even a modest capacity is fine.

### 3b: Initialize shared cache

In `create()` and `open()`, construct the shared cache alongside the thread-local one.

### 3c: Update `read_block` and `write_block`

Route to the appropriate cache based on `CacheMode`:

- `CacheMode::None` → bypass both caches (existing `cache: false` path)
- `CacheMode::ThreadLocal` → use `self.thread_cache()` (existing `cache: true` path)
- `CacheMode::Shared` → use `self.shared_cache.lock().unwrap()`

The logic is identical for both cache types — only the cache reference differs. Extract
the cache-hit/cache-miss logic into a helper that takes a generic cache reference, or
simply duplicate the small amount of routing code (lookup + memcpy). Given both caches
are `LruCache<u64, Vec<u8>>`, a helper like `read_from_cache(cache: &mut LruCache, ...)`
and `write_to_cache(cache: &mut LruCache, ...)` would avoid duplication.

### 3d: Update `allocate_blocks` and `deallocate_block(s)` — evict from both caches

Currently these evict from thread-local only. Add eviction from `shared_cache` as well:

```rust
// Evict from thread-local cache
if CACHE_BUDGET_BYTES > 0 {
    let mut tl = self.thread_cache().borrow_mut();
    for &id in &result { tl.pop(&id); }
}
// Evict from shared cache
if CACHE_BUDGET_BYTES > 0 {
    let mut sh = self.shared_cache.lock().unwrap();
    for &id in &result { sh.pop(&id); }
}
```

Apply to: `allocate_blocks()`, `deallocate_block()`, `deallocate_blocks()`.

### 3e: Remove `invalidate_block_cache()`

Delete the method entirely from `BlocksInFile`.

### 3f: Update `read_contiguous_blocks` and `write_contiguous_blocks` overrides

These don't touch the cache at all in `BlocksInFile` (they do direct L1 I/O), but the
`cache` parameter in their internal `read_block`/`write_block` calls (if any) needs the
type change. In practice, `BlocksInFile` overrides these to bypass per-block calls, so
no change needed in the override bodies.

## Step 4: Update `BlocksFromFiles`

**File:** `yak/src/blocks_from_files.rs`

Change `_cache: bool` → `_cache: CacheMode` in `read_block` and `write_block`. This
implementation has no cache, so it ignores the parameter. Add `use crate::block_layer::CacheMode;`.

## Step 5: Thread `CacheMode` through L3 helper functions

**File:** `yak/src/streams_from_blocks.rs`

These generic helpers currently hard-code `cache: true`. Add a `cache: CacheMode` parameter
that gets forwarded to L2 calls:

| Function | Change |
|----------|--------|
| `read_block_index()` | Add `cache: CacheMode`, forward to `layer2.read_block(..., cache)` |
| `write_block_index()` | Add `cache: CacheMode`, forward to `layer2.write_block(..., cache)` |
| `fill_sentinel()` | Add `cache: CacheMode`, forward to `layer2.write_block(..., cache)` |
| `navigate_to_leaf()` | Add `cache: CacheMode`, forward to `read_block_index(..., cache)` |
| `batch_fill_leaf_redirectors()` | Add `cache: CacheMode`, forward to `layer2.write_block(..., cache)` |
| `pyramid_read()` | Add `cache: CacheMode`, use for redirector reads, `CacheMode::None` for data |
| `pyramid_write()` | Add `cache: CacheMode`, use for redirector reads/writes, `CacheMode::None` for data |
| `collect_tree_blocks_for_dealloc()` | Add `cache: CacheMode`, forward to `read_block_index(...)` |
| `deallocate_tree()` | Add `cache: CacheMode`, forward to `collect_tree_blocks_for_dealloc(...)` |
| `deallocate_slot_at()` | Add `cache: CacheMode`, forward to read/write helpers |
| `ensure_capacity()` | Add `cache: CacheMode`, forward to write helpers |
| `pyramid_reserve()` | Add `cache: CacheMode`, forward to `ensure_capacity(...)` |
| `pyramid_truncate()` | Add `cache: CacheMode`, forward to `deallocate_tree(...)` etc. |
| `resolve_redir_block()` | Add `cache: CacheMode`, forward to `layer2.read_block(...)` |
| `read_comp_redir()` | Add `cache: CacheMode`, forward to `layer2.read_block(...)` |
| `compress_cblock_data()` | Add `cache: CacheMode`, forward for redirector write |
| `pyramid_read_compressed()` | Add `cache: CacheMode`, forward through call chain |
| `pyramid_write_compressed()` | Add `cache: CacheMode`, forward through call chain |

### 5b: Update `PyramidOps` trait

Add `cache: CacheMode` parameter to `init_leaf_block()`, `collect_leaf_blocks_for_dealloc()`,
and `collect_leaf_blocks_verify()`. Update both `UncompressedOps` and `CompressedOps`
implementations to forward it.

## Step 6: Update `StreamLayer` impl call sites

**File:** `yak/src/streams_from_blocks.rs` — the `impl StreamLayer` block

### Streams stream operations → `CacheMode::Shared`

- `read_descriptor()` → pass `CacheMode::Shared` to `pyramid_read()`
- `write_descriptor()` → pass `CacheMode::Shared` to `pyramid_write()`
- `create_stream()` → pass `CacheMode::Shared` to `pyramid_write()` (line ~1971)
- `delete_stream()` → pass `CacheMode::Shared` to `deallocate_tree()` (lines ~2135, 2140)

### User stream operations → `CacheMode::ThreadLocal`

- `read()` / stream read path → pass `CacheMode::ThreadLocal` to `pyramid_read()` /
  `pyramid_read_compressed()`
- `write()` / stream write path → pass `CacheMode::ThreadLocal` to `pyramid_write()` /
  `pyramid_write_compressed()`
- `truncate()` → pass `CacheMode::ThreadLocal` to `pyramid_truncate()`
- `reserve()` → pass `CacheMode::ThreadLocal` to `pyramid_reserve()`

### Remove all `invalidate_block_cache()` calls

Delete all 7 occurrences (lines 1684, 1924, 2005, 2024, 2043, 2080, 2111).

## Step 7: Update doc comments

- `BlocksInFile` struct doc: mention both thread-local and shared caches
- `BlockLayer` trait `read_block`/`write_block` docs: document `CacheMode` semantics
- Remove references to `invalidate_block_cache` from doc comments and architecture

## Step 8: Build, clippy, and test

- `cargo build` across all workspace crates (`yak`, `yak_cl`, `yak_c`, `yak_python`)
- `cargo clippy` — fix any warnings (no `#[allow()]`)
- `cargo fmt`
- Run the full `yak_pytest` test suite to verify correctness
- Run existing Rust unit tests

No new tests strictly required — this is a correctness-preserving cache optimisation.
Existing tests cover the Streams stream operations (create/open/close/delete/read/write)
and multi-stream scenarios. If any test fails, it would indicate a cache coherency bug in
the new design.

---

## Risk assessment

- **Low risk**: The change is mechanical. Every `cache: true` becomes either
  `CacheMode::Shared` or `CacheMode::ThreadLocal` depending on context. Every
  `cache: false` becomes `CacheMode::None`. No new logic is introduced.
- **Correctness**: Write-through semantics on the shared cache ensure all threads see
  fresh Streams data without explicit invalidation. Per-block eviction on alloc/dealloc
  handles block recycling for both caches.
- **Performance**: Net positive. User stream redirector blocks stay cached across
  Streams lock acquisitions. The shared cache Mutex is fast (memory-only critical section).
- **Scope**: Changes confined entirely to the `yak` crate. No API changes for
  `yak_cl`, `yak_c`, `yak_python`.
