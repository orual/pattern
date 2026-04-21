# Pattern v3 Memory Rework — Phase 6 Implementation Plan

**Goal:** Implement storage modes A + B end-to-end, run the Mode C validation spike and ship or fate-marker based on its outcome, parse `.pattern.kdl` mount configs into a typed `MountConfig`, and wire mount attachment (`attach(path)` walks upward for `.pattern/shared/.pattern.kdl`, opens `memory.db` + `messages.db`, spawns subscribers, returns a `MountedStore` handle with clean `detach()` semantics).

**Architecture:** A "mount" is a directory containing Pattern-managed memory state. `StorageMode` (skeleton from Phase 5) is extended with per-mode path resolution + init/attach/detach logic. `MountedStore` is the runtime handle returned from `attach` — it owns the `MemoryCache`, `ConstellationDb`, subscriber supervisor, and `MountWatcher` for its lifetime; `detach` drops them cleanly. Mode A puts block files inside the project repo at `<project>/.pattern/shared/` and delegates history to the host VCS (git or jj); `messages.db` lives outside the repo at `~/.pattern/transient/<project-hash>/`. Mode B puts block files at `~/.pattern/projects/<id>/shared/` with pattern-jj owning history; messages.db at `~/.pattern/projects/<id>/messages/`. Mode C is the sidecar experiment: pattern-jj at `<mount>/.jj/` alongside host `.git/` in the same working copy; upstream jj explicitly cautions this is undertested, so Phase 6 includes a 50-op interleaved validation spike whose outcome gates whether Mode C ships for real or becomes a documented-only enum variant.

**Tech Stack:**
- **`knus`** for typed KDL parsing (derive-macro-based; miette integration built in). knuffel is a near-identical alternative; knus chosen for slightly more-active maintenance.
- **`gix-discover`** for host-git detection (walk-upward for `.git/`; 728 SLoC focused crate).
- **Hand-rolled walk-upward** for `.jj/` detection (trivial loop; no equivalent crate).
- **`blake3`** for project-hash derivation (first 16 hex chars of `blake3::hash(canonical_path.as_bytes())`) — matches workspace content-hash convention (see `pattern_runtime/CLAUDE.md`).
- **`dirs 5.0`** (already workspace-pinned) for `~/.pattern/` resolution.
- Existing: `kdl = "6"` (Phase 4), `miette`, `thiserror`, `tempfile` (dev), `tokio_util::sync::CancellationToken` (Phase 4).

**Scope:** Phase 6 of 8.

**Codebase verified:** 2026-04-19 (codebase-investigator agent ada59ae0b68b379b3).
**External deps verified:** 2026-04-19 (internet-researcher agent a019728ae4cd3875e).

**Execution posture:** Hybrid. Subcomponents A + B (modes, config, attach/detach) are autonomous-friendly mechanical work. **Sub-task 6a (Mode C spike) is a main-executor gate** — the spike's 50-op interleaved test should be run and interpreted by the main executor, with explicit human sign-off on the pass/fail decision before either shipping Mode C's implementation or fate-marking it as documented-only.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC9: Storage modes A + B

- **v3-memory-rework.AC9.1 Success:** Mode A end-to-end: temp host-git repo + mount init + block write + host git commit + verify state on disk
- **v3-memory-rework.AC9.2 Success:** Mode B end-to-end: pattern-jj temp repo + mount init + block write + quiesce → jj commit + verify state
- **v3-memory-rework.AC9.3 Success:** Mode A `messages.db` lives at `~/.pattern/transient/<project-hash>/` (outside the project repo)
- **v3-memory-rework.AC9.4 Success:** Mode B `messages.db` lives at `~/.pattern/projects/<id>/messages/` (outside pattern-jj worktree)
- **v3-memory-rework.AC9.5 Success:** `.pattern.kdl` config parses cleanly for representative configs; malformed configs produce clear diagnostics
- **v3-memory-rework.AC9.6 Success:** `attach(path)` walks upward to find `.pattern.kdl`; sets up subscribers + opens dbs + registers with jj as applicable
- **v3-memory-rework.AC9.7 Failure:** `attach` on a path with no mount produces a clear "no mount found" error with a suggestion to run `pattern mount init`
- **v3-memory-rework.AC9.8 Edge:** `detach` + re-`attach` produces identical state (no leaked workers, clean restart)

### v3-memory-rework.AC10: Mode C spike outcome

- **v3-memory-rework.AC10.1 Success (Mode C ships):** Spike passes 50-op interleaved test (host git ops + pattern jj ops) with zero state divergence; documented in design-plan with 'verified: YYYY-MM-DD' stamp; Mode C implementation ships
- **v3-memory-rework.AC10.2 Failure (Mode C deferred):** Spike fails; fate-marker comment in `pattern_memory::modes` explicitly records the deferral; design-plan updated with findings; `StorageMode::C` enum variant either (a) ships in a documented-only state that explicitly rejects attachment, or (b) is absent from the enum until a future plan
- **v3-memory-rework.AC10.3 Edge:** Spike outcome (pass or fail) produces a note file at `docs/notes/YYYY-MM-DD-mode-c-spike.md` documenting the evidence

---

## Codebase verification findings

- ✓ `pattern_runtime/src/persona_loader.rs` is the typed-config-with-miette template (TOML + serde, `#[non_exhaustive] #[derive(Error, Diagnostic)]` error enum). Phase 6's KDL config loader follows this shape — just swap TOML for knus.
- ✓ `dirs = "5.0"` workspace dep. `dirs::home_dir()` → `PathBuf`. Call site pattern visible at `crates/pattern_provider/src/creds_store/json_fallback.rs:35`. Phase 6 reuses — no bump to `dirs 6.0` for cosmetic reasons.
- ✓ `pattern_cli` is the CLI host. It will be pre-stripped (existing v2-era code moved to `rewrite-staging/`) and rebuilt with minimal ratatui scaffolding BEFORE this plan begins execution — that work is orthogonal to v3-memory-rework. Phase 6 adds `mount init <mode>` + `attach <path>` as clap subcommands alongside whatever TUI scaffolding exists at that point. The subcommands are one-shot CLI ops; they don't need to integrate with the ratatui main-loop.
- ✗ No existing VCS detection (zero matches for `.git/` or `.jj/` inspection code in the workspace). Phase 6 establishes both via `gix-discover` (git) + hand-rolled walk-up (jj).
- ✗ No existing gitignore write support anywhere. Phase 6 adds a thin helper.
- ✗ No existing symlink helper beyond one Unix-specific usage at `pattern_cli/src/discord.rs`. Phase 6's Mode B optional symlink uses `std::os::unix::fs::symlink` directly; Windows gets a config-file fallback (absolute path stored in `.pattern.kdl` instead of a real symlink).
- ✗ No existing project-hash concept. Phase 6 establishes: `blake3::hash(canonical_path.as_bytes())` → first 16 hex chars → `project-hash`.
- ✗ No prior art for dual-VCS shared working copy (Mode C). Upstream jj explicitly cautions: "colocated workspaces are less resilient to concurrency issues if you share the repo... in general, such use of Jujutsu is not currently thoroughly tested." Spike proceeds but with explicit pass criteria documented; expect rough edges.
- ✓ `create_dir_all` is explicitly race-safe per std docs — no locking needed for concurrent mount init.
- ✓ `tempfile` workspace dev-dep sufficient for Mode A/B integration tests + Mode C spike harness.
- ✓ `kdl` 6 + `knus` integrate; knus-derived structs parse `.pattern.kdl` via `knus::parse::<MountConfig>(text)`.
- ✓ Phase 4's `MountWatcher` + subscriber supervisor lifecycle are the primitives `MountedStore::attach/detach` compose on top of.

---

## Dependency changes

`crates/pattern_memory/Cargo.toml`:

```toml
[dependencies]
# ... existing from Phases 1-5 ...

knus = "3"              # typed KDL parsing with miette integration
gix-discover = "0.40"   # host-git detection (walk upward for .git/)
```

No workspace-level Cargo.toml changes. `blake3` + `kdl` + `dirs` already pinned.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

### Subcomponent A: `.pattern.kdl` config + MountedStore scaffold

<!-- START_TASK_1 -->
### Task 1: Typed `MountConfig` + `.pattern.kdl` parsing

**Verifies:** v3-memory-rework.AC9.5

**Files:**
- Create: `crates/pattern_memory/src/config/mod.rs`
- Create: `crates/pattern_memory/src/config/pattern_kdl.rs`
- Create: `crates/pattern_memory/src/config/error.rs`

**Implementation:**

1. `MountConfig` struct using `knus` derive:

   ```rust
   use knus::Decode;
   use std::path::PathBuf;

   /// Parsed representation of a `.pattern.kdl` mount config.
   ///
   /// Example .pattern.kdl:
   /// ```kdl
   /// mount mode="A" memory_db="memory.db"
   ///
   /// personas {
   ///     default "@pattern-default"
   /// }
   ///
   /// isolate_from_persona policy="none"
   ///
   /// jj enabled=true max_new_file_size="100MiB"
   ///
   /// project name="pattern-dev" created_at="2026-04-19T12:00:00Z"
   /// ```
   #[derive(Debug, Clone, Decode)]
   pub struct MountConfig {
       #[knus(child)]
       pub mount: MountSection,

       #[knus(child, default)]
       pub personas: PersonasSection,

       #[knus(child, default)]
       pub isolate_from_persona: IsolateSection,

       #[knus(child, default)]
       pub jj: JjSection,

       #[knus(child)]
       pub project: ProjectSection,
   }

   #[derive(Debug, Clone, Decode)]
   pub struct MountSection {
       #[knus(property)]
       pub mode: ModeKind,          // "A" | "B" | "C" → parsed as enum
       #[knus(property)]
       pub memory_db: String,       // relative path to memory.db
   }

   #[derive(Debug, Clone, Copy, PartialEq, Eq, Decode)]
   pub enum ModeKind {
       #[knus(named="A")] A,
       #[knus(named="B")] B,
       #[knus(named="C")] C,
   }

   #[derive(Debug, Clone, Default, Decode)]
   pub struct PersonasSection {
       #[knus(children)]
       pub entries: Vec<PersonaBinding>,
   }

   #[derive(Debug, Clone, Decode)]
   pub struct PersonaBinding {
       #[knus(node_name)]
       pub slot: String,            // e.g. "default"
       #[knus(argument)]
       pub persona: String,         // e.g. "@pattern-default"
   }

   #[derive(Debug, Clone, Default, Decode)]
   pub struct IsolateSection {
       #[knus(property, default = "none".into())]
       pub policy: String,          // "none" | "core-only" | "full"
   }

   #[derive(Debug, Clone, Default, Decode)]
   pub struct JjSection {
       #[knus(property, default = true)]
       pub enabled: bool,
       #[knus(property, default = "100MiB".into())]
       pub max_new_file_size: String,
   }

   #[derive(Debug, Clone, Decode)]
   pub struct ProjectSection {
       #[knus(property)]
       pub name: String,
       #[knus(property)]
       pub created_at: String,      // parsed as jiff::Timestamp downstream
   }
   ```

   Exact `knus` attribute syntax varies by crate version; implementor verifies against the current knus 3.x API at implementation time and adjusts attributes (`#[knus(property)]` / `#[knus(argument)]` / `#[knus(child)]` / `#[knus(children)]` / `#[knus(named=".")]`) to match. Fall back to hand-written parsers pulling from `KdlDocument` if knus doesn't cover a case cleanly.

2. Loader entry point:

   ```rust
   pub fn load_mount_config(path: &Path) -> Result<MountConfig, ConfigError> {
       let text = std::fs::read_to_string(path)
           .map_err(|e| ConfigError::Io { path: path.to_owned(), source: e })?;
       knus::parse::<MountConfig>(&path.display().to_string(), &text)
           .map_err(|e| ConfigError::Parse { path: path.to_owned(), source: e })
   }
   ```

   `knus::parse` errors integrate with miette directly — line/column spans surface in the diagnostic output.

3. `ConfigError`:

   ```rust
   #[non_exhaustive]
   #[derive(Debug, thiserror::Error, miette::Diagnostic)]
   pub enum ConfigError {
       #[error("io error reading {path}: {source}")]
       #[diagnostic(code(pattern_memory::config::io))]
       Io { path: PathBuf, #[source] source: std::io::Error },

       #[error("parse error in {path}")]
       #[diagnostic(transparent)]
       Parse {
           path: PathBuf,
           #[source]
           source: knus::Error,     // knus errors are miette-native
       },

       #[error("invalid mount config in {path}: {reason}")]
       #[diagnostic(code(pattern_memory::config::validation))]
       Validation { path: PathBuf, reason: String },
   }
   ```

4. Post-parse validation: enforce cross-field rules that KDL can't express (e.g. Mode A with `jj.enabled=true` and host VCS is git → fine; Mode B with `jj.enabled=false` → error; mode A MUST have a hashable project path at attach time, but parse-time doesn't know the path yet so that's deferred to attach).

**Testing:**

Unit tests in `tests/config.rs`:
- Representative `.pattern.kdl` files as string fixtures — one valid per mode, one with each kind of validation error (bad mode string, missing required field, malformed property).
- `insta` snapshots for each valid fixture's parsed `MountConfig` so changes to the schema surface in review.
- Parse error fixtures assert the error's miette output contains the expected line/column pointer.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib config`
Expected: all pass.

**Commit:** `[pattern-memory] .pattern.kdl typed parsing via knus + miette-integrated errors`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: VCS detection helpers — `gix-discover` for git, hand-rolled for jj

**Verifies:** prerequisite for mode detection (AC9.1, AC9.2, AC9.6)

**Files:**
- Create: `crates/pattern_memory/src/vcs/mod.rs`

**Implementation:**

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostVcs {
    Git,
    Jj,
    None,
}

/// Walk upward from `start` to find the nearest host VCS root.
/// Returns the discovered root path + which VCS. If both .git and .jj exist
/// at the same level, prefers jj (jj-managed repos may colocate a .git).
pub fn discover_host_vcs(start: &Path) -> (HostVcs, Option<PathBuf>) {
    // First pass: walk upward for .jj — explicit.
    let mut cur = start;
    loop {
        if cur.join(".jj").is_dir() {
            return (HostVcs::Jj, Some(cur.to_owned()));
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    // Second pass: gix-discover for .git.
    if let Ok(result) = gix_discover::upwards(start) {
        if let Some(git_dir) = result.0.git_dir() {
            // git_dir points to .git/; repo root is its parent.
            return (HostVcs::Git, git_dir.parent().map(|p| p.to_owned()));
        }
    }
    (HostVcs::None, None)
}
```

gix-discover's exact API may vary with version; the implementor verifies against `gix-discover 0.40` at implementation time and adjusts field access / return shape accordingly.

**Testing:**

- Unit tests with `tempfile::TempDir`: create a dir, `git init`, assert `discover_host_vcs(subdir)` returns `HostVcs::Git` with the temp root.
- Same with `jj init` → `HostVcs::Jj`.
- Co-located scenario (both `.git` and `.jj` at the same level): assert jj preference.
- Empty dir → `HostVcs::None`.
- Nested dir detection: `tempdir/sub/sub/sub` → returns the correct ancestor.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib vcs`
Expected: passes.

**Commit:** `[pattern-memory] vcs::discover_host_vcs — gix-discover for git, hand-rolled walk-up for jj`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Project-hash derivation + Pattern home directory helpers

**Verifies:** prerequisite for AC9.3 (Mode A messages.db path)

**Files:**
- Create: `crates/pattern_memory/src/paths.rs`

**Implementation:**

```rust
use std::path::{Path, PathBuf};

/// Return `~/.pattern/` as a PathBuf, resolving via `dirs::home_dir()`.
/// Errors if no home directory is discoverable.
pub fn pattern_home() -> Result<PathBuf, PathError> {
    dirs::home_dir()
        .map(|h| h.join(".pattern"))
        .ok_or(PathError::NoHome)
}

/// Derive a stable 16-char hex project hash from a project repo path.
/// Canonicalizes the path first so relative / `..` segments produce the
/// same hash as their resolved form.
///
/// Uses blake3 to match the workspace content-hash convention
/// (see pattern_runtime/CLAUDE.md — blake3::hash(...).as_bytes()[..8]).
pub fn project_hash(project_root: &Path) -> Result<String, PathError> {
    let canonical = std::fs::canonicalize(project_root)
        .map_err(|e| PathError::Canonicalize { path: project_root.to_owned(), source: e })?;
    let bytes = canonical.to_string_lossy().as_bytes().to_vec();
    let h = blake3::hash(&bytes);
    // First 16 hex chars = 8 bytes = collision-resistant for Pattern's scale.
    Ok(h.to_hex().as_str().chars().take(16).collect())
}

/// Path where Mode A stashes messages.db for a given project.
pub fn mode_a_messages_path(project_root: &Path) -> Result<PathBuf, PathError> {
    let hash = project_hash(project_root)?;
    Ok(pattern_home()?.join("transient").join(hash).join("messages.db"))
}

/// Path where Mode B stashes its mount + messages for a given project id.
pub fn mode_b_mount_path(project_id: &str) -> Result<PathBuf, PathError> {
    Ok(pattern_home()?.join("projects").join(project_id).join("shared"))
}

pub fn mode_b_messages_path(project_id: &str) -> Result<PathBuf, PathError> {
    Ok(pattern_home()?.join("projects").join(project_id).join("messages").join("messages.db"))
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum PathError {
    #[error("no home directory available")]
    #[diagnostic(code(pattern_memory::paths::no_home))]
    NoHome,

    #[error("failed to canonicalize {path}: {source}")]
    #[diagnostic(code(pattern_memory::paths::canonicalize))]
    Canonicalize { path: PathBuf, #[source] source: std::io::Error },
}
```

**Testing:**

- `project_hash(path)` is deterministic: same path → same hash across multiple calls.
- `project_hash(path)` vs `project_hash(path + "/./")`: canonicalization normalizes → same hash.
- Two distinct paths produce distinct hashes.
- `pattern_home` returns the expected `$HOME/.pattern` form.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib paths`
Expected: all pass.

**Commit:** `[pattern-memory] paths::pattern_home + project_hash (blake3) + mode-specific path resolvers`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

### Subcomponent B: Mode A + Mode B end-to-end (MountedStore, attach/detach)

<!-- START_TASK_4 -->
### Task 4: Extend `StorageMode` enum; implement Mode A + Mode B init logic

**Verifies:** v3-memory-rework.AC9.1, AC9.2, AC9.3, AC9.4

**Files:**
- Modify: `crates/pattern_memory/src/modes/mod.rs` (extend the Phase-5 skeleton)
- Create: `crates/pattern_memory/src/modes/mode_a.rs`
- Create: `crates/pattern_memory/src/modes/mode_b.rs`
- Create: `crates/pattern_memory/src/modes/gitignore.rs` (helper)

**Implementation:**

1. Extend `StorageMode`:

   ```rust
   #[non_exhaustive]
   #[derive(Debug, Clone)]
   pub enum StorageMode {
       A { mount_path: PathBuf, project_root: PathBuf },
       B { mount_path: PathBuf, project_id: String },
       // Mode C populated below based on spike outcome (Task 7)
       #[cfg(feature = "mode-c-experimental")]
       C { mount_path: PathBuf },
   }
   ```

   Mode C is feature-gated in the enum so it's either there (if spike passes) or absent (if spike fails, feature is disabled by default). Task 7's post-spike action flips the feature flag or updates the enum.

2. Mode A init (`mode_a.rs`):

   ```rust
   pub fn init(project_root: &Path) -> Result<StorageMode, ModeError> {
       let mount_path = project_root.join(".pattern").join("shared");
       std::fs::create_dir_all(&mount_path)?;                 // race-safe per std docs
       std::fs::create_dir_all(mount_path.join("blocks/core"))?;
       std::fs::create_dir_all(mount_path.join("blocks/working"))?;
       std::fs::create_dir_all(mount_path.join("personas"))?;
       std::fs::create_dir_all(mount_path.join("lib"))?;

       // Scaffold .pattern.kdl with Mode A defaults.
       let kdl = format!(
           r#"mount mode="A" memory_db="memory.db"

   personas {{
       default "@pattern-default"
   }}

   isolate_from_persona policy="none"

   jj enabled=false

   project name={project_name:?} created_at={now:?}
   "#,
           project_name = project_root.file_name().and_then(|n| n.to_str()).unwrap_or("pattern-project"),
           now = jiff::Timestamp::now().to_string(),
       );
       std::fs::write(mount_path.join(".pattern.kdl"), kdl)?;

       // Ensure project-root/.gitignore excludes .pattern/transient/
       // (messages.db transient subtree; mount itself is tracked).
       gitignore::append_if_missing(project_root, ".pattern/transient/")?;

       Ok(StorageMode::A {
           mount_path,
           project_root: project_root.to_owned(),
       })
   }
   ```

3. Mode B init (`mode_b.rs`) — creates `~/.pattern/projects/<id>/shared/` + pattern-jj-init it; no host-git coupling:

   ```rust
   pub fn init(project_id: &str, jj_adapter: &JjAdapter) -> Result<StorageMode, ModeError> {
       let mount_path = paths::mode_b_mount_path(project_id)?;
       std::fs::create_dir_all(&mount_path)?;
       std::fs::create_dir_all(mount_path.join("blocks/core"))?;
       std::fs::create_dir_all(mount_path.join("blocks/working"))?;
       std::fs::create_dir_all(mount_path.join("personas"))?;
       std::fs::create_dir_all(mount_path.join("lib"))?;

       let msgs_path = paths::mode_b_messages_path(project_id)?;
       std::fs::create_dir_all(msgs_path.parent().expect("has parent"))?;

       // Scaffold .pattern.kdl with Mode B defaults.
       // ... similar to Mode A but mode="B" + jj enabled=true ...

       // Init pattern-jj repo inside the mount.
       jj_adapter.init_repo(&mount_path)?;

       Ok(StorageMode::B {
           mount_path,
           project_id: project_id.to_owned(),
       })
   }
   ```

4. `gitignore.rs` helper:

   ```rust
   use std::io::Write;
   use std::path::Path;

   pub fn append_if_missing(project_root: &Path, entry: &str) -> Result<(), ModeError> {
       let path = project_root.join(".gitignore");
       let current = match std::fs::read_to_string(&path) {
           Ok(s) => s,
           Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
           Err(e) => return Err(ModeError::Io { path: path.clone(), source: e }),
       };
       if current.lines().any(|l| l.trim() == entry.trim_end_matches('\n')) {
           return Ok(());
       }
       // Append-only open is atomic on POSIX for a single write() <= PIPE_BUF.
       let mut f = std::fs::OpenOptions::new()
           .create(true).append(true).open(&path)
           .map_err(|e| ModeError::Io { path: path.clone(), source: e })?;
       if !current.is_empty() && !current.ends_with('\n') {
           f.write_all(b"\n").map_err(|e| ModeError::Io { path: path.clone(), source: e })?;
       }
       f.write_all(entry.as_bytes()).map_err(|e| ModeError::Io { path: path.clone(), source: e })?;
       f.write_all(b"\n").map_err(|e| ModeError::Io { path: path.clone(), source: e })?;
       Ok(())
   }
   ```

**Testing:**

- Mode A init in tempdir: verify mount layout + `.pattern.kdl` present + `.gitignore` contains `.pattern/transient/`.
- Mode A init twice (idempotent): second call doesn't duplicate `.gitignore` entries.
- Mode B init in a fresh `~/.pattern/projects/<id>/`: verify layout + pattern-jj repo exists.
- AC9.3 path test: `mode_a_messages_path(project_root)` returns the expected `~/.pattern/transient/<hash>/messages.db`.
- AC9.4 path test: `mode_b_messages_path(id)` returns the expected `~/.pattern/projects/<id>/messages/messages.db`.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib modes`
Expected: passes.

**Commit:** `[pattern-memory] Mode A + Mode B init logic + gitignore append helper`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `MountedStore` runtime handle + `attach(path)` + `detach`

**Verifies:** v3-memory-rework.AC9.6, AC9.7, AC9.8

**Files:**
- Create: `crates/pattern_memory/src/mount/mod.rs`
- Create: `crates/pattern_memory/src/mount/attach.rs`

**Implementation:**

```rust
use std::sync::Arc;
use std::path::{Path, PathBuf};
use pattern_db::ConstellationDb;
use crate::cache::MemoryCache;
use crate::subscriber::SubscriberSupervisor;
use crate::fs::watcher::MountWatcher;
use crate::jj::JjAdapter;
use crate::config::{MountConfig, load_mount_config};
use crate::modes::StorageMode;

/// Runtime handle for an attached mount. Owns the MemoryCache, DB pool,
/// subscriber supervisor, and fs watcher for the mount's lifetime.
pub struct MountedStore {
    pub mount_path: PathBuf,
    pub config: MountConfig,
    pub mode: StorageMode,
    pub cache: Arc<MemoryCache>,
    pub db: Arc<ConstellationDb>,
    supervisor_handle: tokio::task::JoinHandle<()>,
    watcher: MountWatcher,
    // jj adapter is held here for Modes B/C; None in Mode A.
    pub jj: Option<Arc<JjAdapter>>,
}

impl MountedStore {
    pub async fn detach(self) -> Result<(), MountError> {
        // 1. Stop the file watcher (stops enqueueing external-edit events).
        drop(self.watcher);
        // 2. Signal subscribers to drain + exit.
        self.cache.drop_all_docs().await?;  // cancels every per-doc subscriber
        // 3. Cancel supervisor task.
        self.supervisor_handle.abort();
        let _ = self.supervisor_handle.await;
        // 4. Drop DB pool (Arc count reaches zero when cache drops).
        drop(self.cache);
        drop(self.db);
        Ok(())
    }
}

/// Walk upward from `start` looking for `.pattern/shared/.pattern.kdl`.
/// Returns the mount path (the directory containing .pattern.kdl) or
/// `MountError::NotFound` if none found before the filesystem root.
pub fn find_mount(start: &Path) -> Result<PathBuf, MountError> {
    let mut cur = start.to_owned();
    loop {
        let candidate = cur.join(".pattern").join("shared").join(".pattern.kdl");
        if candidate.is_file() {
            return Ok(candidate.parent().unwrap().to_owned());
        }
        match cur.parent() {
            Some(p) => cur = p.to_owned(),
            None => break,
        }
    }
    Err(MountError::NotFound { started_at: start.to_owned() })
}

/// Attach to the mount at the nearest ancestor containing .pattern.kdl.
pub async fn attach(start: &Path, jj_adapter: Option<Arc<JjAdapter>>)
    -> Result<MountedStore, MountError>
{
    let mount_path = find_mount(start)?;
    let config = load_mount_config(&mount_path.join(".pattern.kdl"))?;

    // Resolve DB paths per mode.
    let (memory_db_path, messages_db_path, mode) = match config.mount.mode {
        ModeKind::A => {
            // For Mode A, project_root = the ancestor containing .pattern/
            let project_root = mount_path
                .parent().and_then(|p| p.parent())
                .ok_or_else(|| MountError::InvalidLayout { path: mount_path.clone() })?
                .to_owned();
            let memory_db = mount_path.join(&config.mount.memory_db);
            let messages_db = crate::paths::mode_a_messages_path(&project_root)?;
            (memory_db, messages_db, StorageMode::A { mount_path: mount_path.clone(), project_root })
        }
        ModeKind::B => {
            let memory_db = mount_path.join(&config.mount.memory_db);
            let messages_db = crate::paths::mode_b_messages_path(&config.project.name)?;
            (memory_db, messages_db, StorageMode::B {
                mount_path: mount_path.clone(),
                project_id: config.project.name.clone(),
            })
        }
        ModeKind::C => {
            // Gated on the spike outcome (Task 7).
            return Err(MountError::ModeUnavailable {
                mode: "C",
                reason: "Mode C is experimental; see v3-memory-rework Phase 6 spike".into(),
            });
        }
    };

    // If mode requires jj and adapter is absent, error loudly.
    if mode.requires_jj() && jj_adapter.is_none() {
        return Err(MountError::JjRequired { mode: format!("{:?}", mode) });
    }

    // Open the DB — this runs migrations on both memory + messages (Phase 2).
    let db = Arc::new(ConstellationDb::open(&memory_db_path, &messages_db_path)?);

    // Build cache + supervisor + watcher (Phase 4 primitives).
    let cache = Arc::new(MemoryCache::new(db.clone()));
    let supervisor_handle = SubscriberSupervisor::spawn(cache.clone(), db.clone());
    let watcher = MountWatcher::start(&mount_path, cache.clone())?;

    Ok(MountedStore {
        mount_path,
        config,
        mode,
        cache,
        db,
        supervisor_handle,
        watcher,
        jj: jj_adapter,
    })
}

#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum MountError {
    #[error("no mount found at or above {started_at}")]
    #[diagnostic(
        code(pattern_memory::mount::not_found),
        help("run `pattern mount init <mode>` to initialize a mount here")
    )]
    NotFound { started_at: PathBuf },

    #[error("mount at {path} has invalid directory layout")]
    #[diagnostic(code(pattern_memory::mount::invalid_layout))]
    InvalidLayout { path: PathBuf },

    #[error("mount requires jj adapter but none provided ({mode})")]
    #[diagnostic(code(pattern_memory::mount::jj_required))]
    JjRequired { mode: String },

    #[error("mode {mode} is unavailable: {reason}")]
    #[diagnostic(code(pattern_memory::mount::mode_unavailable))]
    ModeUnavailable { mode: &'static str, reason: String },

    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    #[error(transparent)]
    Db(#[from] pattern_db::DbError),

    #[error(transparent)]
    Paths(#[from] crate::paths::PathError),

    #[error(transparent)]
    Fs(#[from] crate::fs::FsError),

    #[error(transparent)]
    Subscriber(#[from] crate::subscriber::SubscriberError),
}
```

**Testing:**

Integration tests in `tests/mount_lifecycle.rs`:
- `attach` on a path with no mount → `MountError::NotFound` with the diagnostic hinting to run `pattern mount init` (AC9.7).
- Attach/detach round trip in Mode A (tempdir + `mode_a::init` + `attach(tempdir)`): succeeds, writes a block, `detach().await`, re-attach; block still readable (AC9.8).
- Same for Mode B with a fake `JjAdapter` (or real one if `jj` is on PATH).
- Walk-upward test: attach called from a subdirectory 3 levels deep → finds the mount at the right level.
- JjRequired test: Mode B attach with `jj_adapter = None` → `MountError::JjRequired`.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test mount_lifecycle`
Expected: passes.

**Commit:** `[pattern-memory] MountedStore + attach/detach + walk-upward mount discovery`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `pattern_cli` subcommands: `mount init <mode>` + `attach <path>`

**Verifies:** minimum CLI entry points for manual testing + Phase 8 smoke support

**Context:** By the time this phase executes, `pattern_cli` will have been pre-stripped (existing code moved to `rewrite-staging/` for reference) and rebuilt with minimal ratatui scaffolding. Pattern: default invocation enters TUI; named subcommands (like `pattern mount init`) run as one-shot CLI ops + exit. Both live in the same binary via clap. This task adds the `mount` subcommand alongside whatever else exists at that point — no ratatui integration required for `mount`; it's a one-shot command.

**Files:**
- Modify: `crates/pattern_cli/src/main.rs` or equivalent (extend subcommand enum; exact file depends on the post-strip structure)
- Modify: `crates/pattern_cli/Cargo.toml` (add `pattern_memory = { path = "../pattern_memory" }` if not already a dep)

**Implementation:**

```rust
// Extend the existing Cmd enum:
#[derive(clap::Subcommand)]
enum Cmd {
    // ... existing: Auth, Ask, Spawn ...
    Mount(MountCmd),
    Attach {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
}

#[derive(clap::Args)]
struct MountCmd {
    #[command(subcommand)]
    sub: MountSub,
}

#[derive(clap::Subcommand)]
enum MountSub {
    /// Initialize a new mount at the given path with the specified mode.
    Init {
        #[arg(value_enum, long)]
        mode: ModeArg,
        #[arg(long)]
        path: Option<PathBuf>,         // defaults to cwd
        #[arg(long)]
        project_id: Option<String>,    // required for Mode B
    },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum ModeArg { A, B }  // C intentionally absent until spike passes

// Dispatch:
async fn cmd_mount_init(mode: ModeArg, path: PathBuf, project_id: Option<String>) -> Result<(), CliError> {
    match mode {
        ModeArg::A => { pattern_memory::modes::mode_a::init(&path)?; }
        ModeArg::B => {
            let id = project_id.ok_or_else(|| CliError::MissingArg("--project-id required for Mode B".into()))?;
            let adapter = pattern_memory::jj::JjAdapter::detect()?.ok_or(CliError::JjMissing)?;
            pattern_memory::modes::mode_b::init(&id, &adapter)?;
        }
    }
    println!("Mount initialized at {}", path.display());
    Ok(())
}

async fn cmd_attach(path: PathBuf) -> Result<(), CliError> {
    let adapter = pattern_memory::jj::JjAdapter::detect()?.map(Arc::new);
    let mount = pattern_memory::mount::attach(&path, adapter).await?;
    println!("Attached: mode={:?} mount={}", mount.mode, mount.mount_path.display());
    // Drop the mount on exit — this is a smoke test, not a persistent session.
    mount.detach().await?;
    Ok(())
}
```

**Testing:**

Integration test script at `crates/pattern_cli/tests/cli_mount.rs`:
- Spawn `pattern mount init --mode a --path <tempdir>`; assert exit 0 + expected mount layout.
- Spawn `pattern attach <tempdir-from-prior-step>`; assert exit 0 + "Attached" in output.
- Spawn `pattern attach` at a path with no mount; assert non-zero exit + useful diagnostic on stderr.

Library-level tests in `mount_lifecycle.rs` (Task 5) cover the underlying behavior; this test only exercises the CLI wiring (arg parsing, exit codes, stderr format).

**Verification:**

Run: `cargo nextest run -p pattern_cli --test cli_mount`
Expected: passes.

**Commit:** `[pattern-cli] mount init + attach subcommands over pattern_memory library`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 7) -->

### Subcomponent C: Mode C validation spike (main-executor GATE)

<!-- START_TASK_7 -->
### Task 7: Mode C spike — 50-op interleaved host-git + pattern-jj validation

**Verifies:** v3-memory-rework.AC10.1, AC10.2, AC10.3

**Files:**
- Create: `crates/pattern_memory/tests/mode_c_spike.rs` (harness; may not run in CI — see below)
- Create: `docs/notes/YYYY-MM-DD-mode-c-spike.md` (evidence + decision)
- Modify: `crates/pattern_memory/src/modes/mod.rs` (post-spike: either enable `mode-c-experimental` feature + implement Mode C, or fate-marker comment)
- Modify: `crates/pattern_memory/src/modes/mode_c.rs` — **created only if spike passes**

**Gate: main-executor sign-off required.** The spike is not a background mechanical task; it requires interpretation of interleaved-ops results and judgment on whether observed edge cases are acceptable. The main executor (with orual's review) interprets the spike outcome and decides.

**Pre-spike context (record in the note file):**

Upstream jj explicitly cautions about colocated workspaces:

> "Colocated workspaces are less resilient to concurrency issues if you share the repo using an NFS filesystem or Dropbox, and in general, such use of Jujutsu is not currently thoroughly tested."

Pattern's Mode C is similar in shape: pattern-jj at `.pattern/shared/.jj/` + host git at `<project>/.git/` touching the same working directory files. Expect rough edges; document them explicitly in the note file even on pass.

**Spike procedure:**

1. Set up a harness at `tests/mode_c_spike.rs`:
   - `tempfile::TempDir` for the project root.
   - `git init` at root; initial commit with `.gitignore` including `.pattern/shared/.jj/`.
   - `pattern_memory::modes::mode_c::init_experimental(...)` (prototype; ships only if spike passes) to create `.pattern/shared/` + `.pattern/shared/.jj/` + initial `.pattern.kdl`.
   - Harness helper functions for each operation category below.

2. 50-op interleaved sequence (ratios approximate; implementor adjusts):
   - 15 × Host git operations: `add`, `commit`, `checkout <branch>`, `merge`, `stash pop`. Mix directory-level (branch checkout that moves many files) with file-level.
   - 10 × Pattern memory writes (agent-driven): block put, block metadata update, archival insert. Use the Phase 4 subscriber-aware path — writes go through `MemoryStore`, subscribers fire, fs emissions happen.
   - 10 × Pattern jj operations: `jj commit`, `jj bookmark set`, `jj log`, `jj workspace update-stale`.
   - 8 × Pattern attach/detach: mount a project, write some blocks, detach, re-attach.
   - 7 × External `.md` edits via direct file write (simulating human editing outside Pattern).

3. **Divergence check procedure** (after each op cluster):
   - After each host git op: `jj log` without errors; `jj workspace update-stale` if needed; post-sync view reflects the git change.
   - After each pattern jj commit: `git status` clean with respect to tracked files (`.jj/` stays gitignored).
   - At checkpoints: `memory.db` index rows match emitted block files match loro doc state — all three agree.
   - At attach/detach boundaries: no leaked subscriber tasks (`MemoryCache::stats()` or supervisor `active_count()` reports zero post-detach); no stale locks.
   - Final check: `git log --all --oneline` + `jj log` on their respective views — both internally consistent (no orphaned commits, no corrupt refs).

4. **Pass criteria** (all must hold):
   - Zero divergence events across the 50 operations.
   - Every host git operation followed by at most one `jj workspace update-stale` produces a clean consistent state.
   - No manual intervention required for any standard developer workflow (pull, merge, checkout, commit).

5. **Fail criteria** (any one):
   - Any corruption observed (memory.db mismatches files, dangling FTS rows, subscriber can't recover).
   - Any state divergence requiring manual repair.
   - Any scenario where gitignore alone is insufficient and the user would need additional config to avoid breakage.

6. **Outcome documentation.** Regardless of outcome, write `docs/notes/YYYY-MM-DD-mode-c-spike.md` with:
   - Environment (jj version, git version, OS).
   - The exact 50-op sequence.
   - Observations per-op.
   - Pass or fail verdict.
   - If pass: list of rough edges discovered that users should know about (e.g., "after a branch checkout of >50 files, `jj workspace update-stale` must be run manually before the next memory write or else subscriber debounce windows may emit stale content").
   - If fail: specific failure mode(s), reproducer steps, why this makes Mode C unshippable as-is.

**Post-spike action** (main executor, post-gate):

**If PASS:** enable the `mode-c-experimental` feature in `pattern_memory/Cargo.toml`, implement `modes/mode_c.rs` init + attach logic (parallel to Mode B but with sidecar layout + host-gitignore management), extend `pattern mount init` to accept `--mode c`, commit.

**If FAIL:** do not implement `modes/mode_c.rs`. Add a fate-marker comment in `modes/mod.rs`:

```rust
// FATE: Mode C (sidecar pattern-jj over host-git) deferred after 2026-04 spike.
// See docs/notes/YYYY-MM-DD-mode-c-spike.md for the evidence.
// StorageMode::C variant intentionally absent from the enum until a future
// plan re-opens the question with improved primitives (e.g., jj-lib post-1.0
// or upstream support for colocated-with-foreign-VCS).
```

And update the design plan (`docs/design-plans/2026-04-19-v3-memory-rework.md`) with a "verified: YYYY-MM-DD" stamp next to Mode C's description, citing the note file.

**Testing:**

The spike harness itself is a test. If Mode C ships, the harness stays committed as a regression guard. If Mode C doesn't ship, the harness is deleted (fate-markered out of the repo) — don't keep dead code around.

**Verification:**

The verification IS the spike outcome + note file. No `cargo nextest` assertion at this stage — interpretation is human.

**Commit (pass case):** `[pattern-memory] Mode C spike passed: documented rough edges + enabled experimental mode`
**Commit (fail case):** `[pattern-memory] Mode C deferred: 2026-04 spike documented in docs/notes/`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase 6 Done-when recap

- `cargo check --workspace` clean.
- `cargo nextest run -p pattern_memory` green (config parsing, paths, VCS detection, mode init, mount lifecycle, CLI mount tests).
- Mode A end-to-end test passes (AC9.1): temp host-git repo + init + write + commit + verify.
- Mode B end-to-end test passes (AC9.2): pattern-jj + init + write + quiesce + commit + verify.
- AC9.3, AC9.4 path invariants covered by path-resolver unit tests.
- `.pattern.kdl` parsing handles representative configs + surfaces clear errors on malformed ones (AC9.5).
- `attach(path)` walks up + opens dbs + spawns subscribers (AC9.6); missing mount → clear error (AC9.7); detach + re-attach roundtrip (AC9.8).
- Mode C spike executed + documented in `docs/notes/YYYY-MM-DD-mode-c-spike.md` (AC10.3).
- If spike passed: Mode C ships with documented rough edges (AC10.1).
- If spike failed: fate-marker in `modes/mod.rs` + design-plan update (AC10.2).

## Notes for downstream phases

- **Phase 7** (messages backup): `MountedStore` owns the `ConstellationDb`; Phase 7's backup scheduler hangs off `MountedStore`, cancel-token tied to mount lifecycle. Scheduler uses `paths::mode_a_messages_path` / `mode_b_messages_path` to locate the source db (Phase 7 doesn't re-derive).
- **Phase 8** (smoke): capstone smoke test is library-level — calls `pattern_memory::modes::mode_a::init` + `pattern_memory::mount::attach` + write + quiesce + `git commit` + restart (process reload simulation) + re-attach + read directly via the public crate API. CLI-level verification happens in Task 6's `cli_mount` integration test; the capstone doesn't need to shell through the CLI.
- **Mode C future**: if the spike fails and Mode C ships as documented-only, the next opportunity to reconsider is likely when jj-lib hits 1.0 (enabling in-process operations without subprocess fragility) or when upstream jj explicitly blesses dual-VCS usage. Track as a port-list note.
- **Packaging implication** (from Phase 5): non-NixOS distributions bundle `jj`. Also now need to ensure `git` is reachable (for gix-discover detection); typically safe to assume users have git, but document in packaging.
