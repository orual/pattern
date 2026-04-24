# Phase 2: File handler + FileManager

**Goal:** Replace the `FileHandler` stub with a real implementation dispatching into a per-session `FileManager` coordinator. FileManager uses Phase 1's pooled `DirWatcher<PathFanoutRouter>` primitive — one `PathFanoutRouter` shared session-wide and one `DirWatcher` per unique parent directory, lazily created and GC'd. Open files get a `LoroSyncedFile`; watch-only paths get a direct router subscription (no LoroDoc). External edits surface as attachments on the agent's next turn through the same composer step that delivers memory-block snapshots.

**Architecture:** `FileHandler` implements `EffectHandler<SessionContext>` (tightened from the stub's `HasCancelState` bound — matches `SkillsHandler` at `crates/pattern_runtime/src/sdk/handlers/skills.rs:76`). It dispatches `FileReq` variants to `cx.user().file_manager()`. FileManager is `Arc<FileManager>` held on `SessionContext` (new field). Internal state: one shared `PathFanoutRouter`, a `DashMap<PathBuf /* canonical parent dir */, Arc<DirWatcher>>` for lazily-created per-directory watchers, a `DashMap<PathBuf, Arc<LoroSyncedFile>>` of open files, a `DashMap<PathBuf, PathFanoutSubscription>` for watch-only paths, and a compiled `FilePolicy` (ordered rules, last-match-wins, default-deny). Config-KDL shape detection gates writes to pattern-reserved configs through `PermissionBroker`. **External-edit notifications use the canonical pseudo-message pipeline** (`MemoryStoreAdapter::record_pseudo_message` → `TurnOutput::pseudo_messages` → `Segment2Pass::recent_pseudo_messages`), the same mechanism `Pattern.Skills.Load` uses; FileManager's listener threads call `pattern_provider::compose::pseudo_messages::render_file_edit_event(...)` and push the resulting `ChatMessage` via the adapter. **No new `MessageAttachment` variant is introduced** — the existing pipeline already carries handler-originated reminders into segment 2.

**Tech Stack:** Rust, `loro`, `notify` (via Phase 1 primitive), `tidepool_effect`, `knus` (already a dep — persona loader), `globset` (**new workspace dep**), `kdl` (already a transitive dep via knus), `dashmap`, `thiserror`, `tempfile` (tests).

**Scope:** Phase 2 of 5. Depends on Phase 1 (`SyncedDoc`, `DirWatcher`, `PathFanoutRouter`, `LoroSyncedFile`) and on **Plan 3 (v3-multi-agent) Phase 1 being landed** for `CapabilitySet` and per-instance `PermissionBroker`. User confirmed this sequencing — Phase 2 execution parks until Plan 3 Phase 1 lands.

**Codebase verified:** 2026-04-24. Evidence:
- `FileHandler` stub at `crates/pattern_runtime/src/sdk/handlers/file.rs:14-52`.
- `FileReq` enum at `crates/pattern_runtime/src/sdk/requests/file.rs:1-14` — three variants; needs `Open`/`Close`/`Watch` added.
- Template handler with `SessionContext` bound: `crates/pattern_runtime/src/sdk/handlers/skills.rs:76-141`.
- SdkBundle HList: `crates/pattern_runtime/src/sdk/bundle.rs:40-57`; FileHandler at tag 10, no position change.
- `SessionContext`: `crates/pattern_runtime/src/session.rs:40-121` + accessors. Adding `file_manager()` accessor.
- `PersonaSnapshot` at `crates/pattern_core/src/types/snapshot.rs`. Adding `open_files: Vec<PathBuf>`.
- System-reminder mechanism: handler-originated reminders use `MemoryStoreAdapter::record_pseudo_message(ChatMessage)` (introduced by previous work — see `pattern_runtime/src/memory/adapter.rs`, `pattern_core/src/types/turn.rs`, `pattern_runtime/src/agent_loop.rs`, `pattern_provider/src/compose/passes/segment_2.rs`). Pipeline: handler/listener pushes to adapter buffer → `agent_loop` step 5 drains into `TurnOutput::pseudo_messages` → `TurnHistory::most_recent_pseudo_messages()` → `Segment2Pass::new(..., recent_pseudo_messages, ...)` replays into segment 2. `Pattern.Skills.Load` is the canonical template (`crates/pattern_runtime/src/sdk/handlers/skills.rs` — search for `record_pseudo_message`); the renderer lives at `pattern_provider::compose::pseudo_messages::render_skill_loaded_event`. **Phase 2 mirrors this pattern** for file edits — adds `render_file_edit_event` to the same module, calls it from FileManager listener threads.
- KDL parsing: `crates/pattern_runtime/src/persona_loader.rs` (knus); config entry point `pattern_memory::config::pattern_kdl::PatternConfig`.
- `globset`: not yet workspace dep. Add in Task 1.
- `PermissionBroker` at `crates/pattern_core/src/permission.rs:54-100` — Plan 3 changes it to per-instance.

**Agent-facing Haskell helpers:** Per `crates/pattern_runtime/CLAUDE.md` SDK imports: agents use `Pattern.File` qualified (`File.read`, `File.write`, etc.). Each new variant requires (a) Rust enum variant with `#[core(module, name)]`, (b) updated `effect_decl()` constructors + helpers, (c) matching Haskell constructor added to `haskell/Pattern/File.hs`'s `File` GADT. Step (c) is done alongside (a)/(b); no separate SDK module rewrite.

---

## Acceptance Criteria Coverage

### v3-sandbox-io.AC2: File handler
- **v3-sandbox-io.AC2.1 Success:** `File.Read(path)` returns file contents without creating a LoroDoc; subsequent external edits do not generate notifications
- **v3-sandbox-io.AC2.2 Success:** `File.Open(path)` creates a LoroSyncedFile, auto-subscribes to change notifications, returns current content
- **v3-sandbox-io.AC2.3 Success:** `File.Write(path, content)` on an open file goes through loro; on an unopened file, writes directly
- **v3-sandbox-io.AC2.4 Success:** `File.Close(path)` drops LoroSyncedFile; subsequent external edits do not generate notifications
- **v3-sandbox-io.AC2.5 Success:** `File.List(path, "*.rs")` returns matching files in directory with correct metadata
- **v3-sandbox-io.AC2.6 Success:** `File.Watch(path)` subscribes to change notifications without creating a LoroDoc (lighter weight than Open)
- **v3-sandbox-io.AC2.7 Success:** External edit to an open file produces a system reminder with the diff in the agent's next turn
- **v3-sandbox-io.AC2.8 Failure:** `File.Write` to a path outside allowed directories returns `FileError::PermissionDenied` with the denied path and the applicable deny rule
- **v3-sandbox-io.AC2.9 Failure:** `File.Write` to a file that parses as pattern config KDL triggers human approval via PermissionBroker; write blocked until approved
- **v3-sandbox-io.AC2.10 Edge:** KDL config deny rule `/project/.env` blocks writes to that path even when `/project/` is in the allow list — **implementation note:** this plan evaluates rules in declaration order with last-match-wins (see Task 3). AC2.10's "deny evaluated first" is satisfied when the deny is declared after the allow (the natural writing order for the allowlist-with-carve-out case). Tests also cover the inverted "broad-deny + narrow-allow" case and the nested re-allow case, which ordered evaluation supports and strict deny-first would not.
- **v3-sandbox-io.AC2.11 Edge:** Session serialization records open file paths; on session resume, files are re-opened with fresh LoroDoc (no LoroDoc state persisted)

---

## Subcomponent layout

- **A (tasks 1-3): Types + policy.** FileReq expansion, FileError, FilePolicy with ordered-rule KDL parsing, globset dep.
- **B (tasks 4-6): FileManager + SessionContext wiring.** Pooled DirWatcher, open/close/read/write/list/watch entry points, config-KDL shape detection, capability gates.
- **C (tasks 7-8): FileHandler implementation + system reminder pipeline.**
- **D (tasks 9-10): Session state serialization + test suite.**

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Expand `FileReq`; update `effect_decl()` + Haskell GADT

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/file.rs:1-14` — add `Open`, `Close`, `Watch` variants; expand `ListDir` to carry a glob argument.
- Modify: `crates/pattern_runtime/src/sdk/handlers/file.rs:17-34` — update `effect_decl()` constructors + helpers.
- Modify: `crates/pattern_runtime/haskell/Pattern/File.hs` — add matching GADT constructors.
- Modify: `crates/pattern_runtime/src/sdk/requests.rs` parity table (investigator identified at lines 44-330) — add entries for the new variants.

**Implementation:**

```rust
#[derive(Debug, FromCore)]
pub enum FileReq {
    #[core(module = "Pattern.File", name = "Read")]
    Read(String),
    #[core(module = "Pattern.File", name = "Write")]
    Write(String, String),
    #[core(module = "Pattern.File", name = "ListDir")]
    ListDir(String, String), // (path, glob) — empty glob treated as "*"
    #[core(module = "Pattern.File", name = "Open")]
    Open(String),
    #[core(module = "Pattern.File", name = "Close")]
    Close(String),
    #[core(module = "Pattern.File", name = "Watch")]
    Watch(String),
}
```

`ListDir` now takes a glob argument (AC2.5 requires glob filtering). The default `""` means `*`.

`effect_decl()` constructors additions:
```
"Open    :: Path -> File Content",
"Close   :: Path -> File ()",
"Watch   :: Path -> File ()",
"ListDir :: Path -> GlobPattern -> File [FileInfo]",
"type GlobPattern = Text  -- empty means \"*\"",
"type FileInfo = Text     -- JSON: {path:Path, size:Int, mtime:Text, is_dir:Bool}",
```

Helpers (added to `helpers` list):
```
"open :: Member File effs => Path -> Eff effs Content\nopen p = send (Open p)",
"close :: Member File effs => Path -> Eff effs ()\nclose p = send (Close p)",
"watch :: Member File effs => Path -> Eff effs ()\nwatch p = send (Watch p)",
```

Existing `read`/`write` helpers stay; `listDir` gains the glob argument.

**Verifies:** Signature enablement for AC2.2, AC2.4, AC2.5, AC2.6.

**Verification:**
- `cargo check -p pattern-runtime`.
- `cargo nextest run -p pattern-runtime --lib sdk::requests::tests` — parity test passes for all six variants.
- `cargo nextest run -p pattern-runtime --lib sdk::bundle::tests` — `canonical_effect_decls()` returns 16 entries and `FileHandler::effect_decl()` parses cleanly.

**Commit:** `[pattern-runtime] expand FileReq with Open/Close/Watch + ListDir glob`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `FileError` + `FileInfo` + `file_manager` module scaffold

**Files:**
- Create: `crates/pattern_runtime/src/file_manager/mod.rs` — module root.
- Create: `crates/pattern_runtime/src/file_manager/error.rs`.
- Create: `crates/pattern_runtime/src/file_manager/types.rs` — `FileInfo` wire shape.
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod file_manager;`.

**Implementation:**

```rust
// file_manager/error.rs
use std::path::PathBuf;
use pattern_memory::loro_sync::LoroSyncError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    #[error("file not found: {0}")]
    NotFound(PathBuf),
    #[error("permission denied: {path} ({reason})")]
    PermissionDenied { path: PathBuf, reason: String },
    #[error("config-file write requires human approval: {path}")]
    ConfigApprovalRequired { path: PathBuf, matched_keys: Vec<String> },
    #[error("config-file write was denied by the human: {path}")]
    ConfigApprovalDenied { path: PathBuf },
    #[error("capability denied: File effect not in agent's CapabilitySet")]
    CapabilityDenied,
    #[error("io on {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("loro sync: {0}")]
    LoroSync(#[from] LoroSyncError),
    #[error("glob pattern invalid: {0}")]
    BadGlob(String),
    #[error("file not open: {0}")]
    NotOpen(PathBuf),
}

impl FileError {
    pub fn to_effect_message(&self) -> String {
        format!("Pattern.File: {self}")
    }
}
```

```rust
// file_manager/types.rs
use std::path::PathBuf;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: jiff::Timestamp,
    pub is_dir: bool,
}
```

**Verifies:** Scaffolding for AC2.8 (PermissionDenied variant) and AC2.9 (ConfigApprovalRequired).

**Verification:** `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] FileError + FileInfo types`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `FilePolicy` — KDL-backed ordered rules, last-match-wins, default-deny

**Files:**
- Create: `crates/pattern_runtime/src/file_manager/policy.rs`.
- Modify: `Cargo.toml` (workspace) — add `globset = "0.4"` under `[workspace.dependencies]`.
- Modify: `crates/pattern_runtime/Cargo.toml` — add `globset`.
- Modify: `crates/pattern_memory/src/config/pattern_kdl.rs` — add an optional `file-policy` block to `PatternConfig`.

**Evaluation model:** rules are evaluated in declaration order; **last matching rule decides**. No rule matches → default deny. This handles all three practical cases:

1. *Allowlist with carve-out:* `allow /project/**`, then `deny /project/.env` → `.env` denied, everything else under `/project` allowed.
2. *Denylist with carve-out:* `deny /project/**`, then `allow /project/notes/*.md` → notes accessible despite the broad deny.
3. *Nested re-allow/re-deny:* `allow /project/**`, `deny /project/secrets/**`, `allow /project/secrets/public.txt` → `public.txt` accessible; rest of `secrets/` blocked.

Mirrors gitignore / rsync `--filter` semantics. Chosen over specificity-scoring because it's predictable and auditable — reading top to bottom is the debug surface.

**KDL shape:**

```kdl
file-policy {
    allow "/project/**"
    deny "/project/.env"
    deny "/project/.aws/**"
    allow "/project/notes/*.md"   // re-allowed despite broader deny
}
```

Empty / missing block → default deny everything. Log a loud `tracing::warn!` at session open noting file ops will be universally denied until rules are added.

```rust
// file_manager/policy.rs
use std::path::{Path, PathBuf};
use globset::{Glob, GlobMatcher};
use crate::file_manager::error::FileError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode { Allow, Deny }

#[derive(Debug, Clone)]
struct Rule {
    mode: RuleMode,
    matcher: GlobMatcher,
    pattern: String, // original source for diagnostics
}

#[derive(Debug, Clone, Default)]
pub struct FilePolicy {
    rules: Vec<Rule>,
}

impl FilePolicy {
    /// Build from an ordered list of (mode, pattern) rules. Caller (KDL decoder)
    /// supplies them in declaration order.
    pub fn from_rules(rules: Vec<(RuleMode, String)>) -> Result<Self, FileError> {
        let compiled = rules.into_iter()
            .map(|(mode, pattern)| {
                let matcher = Glob::new(&pattern)
                    .map_err(|e| FileError::BadGlob(format!("{pattern}: {e}")))?
                    .compile_matcher();
                Ok(Rule { mode, matcher, pattern })
            })
            .collect::<Result<Vec<_>, FileError>>()?;
        Ok(Self { rules: compiled })
    }

    /// Last-match-wins evaluation. No match → default deny.
    pub fn check_access(&self, path: &Path) -> Result<(), FileError> {
        // Canonicalise so `/project/sub/../../etc` doesn't escape policy.
        // Fall back to as-given for files that don't exist yet (write-new).
        let check = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());

        let mut decision: Option<(usize, &Rule)> = None;
        for (idx, rule) in self.rules.iter().enumerate() {
            if rule.matcher.is_match(&check) {
                decision = Some((idx, rule));
            }
        }
        match decision {
            Some((_, r)) if r.mode == RuleMode::Allow => Ok(()),
            Some((idx, r)) => Err(FileError::PermissionDenied {
                path: check,
                reason: format!("denied by rule {idx}: {}", r.pattern),
            }),
            None => Err(FileError::PermissionDenied {
                path: check,
                reason: "no matching rule (default deny)".to_string(),
            }),
        }
    }

    pub fn default_deny_all() -> Self { Self::default() }
}
```

**KDL decoder** (in `pattern_memory::config::pattern_kdl`): knus's standard `children(name = "...")` does not preserve declaration order across different node names. Use a custom `Decode` impl that walks children in document order and emits `Vec<(RuleMode, String)>`:

```rust
#[derive(Debug, Default)]
pub struct FilePolicySection {
    pub rules: Vec<(RuleMode, String)>,
}

// Hand-rolled knus::Decode to preserve declaration order across mixed
// allow/deny nodes. ~25 lines; reference persona_loader's custom Decode
// impls for analogous patterns.
impl<S: knus::traits::ErrorSpan> knus::Decode<S> for FilePolicySection { /* … */ }
```

Wire into `PatternConfig`:
```rust
#[knus(child, default)]
pub file_policy: FilePolicySection,
```

(If knus has gained an ordered-children helper, prefer it over hand-rolled Decode; verify at execution time.)

**Verifies:** AC2.8 (default-deny error message + reason), AC2.10 (ordered re-allow/re-deny works).

**Verification:**
- `cargo check --workspace`.
- Unit tests in `file_manager/policy.rs`:
    - `last_match_wins_allow_then_deny` — `allow /project/**`, `deny /project/.env` → `.env` denied (reason names deny rule), `lib.rs` allowed (AC2.10 example 1).
    - `last_match_wins_deny_then_allow` — `deny /project/**`, `allow /project/notes/*.md` → `notes/foo.md` allowed despite broad deny (AC2.10 example 2).
    - `nested_re_allow_inside_re_deny` — three-rule scenario; `public.txt` accessible, `secrets/private.txt` denied (AC2.10 example 3).
    - `default_deny_when_no_rules` — empty policy denies every path with reason `"no matching rule (default deny)"`.
    - `default_deny_when_no_match` — non-empty policy with no matching rule denies identically.
    - `invalid_glob_fails_loudly` — `FileError::BadGlob` on malformed pattern.
    - `canonicalisation_resists_dotdot_escape` — `/project/../etc/passwd` rejected when only `/project/**` allowed.
    - `kdl_round_trip_preserves_order` — KDL-decode → `from_rules` → `check_access` matches a hand-built policy with identical rule sequence.
- `cargo nextest run -p pattern-runtime --lib file_manager::policy`.

**Commit:** `[pattern-runtime] FilePolicy with ordered rules, last-match-wins, default-deny`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

<!-- START_TASK_4 -->
### Task 4: `FileManager` core — pooled DirWatcher + read/write/open/close/list/watch

**Files:**
- Create: `crates/pattern_runtime/src/file_manager/manager.rs`.
- Modify: `crates/pattern_runtime/src/file_manager/mod.rs` — re-export `FileManager`.

**Structure:**

FileManager uses Phase 1's pooled-watcher primitive: one `PathFanoutRouter` shared session-wide + one `DirWatcher<PathFanoutRouter>` per unique parent directory, lazily created on first file access in that dir and GC'd on last close. This avoids N inotify watches when an agent opens N files in the same directory.

External edits are surfaced to the agent through the canonical pseudo-message pipe: each open/watched file's listener thread takes an `Arc<MemoryStoreAdapter>` (cloned at construction time) and calls `adapter.record_pseudo_message(render_file_edit_event(...))` — no FileManager-internal pending-edits queue, no separate composer splice point.

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use dashmap::DashMap;
use notify::RecursiveMode;
use tokio_util::sync::CancellationToken;
use pattern_core::permission::PermissionBroker;
use pattern_core::capability::CapabilitySet; // from Plan 3
use pattern_memory::loro_sync::{
    DirWatcher, DirWatcherConfig, LoroSyncedFile, PathFanoutRouter,
    PathFanoutSubscription,
};
use crate::memory::MemoryStoreAdapter;
use crate::file_manager::error::FileError;
use crate::file_manager::policy::FilePolicy;
use crate::file_manager::types::FileInfo;

#[derive(Clone, Copy, Debug)]
pub enum FileEditKind { Open, Watch }

pub struct FileManager {
    policy: FilePolicy,
    router: PathFanoutRouter,
    dir_watchers: DashMap<PathBuf, Arc<DirWatcher>>,
    open_files: DashMap<PathBuf, Arc<LoroSyncedFile>>,
    watch_only_paths: DashMap<PathBuf, PathFanoutSubscription>,
    /// One listener per open/watched file, bridging SyncedDoc change events
    /// or router subscriptions into the per-session adapter's pseudo-message
    /// buffer. Not filesystem watchers themselves — those are the pooled
    /// DirWatchers above.
    edit_listeners: DashMap<PathBuf, JoinHandle<()>>,
    /// Per-session adapter — cloned for each listener thread so reminders
    /// can be pushed via `record_pseudo_message`. Cheap clone (Arc).
    adapter: Arc<MemoryStoreAdapter>,
    capability_set: Arc<CapabilitySet>,
    permission_broker: Arc<PermissionBroker>,
    cancel: CancellationToken,
}

impl FileManager {
    pub fn new(
        policy: FilePolicy,
        adapter: Arc<MemoryStoreAdapter>,
        capability_set: Arc<CapabilitySet>,
        permission_broker: Arc<PermissionBroker>,
    ) -> Self {
        Self {
            policy,
            router: PathFanoutRouter::new(),
            dir_watchers: DashMap::new(),
            open_files: DashMap::new(),
            watch_only_paths: DashMap::new(),
            edit_listeners: DashMap::new(),
            adapter,
            capability_set,
            permission_broker,
            cancel: CancellationToken::new(),
        }
    }

    fn check_capability(&self) -> Result<(), FileError> {
        if !self.capability_set.has_file() {
            return Err(FileError::CapabilityDenied);
        }
        Ok(())
    }

    fn ensure_dir_watcher(&self, parent_dir: &Path) -> Result<Arc<DirWatcher>, FileError> {
        let canonical = std::fs::canonicalize(parent_dir)
            .unwrap_or_else(|_| parent_dir.to_owned());
        // Entry API avoids a race between get/insert when two files in the
        // same dir are opened concurrently.
        let arc = match self.dir_watchers.entry(canonical.clone()) {
            dashmap::mapref::entry::Entry::Occupied(e) => Arc::clone(e.get()),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let w = DirWatcher::start(
                    DirWatcherConfig {
                        root: canonical.clone(),
                        recursive: RecursiveMode::NonRecursive,
                        debounce: Duration::from_millis(500),
                        channel_bound: 256,
                    },
                    self.router.clone(),
                ).map_err(|err| FileError::Io {
                    path: canonical.clone(),
                    source: std::io::Error::other(err.to_string()),
                })?;
                let arc = Arc::new(w);
                e.insert(Arc::clone(&arc));
                arc
            }
        };
        Ok(arc)
    }

    /// Drop the DirWatcher for `parent_dir` if no open file or watch-only
    /// subscription still references it. Called from close paths.
    fn maybe_drop_dir_watcher(&self, parent_dir: &Path) {
        let canonical = std::fs::canonicalize(parent_dir)
            .unwrap_or_else(|_| parent_dir.to_owned());
        let still_used =
            self.open_files.iter().any(|e| e.key().parent() == Some(&canonical)) ||
            self.watch_only_paths.iter().any(|e| e.key().parent() == Some(&canonical));
        if !still_used {
            self.dir_watchers.remove(&canonical);
        }
    }

    pub fn read(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        if let Some(sf) = self.open_files.get(&canonical) {
            // Consistent view for open files — read from loro.
            Ok(sf.read()?.into_bytes())
        } else {
            std::fs::read(path).map_err(|e| FileError::Io { path: path.to_owned(), source: e })
        }
    }

    pub fn write(&self, path: &Path, content: &[u8]) -> Result<(), FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        if crate::file_manager::config_detect::is_pattern_config_write(path, content) {
            self.await_human_approval(path, content)?;
        }
        let canonical = canonicalize_best(path);
        if let Some(sf) = self.open_files.get(&canonical) {
            let s = std::str::from_utf8(content).map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
            sf.write(s)?;
            Ok(())
        } else {
            pattern_memory::fs::atomic_write(path, content).map_err(|e| FileError::Io {
                path: path.to_owned(),
                source: std::io::Error::other(e.to_string()),
            })
        }
    }

    pub fn open(&self, path: &Path) -> Result<Vec<u8>, FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        if self.open_files.contains_key(&canonical) {
            return self.read(path); // idempotent
        }
        let parent = canonical.parent().ok_or_else(|| FileError::Io {
            path: canonical.clone(),
            source: std::io::Error::other("path has no parent"),
        })?;
        self.ensure_dir_watcher(parent)?;
        let sf = LoroSyncedFile::open_with_router(&canonical, &self.router)?;
        let content = sf.read()?.into_bytes();

        // Bridge SyncedDoc external-change events → adapter pseudo-messages.
        // NOT a filesystem watcher — listens on an already-running crossbeam
        // channel from the pooled DirWatcher / SyncedDoc ingest thread.
        let rx = sf.subscribe_external_changes();
        let adapter = Arc::clone(&self.adapter);
        let cancel = self.cancel.clone();
        let path_owned = canonical.clone();
        let listener = std::thread::Builder::new()
            .name(format!("file-edit-listener:{}", canonical.display()))
            .spawn(move || {
                while let Ok(evt) = rx.recv() {
                    if cancel.is_cancelled() { break; }
                    if evt.applied {
                        // Build the canonical pseudo-message and push via the
                        // adapter — same pipeline as Pattern.Skills.Load.
                        // diff is None for now; Task 8 fills the diff payload.
                        let msg = pattern_provider::compose::pseudo_messages::
                            render_file_edit_event(
                                &path_owned,
                                FileEditKind::Open,
                                jiff::Timestamp::now(),
                                None, // diff filled in Task 8
                            );
                        adapter.record_pseudo_message(msg);
                    }
                }
            })
            .map_err(|e| FileError::Io { path: path.to_owned(), source: e })?;
        self.edit_listeners.insert(canonical.clone(), listener);
        self.open_files.insert(canonical, Arc::new(sf));
        Ok(content)
    }

    pub fn close(&self, path: &Path) -> Result<(), FileError> {
        let canonical = canonicalize_best(path);
        let Some((_, sf)) = self.open_files.remove(&canonical) else {
            return Err(FileError::NotOpen(canonical));
        };
        match Arc::try_unwrap(sf) {
            Ok(sf) => sf.close(),
            Err(_) => { /* still held elsewhere; SyncedDoc closes on final drop */ }
        }
        self.edit_listeners.remove(&canonical);
        if let Some(parent) = canonical.parent() {
            self.maybe_drop_dir_watcher(parent);
        }
        Ok(())
    }

    pub fn watch(&self, path: &Path) -> Result<(), FileError> {
        self.check_capability()?;
        self.policy.check_access(path)?;
        let canonical = canonicalize_best(path);
        if self.watch_only_paths.contains_key(&canonical) {
            return Ok(()); // idempotent
        }
        let parent = canonical.parent().ok_or_else(|| FileError::Io {
            path: canonical.clone(),
            source: std::io::Error::other("path has no parent"),
        })?;
        self.ensure_dir_watcher(parent)?;

        // Register a subscription on the shared router directly — no SyncedDoc.
        let (tx, rx) = crossbeam_channel::bounded(64);
        let subscription = self.router.subscribe(canonical.clone(), tx);

        let adapter = Arc::clone(&self.adapter);
        let cancel = self.cancel.clone();
        let path_owned = canonical.clone();
        let listener = std::thread::Builder::new()
            .name(format!("file-watch-listener:{}", canonical.display()))
            .spawn(move || {
                while let Ok(_evt) = rx.recv() {
                    if cancel.is_cancelled() { break; }
                    let msg = pattern_provider::compose::pseudo_messages::
                        render_file_edit_event(
                            &path_owned,
                            FileEditKind::Watch,
                            jiff::Timestamp::now(),
                            None, // watch-only never has diff
                        );
                    adapter.record_pseudo_message(msg);
                }
            })
            .map_err(|e| FileError::Io { path: path.to_owned(), source: e })?;
        self.edit_listeners.insert(canonical.clone(), listener);
        self.watch_only_paths.insert(canonical, subscription);
        Ok(())
    }

    pub fn unwatch(&self, path: &Path) -> Result<(), FileError> {
        let canonical = canonicalize_best(path);
        self.watch_only_paths.remove(&canonical);  // drop guard unregisters router entry
        self.edit_listeners.remove(&canonical);
        if let Some(parent) = canonical.parent() {
            self.maybe_drop_dir_watcher(parent);
        }
        Ok(())
    }

    pub fn list(&self, dir: &Path, glob: &str) -> Result<Vec<FileInfo>, FileError> {
        self.check_capability()?;
        self.policy.check_access(dir)?;
        let matcher = if glob.is_empty() || glob == "*" {
            None
        } else {
            Some(globset::Glob::new(glob)
                .map_err(|e| FileError::BadGlob(format!("{glob}: {e}")))?
                .compile_matcher())
        };
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(dir)
            .map_err(|e| FileError::Io { path: dir.to_owned(), source: e })?
        {
            let entry = entry.map_err(|e| FileError::Io { path: dir.to_owned(), source: e })?;
            let p = entry.path();
            if let Some(m) = &matcher {
                if !m.is_match(&p) { continue; }
            }
            let meta = entry.metadata()
                .map_err(|e| FileError::Io { path: p.clone(), source: e })?;
            entries.push(FileInfo {
                path: p,
                size: meta.len(),
                mtime: meta.modified()
                    .ok()
                    .and_then(|t| jiff::Timestamp::try_from(t).ok())
                    .unwrap_or_else(jiff::Timestamp::now),
                is_dir: meta.is_dir(),
            });
        }
        Ok(entries)
    }

    /// Snapshot open file paths for session serialization (AC2.11).
    pub fn open_paths(&self) -> Vec<PathBuf> {
        self.open_files.iter().map(|e| e.key().clone()).collect()
    }

    fn await_human_approval(&self, path: &Path, content: &[u8]) -> Result<(), FileError> {
        // Task 5.
        crate::file_manager::config_detect::await_approval(
            &self.permission_broker, path, content,
        )
    }
}

impl Drop for FileManager {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Cascade:
        //   1. open_files drops → SyncedDoc drops → router subscriptions guards drop.
        //   2. watch_only_paths drops → router subscription guards drop.
        //   3. dir_watchers drops → each DirWatcher drops → ingest threads exit.
        //   4. edit_listeners drops → each listener's rx.recv() returns Disconnected.
    }
}

fn canonicalize_best(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
```

**Note on `self.capability_set.has_file()`:** defined in Plan 3 Phase 1 — method per effect category. Phase 2 assumes it's landed; if reality differs, surface as a scope question (per implementation guidance), do not stub.

**Verifies:** Mechanism for AC2.1, AC2.2, AC2.3, AC2.4, AC2.5, AC2.6, AC2.8 (all via the methods above). Pooling correctness tested in Task 10.

**Verification:** `cargo check -p pattern-runtime`.

**Commit:** `[pattern-runtime] FileManager core — pooled DirWatcher + CRUD + watch`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Pattern config shape detection + PermissionBroker gate

**Files:**
- Create: `crates/pattern_runtime/src/file_manager/config_detect.rs`.
- Modify: `crates/pattern_core/src/permission.rs` — add `PermissionRequest::FileWriteConfig` variant (within Phase 2 scope; `pattern_core` stays trait-only, but `PermissionRequest` is a data type).

**Implementation:**

Fast path checks filename; slow path parses as KDL and looks for pattern-reserved top-level keys.

```rust
pub fn is_pattern_config_write(path: &Path, content: &[u8]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == ".pattern.kdl" || name.ends_with(".pattern.kdl") {
        return true;
    }
    let Ok(text) = std::str::from_utf8(content) else { return false; };
    let Ok(doc) = kdl::KdlDocument::parse(text) else { return false; };
    const RESERVED: &[&str] = &[
        "capabilities", "policy", "persona", "mount", "isolation",
        "file-policy", "backup", "storage-mode",
    ];
    doc.nodes().iter().any(|n| RESERVED.contains(&n.name().value()))
}

pub(crate) fn await_approval(
    broker: &PermissionBroker,
    path: &Path,
    content: &[u8],
) -> Result<(), FileError> {
    let matched_keys = find_matched_reserved_keys(content);
    let req = PermissionRequest::FileWriteConfig {
        path: path.to_owned(),
        matched_keys: matched_keys.clone(),
        preview: preview_lines(content, 20),
    };
    let decision = broker.request_blocking(req, Duration::from_secs(300))
        .map_err(|e| FileError::Io {
            path: path.to_owned(),
            source: std::io::Error::other(format!("broker: {e}")),
        })?;
    match decision {
        PermissionDecisionKind::ApproveOnce
        | PermissionDecisionKind::ApproveForDuration(_)
        | PermissionDecisionKind::ApproveForScope(_) => Ok(()),
        PermissionDecisionKind::Deny => {
            Err(FileError::ConfigApprovalDenied { path: path.to_owned() })
        }
    }
}
```

**Verifies:** AC2.9.

**Verification:**
- `cargo check --workspace`.
- Unit tests in `config_detect.rs`:
    - `filename_fast_path_accepts_dot_pattern_kdl` — `.pattern.kdl` → true.
    - `reserved_top_level_key_triggers_detection` — KDL with `capabilities { ... }` → true.
    - `arbitrary_kdl_does_not_trigger` — `name "alice"\nage 30` → false.
    - `non_utf8_does_not_trigger` — random bytes → false.
    - `malformed_kdl_does_not_trigger` — invalid KDL → false (err on not-blocking; the goal is catching obvious configs, not guessing intent).

**Commit:** `[pattern-runtime] [pattern-core] config-file shape detection + broker approval`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Wire `FileManager` into `SessionContext`

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs:40-121` — add `file_manager: Arc<FileManager>` field + `file_manager()` accessor.
- Modify: `crates/pattern_runtime/src/session.rs:496-619` (`SessionContext::from_persona` + `TidepoolSession::open_with_agent_loop`) — construct FileManager from parsed mount config.

**Implementation:**

In `SessionContext::from_persona`, after the adapter is constructed and the mount config is parsed:
```rust
let policy = FilePolicy::from_rules(mount_config.file_policy.rules.clone())?;
let file_manager = Arc::new(FileManager::new(
    policy,
    Arc::clone(&adapter),                     // existing per-session adapter
    persona.capability_set.clone(),          // Plan 3
    runtime.permission_broker().clone(),     // Plan 3
));
```

If no `file-policy` block in KDL: `FilePolicy::default_deny_all()` + loud `tracing::warn!` noting all File ops will be denied until rules are added.

Expose: `pub fn file_manager(&self) -> &Arc<FileManager> { &self.file_manager }`.

**Verifies:** Mechanism — all AC2 tests require this wiring.

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing `session_lifecycle.rs` tests still pass (sessions not using File effects unaffected).

**Commit:** `[pattern-runtime] FileManager on SessionContext + mount-config plumbing`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

---

<!-- START_SUBCOMPONENT_C (tasks 7-8) -->

<!-- START_TASK_7 -->
### Task 7: Implement `FileHandler` — dispatch `FileReq` to `FileManager`

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/file.rs` — replace stub body.

**Implementation:**

Tighten bound from `HasCancelState` to `SessionContext` (matches `SkillsHandler`). Dispatch each variant to FileManager; convert `FileError` via `EffectError::Handler(err.to_effect_message())`.

Response shapes (via `cx.respond()`):
- `Read(path)` → `String` (UTF-8 content).
- `Write(path, content)` → `()`.
- `ListDir(path, glob)` → `Vec<String>` of JSON-encoded `FileInfo` (mirrors `SkillsHandler`'s JSON-per-item pattern at `skills.rs:97-99`).
- `Open(path)` → `String` (content, same shape as Read).
- `Close(path)` → `()`.
- `Watch(path)` → `()`.

```rust
impl EffectHandler<SessionContext> for FileHandler {
    type Request = FileReq;

    fn handle(
        &mut self,
        req: FileReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        let fm = cx.user().file_manager().clone();

        match req {
            FileReq::Read(path) => {
                let bytes = fm.read(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let s = String::from_utf8(bytes).map_err(|e| EffectError::Handler(
                    format!("Pattern.File.Read: {path} is not UTF-8: {e}")
                ))?;
                cx.respond(s)
            }
            FileReq::Write(path, content) => {
                fm.write(Path::new(&path), content.as_bytes())
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.respond(())
            }
            FileReq::ListDir(path, glob) => {
                let entries = fm.list(Path::new(&path), &glob)
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let json: Vec<String> = entries.iter()
                    .map(|e| serde_json::to_string(e).unwrap_or_default())
                    .collect();
                cx.respond(json)
            }
            FileReq::Open(path) => {
                let bytes = fm.open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let s = String::from_utf8(bytes).map_err(|e| EffectError::Handler(
                    format!("Pattern.File.Open: {path} is not UTF-8: {e}")
                ))?;
                cx.respond(s)
            }
            FileReq::Close(path) => {
                fm.close(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.respond(())
            }
            FileReq::Watch(path) => {
                fm.watch(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.respond(())
            }
        }
    }
}
```

**Verifies:** AC2.1, AC2.2, AC2.3, AC2.4, AC2.5, AC2.6, AC2.8 (all via FileManager delegation).

**Verification:**
- `cargo check -p pattern-runtime`.
- Existing stub test in the handler file is deleted; new tests in Task 10.

**Commit:** `[pattern-runtime] FileHandler dispatches to FileManager`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: `render_file_edit_event` + diff payload

**Files:**
- Modify: `crates/pattern_provider/src/compose/pseudo_messages.rs` — add `render_file_edit_event(path, kind, at, diff) -> ChatMessage` alongside the existing `render_skill_loaded_event`. Same module, same shape.
- Modify: `crates/pattern_memory/src/loro_sync/synced_doc.rs` (Phase 1 contract — verify Phase 1 ships it; if not, surface as scope feedback) — capture memory_doc content before + after each external-merge cycle and include both in `ExternalChangeEvent::diff_data` (a structured `before: String, after: String` pair, or a single rendered diff string).
- Modify: `crates/pattern_runtime/src/file_manager/manager.rs` — listener threads pass the captured diff payload through to `render_file_edit_event` instead of `None`.

**`render_file_edit_event` shape:**

```rust
// pattern_provider/src/compose/pseudo_messages.rs
pub fn render_file_edit_event(
    path: &Path,
    kind: FileEditKind,        // re-exported from pattern_runtime, or re-defined here
    at: jiff::Timestamp,
    diff: Option<String>,
) -> ChatMessage {
    let kind_label = match kind {
        FileEditKind::Open  => "you had open",
        FileEditKind::Watch => "you were watching",
    };
    let mut body = format!(
        "<system-reminder>\n\
         External edit detected while you were thinking:\n\
         - {at} {} ({kind_label}) changed",
        path.display(),
    );
    if let Some(diff) = diff {
        body.push_str(":\n```\n");
        body.push_str(&diff);
        body.push_str("\n```");
    }
    body.push_str("\n</system-reminder>");
    ChatMessage::user(body)
}
```

(Exact `ChatMessage` constructor matches whatever `render_skill_loaded_event` uses — verify at execution time.)

**Diff computation (default: before/after text blocks, zero new deps):**

Phase 1's `SyncedDoc` ingest thread already captures memory_doc state before applying external edits (it has to, in order to compute the `oplog_vv` for export). Extending it to also expose the rendered before/after content is a small addition. For opaque text (file case), `before = doc.get_text("content").to_string()` pre-merge, `after = doc.get_text("content").to_string()` post-merge; the diff payload is `format!("--- before\n{before}\n+++ after\n{after}")` (literal, no diff library required).

If orual approves the `similar` crate (open question Q1), swap the renderer to emit a unified diff via `similar::TextDiff::from_lines`. Data model unchanged.

**Verifies:** AC2.7 (text content delivered as system reminder in next turn).

**Verification:**
- `cargo check --workspace`.
- Unit test on `render_file_edit_event` — given known inputs, snapshot the rendered ChatMessage body via `insta`.
- Integration test in Task 10 (`external_edit_on_open_file_becomes_attachment`) renamed to `external_edit_on_open_file_becomes_pseudo_message` — asserts on `TurnOutput::pseudo_messages` (or `TurnHistory::most_recent_pseudo_messages`) contains a message whose body contains the file path and the diff payload.

**Commit:** `[pattern-provider] [pattern-runtime] render_file_edit_event + before/after diff payload`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_C -->

---

<!-- START_SUBCOMPONENT_D (tasks 9-10) -->

<!-- START_TASK_9 -->
### Task 9: Session state serialization — `open_files` in PersonaSnapshot

**Files:**
- Modify: `crates/pattern_core/src/types/snapshot.rs` — add `open_files: Vec<PathBuf>` to `PersonaSnapshot`.
- Modify: `crates/pattern_runtime/src/session.rs` — on snapshot creation, populate from `ctx.file_manager().open_paths()`. On restore, re-open each path; log + skip failures (file may be gone between snapshot + restore).

**Implementation:**

```rust
// snapshot.rs
pub struct PersonaSnapshot {
    // … existing fields …
    /// File paths the agent had open at snapshot time. On restore, these
    /// are re-opened with fresh LoroDocs — no LoroDoc state persists across
    /// snapshot boundaries (loro docs are ephemeral per design).
    #[serde(default)]
    pub open_files: Vec<PathBuf>,
}
```

Restore path (`TidepoolSession::restore`):
```rust
for path in &persona_snapshot.open_files {
    if let Err(e) = session_ctx.file_manager().open(path) {
        tracing::warn!(path = ?path, error = %e, "failed to re-open file from snapshot; skipping");
    }
}
```

**Verifies:** AC2.11.

**Verification:**
- `cargo check --workspace`.
- Unit test in `snapshot.rs`: round-trip `PersonaSnapshot` through serde with `open_files` populated and empty.
- Integration test in Task 10: open file → snapshot → drop session → restore → file open + readable.

**Commit:** `[pattern-core] [pattern-runtime] PersonaSnapshot.open_files round-trip`
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: AC2 test suite

**Files:**
- Create: `crates/pattern_runtime/tests/file_handler.rs` — full-handler-path integration tests.
- Expand unit tests in `crates/pattern_runtime/src/file_manager/manager.rs` for FileManager-level behaviour.

**Tests (one per AC case; tempdir-based, no hardcoded paths):**

| AC | Test name | Mechanism |
|----|-----------|-----------|
| 2.1 | `read_does_not_open_loro` | `fm.read(path)`; external `std::fs::write`; wait 750ms; assert `adapter.drain_pending_pseudo_messages()` empty. |
| 2.2 | `open_returns_content_and_subscribes` | `fm.open` content matches disk; external edit → `adapter.drain_pending_pseudo_messages()` non-empty with body containing path + `you had open`. |
| 2.3 | `write_on_open_file_goes_through_loro` | Open + `fm.write("new")` with a concurrent external edit — both preserved per Phase 1 AC1.3. Write on un-opened file: direct `atomic_write`, no loro. |
| 2.4 | `close_drops_watcher` | Open, close, external edit; wait; `adapter.drain_pending_pseudo_messages()` empty. |
| 2.5 | `list_with_glob` | Tempdir with `a.rs`, `b.py`, `c.rs`; `fm.list(dir, "*.rs")` returns 2 entries. |
| 2.6 | `watch_does_not_create_loro` | `fm.watch`; external edit; reminder body contains `you were watching`; `fm.open_files` does not contain path; `fm.watch_only_paths` does. |
| 2.6b | `watcher_pooling_shares_dir_watchers` | Open three files in the same directory; `fm.dir_watchers` has exactly one entry (pooled). Close two; still one. Close last; entry GC'd. |
| 2.7 | `external_edit_on_open_file_becomes_pseudo_message` | Full integration: open session + test persona with file-policy; agent `Pattern.File.Open(path)`; external `std::fs::write`; advance one turn; assert the next turn's `Segment2Pass` receives a `recent_pseudo_messages` entry whose body contains the file path and the diff payload. |
| 2.8 | `write_outside_rules_denied` | Policy `allow /project/**` only; `fm.write("/etc/passwd", ...)` → `FileError::PermissionDenied { reason: "no matching rule (default deny)" }`. |
| 2.9 | `config_write_triggers_broker` | Content that parses as pattern config KDL. Scripted broker auto-approves → write succeeds; scripted broker denies → `FileError::ConfigApprovalDenied`. |
| 2.10 | `ordered_rules_last_match_wins` | Three scenarios in one test. (a) `allow /project/**`, then `deny /project/.env` → `.env` denied by rule 1, `lib.rs` allowed by rule 0. (b) `deny /project/**`, then `allow /project/notes/*.md` → `notes/foo.md` allowed despite broader deny. (c) Nested re-allow — `allow /project/**`, `deny /project/secrets/**`, `allow /project/secrets/public.txt` → `public.txt` allowed, `secrets/private.txt` denied. All verify denial reason names the losing rule. |
| 2.11 | `snapshot_restores_open_files` | Open two files → snapshot → drop session → restore → both files open and readable. |

**Capability stubbing:** tests construct a `CapabilitySet` with File enabled (or use Plan 3's test builder once it exists). Since Plan 3 Phase 1 is a prerequisite, the helper exists by execution time; verify with `grep -rn "CapabilitySet" crates/pattern_core/src` at execution time.

**Verifies:** AC2.1, AC2.2, AC2.3, AC2.4, AC2.5, AC2.6, AC2.7, AC2.8, AC2.9, AC2.10, AC2.11.

**Verification:**
- `cargo nextest run -p pattern-runtime --test file_handler`.
- `cargo nextest run -p pattern-runtime --lib file_manager`.
- All 646 existing tests still pass.

**Commit:** `[pattern-runtime] AC2 tests for FileHandler + FileManager`
<!-- END_TASK_10 -->

<!-- END_SUBCOMPONENT_D -->

---

## Open questions for human review (foreground at end of plan-write)

**Q1: `similar` crate for unified-diff rendering.** Would make file-edit system reminders much more readable (real unified diffs instead of before/after text blocks). Adds a dep for cosmetic polish. Default: ask before adding — per project guidance.

**Q2 [resolved 2026-04-24]:** Originally proposed adding a `MessageAttachment::FileEdits` variant. Updated to use the canonical pseudo-message pipeline (`adapter.record_pseudo_message`) — no new attachment variant, mirrors `Pattern.Skills.Load`. See updated Task 4 + Task 8.

**Q3: Canonicalization fallback on missing files.** `canonicalize_best` falls back to raw path when the file doesn't exist (write-new case). Means allow/deny patterns should be canonical absolute paths — KDL authors writing relative patterns would be surprised. Document in the KDL config schema; flag if a stricter stance is preferred.

**Q4: `FileInfo` JSON vs native tidepool record.** Defaulted to JSON-string-per-item (matches SkillsHandler). Native `FromCore` struct is more ergonomic for agents but more code; flag if reviewer wants it.
