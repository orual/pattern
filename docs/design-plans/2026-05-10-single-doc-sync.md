# Single-Doc Sync Architecture — Design Sketch

**Date:** 2026-05-10  
**Status:** sketch

## Problem

Today `SyncedDoc` carries two `LoroDoc` instances — `memory_doc` (live view returned by `read()`) and `disk_doc` (backing-store target for renders/writes). External-edit handling reconciles them manually via `bridge.apply_external(disk_doc, ...)` followed by a separate import into memory_doc.

Symptoms:
- **`Memory.append` silently loses content** despite the handler explicitly calling `mark_dirty` then `persist_block`. System-reminder reports `content unchanged from previous snapshot` even when the API returned ok. Reproduced twice in 2026-05-09 / 2026-05-10 sessions (partner-notes naming addition; wake-notes #agents forum content).
- **`Memory.put` works reliably** as workaround. `put` goes through `upsert_block_content` which exercises a different sync path.
- **`File.forceWrite` exhibits the same family.**

Hypothesis: `doc.append/replace_text` mutate one `LoroDoc` (through `StructuredDocument.doc.get_text("content").insert/splice`), but the persist path may render from a different doc state. With two docs and an explicit reconciliation step, it's easy to miss the export/import sync — and the failure mode is silent.

Plus: writes deferred-batch at turn close, so within a turn the agent's read-after-write returns stale content. Multi-step memory work fights the model.

## Proposal

1. **Collapse `memory_doc` and `disk_doc` into a single `doc: Arc<LoroDoc>`** on `SyncedDoc`. Track "last persisted state" with `last_saved_frontier: Mutex<Option<VersionVector>>` instead of a parallel doc.
2. **Make Memory.* and File.* writes synchronous at the disk layer.** Every mutating handler renders bytes from the doc and atomic-writes to disk synchronously, updating frontier + echo-suppression state. The handler still calls `mark_dirty` / `persist_block` for the layer-2 (pattern_db) push — those stay; only the disk-sync part becomes inline.
3. **Adopt rebase-before-write** for the concurrent-external-edit case. LoroDoc's CRDT merge handles this natively; just need a check before atomic_write.

## Single-Doc Shape

```rust
pub struct SyncedDoc<B: LoroDocBridge> { inner: Arc<Inner<B>> }

struct Inner<B> {
    bridge: B,
    path: PathBuf,
    /// Source of truth. Agent ops AND external CRDT-merged edits land here.
    /// Plain LoroDoc — Loro is internally Arc'd for cheap sharing, and
    /// `inner` is already `Arc<Inner<B>>`, so wrapping doc in another Arc
    /// is redundant indirection.
    doc: LoroDoc,
    /// Doc's oplog_vv() at last successful disk write.
    last_saved_frontier: Mutex<Option<VersionVector>>,
    /// blake3 of last bytes we wrote. dir_watcher uses this to skip echoes.
    last_written_hash: Mutex<Option<[u8; 32]>>,
    last_written_mtime: Mutex<Option<SystemTime>>,
    /// Held across rebase-import + render + atomic_write so external edits
    /// can't race with local writes.
    write_lock: Mutex<()>,
    /// existing fan-out plumbing
    write_subscribers: Mutex<Vec<Sender<WriteNotification>>>,
    external_change_subscribers: Mutex<Vec<Sender<ExternalChangeEvent>>>,
}
```

## Key Operations

**`read()`** — render from `doc`. single source of truth. no doc-pair to reconcile.

**`write_local()`** — synchronous-write primitive. caller has already mutated `doc` via Loro ops:
1. acquire `write_lock`
2. `rebase_against_disk_if_needed()` — if disk hash != `last_written_hash`, import disk bytes as CRDT update into `doc` (Loro merges)
3. render canonical bytes from `doc`
4. `atomic_write(path, bytes)`
5. update `last_written_hash`, `last_written_mtime`, `last_saved_frontier = doc.oplog_vv()`
6. fan out write notification (FTS5, re-embed)

**`apply_external_bytes()`** (dir_watcher path):
1. compute hash; if matches `last_written_hash`, skip (echo)
2. acquire `write_lock`
3. `bridge.apply_external(&doc, content, &path)` — merges into the one doc
4. notify external_change subscribers

**`has_unsaved_edits()`**: `doc.oplog_vv() != last_saved_frontier`

## Bridge Trait

Today: `render(disk_doc)`, `apply_external(disk_doc, ...)`. After: rename param to `doc`. Trait shape unchanged otherwise.

## Handler Changes

Current Append shape:
```rust
doc.append(&content, false)?;
adapter.mark_dirty(&scope, &label)?;
adapter.persist_block(&scope, &label)?;
```

After:
```rust
doc.append(&content, false)?;          // CRDT op applied to THE doc; SyncedDoc.write_local is invoked
                                       // synchronously inside the doc method (or via an explicit
                                       // call), so disk is up-to-date before the handler returns.
adapter.mark_dirty(&scope, &label)?;   // unchanged — layer-2 (pattern_db) bookkeeping
adapter.persist_block(&scope, &label)?;// unchanged — pushes CRDT update blob + preview to DB
```

The disk-sync (layer 3) becomes inline. The DB push (layer 2) is unchanged. Same for Replace, SetField, Delete, UpdateDesc, Pin, Unpin, Create. File handlers same simplification — `forceWrite`'s disk-sync becomes inline.

## Bug Localization (added 2026-05-10 after reading apply_external)

TextBridge::apply_external does the right thing CRDT-wise — it calls `update_by_line` which computes a Loro-native line-by-line diff and applies it as ops. Not a snapshot-replace.

synced_doc::apply_external (the private helper) does proper reconciliation: export updates from disk_doc since `oplog_vv_before`, then `memory_doc.import(&update)`. That's Loro-native CRDT merge, not snapshot replacement. **In principle**, this should not lose data.

But it loses data anyway in practice. The race:

1. Agent: `Memory.append("foo")` → memory_doc gets op A. version vector advances.
2. Handler: `persist()` → op A exported to pattern_db ✓
3. Subscriber worker notified; will render memory_doc → write file. **Debounced 50ms.**
4. **Before** the subscriber's write reaches disk, dir_watcher fires for some reason (out-of-band read/write, previous-write echo not yet recorded, FS event timing variance, parallel tool).
5. dir_watcher reads disk bytes — **still the old content**, because the subscriber hadn't flushed yet.
6. `apply_external_bytes(stale_disk_bytes)` runs:
   - `bridge.apply_external(disk_doc, stale_bytes, path)` → `update_by_line` on disk_doc, diffing against disk_doc's CURRENT state. Since disk_doc doesn't have op A (memory_doc has it; disk_doc only gets it via subscriber render+write+seed), the resulting CRDT ops on disk_doc may include deletions of content that op A added but disk_doc never saw.
   - export disk_doc's new ops; import into memory_doc.
   - **The imported updates can cancel op A** because they encode "the disk content does not have 'foo'" expressed as Loro ops.
7. memory_doc loses "foo". Agent reads stale. System-reminder reports `content unchanged`.

**Named:** "two-doc reconciliation can synthesize phantom deletions when dir_watcher reads disk before the subscriber has flushed local ops."

The bridge is correct. The system around it races. Single-doc dissolves the race because:
- there is no disk_doc→memory_doc gap window where local ops live in only one of them
- `rebase_before_write` sees external changes via hash mismatch and imports as CRDT update into THE doc; merge preserves both sides via CRDT semantics
- no debounced subscriber sitting between the agent's mutation and the disk write

## Architectural Framing (added 2026-05-10 after orual correction)

The right mental model: **there is one LoroDoc per block/file.** It can be held by multiple references (cache, StructuredDocument, SyncedDoc, subscribers all carry refs to the same doc), temporarily forked for explicit divergence (spawn forks, what-if branches), or replicated across daemon instances (future work) — but the DEFAULT is one doc identity, many refs.

Today we have an accidental second doc identity (`disk_doc`) that exists purely as bookkeeping for "what's on disk." That bookkeeping should be a version vector, not a parallel doc.

**This sharpens the bug hypothesis.** The likely failure mode for `Memory.append` silent loss:

1. agent mutates `StructuredDocument.doc` (memory_doc identity) — version advances
2. `persist()` exports the update blob to pattern_db — DB has the new ops ✓
3. before the subscriber renders + writes the file, the dir_watcher fires (own-write echo not yet recorded, or external edit timing)
4. dir_watcher reads disk bytes (which DON'T have the new ops yet, because the render hasn't flushed)
5. `bridge.apply_external(disk_doc, stale_bytes)` runs against `disk_doc` — disk_doc now reflects stale disk
6. reconciliation imports disk_doc state into memory_doc — if `apply_external` did a snapshot-replace rather than a CRDT import-update, memory_doc loses the local ops
7. agent's read returns stale content; system-reminder reports `content unchanged`

With ONE doc identity, this loop cannot exist:
- there's no separate disk_doc to reconcile from
- `apply_external` on the single doc must be a CRDT import (Loro `import_updates` of update bytes), and Loro's merge cannot drop committed local ops
- the worst case becomes: external bytes don't have local ops → import → merge resolves correctly → ops still present

## Doc Identity Across Layers (the new shape)

```
                          ┌─────────────┐
                          │  LoroDoc    │   ← one identity per block/file
                          └──────┬──────┘
             ┌───────────┬───────┼───────┬──────────────┐
             │           │       │       │              │
       StructuredDoc  cache    SyncedDoc  subscriber   future:
       (handler-      (DB     (disk      (FTS5,        replication,
        facing API)    push)   sync)      re-embed)    spawn fork
```

Forks are explicit operations that create a new doc identity from the current one (Loro supports this). Multi-daemon sync uses CRDT export/import between distinct doc identities. Neither is happening implicitly via memory_doc/disk_doc anymore.

## Scope Clarification (added 2026-05-10 after deeper read)

There are THREE persistence layers, not two:

1. **In-memory LoroDoc** — live state, mutated by `doc.append/replace_text/setField`.
2. **`pattern_db` CRDT update log** — `cache.persist()` exports update blobs from the doc and stores them via `pattern_db::queries::store_update`, plus updates block preview. **This is what `mark_dirty` / `persist_block` are actually for, and is NOT what this design changes.** Recovery, history, sync, and any future replication all key on this layer.
3. **Rendered file on disk** — the SyncedDoc/subscriber path. The two-LoroDoc model lives HERE. **This is what we're collapsing.**

**Revised proposal:**
- Collapse two-LoroDoc → one LoroDoc inside `SyncedDoc` (layer 3).
- Keep `cache.persist()` / `mark_dirty` for layer 2 (pattern_db push). Their contract is unchanged: take a block label, push pending CRDT updates to the DB.
- Make handler calls synchronous in the sense that ALL THREE layers settle before the handler returns (mutate doc, push to DB, write to disk). Currently the disk write is debounced by the subscriber worker; that's where the read-after-write staleness comes from.

**Open bug-localization question:** the partner-notes / wake-notes silent-loss might not be in layer 3 at all. `persist()` short-circuits when `doc.current_version() == last_persisted_frontier`. If `doc.append` somehow doesn't advance the version vector (or if the StructuredDocument the handler mutates isn't the same instance the cache sees), persist is a no-op and the change is stranded in a LoroDoc nobody reads from. Worth confirming this before assuming the layer-3 collapse fixes the bug.

Diagnostic to run: a deterministic test that calls `Memory.append`, then immediately checks via the same cache instance: `doc.current_version()`, `last_persisted_frontier`, and the rendered preview. If version_vector didn't advance after append, the bug is upstream of persist.

## Migration Plan

Direct refactor, not feature-flagged. The two-doc code is proven buggy; keeping it parallel to the new code means maintaining two impls + double the test surface, with no real safety benefit. Tests catch regressions; git rollback exists if catastrophic.

Steps (rough order; some are fluid):

1. Audit callsites of `memory_doc()` / `disk_doc()` accessors across the codebase. Bound the blast radius before cutting.
2. Read `subscriber/worker.rs` to understand the 50ms debounce role and where it lives in the new shape.
3. Refactor `SyncedDoc`: collapse to one `doc: LoroDoc` field; add `last_saved_frontier`. Drop `disk_doc`. Rename `memory_doc` → `doc` everywhere internal. Adjust accessors (likely just one `doc()` returning `&LoroDoc`).
4. Implement `write_local` (synchronous render + rebase + atomic_write + frontier/hash update) and `rebase_against_disk_if_needed`.
5. Inline the bridge.apply_external + commit + import-into-doc into the new `apply_external_bytes` (no more separate disk_doc→memory_doc reconciliation step).
6. Update bridge trait param names (cosmetic; `disk_doc` → `doc`).
7. Update callers — handlers in `pattern_runtime/src/sdk/handlers/{memory,file}.rs` swap their disk-sync call site from the old write path to the new `write_local`. Layer-2 `mark_dirty` / `persist_block` calls unchanged.
8. Adjust subscriber/worker to use the single doc.
9. Update / rewrite tests in `pattern_memory/src/loro_sync/tests.rs`. Add new tests for the failure modes:
   - `append_then_get_returns_appended_content` (deterministic regression for the known bug)
   - `replace_then_get_returns_replaced_content`
   - `concurrent_external_write_merges_on_local_write` (rebase-before-write)
   - `local_write_does_not_lose_unrelated_external_change`
   - `dir_watcher_echoes_skipped`
   - `has_unsaved_edits_reflects_frontier`
10. (optional cleanup) Audit whether `mark_dirty` is still pulling weight as a separate call from `persist_block`. If not, fold it. **`persist_block` stays** — it's the layer-2 DB push primitive.

## Tests

Concrete tests covering observed failure modes:
- `append_then_get_returns_appended_content` (the partner-notes / wake-notes bug, deterministic)
- `replace_then_get_returns_replaced_content`
- `setfield_then_getfield_roundtrip`
- `concurrent_external_write_merges_on_local_write` (rebase-before-write)
- `local_write_does_not_lose_unrelated_external_change`
- `dir_watcher_echoes_skipped` (last_written_hash echo suppression)
- `has_unsaved_edits_reflects_frontier`

Existing pattern_memory loro_sync tests should mostly survive — bridge interface doesn't change shape.

## Open Questions

1. **Subscriber worker (`subscriber/worker.rs`) — RESOLVED, shrinks ~70%.** Today the worker plays three roles: (a) import memory_doc updates into disk_doc, (b) render disk_doc → bytes, (c) write_rendered + echo bookkeeping, then (d) FTS5 + re-embed side effects. With synchronous `write_local` on SyncedDoc, (a)/(b)/(c) all dissolve — agent ops apply directly to THE doc; write_local handles render+write synchronously inside the doc method. The 50ms debounce moves from "writes" to "post-write side effects" (FTS5+reembed) — which is where it belongs semantically. Worker becomes: subscribe to write notifications, debounce for coalescing, trigger FTS5/re-embed pipeline. Three callsites in `cache.rs` (lines 1050, 4538, 4666) that clone `disk_doc` for the worker setup also disappear.
2. **Mount-layer / router:** `pattern_memory/src/mount/` and `pattern_memory/src/loro_sync/router.rs` may have other consumers. Migration step 6 needs an audit.
3. **VCS/backup:** `pattern_memory/src/{backup.rs,vcs.rs,jj.rs}` — do any peek at disk_doc separately from memory_doc?

## Side Effects

- Per-op write latency goes up by one atomic_write per mutation instead of one per turn. Fine for agent ergonomics; might affect bulk-import paths if any exist.
- fsync amortization worse under heavy bulk writes. If a path does many writes in a tight loop, add `batch_write` API holding write_lock across multiple ops, atomic_write once at end. Add only if benchmarks show need.

## What This Fixes

- `Memory.append` silent loss — structurally impossible (one doc, one source of truth)
- `File.forceWrite` similar family — same fix
- read-after-write within a turn returns fresh content — agent mental model matches reality
- merge semantics on external edit — LoroDoc-native instead of manual two-doc reconciliation
