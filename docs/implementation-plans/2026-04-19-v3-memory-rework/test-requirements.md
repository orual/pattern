# Pattern v3 memory rework -- test requirements

Maps all 82 AC cases from the v3-memory-rework design plan to verification steps. 76 cases have automated tests; 3 (AC10.*) require documented human verification; 3 (AC1.3, AC1.6, AC15.4) are structural command checks.

## Automated tests

### v3-memory-rework.AC1: pattern_memory crate extraction is clean and reversible

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC1.1 | `cargo check --workspace` passes after extraction | structural | (command-only) | `cargo check --workspace` |
| AC1.2 | Every moved test passes in pattern_memory | integration | `crates/pattern_memory/tests/api_parity.rs` | `memory_cache_constructs_and_exposes_public_surface` |
| AC1.3 | `cargo doc -p pattern_memory` produces complete rustdoc | structural | (command-only) | `cargo doc -p pattern_memory --no-deps` |
| AC1.4 | pattern_runtime imports split correctly | grep/structural | (command-only) | `grep -rn "pattern_core::memory" crates/ --include="*.rs"` returns zero |
| AC1.5 | Reverse dep pattern_core -> pattern_memory fails to compile | trybuild | `crates/pattern_core/tests/no_pattern_memory_dep.rs` | `pattern_core_cannot_import_pattern_memory` |
| AC1.6 | Workspace members updated; port-list doc records extraction | structural | (command-only) | `cargo metadata --format-version=1 \| grep pattern_memory` |

### v3-memory-rework.AC2: rusqlite migration preserves query semantics end-to-end

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC2.1 | `cargo check --workspace` passes after swap | structural | (command-only) | `cargo check --workspace` |
| AC2.2 | Pre-existing pattern_db integration tests pass | integration | existing `crates/pattern_db/tests/*.rs` | (all pre-existing tests) |
| AC2.3 | FTS5 BM25 insta snapshots identical | snapshot | `crates/pattern_db/tests/fts5_regression.rs` | `fts5_bm25_scoring_snapshot` |
| AC2.4 | Vector KNN ordering identical | snapshot | `crates/pattern_db/tests/vector_regression.rs` | `knn_ordering_snapshot` |
| AC2.5 | Three transaction sites port to rusqlite::Transaction | integration | `crates/pattern_db/tests/transaction_atomicity.rs` | `transaction_sites_port_atomically` |
| AC2.6 | Mid-transaction failure leaves pre-transaction state | integration | `crates/pattern_db/tests/transaction_atomicity.rs` | `forced_failure_rolls_back` |
| AC2.7 | 20 concurrent spawn_blocking callers complete | integration | `crates/pattern_db/tests/pool_stress_20.rs` | `pool_stress_20_callers` |
| AC2.8 | No direct libsqlite3-sys dep | grep/structural | (command-only) | `grep libsqlite3-sys crates/pattern_db/Cargo.toml` returns zero |
| AC2.9 | sqlite-vec 100-vector KNN smoke | integration | `crates/pattern_db/tests/sqlite_vec_smoke.rs` | `vec0_100_vectors_knn` |
| AC2.10 | messages.db ATTACH + cross-db queries work | integration | `crates/pattern_db/tests/cross_db_query.rs` | `cross_db_join_main_msg` |

### v3-memory-rework.AC3: BlockType simplification is clean across call sites

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC3.1 | BlockType contains only Core and Working | unit | `crates/pattern_core/src/types/memory_types/core_types.rs` (inline) | `block_type_variants` |
| AC3.2 | `cargo check --workspace` no warnings about removed variants | structural | (command-only) | `cargo check --workspace` |
| AC3.3 | Log rows migrated to Working + BlockSchema::Log | integration | `crates/pattern_db/tests/migrations_roundtrip.rs` | `migration_log_to_working` |
| AC3.4 | Archival rows converted to archival_entries | integration | `crates/pattern_db/tests/migrations_roundtrip.rs` | `migration_archival_to_entries` |
| AC3.5 | Stale BlockType::Archival/Log on disk produces clear error | unit | `crates/pattern_core/src/types/memory_types/core_types.rs` (inline) | `from_str_rejects_old_variants` |
| AC3.6 | Log-schema block loads into either Core or Working | integration | `crates/pattern_db/tests/migrations_roundtrip.rs` | `log_schema_any_tier` |

### v3-memory-rework.AC4: MemoryStore sync-ification + surface audit

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC4.1 | MemoryStore has no `#[async_trait]` | grep/structural | (command-only) | `grep "async_trait" crates/pattern_core/src/traits/memory_store.rs` returns zero on MemoryStore |
| AC4.2 | Trait has 19 methods | unit | `crates/pattern_core/src/types/memory_types/core_types.rs` (inline) | (builder tests for BlockFilter, BlockMetadataPatch, etc.) |
| AC4.3 | `list_blocks(BlockFilter)` replaces three variants | integration | `crates/pattern_memory/tests/` (inline in cache.rs) | `list_blocks_filter_combinations` |
| AC4.4 | `update_block_metadata` partial patch | integration | `crates/pattern_memory/tests/` (inline in cache.rs) | `metadata_patch_partial_update` |
| AC4.5 | `undo_redo` + `history_depth` equivalent behavior | integration | `crates/pattern_memory/tests/` (inline in cache.rs) | `undo_redo_round_trip` |
| AC4.6 | `search(SearchScope)` scopes correctly | integration | `crates/pattern_memory/tests/` (inline in cache.rs) | `search_scope_persona_project` |
| AC4.7 | All MemoryCache impl tests pass | integration | `crates/pattern_memory/src/cache.rs` (inline tests) | (all existing cache tests) |
| AC4.8 | async_trait dep not removed from pattern_core | grep/structural | (command-only) | `grep -c "async_trait" crates/pattern_core/src/ \| wc -l` returns 8 |
| AC4.9 | delete_archival not reachable via SDK; trybuild compile-fail | trybuild | `crates/pattern_runtime/tests/no_archive_delete.rs` | `archive_delete_no_longer_reachable_via_sdk` |

### v3-memory-rework.AC5: eval worker simplification + async callsite migration

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC5.1 | No per-session multi-thread tokio runtime | grep/structural | (command-only) | `grep "runtime::Builder::new_multi_thread" crates/pattern_runtime/src/agent_loop/eval_worker.rs` returns zero |
| AC5.2 | Worker via std::thread::spawn + std::sync::mpsc | grep/structural | (command-only) | `grep "std::sync::mpsc" crates/pattern_runtime/src/agent_loop/eval_worker.rs` |
| AC5.3 | Zero block_on in memory/recall/search/scope handlers | grep/structural | (command-only) | `grep "Handle::current().block_on" crates/pattern_runtime/src/sdk/handlers/{memory,recall,search,scope}.rs` returns zero |
| AC5.4 | Session::step signature still async | grep/structural | (command-only) | `grep "async fn step" crates/pattern_runtime/src/session.rs` |
| AC5.5 | spawn_blocking search bug resolved | integration | `crates/pattern_runtime/tests/search_spawn_blocking_regression.rs` | `concurrent_search_no_panic` |
| AC5.6 | Async callsites use spawn_blocking for DB ops | integration | `crates/pattern_cli/tests/concurrent_memory_ops.rs` | `concurrent_cli_memory_ops` |
| AC5.7 | 100 eval requests complete without nested-runtime panic | integration | `crates/pattern_runtime/tests/eval_worker_100_requests.rs` | `eval_worker_100_requests` |
| AC5.8 | Eval worker panic surfaces user-visible error | integration | `crates/pattern_runtime/tests/eval_worker_runtime_panic.rs` | `eval_worker_panic_surfaces_error` |

### v3-memory-rework.AC6: canonical file serialization round-trips

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC6.1 | Text block md round-trip | unit | `crates/pattern_memory/src/fs/markdown.rs` (inline) | `text_md_round_trip` |
| AC6.2 | Map block KDL round-trip (proptest) | property | `crates/pattern_memory/tests/kdl_roundtrip_proptest.rs` | `map_kdl_round_trip` |
| AC6.3 | List block KDL round-trip | property | `crates/pattern_memory/tests/kdl_roundtrip_proptest.rs` | `list_kdl_round_trip` |
| AC6.4 | Log block JSONL round-trip | unit | `crates/pattern_memory/src/fs/jsonl.rs` (inline) | `jsonl_round_trip` |
| AC6.5 | Composite block KDL round-trip | property | `crates/pattern_memory/tests/kdl_roundtrip_proptest.rs` | `composite_kdl_round_trip` |
| AC6.6 | LoroValue::Binary produces KdlConversionError | unit | `crates/pattern_memory/src/fs/kdl.rs` (inline) | `binary_produces_error` |
| AC6.7 | KDL numeric precision (i128 boundary, inf, NaN) | unit | `crates/pattern_memory/src/fs/kdl.rs` (inline) | `numeric_precision_edge_cases` |
| AC6.8 | Strings with newlines/quotes/unicode round-trip | unit | `crates/pattern_memory/src/fs/kdl.rs` (inline) | `string_edge_cases_round_trip` |

### v3-memory-rework.AC7: loro-native subscribers + external edit merge

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC7.1 | File emitted within 100ms of block write | integration | `crates/pattern_memory/tests/` (subscriber integration) | `subscriber_emits_file_within_100ms` |
| AC7.2 | FTS5 row updated on write | integration | `crates/pattern_memory/tests/` (subscriber integration) | `subscriber_updates_fts5` |
| AC7.3 | Re-embed queued only on content hash change | integration | `crates/pattern_memory/tests/` (subscriber integration) | `no_spurious_reembed` |
| AC7.4 | External .md edit -> loro merge -> re-emission | integration | `crates/pattern_memory/tests/` (watcher integration) | `external_edit_merge_and_reemit` |
| AC7.5 | Self-emit-echo suppression (single emission) | integration | `crates/pattern_memory/tests/` (subscriber integration) | `self_emit_echo_suppression` |
| AC7.6 | Invalid KDL -> parse_failed counter, no merge | integration | `crates/pattern_memory/tests/` (watcher integration) | `invalid_kdl_no_merge` |
| AC7.7 | Subscriber panic -> supervisor restart within 30s | integration | `crates/pattern_memory/src/subscriber/supervisor.rs` (integration) | `supervisor_restart_on_heartbeat_timeout` |
| AC7.8 | Concurrent human + agent -> CRDT merge both | integration | `crates/pattern_memory/tests/` (watcher integration) | `concurrent_agent_human_crdt_merge` |

### v3-memory-rework.AC8: jj CLI adapter + pre-commit quiesce

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC8.1 | JjAdapter::detect returns Some when jj present | integration | `crates/pattern_memory/tests/detect.rs` | `detect_returns_some_when_jj_present` |
| AC8.2 | All adapter functions parse JSON output correctly | integration | `crates/pattern_memory/tests/jj_adapter_read.rs`, `jj_adapter_mutate.rs` | `log_parses_json`, `workspace_list_parses`, etc. |
| AC8.3 | quiesce drains + wal_checkpoint + fsync | integration | `crates/pattern_memory/tests/quiesce.rs` | `quiesce_drains_and_checkpoints` |
| AC8.4 | Mode A quiesce works without jj | integration | `crates/pattern_memory/tests/quiesce.rs` | `quiesce_mode_a_no_jj` |
| AC8.5 | detect returns None when jj missing; no panic | integration | `crates/pattern_memory/tests/detect.rs` | `detect_none_when_missing` |
| AC8.6 | Unsupported version -> JjError::UnsupportedVersion | unit | `crates/pattern_memory/src/jj/version.rs` (inline) | `unsupported_version_error` |
| AC8.7 | Subprocess failure -> typed JjError::SubprocessFailed | integration | `crates/pattern_memory/tests/jj_adapter_read.rs` | `invalid_revset_subprocess_failed` |
| AC8.8 | --color=never in all invocations | grep/structural | (command-only) | `grep "color.*never" crates/pattern_memory/src/jj/adapter.rs` confirms base cmd helper |

### v3-memory-rework.AC9: storage modes A + B

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC9.1 | Mode A end-to-end | integration | `crates/pattern_memory/tests/mount_lifecycle.rs` | `mode_a_end_to_end` |
| AC9.2 | Mode B end-to-end | integration | `crates/pattern_memory/tests/mount_lifecycle.rs` | `mode_b_end_to_end` |
| AC9.3 | Mode A messages.db at ~/.pattern/transient/<hash>/ | unit | `crates/pattern_memory/src/paths.rs` (inline) | `mode_a_messages_path` |
| AC9.4 | Mode B messages.db at ~/.pattern/projects/<id>/messages/ | unit | `crates/pattern_memory/src/paths.rs` (inline) | `mode_b_messages_path` |
| AC9.5 | .pattern.kdl parses cleanly; malformed -> diagnostics | unit | `crates/pattern_memory/tests/config.rs` | `pattern_kdl_parse_valid`, `pattern_kdl_parse_malformed` |
| AC9.6 | attach walks upward, sets up subscribers + dbs | integration | `crates/pattern_memory/tests/mount_lifecycle.rs` | `attach_walk_upward` |
| AC9.7 | attach with no mount -> clear error | integration | `crates/pattern_memory/tests/mount_lifecycle.rs` | `attach_no_mount_error` |
| AC9.8 | detach + re-attach identical state | integration | `crates/pattern_memory/tests/mount_lifecycle.rs` | `detach_reattach_round_trip` |

### v3-memory-rework.AC11: messages.db backup + restore + rotation

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC11.1 | Snapshot at expected path via rusqlite backup API | integration | `crates/pattern_memory/tests/backup_snapshot.rs` | `create_snapshot_happy_path` |
| AC11.2 | Snapshot is valid SQLite with same schema | integration | `crates/pattern_memory/tests/backup_snapshot.rs` | `snapshot_opens_cleanly` |
| AC11.3 | Restore replaces messages.db; all messages present | integration | `crates/pattern_memory/tests/backup_restore.rs` | `restore_replaces_messages` |
| AC11.4 | Pre-restore auto-snapshot as rollback point | integration | `crates/pattern_memory/tests/backup_restore.rs` | `pre_restore_safety_snapshot` |
| AC11.5 | Rotation retains per GFS bands | unit | `crates/pattern_memory/src/backup/rotation.rs` (inline) | `gfs_retention_bands` |
| AC11.6 | Restore with bad timestamp -> error listing snapshots | integration | `crates/pattern_memory/tests/backup_restore.rs` | `restore_bad_timestamp_lists_available` |
| AC11.7 | Concurrent backup + write -> atomic snapshot | integration | `crates/pattern_memory/tests/backup_snapshot.rs` | `concurrent_write_atomic_snapshot` |

### v3-memory-rework.AC12: MemoryScope + isolate_from_persona

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC12.1 | None: reads merge persona + project | integration | `crates/pattern_memory/tests/scope_isolation.rs` | `none_reads_merge` |
| AC12.2 | CoreOnly: persona core read-only | integration | `crates/pattern_memory/tests/scope_isolation.rs` | `coreonly_persona_readonly` |
| AC12.3 | Full: persona content invisible | integration | `crates/pattern_memory/tests/scope_isolation.rs` | `full_persona_invisible` |
| AC12.4 | write_to_persona succeeds under None | integration | `crates/pattern_runtime/tests/sdk_write_to_persona.rs` | `write_to_persona_none_ok` |
| AC12.5 | write_to_persona denied under CoreOnly/Full | integration | `crates/pattern_runtime/tests/sdk_write_to_persona.rs` | `write_to_persona_coreonly_denied` |
| AC12.6 | Default write target is project in None mode | integration | `crates/pattern_memory/tests/scope_isolation.rs` | `none_default_write_project` |

### v3-memory-rework.AC13: project-scoped personas

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC13.1 | Project persona loads + invokable | integration | `crates/pattern_memory/tests/persona_discovery.rs` | `project_persona_loads` |
| AC13.2 | Project persona not visible from different project | integration | `crates/pattern_memory/tests/persona_discovery.rs` | `project_persona_not_visible_elsewhere` |
| AC13.3 | Global persona works across projects | integration | `crates/pattern_memory/tests/persona_discovery.rs` | `global_persona_cross_project` |
| AC13.4 | Missing required fields -> clear parse error | integration | `crates/pattern_memory/tests/persona_discovery.rs` | `malformed_persona_parse_error` |
| AC13.5 | Same-name collision: project wins | integration | `crates/pattern_memory/tests/persona_discovery.rs` | `project_takes_precedence` |

### v3-memory-rework.AC14: project utilities + Pattern.Diagnostics

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC14.1 | lib/Project/Foo.hs compiles; import resolves | integration | `crates/pattern_runtime/tests/lib_modules.rs` | `lib_module_compiles_and_imports` |
| AC14.2 | Broken Bar.hs excluded; session opens | integration | `crates/pattern_runtime/tests/lib_modules.rs` | `broken_lib_excluded_session_opens` |
| AC14.3 | Pattern.Diagnostics returns compile failures | integration | `crates/pattern_runtime/tests/sdk_diagnostics.rs` | `diagnostics_returns_lib_failures` |
| AC14.4 | Import of broken module -> clear diagnostic | integration | `crates/pattern_runtime/tests/lib_modules.rs` | `import_broken_module_fails_clear` |
| AC14.5 | Compile errors never crash; source location present | integration | `crates/pattern_runtime/tests/lib_modules.rs` | `compile_errors_have_source_location` |
| AC14.6 | No lib/ dir -> session opens cleanly | integration | `crates/pattern_runtime/tests/lib_modules.rs` | `no_lib_dir_session_ok` |

### v3-memory-rework.AC15: end-to-end smoke test

| AC case | Description | Test type | Test file | Test name hint |
|---|---|---|---|---|
| AC15.1 | smoke_e2e passes deterministically in CI | integration | `crates/pattern_memory/tests/smoke_e2e.rs` | `smoke_e2e` |
| AC15.2 | Full DoD flow exercised | integration | `crates/pattern_memory/tests/smoke_e2e.rs` | `smoke_e2e` (single test covers all steps) |
| AC15.3 | `cargo nextest run --workspace` passes | structural | (command-only) | `cargo nextest run --workspace --profile ci` |
| AC15.4 | FTS5 + vector insta snapshots stable | snapshot | `crates/pattern_db/tests/snapshots/*.snap` | `cargo insta pending-snapshots` returns zero |
| AC15.5 | Any smoke step failure -> loud specific error | integration | `crates/pattern_memory/tests/smoke_e2e.rs` | `smoke_e2e` (assert messages identify step) |
| AC15.6 | Multi-agent concurrent stress: no deadlock or data loss | integration | `crates/pattern_memory/tests/concurrent_stress.rs` | `concurrent_memory_cache_stress` |

---

## Human verification

### v3-memory-rework.AC10: Mode C spike outcome

**Criteria:** AC10.1, AC10.2, AC10.3

**Justification:** Mode C spike is an interpretive exercise. The 50-op interleaved test's outcome (pass / fail / acceptable-with-rough-edges) requires human reading of a note file at `docs/notes/YYYY-MM-DD-mode-c-spike.md`. Fully-automated detection of "acceptable concurrency behavior" is not feasible -- upstream jj itself cautions the dual-VCS-colocated pattern is "not currently thoroughly tested."

**Verification approach:**

1. Main executor runs the 50-op interleaved test harness (`crates/pattern_memory/tests/mode_c_spike.rs`, Phase 6 Task 7).
2. Writes `docs/notes/YYYY-MM-DD-mode-c-spike.md` with observations, the exact 50-op sequence, per-op divergence checks, and a pass/fail verdict.
3. Reviewer reads the note file and signs off or kicks back.
4. Per the plan: pass -> Mode C ships with documented rough edges (AC10.1); fail -> fate-marker in `pattern_memory::modes` + design-plan update (AC10.2). Either outcome produces the note file (AC10.3).

---

## Test file inventory

Grouped by crate, listing all test files this plan introduces or modifies.

**pattern_core**

- `crates/pattern_core/tests/no_pattern_memory_dep.rs` -- reverse-dep trybuild guard (AC1.5)
- `crates/pattern_core/tests/trybuild/no_pattern_memory_dep.rs` -- compile-fail input for AC1.5

**pattern_db**

- `crates/pattern_db/tests/fts5_regression.rs` -- FTS5 BM25 insta snapshots (AC2.3)
- `crates/pattern_db/tests/vector_regression.rs` -- vector KNN insta snapshots (AC2.4)
- `crates/pattern_db/tests/sqlite_vec_smoke.rs` -- sqlite-vec 100-vector KNN regression (AC2.9)
- `crates/pattern_db/tests/transaction_atomicity.rs` -- transaction rollback semantics (AC2.5, AC2.6)
- `crates/pattern_db/tests/pool_stress_20.rs` -- 20-caller concurrent pool stress (AC2.7)
- `crates/pattern_db/tests/cross_db_query.rs` -- ATTACH + cross-db join (AC2.10)
- `crates/pattern_db/tests/migrations_roundtrip.rs` -- migration coverage (AC3.3, AC3.4, AC3.6)

**pattern_memory**

- `crates/pattern_memory/tests/api_parity.rs` -- API surface smoke (AC1.2)
- `crates/pattern_memory/tests/kdl_roundtrip_proptest.rs` -- KDL proptest round-trips (AC6.2, AC6.3, AC6.5)
- `crates/pattern_memory/tests/detect.rs` -- jj binary detection (AC8.1, AC8.5)
- `crates/pattern_memory/tests/jj_adapter_read.rs` -- jj read-only adapter functions (AC8.2, AC8.7)
- `crates/pattern_memory/tests/jj_adapter_mutate.rs` -- jj mutation functions (AC8.2)
- `crates/pattern_memory/tests/quiesce.rs` -- quiesce drain + WAL checkpoint (AC8.3, AC8.4)
- `crates/pattern_memory/tests/config.rs` -- .pattern.kdl parsing (AC9.5)
- `crates/pattern_memory/tests/mount_lifecycle.rs` -- Mode A/B attach/detach (AC9.1, AC9.2, AC9.6--AC9.8)
- `crates/pattern_memory/tests/mode_c_spike.rs` -- Mode C 50-op harness (AC10.1--AC10.3, human-interpreted)
- `crates/pattern_memory/tests/backup_snapshot.rs` -- snapshot creation + atomicity (AC11.1, AC11.2, AC11.7)
- `crates/pattern_memory/tests/backup_restore.rs` -- restore + pre-restore safety (AC11.3, AC11.4, AC11.6)
- `crates/pattern_memory/tests/scope_isolation.rs` -- MemoryScope routing (AC12.1--AC12.3, AC12.6)
- `crates/pattern_memory/tests/persona_discovery.rs` -- project-scoped personas (AC13.1--AC13.5)
- `crates/pattern_memory/tests/smoke_e2e.rs` -- capstone end-to-end DoD flow (AC15.1, AC15.2, AC15.5)
- `crates/pattern_memory/tests/concurrent_stress.rs` -- multi-agent concurrent stress (AC15.6)

**pattern_runtime**

- `crates/pattern_runtime/tests/no_archive_delete.rs` -- trybuild driver for SDK removal (AC4.9)
- `crates/pattern_runtime/tests/trybuild/no_archive_delete.rs` -- compile-fail input for AC4.9
- `crates/pattern_runtime/tests/eval_worker_100_requests.rs` -- 100-request eval worker stress (AC5.7)
- `crates/pattern_runtime/tests/eval_worker_runtime_panic.rs` -- panic handling (AC5.8)
- `crates/pattern_runtime/tests/search_spawn_blocking_regression.rs` -- spawn_blocking search bug regression (AC5.5)
- `crates/pattern_runtime/tests/sdk_write_to_persona.rs` -- write_to_persona effect (AC12.4, AC12.5)
- `crates/pattern_runtime/tests/lib_modules.rs` -- per-module compile isolation (AC14.1, AC14.2, AC14.4--AC14.6)
- `crates/pattern_runtime/tests/sdk_diagnostics.rs` -- Pattern.Diagnostics effect (AC14.3)

**pattern_cli**

- `crates/pattern_cli/tests/concurrent_memory_ops.rs` -- async callsite spawn_blocking (AC5.6)
- `crates/pattern_cli/tests/cli_mount.rs` -- mount init + attach CLI wiring (AC9.*)
- `crates/pattern_cli/tests/cli_backup.rs` -- backup CLI wiring (AC11.*)
