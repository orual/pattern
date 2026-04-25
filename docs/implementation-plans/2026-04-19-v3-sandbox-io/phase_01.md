# Phase 1: SyncedDoc + DirWatcher shared core

**Goal:** Extract the doc-sync and directory-watching primitives currently embedded in `pattern_memory`'s block subscriber into a pair of generic types. `DirWatcher<R>` owns a notify-debouncer + ingest thread for a single root directory and delegates event handling to a pluggable `EventRouter`. `SyncedDoc<B>` owns the two-doc CRDT machinery for a single file and accepts either a `DirWatcher` subscription or a standalone per-file watcher. Refactor the block subscriber and the mount watcher to be concrete instantiations of these primitives; introduce `LoroSyncedFile` (opaque text + `DirWatcher<PathFanoutRouter>`) as the other instantiation, which Phase 2's `FileHandler` consumes.

**Architecture:** Two orthogonal primitives:

1. **`DirWatcher<R>`** — one `notify_debouncer_full::Debouncer` per root directory, one ingest thread that drains debounced events and calls `R::handle(events)`. Router trait is intentionally tiny (one method) so routing logic is injected rather than inherited. Two router implementations ship in this phase: `PathFanoutRouter` (exact-path → `crossbeam_channel::Sender`, for file callers) and `BlockFanoutRouter` (stem → block_id lookup + `cache.apply_external_edit`, ports existing mount-watcher behaviour).
2. **`SyncedDoc<B>`** — one `LoroDoc memory_doc` (caller-supplied) + one `LoroDoc disk_doc` (owned) + mtime/blake3 echo suppression + a subscription to external-change events for its file. Two constructors: `open_with_subscription` (receives events from an externally-owned `DirWatcher<PathFanoutRouter>`, for pool usage) and `open_standalone` (spawns its own single-file `DirWatcher<PathFanoutRouter>` internally, for tests and one-off usage). The `LoroDocBridge` trait abstracts schema-specific `render(doc)` + `apply_external(doc, bytes)`.

Block subscriber becomes `SyncedDoc<BlockSchemaBridge>` + `DirWatcher<BlockFanoutRouter>`. LoroSyncedFile is `SyncedDoc<TextBridge>` + (via FileManager in Phase 2) a pooled `DirWatcher<PathFanoutRouter>`. Block-specific outer scaffolding (heartbeat, FTS5, reembed, quiesce pause/resume) stays in `subscriber/worker.rs` and calls into SyncedDoc.

**Tech Stack:** Rust 1.83+, `loro = "1.10"`, `notify = "8"`, `notify-debouncer-full = "0.5"`, `blake3`, `crossbeam-channel = "0.5"`, `tokio-util` (CancellationToken), `thiserror`, `smol_str` (zero-alloc file extensions — already a workspace dep; **add to `crates/pattern_memory/Cargo.toml` in Task 1**).

**Scope:** Phase 1 of 5 from `docs/design-plans/2026-04-19-v3-sandbox-io.md`. Pure `pattern_memory` work — no `pattern_runtime` or `pattern_core` changes. No dependency on Plan 3.

**Codebase verified:** 2026-04-24. Plan 1 (v3-memory-rework) is fully shipped (646/646 tests). Existing primitives identified:
- `pattern_memory/src/subscriber/worker.rs` (2152 lines) — schema-aware block sync worker; the parts moving into `SyncedDoc<BlockSchemaBridge>` are the two-doc machinery, `render_canonical_from_disk_doc`, echo tracking, and atomic-write.
- `pattern_memory/src/fs/watcher.rs` (432 lines) — current `MountWatcher`: recursive notify-debouncer on a mount root + `ingest_loop` (`is_block_path`, `block_id_from_path`, `is_self_echo`, format validation, `cache.apply_external_edit`). These routing responsibilities move into `BlockFanoutRouter`.
- `pattern_memory/src/cache.rs:809-1044` — `MemoryCache::apply_external_edit` (per-schema parse + disk_doc apply + memory_doc import). The per-schema match body moves into `apply_block_external_edit`; the cache method becomes a thin lookup+delegate.
- `pattern_memory/src/fs.rs:29-52` — `atomic_write` helper (already public, reused as-is).
- `pattern_memory/src/subscriber.rs:50-78` — `SubscriberHandle` (cancel, thread, event_tx, disk_doc, last_written_mtime, paused, pause_complete, resume_signal). The `disk_doc`/`last_written_mtime` fields are the ones SyncedDoc subsumes; the pause/resume fields stay on the handle.

**External-dep verification:** `loro 1.10`, `notify 8`, `notify-debouncer-full 0.5` in `Cargo.toml`. Latest published is `loro 1.11.1` / `notify-debouncer-full 0.7.0`, bumping out of scope. Relevant APIs stable across: `LoroDoc::get_text("content")`, `text.update(&str, UpdateOptions)`, `doc.export(ExportMode::updates(&vv))`, `doc.import(bytes)`, `doc.subscribe_local_update(cb)`, `notify_debouncer_full::new_debouncer(timeout, tick, cb)`, `Debouncer::watch(path, RecursiveMode)`.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-sandbox-io.AC1: LoroSyncedFile infrastructure
- **v3-sandbox-io.AC1.1 Success:** `LoroSyncedFile::open(path)` reads file content into a LoroDoc and starts a notify-watcher subscription
- **v3-sandbox-io.AC1.2 Success:** `write(content)` updates the LoroDoc and writes to disk; file content matches
- **v3-sandbox-io.AC1.3 Success:** External edit to a watched file triggers `on_external_change()` which merges via loro CRDT; both the agent's edits and the external edits are preserved
- **v3-sandbox-io.AC1.4 Success:** Self-emit-echo detection: agent write → file change → watcher fires → content hash match → no redundant merge triggered
- **v3-sandbox-io.AC1.5 Success:** `close()` drops the LoroDoc and unsubscribes the watcher; no resources leaked
- **v3-sandbox-io.AC1.6 Failure:** Opening a nonexistent file returns `FileError::NotFound(path)` (here `LoroSyncError::NotFound`; Phase 2's `FileError` wraps it)
- **v3-sandbox-io.AC1.7 Edge:** Concurrent edits by agent and external process to different regions of the same file merge cleanly (both changes preserved, no data loss)
- **v3-sandbox-io.AC1.8 Edge:** Concurrent edits to the same region merge via loro CRDT semantics (last-writer-wins per character position, deterministic)

---

## Subcomponent layout

- **A (tasks 1-5): Generic `DirWatcher<R>` + `SyncedDoc<B>` + `TextBridge`/`LoroSyncedFile`.** Net-new module, no behaviour change to existing code.
- **B (tasks 6-8): Refactor the block path onto the generic primitives.** Introduce `BlockSchemaBridge` + `BlockFanoutRouter`; refactor `SyncWorker` and `MountWatcher` to compose them. Behaviour-preserving — all 646 existing tests must still pass.

Subcomponent A's tests (Task 5) close AC1.1-1.8. Subcomponent B is a refactor; verification is the full `just pre-commit-all` suite plus the existing block/mount/cache tests.

---

<!-- START_SUBCOMPONENT_A (tasks 1-5) -->

<!-- START_TASK_1 -->
### Task 1: `loro_sync` module skeleton — traits, errors, types

**Files:**
- Create: `crates/pattern_memory/src/loro_sync/mod.rs` — module root + re-exports.
- Create: `crates/pattern_memory/src/loro_sync/bridge.rs` — `LoroDocBridge` trait + `BridgeError`.
- Create: `crates/pattern_memory/src/loro_sync/router.rs` — `EventRouter` trait.
- Create: `crates/pattern_memory/src/loro_sync/error.rs` — `SyncedDocError` + `LoroSyncError` alias.
- Modify: `crates/pattern_memory/Cargo.toml` — add `smol_str = { workspace = true }` under `[dependencies]`. Verify with `grep -n 'smol_str' crates/pattern_memory/Cargo.toml` first — if it's already present (transitively elevated by a later phase), skip the addition (M20 fix).
- Modify: `crates/pattern_memory/src/lib.rs:17-37` — add `pub mod loro_sync;` between `jj` and `modes`.

**Implementation:**

```rust
// loro_sync/bridge.rs
use std::path::Path;
use loro::LoroDoc;
use smol_str::SmolStr;

/// Pluggable schema/format adapter for a `SyncedDoc`.
///
/// One bridge per concrete representation: `TextBridge` for opaque file
/// content, `BlockSchemaBridge` for memory-block schemas. Bridges are
/// stateless adapters — schema configuration lives on `Self`; per-doc
/// state lives on the SyncedDoc.
pub trait LoroDocBridge: Send + Sync + 'static {
    /// Render `disk_doc` to the canonical on-disk bytes. Returns
    /// `(file_extension_without_dot, bytes)`. The extension is `SmolStr`
    /// so bridges can use `SmolStr::new_static("md")` with zero allocation
    /// for compile-time-known constants.
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError>;

    /// Apply external file `content` to `disk_doc` as Loro operations.
    /// `path` is diagnostic context only. Caller (SyncedDoc) handles
    /// exporting disk_doc's new ops and importing into memory_doc.
    fn apply_external(
        &self,
        disk_doc: &LoroDoc,
        content: &[u8],
        path: &Path,
    ) -> Result<(), BridgeError>;

    /// Initial population of memory_doc + disk_doc from file bytes at open
    /// time. Default impl delegates to `apply_external` against both docs.
    fn seed(
        &self,
        memory_doc: &LoroDoc,
        disk_doc: &LoroDoc,
        content: &[u8],
        path: &Path,
    ) -> Result<(), BridgeError> {
        self.apply_external(disk_doc, content, path)?;
        self.apply_external(memory_doc, content, path)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    #[error("invalid utf-8 from file {path}: {source}")]
    Utf8 { path: std::path::PathBuf, source: std::str::Utf8Error },
    #[error("parse failed for {path}: {message}")]
    Parse { path: std::path::PathBuf, message: String },
    #[error("loro operation failed: {0}")]
    Loro(String),
    #[error("render failed: {0}")]
    Render(String),
}
```

```rust
// loro_sync/router.rs
use notify_debouncer_full::DebouncedEvent;

/// Pluggable event routing strategy for `DirWatcher`. Called from the
/// ingest thread with a batch of debounced events. Implementations decide
/// what to do — fanout to per-path subscribers (PathFanoutRouter), dispatch
/// to a block cache (BlockFanoutRouter), etc.
///
/// Must be `Send` because it runs on a dedicated thread. No `Sync` bound
/// because `handle(&mut self, ...)` gives exclusive access per call.
pub trait EventRouter: Send + 'static {
    fn handle(&mut self, events: Vec<DebouncedEvent>);
}
```

```rust
// loro_sync/error.rs
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncedDocError {
    #[error("file not found: {0}")]
    NotFound(PathBuf),
    #[error("io error on {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("watcher setup failed for {path}: {message}")]
    Watcher { path: PathBuf, message: String },
    #[error("bridge failure: {0}")]
    Bridge(#[from] super::bridge::BridgeError),
    #[error("doc closed")]
    Closed,
}

pub type LoroSyncError = SyncedDocError;
```

```rust
// loro_sync/mod.rs
//! CRDT-backed file sync primitives. Shared by the block subscriber and
//! the `FileHandler`'s `FileManager` coordinator.

pub mod bridge;
pub mod dir_watcher;     // Task 2
pub mod error;
pub mod router;
pub mod routers;          // Task 2 (PathFanoutRouter); Task 6 (BlockFanoutRouter)
pub mod synced_doc;       // Task 3
pub mod text;             // Task 4

pub use bridge::{BridgeError, LoroDocBridge};
pub use dir_watcher::{DirWatcher, DirWatcherConfig};
pub use error::{LoroSyncError, SyncedDocError};
pub use router::EventRouter;
pub use routers::PathFanoutRouter;
pub use synced_doc::{SyncedDoc, SyncedDocConfig, ExternalChangeEvent};
pub use text::{LoroSyncedFile, TextBridge};
```

Submodules `dir_watcher`, `synced_doc`, `text`, `routers` start as stubs that compile (e.g., `pub struct DirWatcher<R>(std::marker::PhantomData<R>);`). Tasks 2-4 fill them in.

**Verifies:** None (scaffolding).

**Verification:**
- `cargo check -p pattern-memory`.
- `cargo nextest run -p pattern-memory --lib` — no new tests, existing 646 must pass.

**Commit:** `[pattern-memory] loro_sync module skeleton — bridge + router + error types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `DirWatcher<R>` primitive + `PathFanoutRouter`

**Files:**
- Modify: `crates/pattern_memory/src/loro_sync/dir_watcher.rs` — full impl.
- Modify: `crates/pattern_memory/src/loro_sync/routers.rs` — `PathFanoutRouter` impl (the exact-path → sender fanout).

**Reference:** `crates/pattern_memory/src/fs/watcher.rs:51-99` (current MountWatcher debouncer setup + ingest thread scaffolding). This task's `DirWatcher` is the generalisation of that scaffolding.

**Implementation:**

```rust
// loro_sync/dir_watcher.rs
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;
use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use crossbeam_channel::{bounded, Sender};
use tokio_util::sync::CancellationToken;
use crate::loro_sync::{EventRouter, SyncedDocError};

pub struct DirWatcherConfig {
    pub root: PathBuf,
    pub recursive: RecursiveMode,   // NonRecursive for per-dir file pool; Recursive for mount
    pub debounce: Duration,          // default 500ms (matches existing MountWatcher)
    pub channel_bound: usize,        // default 256
}

pub struct DirWatcher {
    /// Holds the underlying `notify::RecommendedWatcher` and its background
    /// thread. Dropped on close.
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    _ingest_thread: JoinHandle<()>,
    cancel: CancellationToken,
}

impl DirWatcher {
    /// Start a directory watcher. The `router` runs on a dedicated OS
    /// thread named `dir-watcher:<root-basename>`; it is moved in and
    /// exclusively owned by the thread.
    pub fn start<R: EventRouter>(
        cfg: DirWatcherConfig,
        mut router: R,
    ) -> Result<Self, SyncedDocError> {
        let (tx, rx) = bounded::<Vec<notify_debouncer_full::DebouncedEvent>>(cfg.channel_bound);

        let mut debouncer = new_debouncer(
            cfg.debounce,
            None,
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    let _ = tx.try_send(events);
                }
            },
        ).map_err(|e| SyncedDocError::Watcher {
            path: cfg.root.clone(),
            message: e.to_string(),
        })?;

        debouncer.watch(&cfg.root, cfg.recursive).map_err(|e| SyncedDocError::Watcher {
            path: cfg.root.clone(),
            message: e.to_string(),
        })?;

        let cancel = CancellationToken::new();
        let cancel_thread = cancel.clone();
        let root_name = cfg.root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("root")
            .to_string();

        let ingest_thread = std::thread::Builder::new()
            .name(format!("dir-watcher:{root_name}"))
            .spawn(move || {
                while let Ok(events) = rx.recv() {
                    if cancel_thread.is_cancelled() { break; }
                    router.handle(events);
                }
            })
            .map_err(|e| SyncedDocError::Io { path: cfg.root.clone(), source: e })?;

        Ok(DirWatcher { _debouncer: debouncer, _ingest_thread: ingest_thread, cancel })
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Sender is owned by debouncer closure; dropping _debouncer drops
        // the sender, which causes the ingest thread's recv() to fail and
        // the thread to exit.
    }
}
```

```rust
// loro_sync/routers.rs
use std::path::PathBuf;
use std::sync::Arc;
use dashmap::DashMap;
use crossbeam_channel::Sender;
use notify_debouncer_full::DebouncedEvent;
use crate::loro_sync::EventRouter;

/// Exact-path fanout router. Subscribers register `(path, sender)`; for
/// each debounced event, events whose `paths` contain a subscribed path
/// are forwarded to the matching sender. Events on unsubscribed paths
/// are dropped silently.
///
/// Used by the `FileHandler`'s `FileManager` (Phase 2): one
/// `DirWatcher<PathFanoutRouter>` per parent directory, multiple
/// `SyncedDoc` instances subscribing to exact file paths within.
#[derive(Clone, Default)]
pub struct PathFanoutRouter {
    inner: Arc<PathFanoutInner>,
}

#[derive(Default)]
struct PathFanoutInner {
    subscribers: DashMap<PathBuf, Sender<DebouncedEvent>>,
}

impl PathFanoutRouter {
    pub fn new() -> Self { Self::default() }

    /// Register a subscription for `path`. Returns a guard that removes
    /// the subscription on drop. Sender is the caller's side of a
    /// crossbeam channel.
    pub fn subscribe(&self, path: PathBuf, sender: Sender<DebouncedEvent>)
        -> PathFanoutSubscription
    {
        self.inner.subscribers.insert(path.clone(), sender);
        PathFanoutSubscription { inner: Arc::clone(&self.inner), path }
    }
}

impl EventRouter for PathFanoutRouter {
    fn handle(&mut self, events: Vec<DebouncedEvent>) {
        for debounced in events {
            for path in &debounced.event.paths {
                if let Some(sender) = self.inner.subscribers.get(path) {
                    // try_send: if a subscriber is slow, drop the event
                    // rather than block the whole router. Subscribers should
                    // size their channel for a typical edit burst.
                    let _ = sender.try_send(debounced.clone());
                }
            }
        }
    }
}

pub struct PathFanoutSubscription {
    inner: Arc<PathFanoutInner>,
    path: PathBuf,
}

impl Drop for PathFanoutSubscription {
    fn drop(&mut self) {
        self.inner.subscribers.remove(&self.path);
    }
}
```

**Note on `DirWatcher` holding `R` generically.** The router is moved into the ingest thread, so `DirWatcher` itself doesn't need `R` in its type — the phantom parameter is unnecessary and complicates ownership. `DirWatcher::start<R>` is generic on the constructor only; the returned `DirWatcher` is monomorphic.

**Verifies:** Mechanism for AC1.1, AC1.3 (watcher delivery).

**Verification:**
- `cargo check -p pattern-memory`.
- Unit tests in `loro_sync/dir_watcher.rs`:
    - `dir_watcher_routes_events_to_subscriber` — start `DirWatcher<PathFanoutRouter>` on a tempdir, subscribe to `tempdir/foo.txt`, `std::fs::write(foo.txt, "hello")`, assert the subscriber's receiver gets an event within 2s.
    - `dir_watcher_drops_unsubscribed_events` — no subscription → no delivery (use atomic counter + sleep to verify).
    - `subscription_drop_removes_entry` — subscribe, drop guard, write to the file, assert no event delivered.
    - `multiple_subscribers_in_same_dir` — two `.txt` files in one dir, each with its own subscription; writes to each only fire that subscriber's channel.
- `cargo nextest run -p pattern-memory --lib loro_sync::dir_watcher`.

**Commit:** `[pattern-memory] DirWatcher + PathFanoutRouter primitives`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `SyncedDoc<B>` core — open/write/close + echo suppression

**Files:**
- Modify: `crates/pattern_memory/src/loro_sync/synced_doc.rs` — full impl.

**Reference:** `crates/pattern_memory/src/subscriber/worker.rs` (two-doc model, local-update subscribe, import-export pattern around lines 350-500). `crates/pattern_memory/src/fs.rs:29-52` (atomic_write). `crates/pattern_memory/src/fs/watcher.rs:129-142` (is_self_echo via mtime).

**Implementation:**

`SyncedDoc<B>` owns the per-file machinery: memory_doc (caller-supplied Arc) + disk_doc (owned Arc) + local-update subscription (`_subscription: loro::Subscription`) + mtime/hash echo state + ingest thread that fans `IngestEvent`s into disk operations. It does NOT own a watcher — it consumes events from an externally-supplied `crossbeam_channel::Receiver<DebouncedEvent>` (from a `DirWatcher<PathFanoutRouter>` subscription held by the caller, or from its own standalone DirWatcher for tests).

```rust
pub struct SyncedDocConfig<B: LoroDocBridge> {
    pub path: PathBuf,
    pub memory_doc: Arc<LoroDoc>,
    pub bridge: Arc<B>,
    pub event_channel_bound: usize, // default 256
}

pub struct SyncedDoc<B: LoroDocBridge> {
    inner: Arc<SyncedDocInner<B>>,
}

struct SyncedDocInner<B: LoroDocBridge> {
    path: PathBuf,
    memory_doc: Arc<LoroDoc>,
    disk_doc: Arc<LoroDoc>,
    bridge: Arc<B>,
    last_written_mtime: Arc<Mutex<Option<SystemTime>>>,
    last_written_hash: Arc<Mutex<Option<[u8; 32]>>>,
    external_subscribers: Arc<Mutex<Vec<Sender<ExternalChangeEvent>>>>,
    cancel: CancellationToken,
    _ingest_thread: JoinHandle<()>,
    _local_update_sub: loro::Subscription,
    /// Only present for `open_standalone`; keeps the watcher alive.
    _standalone_watcher: Option<DirWatcher>,
    /// Only present for `open_with_subscription`; dropped on close to
    /// unregister from the external router.
    _fanout_guard: Option<PathFanoutSubscription>,
}

#[derive(Clone, Debug)]
pub struct ExternalChangeEvent {
    pub path: PathBuf,
    pub applied: bool,
}

impl<B: LoroDocBridge> SyncedDoc<B> {
    /// Open against an externally-owned `DirWatcher<PathFanoutRouter>`.
    /// `router.subscribe(path, tx)` is called internally — caller passes
    /// the router, receiver wiring is private.
    pub fn open_with_subscription(
        cfg: SyncedDocConfig<B>,
        router: &PathFanoutRouter,
    ) -> Result<Self, SyncedDocError> { /* … */ }

    /// Convenience: spawn a private single-file `DirWatcher<PathFanoutRouter>`
    /// on the file's parent directory and open against it. For tests and
    /// one-off usage; production code should use pooled `open_with_subscription`.
    pub fn open_standalone(cfg: SyncedDocConfig<B>) -> Result<Self, SyncedDocError> { /* … */ }

    pub fn write(&self, bytes: &[u8]) -> Result<(), SyncedDocError> { /* … */ }
    pub fn read(&self) -> Result<Vec<u8>, SyncedDocError> { /* … */ }
    pub fn subscribe_external_changes(&self)
        -> Receiver<ExternalChangeEvent> { /* … */ }

    pub fn path(&self) -> &Path { &self.inner.path }
    pub fn memory_doc(&self) -> &Arc<LoroDoc> { &self.inner.memory_doc }
    pub fn disk_doc(&self) -> &Arc<LoroDoc> { &self.inner.disk_doc }

    pub fn close(self) { /* cancel + drop */ }
}
```

**Open flow (`open_with_subscription`):**

1. Check `cfg.path.exists()` → `SyncedDocError::NotFound` if missing.
2. Read file bytes; `bridge.seed(memory_doc, disk_doc, &bytes, &path)`.
3. Compute initial blake3 hash + record mtime.
4. Create `crossbeam::bounded(cfg.event_channel_bound)` for the ingest channel.
5. `router.subscribe(cfg.path.clone(), ingest_tx)` → guard stored on `inner`.
6. `memory_doc.subscribe_local_update(move |bytes| { ingest_tx2.try_send(IngestEvent::LocalUpdate(bytes.to_vec())); })` — stash the subscription on `inner` (dropping it unsubscribes).
7. Spawn ingest thread.

**Ingest thread:** handles `LocalUpdate(bytes)` and `External(DebouncedEvent)` + synchronous `SyncWrite { bytes, reply }`. Same shape as previously described — see inline comments in the code.

**LocalUpdate processing:**
- `disk_doc.import(&bytes)`. Ignore errors (log); continue.
- `bridge.render(&disk_doc)` → `(ext, rendered_bytes)`.
- `atomic_write(&path, &rendered_bytes)`.
- Record `std::fs::metadata(&path)?.modified()?` as `last_written_mtime`.
- Record `blake3::hash(&rendered_bytes).into()` as `last_written_hash`.

**External processing:**
- For events whose `event.kind` is `Modify(_)` or `Create(_)` with `paths` containing exactly `cfg.path`:
  - `file_mtime = metadata.modified()?` — if equal to `*last_written_mtime`, skip (mtime echo).
  - `bytes = std::fs::read(&path)?`; `hash = blake3::hash(&bytes)` — if equal to `*last_written_hash`, skip (content echo; handles `touch`).
  - `oplog_vv_before = disk_doc.oplog_vv();`
  - `bridge.apply_external(&disk_doc, &bytes, &path)?`; `disk_doc.commit();`
  - `let update = disk_doc.export(ExportMode::updates(&oplog_vv_before));`
  - `memory_doc.import(&update)` — CRDT merge preserves both sides.
  - Fan `ExternalChangeEvent { path, applied: true }` to each `external_subscribers` sender via `try_send` (bounded channels; if a subscriber is slow, drop).

**SyncWrite processing:** same as LocalUpdate but bytes come from caller; `reply.send(result)` on completion.

**write():** enqueues `SyncWrite { bytes, reply_tx }`, blocks on `reply_rx.recv()`, returns. Gives callers a fence — return value guarantees disk has been updated.

**Verifies:** AC1.1, AC1.2, AC1.3, AC1.4, AC1.5, AC1.6, AC1.7, AC1.8 mechanism. Tests in Task 5 exercise these.

**Verification:**
- `cargo check -p pattern-memory`.

**Commit:** `[pattern-memory] SyncedDoc — two-doc CRDT sync with injected event subscription`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: `TextBridge` + `LoroSyncedFile`

**Files:**
- Modify: `crates/pattern_memory/src/loro_sync/text.rs` — full impl.

**Implementation:**

```rust
// loro_sync/text.rs
use std::path::{Path, PathBuf};
use std::sync::Arc;
use loro::LoroDoc;
use smol_str::SmolStr;
use crossbeam_channel::Receiver;
use crate::loro_sync::{
    BridgeError, ExternalChangeEvent, LoroDocBridge, LoroSyncError,
    PathFanoutRouter, SyncedDoc, SyncedDocConfig,
};

/// Opaque-text bridge: file content as a single `LoroText` at root key
/// `"content"`. Render returns the text bytes verbatim under the configured
/// extension. Apply-external calls `text.update(content_str)` (loro's
/// Myers-diff text update).
pub struct TextBridge {
    extension: SmolStr,
}

impl TextBridge {
    /// Construct with a statically-known extension (zero alloc):
    /// `TextBridge::new(SmolStr::new_static("md"))`.
    pub fn new(extension: SmolStr) -> Self { Self { extension } }

    /// Convenience: derive the extension from a path.
    pub fn from_path(path: &Path) -> Self {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(SmolStr::from)
            .unwrap_or_else(|| SmolStr::new_static("txt"));
        Self { extension: ext }
    }
}

impl LoroDocBridge for TextBridge {
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError> {
        let text = disk_doc.get_text("content").to_string();
        Ok((self.extension.clone(), text.into_bytes()))
    }

    fn apply_external(&self, disk_doc: &LoroDoc, content: &[u8], path: &Path)
        -> Result<(), BridgeError>
    {
        let s = std::str::from_utf8(content)
            .map_err(|e| BridgeError::Utf8 { path: path.to_owned(), source: e })?;
        disk_doc.get_text("content")
            .update(s, Default::default())
            .map_err(|e| BridgeError::Loro(format!("text.update failed: {e}")))?;
        Ok(())
    }
}

/// Public file-oriented wrapper around `SyncedDoc<TextBridge>`.
/// Keeping it as a newtype (not a `pub type` alias) lets us add
/// file-specific methods without leaking the SyncedDoc generic into
/// the FileHandler signatures. Phase 2's FileManager consumes this.
pub struct LoroSyncedFile {
    inner: SyncedDoc<TextBridge>,
}

impl LoroSyncedFile {
    /// Open against a pooled `DirWatcher<PathFanoutRouter>` (production path).
    /// Phase 2's FileManager owns the router.
    pub fn open_with_router(path: impl Into<PathBuf>, router: &PathFanoutRouter)
        -> Result<Self, LoroSyncError>
    {
        let path: PathBuf = path.into();
        if !path.exists() { return Err(LoroSyncError::NotFound(path)); }
        let bridge = Arc::new(TextBridge::from_path(&path));
        let memory_doc = Arc::new(LoroDoc::new());
        let inner = SyncedDoc::open_with_subscription(
            SyncedDocConfig {
                path,
                memory_doc,
                bridge,
                event_channel_bound: 256,
            },
            router,
        )?;
        Ok(Self { inner })
    }

    /// Open with a private per-file watcher (standalone / test usage).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, LoroSyncError> {
        let path: PathBuf = path.into();
        if !path.exists() { return Err(LoroSyncError::NotFound(path)); }
        let bridge = Arc::new(TextBridge::from_path(&path));
        let memory_doc = Arc::new(LoroDoc::new());
        let inner = SyncedDoc::open_standalone(SyncedDocConfig {
            path,
            memory_doc,
            bridge,
            event_channel_bound: 256,
        })?;
        Ok(Self { inner })
    }

    pub fn read(&self) -> Result<String, LoroSyncError> {
        let bytes = self.inner.read()?;
        String::from_utf8(bytes).map_err(|e| LoroSyncError::Bridge(BridgeError::Utf8 {
            path: self.inner.path().to_owned(),
            source: e.utf8_error(),
        }))
    }

    pub fn write(&self, content: &str) -> Result<(), LoroSyncError> {
        self.inner.write(content.as_bytes())
    }

    pub fn subscribe_external_changes(&self) -> Receiver<ExternalChangeEvent> {
        self.inner.subscribe_external_changes()
    }

    pub fn path(&self) -> &Path { self.inner.path() }
    pub fn close(self) { self.inner.close() }
}
```

**Verifies:** AC1.1 (public API), AC1.6 (NotFound on missing path).

**Verification:**
- `cargo check -p pattern-memory`.

**Commit:** `[pattern-memory] TextBridge + LoroSyncedFile`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Tests for AC1.1-1.8

**Files:**
- Create: `crates/pattern_memory/src/loro_sync/tests.rs`.
- Modify: `crates/pattern_memory/src/loro_sync/mod.rs` — add `#[cfg(test)] mod tests;`.

**Template:** the existing `fs/watcher.rs` tests at lines 286-431 use real notify events, real file edits, and condition-based waits. Follow that pattern. No mocked filesystem, no mocked notify.

**Tests (one per AC, using `LoroSyncedFile::open` standalone mode to keep tests self-contained):**

| AC | Test name | Behaviour |
|----|-----------|-----------|
| 1.1 | `open_seeds_doc_and_starts_watcher` | Write `"hello"` to tempfile; `LoroSyncedFile::open` succeeds; `read()` returns `"hello"`; external edit fires `subscribe_external_changes` event. |
| 1.2 | `write_updates_doc_and_disk` | Open empty tempfile; `write("agent content")`; disk content and `read()` both match. |
| 1.3 | `external_edit_merges_into_doc` | Open tempfile `"abc\n"`; agent `write("abcXYZ\n")`; external `std::fs::write` with `"abc\ndef\n"`; wait for merge; assert final content has both XYZ and def (loro CRDT). |
| 1.4 | `self_echo_is_suppressed` | Open tempfile; subscribe; `write("once")`; wait 750ms; assert no event arrived (mtime + hash dedupe). |
| 1.5 | `close_drops_watcher_and_doc` | Open tempfile; close; external edit after close; wait; verify subscribe receiver is disconnected (channel closed) — no panics. Double-close is a no-op. |
| 1.6 | `open_nonexistent_returns_not_found` | `LoroSyncedFile::open("/tmp/nope-<rand>")` → `Err(LoroSyncError::NotFound(_))`. |
| 1.7 | `concurrent_edits_different_regions_merge` | Tempfile `"line1\nline2\nline3\n"`; agent writes `"line1-EDITED\nline2\nline3\n"`; external writes `"line1\nline2\nline3-EDITED\n"`; both EDITED tokens preserved post-merge. |
| 1.8 | `concurrent_edits_same_region_lww_per_position_deterministic` | Tempfile `"abcdef"`; agent writes `"aXcdef"`; external writes `"abcdYf"`; wait; assert deterministic result via `insta::assert_snapshot!`. Regression lock — first run verifies experimentally, subsequent runs guard against loro-version drift. |

**Async-event wait helper** (to avoid raw `sleep`):

```rust
fn wait_for<F: Fn() -> bool>(deadline: Duration, check: F) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if check() { return true; }
        std::thread::sleep(Duration::from_millis(25));
    }
    check()
}
```

5-second deadlines. Tests that race are real bugs, not flakes — do not weaken.

**Additional test on the router:** `dir_watcher_with_path_fanout_round_trip` in `dir_watcher.rs` (Task 2) — covered there, not repeated here.

**Verifies:** AC1.1, AC1.2, AC1.3, AC1.4, AC1.5, AC1.6, AC1.7, AC1.8.

**Verification:**
- `cargo nextest run -p pattern-memory --lib loro_sync::tests`.
- Run with `--test-threads 4` to surface global-state cross-talk.

**Commit:** `[pattern-memory] tests for LoroSyncedFile (AC1.1-1.8)`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 6-8) -->

<!-- START_TASK_6 -->
### Task 6: `BlockSchemaBridge` + `BlockFanoutRouter` — port existing block-watcher logic

**Files:**
- Create: `crates/pattern_memory/src/subscriber/bridge.rs` — `BlockSchemaBridge`.
- Modify: `crates/pattern_memory/src/loro_sync/routers.rs` — add `BlockFanoutRouter` alongside `PathFanoutRouter`.
- Modify: `crates/pattern_memory/src/subscriber.rs:28-31` — add `pub mod bridge;`.

**`BlockSchemaBridge`:** wraps the existing `render_canonical_from_disk_doc(disk_doc, &schema)` (now called from the bridge's `render`) plus a new free function `apply_block_external_edit(disk_doc, &schema, content, path)` extracted from the per-schema match arms currently in `MemoryCache::apply_external_edit:841-1044`. The extraction is mechanical — each match arm becomes the corresponding arm in the free function, with `String` errors replaced by `BridgeError` variants.

```rust
// subscriber/bridge.rs
pub struct BlockSchemaBridge { schema: BlockSchema }
impl BlockSchemaBridge {
    pub fn new(schema: BlockSchema) -> Self { Self { schema } }
    pub fn schema(&self) -> &BlockSchema { &self.schema }
}
impl LoroDocBridge for BlockSchemaBridge {
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError> {
        let (ext, bytes) = crate::subscriber::worker::render_canonical_from_disk_doc(disk_doc, &self.schema)
            .map_err(BridgeError::Render)?;
        Ok((SmolStr::new(ext), bytes))  // one alloc per render; acceptable
        // Alternative: change render_canonical_from_disk_doc to return SmolStr
        // directly. Out of scope for this task; optional follow-up.
    }
    fn apply_external(&self, disk_doc: &LoroDoc, content: &[u8], path: &Path)
        -> Result<(), BridgeError>
    {
        apply_block_external_edit(disk_doc, &self.schema, content, path)
    }
}
pub(crate) fn apply_block_external_edit(
    disk_doc: &LoroDoc,
    schema: &BlockSchema,
    content: &[u8],
    path: &Path,
) -> Result<(), BridgeError> {
    // BODY: port the existing per-schema match arms from
    // crates/pattern_memory/src/cache.rs:841-1044. One arm per BlockSchema
    // variant (Text, Map, Composite, List, Log, TaskList, Skill). String
    // errors in the original become BridgeError::Utf8 / Parse / Loro variants
    // here. The body is mechanical translation; do not ship a `todo!()` —
    // implement fully in this task. (See phase 1 task 8 regression sweep.)
    unimplemented!(
        "TASK 6 IMPLEMENTOR: port cache.rs:841-1044 per-schema arms here. \
         Do NOT leave this unimplemented!() in a commit — task 8 regression \
         sweep verifies no `todo!`/`unimplemented!` lingers in pattern_memory."
    )
}
```

**`BlockFanoutRouter`:** holds `Arc<MemoryCache>` + the existing block-routing logic from `fs/watcher.rs:144-244` (`is_block_path`, `block_id_from_path`, `is_self_echo`, per-extension format validation, `cache.apply_external_edit(block_id, content)`). This is a direct port — rename the free function `ingest_loop` into `BlockFanoutRouter::handle`.

```rust
// loro_sync/routers.rs (addition)
pub struct BlockFanoutRouter {
    cache: Arc<crate::cache::MemoryCache>,
}

impl BlockFanoutRouter {
    pub fn new(cache: Arc<crate::cache::MemoryCache>) -> Self { Self { cache } }
}

impl EventRouter for BlockFanoutRouter {
    fn handle(&mut self, events: Vec<DebouncedEvent>) {
        // BODY: port the existing ingest_loop from
        // crates/pattern_memory/src/fs/watcher.rs:144-244. Steps:
        //   1. Filter events to Modify/Create.
        //   2. Filter paths via is_block_path (.md | .kdl | .jsonl).
        //   3. Extract block_id = path.file_stem().
        //   4. Look up subscriber; is_self_echo via mtime → skip if echo.
        //   5. Read file; validate format (parse as KDL/JSONL, or passthrough for MD).
        //   6. self.cache.apply_external_edit(block_id, content).
        // The is_block_path / block_id_from_path / is_self_echo helpers
        // move here (or stay pub(crate) in fs/watcher.rs; task implementor
        // picks one — both are fine, neither is a stub).
        // Do NOT leave this unimplemented!() in a commit — task 8
        // regression sweep verifies no `todo!`/`unimplemented!` lingers.
        unimplemented!("TASK 6 IMPLEMENTOR: port the ingest_loop body here")
    }
}
```

**Verifies:** None directly; Task 8's regression sweep proves the port.

**Verification:**
- `cargo check -p pattern-memory`.
- `cargo nextest run -p pattern-memory` — all existing tests still pass (the new types are not yet wired in — that's Task 7).

**Commit:** `[pattern-memory] BlockSchemaBridge + BlockFanoutRouter port`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Wire `SyncedDoc<BlockSchemaBridge>` + `DirWatcher<BlockFanoutRouter>` into the block subscriber

**Files:**
- Modify: `crates/pattern_memory/src/subscriber/worker.rs` (substantial refactor — see scope).
- Modify: `crates/pattern_memory/src/fs/watcher.rs` (`MountWatcher::start`) — becomes a thin wrapper that constructs `DirWatcher::start(cfg, BlockFanoutRouter::new(cache))`.
- Modify: `crates/pattern_memory/src/cache.rs:809-1044` — `apply_external_edit` becomes a thin lookup + delegate to the SyncedDoc of the identified block.

**Worker refactor scope:** same shape as previously described.

- **Stays in `SyncWorker`/`SubscriberHandle`:** OS thread, WorkerConfig, SubscriberHandle (unchanged public fields), pause/resume signalling (quiesce machinery), heartbeat emission, FTS5 update via `update_block_preview`, reembed queue push, cancellation token check.
- **Moves into `SyncedDoc<BlockSchemaBridge>`:** two-doc model, `last_written_mtime` + `last_written_hash`, `atomic_write` on local updates, schema-aware render (via bridge), schema-aware external-edit apply (via bridge), local-update subscription on memory_doc.

**Watcher ownership for the block path:** the block subscriber's `SyncedDoc` does NOT own a watcher. The mount's single `DirWatcher<BlockFanoutRouter>` (replacement for the existing `MountWatcher`) is the sole filesystem watcher; it routes events by `block_id` (via stem lookup) to `cache.apply_external_edit`, which now delegates to the appropriate `SyncedDoc`'s external-edit handler (exposed via a new method `SyncedDoc::apply_external_from_router(bytes)`). The local-update side (agent writes propagating to disk) continues to work via the `subscribe_local_update` hook wired inside SyncedDoc. This avoids any double-watching: blocks are routed by `block_id` not exact path, so the BlockFanoutRouter owns path→block_id resolution; the SyncedDoc just handles bytes.

Concretely, add to `SyncedDoc`:

```rust
impl<B: LoroDocBridge> SyncedDoc<B> {
    /// External-edit path for cases where the watcher is not owned by this
    /// SyncedDoc (e.g., block subscriber using mount-wide MountWatcher).
    /// Caller already did any schema-specific path filtering; content is
    /// the raw file bytes. Runs synchronously — appropriate for callers
    /// that already batch or defer.
    pub fn apply_external_bytes(&self, content: &[u8]) -> Result<(), SyncedDocError> {
        /* same logic as the ingest thread's External branch, minus the
           watcher interaction. Does mtime+hash echo check, bridge apply,
           export/import, fanout event. */
    }
}
```

Cache's `apply_external_edit` becomes:

```rust
pub(crate) fn apply_external_edit(&self, block_id: &str, content: &[u8]) {
    let Some(subscriber) = self.subscribers.get(block_id) else { return };
    let Some(synced_doc) = &subscriber.synced_doc else { return };
    if let Err(e) = synced_doc.apply_external_bytes(content) {
        tracing::warn!(block_id = %block_id, error = %e, "external edit apply failed");
    }
    // FTS5 + reembed are triggered from the worker's subscribe_external_changes
    // listener on the SyncedDoc, not from cache directly.
}
```

SubscriberHandle gains `synced_doc: Option<Arc<SyncedDoc<BlockSchemaBridge>>>`. The existing `disk_doc` field can either stay (redundant but cheap — same Arc held twice) or be removed if no external consumer depends on it. Check quiesce code (`quiesce.rs`) for uses.

**Pause/resume interaction:** the worker's pause flag continues to gate FTS5/reembed work; the SyncedDoc keeps running through pauses. After resume, the worker processes any backlog from SyncedDoc's external-changes channel as a batch.

**Verifies:** AC1 mechanism through the block path.

**Verification:**
- `cargo check -p pattern-memory`.
- `cargo nextest run -p pattern-memory` — **all 646 existing tests must still pass.** This is the gate. Any failure means behaviour changed — fix the regression, do not weaken the test.
- `cargo clippy -p pattern-memory --all-targets` — no new warnings.

**Commit:** `[pattern-memory] block subscriber + MountWatcher on SyncedDoc + DirWatcher primitives`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Workspace-wide regression sweep

**Files:** no code changes. Verification-only gate.

**Verification:** `just pre-commit-all`. `cargo fmt --check`, `cargo clippy --all-features --all-targets`, `cargo nextest run`, `cargo test --doc`. All must pass.

Any failing test gets fixed at root cause, not weakened. Per `~/.claude/CLAUDE.md`: "It is virtually never out of scope to investigate and fix a failing test."

**Verifies:** AC1 end-to-end through the block path.

**Commit:** Only if incidental fixes are made — `[pattern-memory] regression fixes from SyncedDoc + DirWatcher refactor`.
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_B -->

---

## Open questions for human review (foreground at end of plan-write)

**Q1: `render_canonical_from_disk_doc` return type.** Currently `(&'static str, Vec<u8>)`. Bridge wraps as `(SmolStr, Vec<u8>)` with `SmolStr::new(ext)` — one alloc per render. Alternative: change the existing function to return `SmolStr` directly (adjust all internal call sites in the worker). Low-churn either way; defaulted to the wrap. Flag if reviewer wants the root-level change.

**Q2: `SubscriberHandle.disk_doc` field retention.** After Task 7, the handle's `disk_doc: Arc<LoroDoc>` is a redundant alias of `synced_doc.disk_doc()`. Keeping it is a no-op cost; removing it requires checking `quiesce.rs` + any external consumers. Defaulted to keep; reviewer may prefer the cleanup.

**Q3: Naming.** `SyncedDoc<B>` + `DirWatcher<R>` + `LoroDocBridge` + `EventRouter` + `PathFanoutRouter` + `BlockFanoutRouter` + `TextBridge` + `BlockSchemaBridge`. That's eight types across two generics. Alternative namings: `DocSync`, `FileWatcher`, `Bridge`, `Router`. Defaulted to the descriptive forms; reviewer may prefer terser.

**Q4: Per-file watcher fallback for `open_standalone`.** Phase 1 Task 3 ships both a pooled and a standalone constructor. Standalone is used only in Phase 1 tests — production always uses the pool (via FileManager in Phase 2). Ship standalone anyway for clean testability, or only pooled? Defaulted to both; reviewer may prefer cut.
