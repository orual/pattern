# Pattern v3 Memory Rework — Phase 7 Implementation Plan

**Goal:** Implement atomic `messages.db` backup + restore + rotation machinery. Snapshots land at `~/.pattern/backups/<project-id>/messages/<timestamp>.sqlite` via rusqlite's `Backup::run_to_completion` (atomic under concurrent writers via WAL); rotation thins per a GFS-style policy (keep last N + hourly-for-day / daily-for-month / monthly-forever); restore copies current state to `messages.db.pre-restore-<ts>` as a rollback safety net before swapping in the selected snapshot; the scheduler runs on tokio interval with `MissedTickBehavior::Delay` and is cancel-token-gated by `MountedStore` lifecycle; `pattern_cli` exposes `pattern backup {create,list,restore,info}` as clap subcommands that build on the library APIs.

**Architecture:** All backup logic lives as library functions in `pattern_memory::backup::*`. `pattern_cli` is a thin consumer — one-shot subcommands that call the library and print results. The scheduler is a tokio task owned by `MountedStore`; it wakes every `snapshot_interval` (default 1h), checks whether at least one message has been written since the last snapshot, and either creates one + applies rotation or skips silently. The mount's `CancellationToken` (same lifecycle pattern Phase 4 established for subscribers) cancels the scheduler on detach; the scheduler's JoinHandle is awaited in detach with a short timeout. `pattern_cli` was pre-stripped (v2-era code moved to `rewrite-staging/`) and rebuilt with minimal ratatui scaffolding before this plan's execution; Phase 7 adds the `backup` subcommand tree alongside `mount` (from Phase 6). Subcommands are one-shot ops that don't integrate with the ratatui main-loop — they're clap-dispatched and exit on completion.

**Tech Stack:**
- rusqlite's `backup` feature (included in `bundled-full` per Phase 2's pin).
- `tokio::time::interval` with `MissedTickBehavior::Delay`.
- `tokio_util::sync::CancellationToken` (already a workspace dep via Phase 4).
- `jiff 0.2` (workspace-pinned) for ISO-8601 timestamp formatting — filename format `%Y-%m-%dT%H%M%SZ` (Windows-safe; no colons).
- `tempfile::NamedTempFile::new_in(&backup_dir)` for atomic rename-into-place (avoids EXDEV by keeping temp on same filesystem as destination).
- `blake3` (workspace content-hash convention) for snapshot metadata integrity checking.
- Hand-rolled GFS rotation (no single crate fits the tiered-retention pattern cleanly).

**Scope:** Phase 7 of 8.

**External deps verified:** 2026-04-19 (internet-researcher a83f29418d8f04c43).
**Codebase verified:** 2026-04-19 (codebase-investigator a3ac6f531e33ad232).

**Execution posture:** Autonomous. No gates requiring human sign-off. Main executor reviews the final diff before Phase 8.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC11: Messages.db backup + restore + rotation

- **v3-memory-rework.AC11.1 Success:** `pattern backup create` produces a snapshot at `~/.pattern/backups/<project-id>/messages/<iso8601>.sqlite` using rusqlite's backup API
- **v3-memory-rework.AC11.2 Success:** Snapshot is a valid SQLite file that opens cleanly with the same schema as the source
- **v3-memory-rework.AC11.3 Success:** `pattern backup restore <timestamp>` replaces `messages.db` with the snapshot; all messages present + searchable after restore
- **v3-memory-rework.AC11.4 Success:** Pre-restore safety: current state is auto-snapshotted before replacement; label makes it distinguishable as a rollback point
- **v3-memory-rework.AC11.5 Success:** Rotation policy retains last N snapshots + thins older per the configured hourly/daily/monthly bands
- **v3-memory-rework.AC11.6 Failure:** `pattern backup restore <timestamp>` with a non-existent timestamp produces a clear error listing available snapshots
- **v3-memory-rework.AC11.7 Edge:** Concurrent backup + write: snapshot is atomic; no mid-write corruption observable in the snapshot file

---

## Codebase verification findings

- ✓ `jiff 0.2` is workspace-pinned (`Cargo.toml:58`). Used in `pattern_runtime/src/compaction.rs:45`, `tests/turn_history_restore.rs`, etc. Preferred over chrono for new code per global CLAUDE.md.
- ✓ `tokio_util::sync::CancellationToken` is introduced in Phase 4 as the workspace cancel primitive; Phase 7 uses the same pattern (no new dep).
- ✓ `MountedStore` (Phase 6) owns cache + db + supervisor + watcher. Phase 7 extends it with a backup scheduler task field + a dedicated cancel token for the scheduler (separate from the subscriber supervisor's token so detach can unwind them independently).
- ✓ `rusqlite::backup` feature is enabled via Phase 2's `bundled-full`. No Cargo changes needed here.
- ✗ No prior backup/snapshot infra in the workspace. Phase 7 is entirely new surface.
- ✗ No existing tokio interval scheduler pattern; Phase 7 establishes it.
- ✗ No existing rotation/retention code. GFS implementation is hand-rolled.
- ✓ `pattern_cli` will have minimal ratatui scaffolding at execution time (pre-strip + rebuild handled by orual outside this plan). Phase 7 adds clap `backup` subcommand tree; both clap subcommands and the ratatui default path coexist in the same binary.
- ✓ `tempfile` is workspace dev-dep. `NamedTempFile::new_in(&dir)` is the atomic-write-into-same-fs pattern research identified.
- ✓ `.pattern.kdl` schema (Phase 6) is extensible via knus derive. Phase 7 adds an optional `backup` section with rotation policy + snapshot interval.

---

## Dependency changes

`crates/pattern_memory/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
# No new deps — rusqlite backup feature already enabled via bundled-full (Phase 2);
# jiff, tokio, tokio-util, blake3, tempfile already workspace-pinned.
```

`crates/pattern_cli/Cargo.toml`:

```toml
[dependencies]
# ... existing ratatui scaffolding deps ...
pattern_memory = { path = "../pattern_memory" }   # if not already a dep from Phase 6
jiff = { workspace = true }                        # for parsing user timestamp args
```

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

### Subcomponent A: Snapshot + restore + rotation library

<!-- START_TASK_1 -->
### Task 1: `backup::snapshot` — atomic snapshot creation + metadata

**Verifies:** v3-memory-rework.AC11.1, AC11.2, AC11.7

**Files:**
- Create: `crates/pattern_memory/src/backup/mod.rs`
- Create: `crates/pattern_memory/src/backup/snapshot.rs`
- Create: `crates/pattern_memory/src/backup/error.rs`
- Create: `crates/pattern_memory/src/backup/types.rs` (SnapshotInfo, retention config types)
- Modify: `crates/pattern_memory/src/paths.rs` (add `backup_dir(project_id: &str) -> Result<PathBuf>` + `backup_snapshot_path(project_id: &str, ts: &Timestamp)`)

**Implementation:**

1. Filename format (Windows-safe, no colons):

   ```rust
   use jiff::{Timestamp, fmt::strtime};

   pub const SNAPSHOT_FILENAME_FORMAT: &str = "%Y-%m-%dT%H%M%SZ";

   pub fn format_snapshot_name(ts: &Timestamp) -> String {
       // jiff's strtime module formats Timestamps; verify exact API at implementation time.
       strtime::format(SNAPSHOT_FILENAME_FORMAT, ts)
           .expect("static format string is valid")
   }
   ```

2. Path helper in `paths.rs`:

   ```rust
   pub fn backup_dir(project_id: &str) -> Result<PathBuf, PathError> {
       Ok(pattern_home()?.join("backups").join(project_id).join("messages"))
   }
   pub fn backup_snapshot_path(project_id: &str, ts: &Timestamp) -> Result<PathBuf, PathError> {
       let name = format!("{}.sqlite", format_snapshot_name(ts));
       Ok(backup_dir(project_id)?.join(name))
   }
   ```

3. `SnapshotInfo` type:

   ```rust
   #[derive(Debug, Clone)]
   pub struct SnapshotInfo {
       pub timestamp: Timestamp,
       pub path: PathBuf,
       pub size_bytes: u64,
       pub content_hash: [u8; 32],   // blake3 of snapshot file contents
   }
   ```

4. `create_snapshot` core function:

   ```rust
   use rusqlite::backup::Backup;
   use std::time::Duration;

   /// Create an atomic snapshot of `source_db_path` into the backup directory
   /// for `project_id`. Uses rusqlite's Backup API which is safe under
   /// concurrent writers via WAL (retries on SQLITE_BUSY transparently).
   pub fn create_snapshot(
       source_db_path: &Path,
       project_id: &str,
   ) -> Result<SnapshotInfo, BackupError> {
       let now = Timestamp::now();
       let dest_path = crate::paths::backup_snapshot_path(project_id, &now)?;
       let dest_dir = dest_path.parent().expect("has parent");
       std::fs::create_dir_all(dest_dir)?;

       // Write to a temp file in the SAME directory as the destination to
       // avoid EXDEV on cross-filesystem rename.
       let tmp = tempfile::NamedTempFile::new_in(dest_dir)
           .map_err(|e| BackupError::TempFile { path: dest_dir.to_owned(), source: e })?;

       {
           let src = rusqlite::Connection::open_with_flags(
               source_db_path,
               rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
           ).map_err(BackupError::OpenSource)?;
           let mut dst = rusqlite::Connection::open(tmp.path())
               .map_err(BackupError::OpenDest)?;

           let backup = Backup::new(&src, &mut dst).map_err(BackupError::BackupInit)?;
           // run_to_completion handles SQLITE_BUSY retries transparently.
           backup.run_to_completion(
               /* pages_per_step */ 100,
               /* pause_between_steps */ Duration::from_millis(5),
               /* progress_callback */ None,
           ).map_err(BackupError::BackupRun)?;
       }

       // fsync the temp file before rename for durability.
       {
           let f = std::fs::File::open(tmp.path())
               .map_err(|e| BackupError::Io { path: tmp.path().to_owned(), source: e })?;
           f.sync_all()
               .map_err(|e| BackupError::Io { path: tmp.path().to_owned(), source: e })?;
       }

       // Compute blake3 hash + size before rename.
       let bytes = std::fs::read(tmp.path())
           .map_err(|e| BackupError::Io { path: tmp.path().to_owned(), source: e })?;
       let content_hash: [u8; 32] = blake3::hash(&bytes).into();
       let size_bytes = bytes.len() as u64;

       // Atomic rename into place (within same filesystem).
       tmp.persist(&dest_path)
           .map_err(|e| BackupError::TempPersist { path: dest_path.clone(), source: e.error })?;

       Ok(SnapshotInfo {
           timestamp: now,
           path: dest_path,
           size_bytes,
           content_hash,
       })
   }
   ```

5. `BackupError` — standard `#[non_exhaustive] #[derive(Error, Diagnostic)]` shape matching the codebase pattern.

**Testing:**

Integration tests in `crates/pattern_memory/tests/backup_snapshot.rs`:

- **Happy path**: create messages.db via `ConstellationDb::open`, insert N messages, call `create_snapshot`, verify the snapshot file exists + opens cleanly + has same tables + same row count.
- **Concurrent writer (AC11.7)**: spawn a tokio task doing continuous INSERTs on messages.db at ~100/sec; call `create_snapshot` mid-flight; verify snapshot file opens cleanly + passes `PRAGMA integrity_check`; verify snapshot contains a consistent point-in-time view (row count is ≤ current source count, no partial-write corruption).
- **EXDEV resilience**: no explicit test required since `NamedTempFile::new_in(dest_dir)` sidesteps EXDEV by construction. Document the invariant in the code comment.
- **Destination dir auto-create**: call `create_snapshot` when backup dir doesn't exist yet; verify it's created.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test backup_snapshot`
Expected: all pass.

**Commit:** `[pattern-memory] backup::snapshot — atomic messages.db snapshots via rusqlite Backup API`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `backup::rotation` — GFS retention policy

**Verifies:** v3-memory-rework.AC11.5

**Files:**
- Create: `crates/pattern_memory/src/backup/rotation.rs`

**Implementation:**

1. Retention policy struct (parsed from `.pattern.kdl` in Task 5):

   ```rust
   #[derive(Debug, Clone)]
   pub struct RetentionPolicy {
       /// Keep the N most-recent snapshots unconditionally.
       pub keep_recent: usize,           // default 24
       /// Keep one snapshot per hour for the last `hourly_days` days.
       pub hourly_days: u32,             // default 1
       /// Keep one snapshot per day for the last `daily_months` months.
       pub daily_months: u32,            // default 1
       /// Keep one snapshot per month indefinitely.
       pub monthly_forever: bool,        // default true
   }

   impl Default for RetentionPolicy {
       fn default() -> Self {
           Self { keep_recent: 24, hourly_days: 1, daily_months: 1, monthly_forever: true }
       }
   }
   ```

2. List all snapshots in a backup dir (ordered newest-first):

   ```rust
   pub fn list_snapshots(project_id: &str) -> Result<Vec<SnapshotInfo>, BackupError> {
       let dir = crate::paths::backup_dir(project_id)?;
       if !dir.is_dir() {
           return Ok(Vec::new());
       }
       let mut out = Vec::new();
       for entry in std::fs::read_dir(&dir)? {
           let entry = entry?;
           let path = entry.path();
           if path.extension().and_then(|e| e.to_str()) != Some("sqlite") {
               continue;
           }
           // Parse timestamp from filename.
           let name = path.file_stem().and_then(|s| s.to_str())
               .ok_or_else(|| BackupError::InvalidSnapshotName { path: path.clone() })?;
           let ts = parse_snapshot_name(name)
               .map_err(|e| BackupError::InvalidSnapshotName { path: path.clone() })?;
           let metadata = entry.metadata()?;
           // content_hash is NOT computed here — expensive; lazy-loaded elsewhere.
           out.push(SnapshotInfo {
               timestamp: ts,
               path: path.clone(),
               size_bytes: metadata.len(),
               content_hash: [0u8; 32],   // placeholder; see Task 3 for when this matters
           });
       }
       out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));   // newest first
       Ok(out)
   }

   fn parse_snapshot_name(name: &str) -> Result<Timestamp, BackupError> {
       // Reverse of format_snapshot_name — strtime::parse
       jiff::fmt::strtime::parse(SNAPSHOT_FILENAME_FORMAT, name)
           .and_then(|p| p.to_timestamp())
           .map_err(|_| BackupError::InvalidSnapshotName { path: name.into() })
   }
   ```

3. GFS apply logic:

   ```rust
   /// Apply the retention policy: returns the list of snapshots to DELETE.
   /// Keeps: (a) the N newest unconditionally, (b) one per hour for the last
   /// `hourly_days` days, (c) one per day for the last `daily_months` months,
   /// (d) one per calendar month indefinitely if `monthly_forever`.
   pub fn select_deletions(
       snapshots: &[SnapshotInfo],
       policy: &RetentionPolicy,
       now: &Timestamp,
   ) -> Vec<PathBuf> {
       let mut keep = std::collections::HashSet::<&Path>::new();

       // (a) Keep the N newest.
       for s in snapshots.iter().take(policy.keep_recent) {
           keep.insert(&s.path);
       }

       // (b) Hourly retention: within `hourly_days * 24` hours, keep one per hour.
       let hourly_cutoff = now.checked_sub(jiff::Span::new().days(policy.hourly_days as i32)).unwrap();
       let mut seen_hours = std::collections::HashSet::<(i16, i16, i8, i8)>::new();  // year, month, day, hour
       for s in snapshots.iter().filter(|s| s.timestamp >= hourly_cutoff) {
           let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
           let bucket = (zoned.year(), zoned.month() as i16, zoned.day(), zoned.hour());
           if seen_hours.insert(bucket) {
               keep.insert(&s.path);
           }
       }

       // (c) Daily retention: within `daily_months * ~30` days, keep one per day (the first snapshot in each day bucket that we encounter; since the list is sorted newest-first, this naturally retains the most recent snapshot per day).
       let daily_cutoff = now.checked_sub(jiff::Span::new().days((policy.daily_months as i32) * 30)).unwrap();
       let mut seen_days = std::collections::HashSet::<(i16, i16, i8)>::new();  // year, month, day
       for s in snapshots.iter().filter(|s| s.timestamp >= daily_cutoff) {
           let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
           let bucket = (zoned.year(), zoned.month() as i16, zoned.day());
           if seen_days.insert(bucket) {
               keep.insert(&s.path);
           }
       }

       // (d) Monthly retention: keep the most recent snapshot of each calendar month, indefinitely.
       // Iteration is newest-first, so the first encounter per month is the most recent.
       if policy.monthly_forever {
           let mut seen_months = std::collections::HashSet::<(i16, i16)>::new();  // year, month
           for s in snapshots.iter() {
               let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
               let bucket = (zoned.year(), zoned.month() as i16);
               if seen_months.insert(bucket) {
                   keep.insert(&s.path);
               }
           }
       }

       // Safety: always keep at least one snapshot if any exist, regardless of policy.
       // Prevents a pathological config with all-zero retention from wiping the entire backup history.
       if keep.is_empty() && !snapshots.is_empty() {
           keep.insert(&snapshots[0].path);
       }

       // Return the set of paths NOT in keep.
       snapshots.iter()
           .filter(|s| !keep.contains(s.path.as_path()))
           .map(|s| s.path.clone())
           .collect()
   }

   /// Apply the policy + delete the selected snapshots. Returns the count deleted.
   pub fn apply_rotation(
       project_id: &str,
       policy: &RetentionPolicy,
   ) -> Result<usize, BackupError> {
       let snapshots = list_snapshots(project_id)?;
       let to_delete = select_deletions(&snapshots, policy, &Timestamp::now());
       for path in &to_delete {
           std::fs::remove_file(path)
               .map_err(|e| BackupError::Io { path: path.clone(), source: e })?;
       }
       Ok(to_delete.len())
   }
   ```

**Testing:**

Unit tests in `backup/rotation.rs`:

- Generate synthetic `SnapshotInfo` lists spanning: (i) last 24h at 10-minute intervals, (ii) last 30 days at hourly intervals, (iii) last year at daily intervals. Call `select_deletions` with default policy. Assert:
  - The N newest are kept (a).
  - Exactly one per hour in the last `hourly_days` is kept (b).
  - Exactly one per day in the last `daily_months` is kept (c).
  - One per calendar month for older entries is kept (d).
- Edge cases: empty snapshot list → no deletions. Single snapshot → kept. Policy with all zeros → keep nothing? (Document: `keep_recent = 0` AND no other retention = delete all older than `hourly_cutoff`? Probably not desirable — add a sanity-check that keeps at least 1.)

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib backup::rotation`
Expected: passes.

**Commit:** `[pattern-memory] backup::rotation — GFS keep-N + hourly/daily/monthly thinning`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `backup::restore` — pre-restore safety snapshot + atomic swap

**Verifies:** v3-memory-rework.AC11.3, AC11.4, AC11.6

**Files:**
- Create: `crates/pattern_memory/src/backup/restore.rs`

**Implementation:**

```rust
use jiff::Timestamp;
use std::path::{Path, PathBuf};

/// Restore `messages.db` from the snapshot at `snapshot_path`. Before swapping
/// in the snapshot, the current messages.db is copied to
/// `messages.db.pre-restore-<ts>` as a rollback safety net.
///
/// Returns the pre-restore path so the caller can surface it to the user
/// ("if the restored state is wrong, here's where to find your pre-restore state").
pub fn restore_snapshot(
    messages_db_path: &Path,
    snapshot_path: &Path,
) -> Result<PathBuf, BackupError> {
    if !snapshot_path.is_file() {
        return Err(BackupError::SnapshotNotFound { path: snapshot_path.to_owned() });
    }

    // Verify the snapshot is a valid SQLite file BEFORE touching messages.db.
    {
        let conn = rusqlite::Connection::open_with_flags(
            snapshot_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ).map_err(BackupError::OpenSource)?;
        let ok: String = conn.query_row(
            "PRAGMA integrity_check",
            [],
            |r| r.get(0),
        ).map_err(BackupError::IntegrityCheck)?;
        if ok != "ok" {
            return Err(BackupError::CorruptSnapshot {
                path: snapshot_path.to_owned(),
                detail: ok,
            });
        }
    }

    // Pre-restore safety: copy current messages.db to .pre-restore-<ts> before replacing.
    let pre_restore_ts = Timestamp::now();
    let pre_restore_name = format!(
        ".pre-restore-{}",
        crate::backup::snapshot::format_snapshot_name(&pre_restore_ts)
    );
    let pre_restore_path = {
        let parent = messages_db_path.parent().expect("has parent");
        let stem = messages_db_path.file_name().unwrap().to_string_lossy();
        parent.join(format!("{}{}", stem, pre_restore_name))
    };

    if messages_db_path.exists() {
        std::fs::copy(messages_db_path, &pre_restore_path)
            .map_err(|e| BackupError::Io { path: pre_restore_path.clone(), source: e })?;
        // fsync the pre-restore copy before doing the swap.
        std::fs::File::open(&pre_restore_path)
            .and_then(|f| f.sync_all())
            .map_err(|e| BackupError::Io { path: pre_restore_path.clone(), source: e })?;
    }

    // Atomic swap: copy snapshot over messages.db (via temp + rename in same dir).
    let messages_dir = messages_db_path.parent().expect("has parent");
    let tmp = tempfile::NamedTempFile::new_in(messages_dir)
        .map_err(|e| BackupError::TempFile { path: messages_dir.to_owned(), source: e })?;
    std::fs::copy(snapshot_path, tmp.path())
        .map_err(|e| BackupError::Io { path: tmp.path().to_owned(), source: e })?;
    std::fs::File::open(tmp.path())
        .and_then(|f| f.sync_all())
        .map_err(|e| BackupError::Io { path: tmp.path().to_owned(), source: e })?;
    tmp.persist(messages_db_path)
        .map_err(|e| BackupError::TempPersist { path: messages_db_path.to_owned(), source: e.error })?;

    Ok(pre_restore_path)
}

/// Look up a snapshot by user-provided timestamp string. Supports:
/// - exact filename: "2026-04-19T120000Z"
/// - date prefix: "2026-04-19" → latest snapshot on that date
/// - shorthand: "latest" → most recent snapshot
pub fn resolve_snapshot(project_id: &str, spec: &str) -> Result<SnapshotInfo, BackupError> {
    let snapshots = rotation::list_snapshots(project_id)?;
    if snapshots.is_empty() {
        return Err(BackupError::NoSnapshots { project_id: project_id.into() });
    }

    if spec == "latest" {
        return Ok(snapshots.into_iter().next().unwrap());
    }

    // Try exact filename match first.
    if let Some(matched) = snapshots.iter().find(|s| {
        s.path.file_stem().and_then(|n| n.to_str()) == Some(spec)
    }) {
        return Ok(matched.clone());
    }

    // Try date-prefix match (latest on that date).
    if let Some(matched) = snapshots.iter().find(|s| {
        let zoned = s.timestamp.to_zoned(jiff::tz::TimeZone::UTC);
        let iso_date = format!("{:04}-{:02}-{:02}", zoned.year(), zoned.month(), zoned.day());
        iso_date == spec
    }) {
        return Ok(matched.clone());
    }

    // No match — build a helpful error listing available timestamps.
    Err(BackupError::SnapshotNotFoundBySpec {
        spec: spec.into(),
        available: snapshots.iter().map(|s| {
            crate::backup::snapshot::format_snapshot_name(&s.timestamp)
        }).collect(),
    })
}
```

`BackupError::SnapshotNotFoundBySpec` uses miette to render the available-snapshots list in the error's help text (AC11.6).

**Testing:**

Integration test in `tests/backup_restore.rs`:

- **Happy path (AC11.3)**: create messages.db + insert 3 messages → snapshot → modify messages.db (insert 2 more) → restore from snapshot → assert 3 messages (not 5).
- **Pre-restore safety (AC11.4)**: verify `.pre-restore-<ts>` file exists post-restore with the 5-message state.
- **Rollback from pre-restore**: invoke `restore_snapshot(messages_db_path, pre_restore_path)` → messages.db is back to 5-message state.
- **Corrupt snapshot**: create a file with garbage contents ending in `.sqlite`; call `restore_snapshot` → `BackupError::CorruptSnapshot`; messages.db is UNCHANGED (safety invariant).
- **Timestamp lookup (AC11.6)**: `resolve_snapshot("nonexistent-ts")` → error lists available timestamps in the help text.
- **`latest` shorthand**: `resolve_snapshot("latest")` returns the most recent.
- **Date prefix**: create two snapshots on same day; `resolve_snapshot("2026-04-19")` returns the later one.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test backup_restore`
Expected: passes.

**Commit:** `[pattern-memory] backup::restore — pre-restore safety + atomic swap + timestamp spec resolution`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-6) -->

### Subcomponent B: Scheduler + config + CLI integration

<!-- START_TASK_4 -->
### Task 4: `backup::scheduler` — tokio interval task tied to MountedStore lifecycle

**Verifies:** prerequisite for AC11.1 (periodic snapshots happen automatically)

**Files:**
- Create: `crates/pattern_memory/src/backup/scheduler.rs`
- Modify: `crates/pattern_memory/src/mount/mod.rs` (MountedStore gains scheduler_handle + scheduler_cancel fields)

**Implementation:**

```rust
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use jiff::Timestamp;

pub struct BackupScheduler {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl BackupScheduler {
    pub fn spawn(
        messages_db_path: Arc<std::path::PathBuf>,
        project_id: Arc<String>,
        policy: Arc<BackupPolicy>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let mut tick = interval(policy.snapshot_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            // Initial catch-up: if the last snapshot is older than snapshot_interval,
            // take one now (handles the case where Pattern was offline for a long period).
            if let Ok(snapshots) = crate::backup::rotation::list_snapshots(&project_id) {
                let should_snapshot_now = snapshots.first()
                    .map(|s| (Timestamp::now() - s.timestamp).get_seconds() as u64 > policy.snapshot_interval.as_secs())
                    .unwrap_or(true);
                if should_snapshot_now {
                    let _ = try_snapshot(&messages_db_path, &project_id, &policy).await;
                }
            }

            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    _ = tick.tick() => {
                        if should_snapshot(&messages_db_path, &project_id).await {
                            let _ = try_snapshot(&messages_db_path, &project_id, &policy).await;
                        }
                    }
                }
            }
        });

        Self { handle, cancel }
    }

    pub fn cancel(&self) { self.cancel.cancel(); }

    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.handle.await
    }
}

async fn should_snapshot(messages_db_path: &std::path::Path, project_id: &str) -> bool {
    // Query messages.db: "any row written since the last snapshot's timestamp?"
    // If yes → snapshot. If no → skip silently.
    let last = crate::backup::rotation::list_snapshots(project_id).ok()
        .and_then(|mut v| v.pop())
        .map(|s| s.timestamp)
        .unwrap_or_else(|| Timestamp::from_second(0).unwrap());

    // spawn_blocking because rusqlite is sync.
    let path = messages_db_path.to_owned();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ).ok()?;
        // Adjust query to match the actual messages schema (position/created_at column).
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE created_at > ?1)",
            rusqlite::params![last.to_string()],
            |r| r.get::<_, bool>(0),
        ).ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

async fn try_snapshot(
    messages_db_path: &std::path::Path,
    project_id: &str,
    policy: &BackupPolicy,
) -> Result<(), BackupError> {
    let path = messages_db_path.to_owned();
    let pid = project_id.to_owned();
    let policy_clone = policy.retention.clone();

    tokio::task::spawn_blocking(move || {
        crate::backup::snapshot::create_snapshot(&path, &pid)?;
        crate::backup::rotation::apply_rotation(&pid, &policy_clone)?;
        Ok::<_, BackupError>(())
    })
    .await
    .map_err(BackupError::JoinError)?
}
```

`BackupPolicy` combines snapshot_interval + retention policy (parsed from `.pattern.kdl` in Task 5).

Wire the scheduler into `MountedStore::attach`:

```rust
// In attach() after building cache + supervisor + watcher:
let backup_scheduler = if let Some(backup_config) = &config.backup {
    Some(BackupScheduler::spawn(
        Arc::new(messages_db_path.clone()),
        Arc::new(project_id.clone()),
        Arc::new(BackupPolicy::from(backup_config.clone())),
    ))
} else {
    None
};

// In MountedStore::detach(), between watcher-drop and supervisor-abort:
if let Some(sched) = self.backup_scheduler.take() {
    sched.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), sched.join()).await;
}
```

**Testing:**

Integration test in `tests/backup_scheduler.rs`:

- Attach a mount with `snapshot_interval = 1s`, `retention = keep 2`. Insert messages. Wait 2.5s. Detach. Verify 2 snapshots exist in the backup dir.
- Scheduler cancel on detach: start scheduler, wait for one tick, detach; verify no stray tokio task continues (use `tokio::task::JoinSet::active_tasks_count` or trace the handle's completion).
- Skip on no writes: insert one message, wait for 3 ticks with no further writes. Verify only 1-2 snapshots created (the initial + possibly the first tick; subsequent ticks skip because `should_snapshot` returns false).

**Verification:**

Run: `cargo nextest run -p pattern_memory --test backup_scheduler`
Expected: passes.

**Commit:** `[pattern-memory] backup::scheduler — tokio interval task with MissedTickBehavior::Delay + mount-lifecycle cancel`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `.pattern.kdl` backup section + typed parsing

**Verifies:** config plumbing for AC11.5 (rotation policy configurable)

**Files:**
- Modify: `crates/pattern_memory/src/config/pattern_kdl.rs` (extend `MountConfig` with `backup: Option<BackupSection>`)

**Implementation:**

Extend the KDL schema:

```kdl
# .pattern.kdl (optional backup section):
backup snapshot_interval="1h" {
    keep_recent 24
    hourly_days 1
    daily_months 1
    monthly_forever #true
}
```

Corresponding Rust types:

```rust
#[derive(Debug, Clone, Default, knus::Decode)]
pub struct BackupSection {
    #[knus(property, default = "1h".into())]
    pub snapshot_interval: String,   // parsed into Duration by the caller

    #[knus(child, default = 24)]
    pub keep_recent: usize,

    #[knus(child, default = 1)]
    pub hourly_days: u32,

    #[knus(child, default = 1)]
    pub daily_months: u32,

    #[knus(child, default = true)]
    pub monthly_forever: bool,
}

// Add to MountConfig:
// #[knus(child, default)]
// pub backup: BackupSection,
```

Parse `"1h"` / `"30m"` / `"24h"` into `Duration` via a small helper (jiff's `Span` parser or hand-rolled).

**Testing:**

- KDL fixture with a valid `backup` section parses to expected `BackupSection`.
- Missing `backup` section uses defaults.
- Invalid duration string (`"not-a-duration"`) produces a clear miette diagnostic.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib config::pattern_kdl`
Expected: passes.

**Commit:** `[pattern-memory] .pattern.kdl backup section + retention policy parsing`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `pattern backup {create,list,restore,info}` subcommands in `pattern_cli`

**Verifies:** v3-memory-rework.AC11.1, AC11.3, AC11.6 (CLI-level)

**Files:**
- Modify: `crates/pattern_cli/src/main.rs` (or the post-strip equivalent) — add `Backup` variant with nested subcommand
- Create: `crates/pattern_cli/src/commands/backup.rs` — one file per subcommand or grouped
- Modify: `crates/pattern_cli/Cargo.toml` — add `pattern_memory = { path = "../pattern_memory" }` + `jiff` if not already deps

**Implementation:**

```rust
// In main.rs top-level Cmd enum:
#[derive(Subcommand)]
enum Cmd {
    // ... Mount (from Phase 6) ...
    Backup {
        #[command(subcommand)]
        sub: BackupCmd,
    },
    // ... ratatui default when no subcommand ...
}

#[derive(Subcommand)]
enum BackupCmd {
    /// Create an immediate snapshot of messages.db for the attached mount.
    Create {
        #[arg(long)]
        path: Option<PathBuf>,    // defaults to cwd + walk-upward
    },
    /// List all snapshots for the attached mount.
    List {
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Restore messages.db from a snapshot (pre-restore safety applies).
    Restore {
        #[arg(value_name = "TIMESTAMP")]
        spec: String,             // "latest" | "2026-04-19" | "2026-04-19T120000Z"
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Show metadata for a specific snapshot.
    Info {
        #[arg(value_name = "TIMESTAMP")]
        spec: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
}
```

Subcommand implementations call `pattern_memory::backup::*` directly:

```rust
async fn cmd_backup_create(path: Option<PathBuf>) -> Result<(), CliError> {
    let start = path.unwrap_or_else(|| std::env::current_dir().unwrap());
    let mount = pattern_memory::mount::attach(&start, None).await?;
    let messages_db = mount.db.messages_path();
    let info = pattern_memory::backup::snapshot::create_snapshot(messages_db, &mount.config.project.name)?;
    println!("Snapshot created: {}", info.path.display());
    println!("  timestamp: {}", info.timestamp);
    println!("  size: {} bytes", info.size_bytes);
    println!("  hash: {}", hex::encode(&info.content_hash[..8]));
    mount.detach().await?;
    Ok(())
}

async fn cmd_backup_list(path: Option<PathBuf>) -> Result<(), CliError> {
    let start = path.unwrap_or_else(|| std::env::current_dir().unwrap());
    let mount = pattern_memory::mount::attach(&start, None).await?;
    let snapshots = pattern_memory::backup::rotation::list_snapshots(&mount.config.project.name)?;
    if snapshots.is_empty() {
        println!("No snapshots yet for project {}", mount.config.project.name);
    } else {
        println!("{:<24} {:>12} {:<18}", "TIMESTAMP", "SIZE", "HASH");
        for s in snapshots {
            println!(
                "{:<24} {:>10} B {}",
                pattern_memory::backup::snapshot::format_snapshot_name(&s.timestamp),
                s.size_bytes,
                hex::encode(&s.content_hash[..8]),
            );
        }
    }
    mount.detach().await?;
    Ok(())
}

async fn cmd_backup_restore(spec: String, path: Option<PathBuf>) -> Result<(), CliError> {
    let start = path.unwrap_or_else(|| std::env::current_dir().unwrap());
    let mount = pattern_memory::mount::attach(&start, None).await?;
    let snapshot = pattern_memory::backup::restore::resolve_snapshot(&mount.config.project.name, &spec)?;
    let pre_restore = pattern_memory::backup::restore::restore_snapshot(
        mount.db.messages_path(),
        &snapshot.path,
    )?;
    println!("Restored from {}", snapshot.path.display());
    println!("Pre-restore state saved at: {}", pre_restore.display());
    println!("  (to roll back: pattern backup restore --from-path {})", pre_restore.display());
    mount.detach().await?;
    Ok(())
}
```

**Testing:**

Integration tests in `crates/pattern_cli/tests/cli_backup.rs`:

- `pattern backup create` on a mounted project → exit 0, snapshot file exists.
- `pattern backup list` → exit 0, output contains the created snapshot.
- `pattern backup restore latest` → exit 0, messages.db reflects restored state.
- `pattern backup restore nonexistent-id` → non-zero exit + miette-formatted available-snapshots list on stderr.
- `pattern backup info <ts>` → prints metadata.

These tests complement the library-level tests in Tasks 1-4; they only verify arg parsing + exit codes + stderr format. Underlying behavior is already covered library-side.

**Verification:**

Run: `cargo nextest run -p pattern_cli --test cli_backup`
Expected: passes.

**Commit:** `[pattern-cli] backup {create,list,restore,info} subcommands over pattern_memory::backup library`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 7 Done-when recap

- `cargo check --workspace` clean.
- `cargo nextest run -p pattern_memory` covers all library-level tests (snapshot, rotation, restore, scheduler, config) and `-p pattern_cli` covers the CLI wiring.
- Backup round-trip: write → snapshot → clear → restore → writes recovered (AC11.1, AC11.3).
- Snapshot is a valid SQLite file with same schema + passes `PRAGMA integrity_check` (AC11.2).
- Pre-restore safety file exists post-restore, distinguishable by `.pre-restore-<ts>` suffix (AC11.4).
- Rotation retains last N + thins per GFS bands; unit tests cover each retention band (AC11.5).
- Restore with bogus spec surfaces a clear error listing available snapshots (AC11.6).
- Concurrent-writer test: snapshot atomicity holds while inserts race (AC11.7).
- `MountedStore::detach` cleanly cancels + joins the scheduler task (no leaked tokio tasks).
- `.pattern.kdl` `backup` section parses + validates; defaults applied when absent.
- `pattern_cli` has `backup {create,list,restore,info}` subcommands, each ≤60 lines of glue calling library functions.

## Notes for downstream phases

- **Phase 8 (smoke capstone)**: the smoke test is library-level — calls `pattern_memory::backup::snapshot::create_snapshot` + `restore_snapshot` directly, no CLI shell. The CLI tests from Task 6 provide the CLI-level regression check independently.
- **Packaging**: no new binaries or deps beyond existing workspace. `pattern_cli` binary gains the `backup` subcommand tree; users invoke `pattern backup create` etc. as one-shot ops.
- **Future**: snapshot scheduling currently time-based only. Future enhancements may add cycle-based triggers (on compaction cycle end) or size-based triggers (if messages.db grows by > X MB). Not part of this phase.
- **Archival consideration**: memory.db is versioned via host VCS or pattern-jj (Phase 5). messages.db is versioned via backup/restore (this phase). These are deliberately different mechanisms for data with different access patterns. Agents CAR-export + import across v2/v3 boundaries for long-term migration (per Phase 2's note).
