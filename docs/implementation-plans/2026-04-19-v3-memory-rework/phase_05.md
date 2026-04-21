# Pattern v3 Memory Rework — Phase 5 Implementation Plan

**Goal:** Implement a thin jj CLI adapter (~15-18 functions) that shells out to the user's `jj` binary with `-T 'json(...)'` templates, plus a universal `quiesce()` step that drains Phase 4 sync subscribers, runs `PRAGMA wal_checkpoint(TRUNCATE)` on `memory.db`, and fsyncs emitted canonical files. Introduce the `StorageMode` enum (per-mode detection + jj integration gating).

**Architecture:** `JjAdapter` is a bounded, resolution-minimal wrapper over the `jj` CLI. Every parseable command uses `-T 'json(...)'` with a minimal set of fields — fewer fields = less fragility to jj-version drift. Free-form outputs (commit messages, diffs) pass through as strings without parsing. The adapter serializes workspace-mutation calls via an internal `Mutex` because jj's documented concurrent-workspace-add issue #9314 can leave workspaces in an unusable sibling-operation state. `JjAdapter::detect()` checks binary availability + version range; missing binary OR unsupported version returns a typed error that Mode A tolerates (it never asks for jj) and Modes B/C surface loudly at attach time. `quiesce()` is a universal pre-commit step (all modes) that drains subscribers through the Phase 4 cancel-token pattern, runs the WAL checkpoint, and fsyncs emitted files. In Mode A the caller invokes `quiesce()` before the host VCS commit; in Modes B/C, `quiesce()` runs as the first step of `JjAdapter::commit(...)`.

**Why CLI, not jj-lib (explicit decision record):**

Both options are pre-1.0-fragile. The deciding factor is **on-disk format ownership**:

- With CLI: whichever `jj` binary the user has installed owns `.jj/` format. Pattern is a stateless subprocess invoker. If user runs `jj` directly against `.pattern/shared/.jj/` (normal in Mode A if their host VCS is jj; possible in Mode C), Pattern and the user always speak the same on-disk dialect — by construction.
- With jj-lib: Pattern pins a specific jj-lib version in Cargo.toml. If user upgrades their installed `jj` to a newer version that writes `.jj/` in an incompatible format, and then Pattern (pinned to older jj-lib) reads or writes that same directory in Mode A or C, the formats can drift and corrupt. Pattern could minimize this in Mode B (Pattern is sole writer), but A + C remain exposed.

Additional CLI-favoring factors:
- jj-lib pre-1.0 API churn forces invasive adapter refactors on each jj release; CLI changes are template/flag tweaks in one module.
- jj-lib panics propagate into Pattern's process ("comparable to segfaults in impact" per upstream research); CLI failures surface as Results carrying stderr.
- The CLI treats free-form output (commit messages, diffs) as opaque strings — no parsing fragility there at all.

Mitigations we bake in to keep CLI-fragility bounded:
- **Minimal template fields per call** — request only what we strictly need.
- **Forgiving serde parse** (`deny_unknown_fields = false`) — tolerant of new fields; only missing/renamed fields trip an error.
- **Tested version range** constants (`MIN_SUPPORTED_VERSION`, `MAX_TESTED_VERSION`) in the adapter module.
- **Pass-through for truly free-form output** — don't parse diffs or commit messages; return as strings.
- **CI canary** that exercises every adapter function against whichever jj the CI runner has.

**Packaging implication:** non-NixOS release bundles must ship the `jj` binary alongside `tidepool-extract`. The tidepool distribution pipeline already handles "bundle a hefty external tool"; adding jj is incremental. Document in packaging tooling; out of scope for this implementation plan.

**Tech Stack:**
- Uses existing workspace deps: `which = "8.0"` (binary detection, already workspace-level); `tempfile 3` (integration test fixtures); `miette + thiserror` (error-enum shape matching pattern_core::error::MemoryError).
- New dev-dep: none (tempfile + insta already present).
- Sync subprocess pattern via `std::process::Command`, mirroring `pattern_runtime::preflight` conventions.

**Scope:** Phase 5 of 8.

**Codebase verified:** 2026-04-19 (codebase-investigator agent acd2ffde5182ea8a1).
**jj CLI state verified:** 2026-04-19 (internet-researcher agent acf2e5c828ff33b45).

**Execution posture:** Autonomous subagent delegation appropriate for the adapter shell + templates (mechanical). Main executor reviews quiesce integration with Phase 4's drain surface (single checkpoint). No sub-task gates within Phase 5.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC8: jj CLI adapter + pre-commit quiesce

- **v3-memory-rework.AC8.1 Success:** `JjAdapter::detect` returns `Some` on systems with `jj` in PATH and supported version
- **v3-memory-rework.AC8.2 Success:** All ~15-18 adapter functions execute their jj subcommand and parse JSON-templated output correctly
- **v3-memory-rework.AC8.3 Success:** `quiesce()` drains all sync_workers, calls `wal_checkpoint(TRUNCATE)`, and fsyncs emitted files before returning
- **v3-memory-rework.AC8.4 Success:** In Mode A (no jj adapter), `quiesce()` still runs and produces a canonical `memory.db` for host VCS to commit
- **v3-memory-rework.AC8.5 Failure:** `JjAdapter::detect` returns `None` on systems without `jj`; no panic; Mode A continues working
- **v3-memory-rework.AC8.6 Failure:** `jj --version` returning an unsupported version surfaces `JjError::UnsupportedVersion` with clear message
- **v3-memory-rework.AC8.7 Failure:** jj subcommand failure surfaces `JjError::SubprocessFailed` carrying stderr; caller gets typed error, not stringly-typed
- **v3-memory-rework.AC8.8 Edge:** Adapter respects `--color=never` in all invocations; output parsing doesn't choke on ANSI codes

---

## Codebase verification findings

Relevant realities shaping the task breakdown:

- ✓ `which = "8.0"` is a workspace dep (`Cargo.toml:114`), already inherited by pattern_runtime. Phase 5 inherits it in pattern_memory.
- ✓ Subprocess convention is `std::process::Command` sync with piped stdout/stderr, manual `try_wait` loop — see `pattern_runtime/src/preflight.rs:15, 101-174`. Phase 5 matches this.
- ✓ `pattern_core::error::MemoryError` establishes the `#[non_exhaustive] #[derive(Error, Diagnostic)]` pattern with miette `#[diagnostic(code(...), help(...))]`. Phase 5's `JjError` follows the same shape.
- ✓ `tempfile 3` is workspace dev-dep; `TempDir::new()` is the idiomatic fixture. Phase 5's jj adapter integration tests spawn real `jj` against tempdir-init'd repos.
- ✓ Phase 4's sync subscribers are supervised via a tokio task; the supervisor owns a `tokio_util::sync::CancellationToken` per worker. Phase 5's `quiesce()` signals the supervisor's "drain all" method — exact shape is TBD by Phase 4 executor; Phase 5 assumes a method signature like `supervisor.drain_all().await -> Result<(), SubscriberError>` which exits all workers cleanly and joins their threads before returning. If Phase 4 lands with a different surface, Phase 5 adapts.
- ✓ `PRAGMA wal_checkpoint(TRUNCATE)` has no prior usage in the codebase. Phase 5 introduces it fresh, invoked against the dedicated `memory.db` connection borrowed from the pool.
- ✓ No `StorageMode` enum exists. Phase 5 creates `crates/pattern_memory/src/modes/mod.rs` with `StorageMode::A | B | C`; Phase 6 extends with mode-specific path resolution + attachment logic.
- ✓ `metrics` crate: Phase 4 introduces; Phase 5 uses as a consumer. Counters: `memory.jj.subprocess.failed`, `memory.jj.version_check.failed`, `memory.quiesce.drain_timeout`. Gauge: none in this phase.
- ✓ fsync pattern: Phase 4 established `File::sync_all()` in `fs::atomic_write`. Phase 5's quiesce fsyncs individual canonical files by opening each, calling `sync_all()`, dropping — mirrors the pattern.
- ✓ No `.jjconfig.toml` in the repo root; no established minimum jj version anywhere documented. Phase 5 defines `MIN_SUPPORTED_VERSION = "0.38"` based on research: that's the latest confirmed stable per crates.io as of 2026-04, and the version where `-T 'json(...)'` is mature.
- ✓ Pattern CI doesn't currently invoke jj. Phase 5 adds a CI canary test that runs the full adapter suite against whichever jj is on the runner — details under Task 6.
- ✓ `pattern_memory/CLAUDE.md` will have been created in Phase 1 (stub) + freshened in Phase 4 (subscriber section). Phase 5 adds the jj adapter + quiesce + StorageMode sections.

---

## Dependency changes

`crates/pattern_memory/Cargo.toml`:

```toml
[dependencies]
# ... existing from Phases 1-4 ...

which = { workspace = true }     # binary detection
semver = "1"                     # parsing jj --version output

[dev-dependencies]
# ... existing ...
# no new dev-deps; tempfile + insta already present
```

No workspace-level `Cargo.toml` changes.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

### Subcomponent A: `JjAdapter` — detection, error types, adapter functions

<!-- START_TASK_1 -->
### Task 1: `JjError` + `JjAdapter::detect()` + version check

**Verifies:** v3-memory-rework.AC8.1, AC8.5, AC8.6

**Files:**
- Create: `crates/pattern_memory/src/jj/mod.rs` (re-exports)
- Create: `crates/pattern_memory/src/jj/error.rs`
- Create: `crates/pattern_memory/src/jj/adapter.rs` (partial — struct + detect())
- Create: `crates/pattern_memory/src/jj/version.rs` (version parsing via `semver`)

**Implementation:**

1. `jj/error.rs`:

   ```rust
   use std::path::PathBuf;

   #[non_exhaustive]
   #[derive(Debug, thiserror::Error, miette::Diagnostic)]
   pub enum JjError {
       #[error("jj binary not found on PATH")]
       #[diagnostic(
           code(pattern_memory::jj::binary_not_found),
           help("install jj via your package manager or https://jj-vcs.github.io/jj/install-and-setup/. Pattern requires jj >= {min}")
       )]
       BinaryNotFound { min: String },

       #[error("jj version {installed} is below minimum supported {min}")]
       #[diagnostic(
           code(pattern_memory::jj::unsupported_version),
           help("upgrade jj to {min} or later")
       )]
       UnsupportedVersion { installed: String, min: String },

       #[error("could not parse jj --version output: {raw}")]
       #[diagnostic(code(pattern_memory::jj::version_parse))]
       VersionParse { raw: String },

       #[error("jj subprocess failed (exit {status}): {stderr}")]
       #[diagnostic(code(pattern_memory::jj::subprocess_failed))]
       SubprocessFailed {
           command: String,
           status: i32,
           stderr: String,
       },

       #[error("jj output parse failed for command {command}: {reason}")]
       #[diagnostic(code(pattern_memory::jj::output_parse))]
       OutputParseFailed {
           command: String,
           reason: String,
       },

       #[error("workspace not found: {name}")]
       #[diagnostic(code(pattern_memory::jj::workspace_not_found))]
       WorkspaceNotFound { name: String },

       #[error("bookmark not found: {name}")]
       #[diagnostic(code(pattern_memory::jj::bookmark_not_found))]
       BookmarkNotFound { name: String },

       #[error("io error invoking jj: {source}")]
       #[diagnostic(code(pattern_memory::jj::io))]
       Io {
           #[source]
           source: std::io::Error,
           context: String,
       },
   }

   pub type JjResult<T> = Result<T, JjError>;
   ```

2. `jj/version.rs`:

   ```rust
   use semver::Version;

   /// Minimum jj version Pattern supports. Bump along with `MAX_TESTED_VERSION`
   /// when regression-testing against a newer jj.
   pub const MIN_SUPPORTED_VERSION: &str = "0.38.0";
   /// Most recent jj version Pattern's adapter has been regression-tested against.
   /// Values above this may work but are not guaranteed; log a warning.
   pub const MAX_TESTED_VERSION: &str = "0.40.0";

   pub fn parse_jj_version(raw: &str) -> Result<Version, JjError> {
       // jj --version output is like: "jj 0.38.0" or "jj 0.40.0-1234-g..."
       let token = raw
           .split_whitespace()
           .nth(1)
           .ok_or_else(|| JjError::VersionParse { raw: raw.to_owned() })?;
       // Strip any git-rev suffix: "0.40.0-1234-gabcdef" → "0.40.0"
       let clean = token.split('-').next().unwrap_or(token);
       Version::parse(clean)
           .map_err(|_| JjError::VersionParse { raw: raw.to_owned() })
   }

   pub fn is_supported(v: &Version) -> bool {
       let min = Version::parse(MIN_SUPPORTED_VERSION).expect("static version parses");
       v >= &min
   }

   pub fn is_tested(v: &Version) -> bool {
       let max = Version::parse(MAX_TESTED_VERSION).expect("static version parses");
       v <= &max
   }
   ```

3. `jj/adapter.rs` (partial):

   ```rust
   use std::path::PathBuf;
   use std::process::Command;
   use std::sync::Mutex;

   /// Thin wrapper over the `jj` CLI. Serializes workspace mutations via an
   /// internal Mutex to avoid jj's documented concurrent-workspace-add hazard
   /// (jj-vcs/jj#9314). Holds the absolute path to the detected binary.
   pub struct JjAdapter {
       binary: PathBuf,
       version: semver::Version,
       mutation_lock: Mutex<()>,
   }

   impl JjAdapter {
       /// Probe for `jj` on PATH and check version. Returns `Ok(Some(..))` if
       /// a supported jj is found; `Ok(None)` if jj is missing (Mode A is fine
       /// with this); `Err(..)` if jj is found but the version is unsupported
       /// or the probe itself fails.
       pub fn detect() -> JjResult<Option<Self>> {
           let binary = match which::which("jj") {
               Ok(path) => path,
               Err(_) => return Ok(None),  // AC8.5: missing is not an error
           };

           let output = Command::new(&binary)
               .args(["--version", "--color", "never"])
               .output()
               .map_err(|e| JjError::Io {
                   source: e,
                   context: "invoking jj --version".into(),
               })?;

           if !output.status.success() {
               return Err(JjError::SubprocessFailed {
                   command: "jj --version".into(),
                   status: output.status.code().unwrap_or(-1),
                   stderr: String::from_utf8_lossy(&output.stderr).to_string(),
               });
           }

           let raw = String::from_utf8_lossy(&output.stdout).to_string();
           let version = version::parse_jj_version(&raw)?;

           if !version::is_supported(&version) {
               return Err(JjError::UnsupportedVersion {
                   installed: version.to_string(),
                   min: version::MIN_SUPPORTED_VERSION.into(),
               });
           }

           if !version::is_tested(&version) {
               tracing::warn!(
                   installed = %version,
                   tested_max = version::MAX_TESTED_VERSION,
                   "jj version exceeds Pattern's regression-tested maximum; \
                    proceed with caution if behavior drift is observed"
               );
           }

           Ok(Some(Self {
               binary,
               version,
               mutation_lock: Mutex::new(()),
           }))
       }

       pub fn version(&self) -> &semver::Version { &self.version }

       /// Build a base command with `--color=never` always set.
       /// All adapter functions call this helper rather than `Command::new`
       /// directly, guaranteeing AC8.8 (no ANSI in parsed output).
       pub(crate) fn cmd(&self) -> Command {
           let mut c = Command::new(&self.binary);
           c.args(["--color", "never"]);
           c
       }
   }
   ```

**Testing:**

Unit tests (`tests/version_parse.rs` or inline):
- Parse `"jj 0.38.0"` → `Version::new(0, 38, 0)`.
- Parse `"jj 0.40.0-1234-g0abcdef"` → strip git-rev → `Version::new(0, 40, 0)`.
- `is_supported(0.37)` → false; `is_supported(0.38)` → true.
- `parse_jj_version("garbled")` → `Err(VersionParse)`.

Integration test (`tests/detect.rs`):
- `JjAdapter::detect()` on the CI runner returns `Ok(Some(_))` if jj is installed, `Ok(None)` otherwise. Test uses an env-var to select expected outcome, so the same test runs on both NixOS (has jj) and minimal containers.
- Use `PATH=""` env override to simulate "missing jj" → assert `Ok(None)`.
- Feed a mocked version via a test-only binary shim (or skip on CI without ability to set up). If mocking is hard, document the UnsupportedVersion path via unit test of the predicate alone.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib jj::version`
Expected: all unit tests pass.

Run: `cargo nextest run -p pattern_memory --test detect`
Expected: passes on the CI runner (jj present or absent handled).

**Commit:** `[pattern-memory] jj::adapter detect + version check + JjError type`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Core adapter functions — log, workspace_list, bookmark_list (read-only template-parsing path)

**Verifies:** v3-memory-rework.AC8.2, AC8.7, AC8.8

**Files:**
- Modify: `crates/pattern_memory/src/jj/adapter.rs` (add read-only functions)
- Create: `crates/pattern_memory/src/jj/templates.rs` (template strings as constants)
- Create: `crates/pattern_memory/src/jj/types.rs` (typed output structs)

**Implementation:**

1. `jj/templates.rs` — minimal template strings, each requests only fields Pattern needs:

   ```rust
   /// Template for `jj log -T '<this>' --no-graph`.
   /// Requests exactly the fields our adapter's `log()` returns.
   /// Keep minimal to reduce fragility on jj version upgrades.
   pub const LOG_TEMPLATE: &str =
       "json({ change_id: change_id.shortest(), commit_id: commit_id.shortest(), description: description }) ++ \"\\n\"";

   pub const WORKSPACE_LIST_TEMPLATE: &str =
       "json({ name: name, working_copy: working_copy.commit_id().shortest() }) ++ \"\\n\"";

   pub const BOOKMARK_LIST_TEMPLATE: &str =
       "json({ name: name, target: target.commit_id().shortest() }) ++ \"\\n\"";
   ```

   **Template shape verification** happens at Task 2 implementation time — jj template syntax for shortest(), method calls on types, etc. may need minor tweaks. The implementor runs each template against a real jj workspace and adjusts until the JSON lines parse. Document the exact template string used in the commit message for traceability.

2. `jj/types.rs`:

   ```rust
   use serde::Deserialize;

   /// Minimal log entry as returned by `JjAdapter::log()`. Tolerant of extra
   /// fields jj might add in future versions (deny_unknown_fields = false by
   /// default; we rely on the default).
   #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
   pub struct JjLogEntry {
       pub change_id: String,
       pub commit_id: String,
       pub description: String,
   }

   #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
   pub struct JjWorkspace {
       pub name: String,
       pub working_copy: String,
   }

   #[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
   pub struct JjBookmark {
       pub name: String,
       pub target: String,
   }
   ```

3. Adapter functions:

   ```rust
   impl JjAdapter {
       /// `jj log -r <revset> -T LOG_TEMPLATE --no-graph`.
       /// One JSON line per commit; parses newline-delimited.
       pub fn log(&self, workspace_root: &Path, revset: &str) -> JjResult<Vec<JjLogEntry>> {
           let output = self.cmd()
               .current_dir(workspace_root)
               .args([
                   "log",
                   "-r", revset,
                   "-T", templates::LOG_TEMPLATE,
                   "--no-graph",
               ])
               .output()
               .map_err(|e| JjError::Io { source: e, context: "jj log".into() })?;
           check_success(&output, "jj log")?;
           parse_jsonl::<JjLogEntry>(&output.stdout, "jj log")
       }

       pub fn workspace_list(&self, repo_root: &Path) -> JjResult<Vec<JjWorkspace>> {
           let output = self.cmd()
               .current_dir(repo_root)
               .args([
                   "workspace", "list",
                   "-T", templates::WORKSPACE_LIST_TEMPLATE,
               ])
               .output()
               .map_err(|e| JjError::Io { source: e, context: "jj workspace list".into() })?;
           check_success(&output, "jj workspace list")?;
           parse_jsonl::<JjWorkspace>(&output.stdout, "jj workspace list")
       }

       pub fn bookmark_list(&self, repo_root: &Path) -> JjResult<Vec<JjBookmark>> {
           let output = self.cmd()
               .current_dir(repo_root)
               .args([
                   "bookmark", "list",
                   "-T", templates::BOOKMARK_LIST_TEMPLATE,
               ])
               .output()
               .map_err(|e| JjError::Io { source: e, context: "jj bookmark list".into() })?;
           check_success(&output, "jj bookmark list")?;
           parse_jsonl::<JjBookmark>(&output.stdout, "jj bookmark list")
       }
   }

   fn check_success(output: &std::process::Output, cmd: &str) -> JjResult<()> {
       if output.status.success() { return Ok(()); }
       Err(JjError::SubprocessFailed {
           command: cmd.into(),
           status: output.status.code().unwrap_or(-1),
           stderr: String::from_utf8_lossy(&output.stderr).to_string(),
       })
   }

   fn parse_jsonl<T: for<'de> serde::Deserialize<'de>>(
       bytes: &[u8],
       cmd: &str,
   ) -> JjResult<Vec<T>> {
       let text = std::str::from_utf8(bytes).map_err(|e| JjError::OutputParseFailed {
           command: cmd.into(),
           reason: format!("invalid utf-8: {e}"),
       })?;
       let mut out = Vec::new();
       for (line_no, line) in text.lines().enumerate() {
           if line.trim().is_empty() { continue; }
           let v: T = serde_json::from_str(line).map_err(|e| JjError::OutputParseFailed {
               command: cmd.into(),
               reason: format!("line {}: {}", line_no + 1, e),
           })?;
           out.push(v);
       }
       Ok(out)
   }
   ```

**Testing:**

Integration tests in `crates/pattern_memory/tests/jj_adapter_read.rs`:
- Spin up a tempdir with `jj init`; create 2-3 commits via `jj describe + jj new`; call `adapter.log(..)` and assert 2-3 entries returned with non-empty change_id/commit_id/description.
- Create a bookmark via `jj bookmark set mybm -r @-`; call `bookmark_list`; assert the bookmark is found with correct target.
- Call `workspace_list`; assert at least one workspace is returned.
- Invalid revset → `SubprocessFailed` with stderr containing "revset" or similar (AC8.7).
- `insta` snapshots for each adapter function's parsed output, pinning the exact shape. Snapshots updated only on intentional changes.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test jj_adapter_read`
Expected: passes on CI runner with jj installed.

**Commit:** `[pattern-memory] jj adapter: log, workspace_list, bookmark_list with JSON template parsing`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Mutation adapter functions — init_repo, workspace_add/forget/update-stale, commit, describe, bookmark_set/delete, new(merge), restore

**Verifies:** v3-memory-rework.AC8.2

**Files:**
- Modify: `crates/pattern_memory/src/jj/adapter.rs` (add mutation functions)

**Implementation:**

All mutation functions acquire `self.mutation_lock` before spawning jj to serialize against the concurrent-workspace-add hazard (jj-vcs/jj#9314).

```rust
impl JjAdapter {
    /// `jj init` at the given path. For Mode B (separate pattern-jj repo) or
    /// Mode C spike (sidecar over host git).
    pub fn init_repo(&self, path: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(path)
            .args(["init"])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj init".into() })?;
        check_success(&output, "jj init")?;
        // Configure local user.name/email to avoid "empty identity" errors
        // on subsequent commits. These can be overridden by the user's global
        // jj config — we only set if missing.
        // (Exact invocation: `jj config set --repo user.name pattern-runtime` etc.
        //  Implementor verifies the idempotent-set pattern against jj 0.38.)
        Ok(())
    }

    pub fn workspace_add(&self, repo_root: &Path, new_workspace_path: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(repo_root)
            .args(["workspace", "add", new_workspace_path.to_str().unwrap_or("")])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj workspace add".into() })?;
        check_success(&output, "jj workspace add")
    }

    pub fn workspace_forget(&self, repo_root: &Path, name: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(repo_root)
            .args(["workspace", "forget", name])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj workspace forget".into() })?;
        // Forget of nonexistent → typed WorkspaceNotFound
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist") || stderr.contains("not found") {
                return Err(JjError::WorkspaceNotFound { name: name.into() });
            }
            return Err(JjError::SubprocessFailed {
                command: format!("jj workspace forget {name}"),
                status: output.status.code().unwrap_or(-1),
                stderr: stderr.to_string(),
            });
        }
        Ok(())
    }

    pub fn workspace_update_stale(&self, workspace_root: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(workspace_root)
            .args(["workspace", "update-stale"])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj workspace update-stale".into() })?;
        check_success(&output, "jj workspace update-stale")
    }

    /// Commit the current working copy with message. Design allows empty commits
    /// for the quiesce-anchored checkpoint pattern; use --allow-empty if available
    /// in the current jj version (the research flagged this flag as undocumented;
    /// implementor verifies at runtime and falls back to skipping the commit if
    /// there's nothing to commit and the flag is missing).
    pub fn commit(&self, workspace_root: &Path, message: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(workspace_root)
            .args(["commit", "-m", message])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj commit".into() })?;
        check_success(&output, "jj commit")
    }

    /// Edit in place — update working-copy commit's message without creating a new commit.
    pub fn describe(&self, workspace_root: &Path, message: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(workspace_root)
            .args(["describe", "-m", message])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj describe".into() })?;
        check_success(&output, "jj describe")
    }

    pub fn bookmark_set(&self, repo_root: &Path, name: &str, revset: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(repo_root)
            .args(["bookmark", "set", name, "-r", revset])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj bookmark set".into() })?;
        check_success(&output, "jj bookmark set")
    }

    pub fn bookmark_delete(&self, repo_root: &Path, name: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self.cmd()
            .current_dir(repo_root)
            .args(["bookmark", "delete", name])
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj bookmark delete".into() })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist") || stderr.contains("not found") {
                return Err(JjError::BookmarkNotFound { name: name.into() });
            }
            return Err(JjError::SubprocessFailed {
                command: format!("jj bookmark delete {name}"),
                status: output.status.code().unwrap_or(-1),
                stderr: stderr.to_string(),
            });
        }
        Ok(())
    }

    /// Merge using `jj new <parents>` (jj merge is deprecated since 0.14.0).
    pub fn merge(&self, workspace_root: &Path, parent_revs: &[&str], message: Option<&str>) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let mut args: Vec<&str> = vec!["new"];
        args.extend(parent_revs);
        let output = self.cmd()
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj new (merge)".into() })?;
        check_success(&output, "jj new (merge)")?;
        if let Some(msg) = message {
            self.describe(workspace_root, msg)?;
        }
        Ok(())
    }

    /// Restore paths from a source revision. Uses `--from` / `--into` per jj 0.38+.
    pub fn restore(&self, workspace_root: &Path, from_rev: &str, paths: &[&Path]) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let mut args: Vec<String> = vec![
            "restore".into(),
            "--from".into(), from_rev.into(),
        ];
        for p in paths {
            args.push(p.to_string_lossy().to_string());
        }
        let output = self.cmd()
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .map_err(|e| JjError::Io { source: e, context: "jj restore".into() })?;
        check_success(&output, "jj restore")
    }
}

fn poisoned() -> JjError {
    JjError::SubprocessFailed {
        command: "internal mutex".into(),
        status: -2,
        stderr: "mutation lock was poisoned by a prior panic".into(),
    }
}
```

**Testing:**

Integration tests in `tests/jj_adapter_mutate.rs`:
- Init a tempdir repo; call `workspace_add` to create a sibling workspace; call `workspace_list` and verify both appear.
- Commit a change; verify `log` returns it.
- Bookmark set/delete round-trip.
- Merge: create two diverging commits, call `merge`, assert the merge commit has both as parents via `log`.
- `workspace_forget` on nonexistent name → `WorkspaceNotFound` error (AC8.7).
- `bookmark_delete` on nonexistent name → `BookmarkNotFound` error.
- Concurrent mutation test: spawn 5 threads, each calling `bookmark_set` with a different name; assert all succeed serially (no sibling-op errors).

**Verification:**

Run: `cargo nextest run -p pattern_memory --test jj_adapter_mutate`
Expected: passes.

**Commit:** `[pattern-memory] jj adapter mutation functions with mutation lock + typed not-found errors`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

### Subcomponent B: `quiesce()` + `StorageMode` + CI canary

<!-- START_TASK_4 -->
### Task 4: `StorageMode` enum + `JjAdapter` integration points

**Verifies:** v3-memory-rework.AC8.4 (prerequisite — Mode A doesn't need adapter)

**Files:**
- Create: `crates/pattern_memory/src/modes/mod.rs` (enum definition)

**Implementation:**

```rust
use std::path::PathBuf;
use crate::jj::JjAdapter;

/// Storage mode for a Pattern mount.
///
/// - Mode A: in-repo, host VCS owns history; Pattern never runs `jj`.
/// - Mode B: separate `~/.pattern/projects/<id>/` directory; pattern-jj owns history.
/// - Mode C: sidecar — `.pattern/shared/.jj/` alongside host `.git/` in the same
///   working copy. Gated on Phase 6 validation spike.
///
/// Phase 5 introduces this enum skeleton. Phase 6 adds the per-mode attach/detach
/// logic + `.pattern.kdl` config parsing.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum StorageMode {
    A { mount_path: PathBuf },
    B { mount_path: PathBuf, project_id: String },
    C { mount_path: PathBuf },
}

impl StorageMode {
    pub fn mount_path(&self) -> &std::path::Path {
        match self {
            StorageMode::A { mount_path } => mount_path,
            StorageMode::B { mount_path, .. } => mount_path,
            StorageMode::C { mount_path } => mount_path,
        }
    }

    /// Whether this mode requires a jj adapter at attach time.
    /// Mode A works without jj; Modes B and C require it.
    pub fn requires_jj(&self) -> bool {
        matches!(self, StorageMode::B { .. } | StorageMode::C { .. })
    }
}
```

**Testing:** Simple unit tests on `requires_jj()` and `mount_path()`.

**Verification:** `cargo check -p pattern_memory` compiles.

**Commit:** `[pattern-memory] StorageMode enum + JjAdapter integration points`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `quiesce()` — drain subscribers + wal_checkpoint + fsync emitted files

**Verifies:** v3-memory-rework.AC8.3, AC8.4

**Files:**
- Create: `crates/pattern_memory/src/jj/quiesce.rs`

**Implementation:**

```rust
use std::time::{Duration, Instant};
use std::path::Path;
use crate::subscriber::SubscriberSupervisor;
use pattern_db::ConstellationDb;

/// Quiesce the mount: drain all sync_workers, checkpoint memory.db's WAL,
/// and fsync emitted canonical files. Universal across storage modes:
/// - Mode A: caller invokes before the host VCS commit.
/// - Modes B/C: `JjAdapter::commit` invokes this as step 1.
///
/// Returns when the mount's on-disk state is canonical + resumable.
/// Drain timeout defaults to 10s; if exceeded, logs ERROR and proceeds
/// with a best-effort checkpoint (the supervisor may restart workers
/// after quiesce returns — acceptable, since the checkpoint capturing
/// in-flight state is better than hanging indefinitely).
pub async fn quiesce(
    supervisor: &SubscriberSupervisor,
    db: &ConstellationDb,
    emitted_file_paths: impl IntoIterator<Item = impl AsRef<Path>>,
    drain_timeout: Duration,
) -> Result<QuiesceOutcome, QuiesceError> {
    let t0 = Instant::now();

    // 1. Drain subscribers. Phase 4's supervisor exposes drain_all() which
    //    signals every sync_worker's cancel token + awaits join.
    match tokio::time::timeout(drain_timeout, supervisor.drain_all()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = %e, "subscriber drain failed");
            metrics::counter!("memory.quiesce.drain_failed").increment(1);
            // Proceed to checkpoint anyway — partial drain + checkpoint is still
            // better than aborting. The downstream commit captures whatever state
            // landed; future writes go into a fresh subscriber after quiesce returns.
        }
        Err(_) => {
            tracing::error!(timeout_ms = drain_timeout.as_millis(), "subscriber drain timed out");
            metrics::counter!("memory.quiesce.drain_timeout").increment(1);
        }
    }

    // 2. Checkpoint WAL on memory.db (not messages.db — messages lives outside VCS).
    let conn = db.get().map_err(QuiesceError::PoolAcquire)?;
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", [])
        .map_err(QuiesceError::WalCheckpoint)?;

    // 3. fsync emitted canonical files. Best-effort: log on per-file failures
    //    but continue — partial fsync is still better than none.
    let mut fsync_failures = 0usize;
    for path in emitted_file_paths {
        let p = path.as_ref();
        if let Err(e) = fsync_file(p) {
            tracing::warn!(path = %p.display(), error = %e, "fsync failed");
            fsync_failures += 1;
        }
    }

    let duration = t0.elapsed();
    metrics::histogram!("memory.quiesce.duration_ms").record(duration.as_millis() as f64);

    Ok(QuiesceOutcome {
        duration,
        fsync_failures,
    })
}

fn fsync_file(path: &Path) -> std::io::Result<()> {
    let f = std::fs::File::open(path)?;
    f.sync_all()
}

#[derive(Debug)]
pub struct QuiesceOutcome {
    pub duration: Duration,
    pub fsync_failures: usize,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QuiesceError {
    #[error("pool acquire failed during quiesce: {0}")]
    PoolAcquire(#[source] r2d2::Error),

    #[error("wal_checkpoint failed: {0}")]
    WalCheckpoint(#[source] rusqlite::Error),
}
```

**Implementation note on drain surface:** Phase 4's `SubscriberSupervisor::drain_all()` is assumed to exist. If Phase 4 lands with a different method name or signature (e.g., takes a timeout as a parameter, returns a different error type), the implementor adapts at execution time. This is the one external-dependency-on-sibling-phase point in Phase 5.

**Testing:**

Integration test at `tests/quiesce.rs`:
- Set up a `MemoryCache` + `ConstellationDb` + spawned subscribers for 3 docs.
- Write to each doc, creating pending in-flight subscriber work.
- Call `quiesce(...)`.
- Assert: all subscribers exited cleanly (supervisor reports zero active workers).
- Assert: `PRAGMA wal_autocheckpoint` or equivalent shows wal file is small (checkpoint happened).
- Assert: all emitted file paths were fsynced (manually verify via `open(O_DIRECT)` or trust that `sync_all()` returned Ok).
- Mode A test: call quiesce without any jj adapter involvement; succeeds; memory.db is canonical post-call (AC8.4).
- Drain-timeout test: stub a subscriber that never exits on cancel; assert `drain_timeout` counter increments; quiesce still returns successfully (best-effort semantics).

**Verification:**

Run: `cargo nextest run -p pattern_memory --test quiesce`
Expected: passes.

**Commit:** `[pattern-memory] quiesce() — drain subscribers, WAL checkpoint, fsync canonical files`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: CI canary + CLAUDE.md update + port-list entry

**Verifies:** regression prevention for future jj version drift

**Files:**
- Create: `.github/workflows/jj-adapter-canary.yml` (or extend existing CI workflow with a `cargo nextest run -p pattern_memory --test 'jj_*'` step)
- Modify: `crates/pattern_memory/CLAUDE.md` (add jj adapter + quiesce + StorageMode sections)
- Modify: `docs/plans/rewrite-v3-portlist.md` (add Phase 5 entry)

**Implementation:**

1. **CI canary.** Either extend the existing `.github/workflows/ci.yml` with a new step that runs the jj adapter tests with `jj --version` logged, OR create a dedicated canary workflow that runs nightly against the latest jj release to catch breakage between Pattern releases.

   Minimum viable approach: extend the main CI workflow with:

   ```yaml
   - name: jj adapter canary
     run: |
       jj --version
       cargo nextest run -p pattern_memory --test 'jj_adapter_*' --nocapture
   ```

   Requires CI runners to have jj installed. For GitHub Actions Linux runners: add an install step (`curl -L https://github.com/jj-vcs/jj/releases/download/v0.38.0/... | tar -xz`). Document the version install in the workflow.

2. **CLAUDE.md additions** (append to existing file from Phase 1 + Phase 4):

   ```markdown
   ## jj adapter (`src/jj/`)

   Thin wrapper over the `jj` CLI. Shells out via `std::process::Command`;
   serializes workspace mutations via an internal Mutex (avoids jj-vcs/jj#9314
   concurrent-workspace-add hazard). Version range is `MIN_SUPPORTED_VERSION`
   ..= `MAX_TESTED_VERSION`; detect() refuses older versions loudly.

   **Why CLI, not jj-lib:** on-disk format drift risk in Modes A+C is worse
   than template fragility. See phase_05.md for the full decision record.

   **Mitigations:**
   - Minimal template fields per call (reduces breakage on jj upgrades).
   - Forgiving serde parse (unknown fields tolerated; missing fields flagged).
   - CI canary runs the full adapter suite on every commit against whichever
     jj is on the runner.

   ## quiesce (`src/jj/quiesce.rs`)

   Universal pre-commit step. Order: (1) drain sync_workers via supervisor's
   drain_all(), (2) PRAGMA wal_checkpoint(TRUNCATE) on memory.db, (3) fsync
   emitted canonical files. Mode A callers invoke before host VCS commit;
   Modes B/C invoke as step 1 of JjAdapter::commit.

   Freshness: YYYY-MM-DD (v3-memory-rework Phase 5).
   ```

3. **Port-list entry:**

   ```markdown
   ### jj CLI adapter + quiesce (Phase 5 — completed YYYY-MM-DD)

   - `pattern_memory::jj::adapter::JjAdapter` shells out to `jj` for workspace,
     commit, bookmark, merge, restore operations.
   - `pattern_memory::jj::quiesce::quiesce()` drains sync_workers + WAL checkpoint
     + fsync; universal across storage modes.
   - `pattern_memory::modes::StorageMode` skeleton (Phase 6 fills in attach logic).
   - `MIN_SUPPORTED_VERSION = "0.38.0"`, `MAX_TESTED_VERSION = "0.40.0"`.
   - CI canary: `.github/workflows/ci.yml` runs jj adapter tests on every PR.
   - Decision: CLI over jj-lib for on-disk format ownership. See
     `docs/implementation-plans/2026-04-19-v3-memory-rework/phase_05.md`.
   - Packaging: non-NixOS distribution bundles must ship `jj` binary alongside
     `tidepool-extract`.
   ```

**Testing:**

No new tests here — all regression coverage lives in Tasks 1-5. This task is documentation + CI wiring.

**Verification:**

Push a branch; confirm CI canary runs the jj adapter tests; confirm `jj --version` log line appears in CI output.

**Commit:** `[pattern-memory] [meta] CI canary for jj adapter + CLAUDE.md + port-list entry for Phase 5`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 5 Done-when recap

- `cargo check --workspace` clean.
- `cargo nextest run -p pattern_memory --test 'jj_*'` passes every jj adapter test (detect, log, workspace ops, bookmark ops, merge via `jj new`, restore, mutation serialization stress test).
- Quiesce integration test passes: subscribers drain, WAL checkpoints, fsync succeeds, Mode A works without jj (AC8.3, AC8.4).
- JjError's typed variants each exercised: BinaryNotFound (AC8.5), UnsupportedVersion (AC8.6), SubprocessFailed with stderr (AC8.7), OutputParseFailed, WorkspaceNotFound, BookmarkNotFound.
- `--color=never` universally applied via `JjAdapter::cmd()` helper (AC8.8).
- CI canary added + green.
- `pattern_memory/CLAUDE.md` updated with jj adapter + quiesce sections.
- Port-list doc records Phase 5 completion.

## Notes for downstream phases

- **Phase 6** (storage modes + attach/detach): uses `StorageMode` skeleton from this phase; adds per-mode path resolution + `MountedStore::attach/detach` + Mode C spike. Mode B's init calls `JjAdapter::init_repo` from this phase.
- **Phase 7** (messages backup): unrelated to jj — messages.db is never VCS-tracked. But if a future enhancement wants CAR export to use pattern-jj for compression, it'd use this phase's adapter.
- **Phase 8 capstone**: smoke_e2e test exercises quiesce + host git commit in Mode A. No jj adapter invocation in the smoke flow.
- **Packaging** (out of scope for this plan): non-NixOS Pattern release bundles need `jj` binary included. Tracked as a follow-up task in the packaging workstream.
- **jj upgrades**: when a new jj release lands, update `MAX_TESTED_VERSION` after running the canary against it. If templates break, update template strings + types; ideally keep backward compat by supporting both old and new shapes via serde defaults.
