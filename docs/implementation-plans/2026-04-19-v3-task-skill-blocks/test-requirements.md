# v3-task-skill-blocks -- Test Requirements

**Generated:** 2026-04-19
**Covers:** AC1--AC10 per docs/design-plans/2026-04-19-v3-task-skill-blocks.md

## Automated tests

### AC1: TaskList schema + KDL round-trip

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC1.1 | `BlockSchema::TaskList` serde round-trip | unit | `crates/pattern_core/src/types/memory_types/schema.rs` tests | Construct variant with all fields, JSON round-trip, assert equality |
| AC1.2 | Task types exist with documented fields + kebab-case serde | unit | `crates/pattern_core/src/types/memory_types/task.rs` tests | Status enum all-variant round-trip; `TaskEdgeRef::from_str` both forms; `TaskItem` JSON round-trip with populated fields |
| AC1.3 | Proptest TaskList KDL round-trip | property | `crates/pattern_memory/tests/task_list_kdl_roundtrip.rs` | Arbitrary TaskList (0..8 items, edges, comments) -> KDL -> LoroValue; canonical JSON comparison |
| AC1.4 | TaskEdgeRef parses both KDL annotation forms | unit | `crates/pattern_memory/src/fs/kdl.rs` tests (or `kdl_task_list.rs`) | `(block)"handle"` and `(block)"handle#item_id"` both parse to correct `TaskEdgeRef` |
| AC1.5 | Malformed TaskEdgeRef produces `KdlConversionError` with span | unit | `crates/pattern_memory/src/fs/kdl.rs` tests | Empty `(block)""`, missing annotation, `"#no-handle"`, `"handle#"` each return typed error with miette `SourceSpan` |
| AC1.6 | Empty TaskList + self-edge round-trip | unit + property | `crates/pattern_memory/src/fs/kdl.rs` tests + `crates/pattern_memory/tests/task_list_kdl_roundtrip.rs` | Zero-item list round-trips; self-referential `A.blocks=[A]` round-trips; proptest strategy allows self-edges |
| AC1.7 | Item reorder preserves ids across round-trip | property | `crates/pattern_memory/tests/task_list_kdl_roundtrip.rs` | `LoroMovableList.mov()` permutations -> KDL -> parse; all original `TaskItemId` values present exactly once |
| AC1.8 | `new_snowflake_id()` produces non-empty base32 id | unit | `crates/pattern_core/src/types/ids.rs` tests | `TaskItemId = SmolStr` alias per house convention; empty-string rejection lives at `TaskEdgeRef::from_str` wire boundary (see AC1.5) |
| AC1.9 | Concurrent `new_snowflake_id()` produces distinct ids | unit | `crates/pattern_core/src/types/ids.rs` tests | 32 threads each call `new_snowflake_id()`, collect into `HashSet<SmolStr>`, assert 32 distinct values |

### AC2: Task block index tables + migration

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC2.1 | Migration applies to fresh DB | integration | `crates/pattern_db/tests/migration_task_block_index.rs` | Run all migrations through memory/0011; assert `tasks`, `task_edges`, `tasks_fts` tables exist |
| AC2.2 | Pre-existing task rows preserved with new column defaults | integration | `crates/pattern_db/tests/migration_task_block_index.rs` | Insert pre-migration row, apply memory/0011, assert row survives with `block_handle=NULL`, `comments_json='[]'` |
| AC2.3 | `coordination_tasks` table dropped; no active callers | integration + compile | `crates/pattern_db/tests/migration_task_block_index.rs` + `cargo check -p pattern-db` | Post-migration table absent; grep confirms no active-path references |
| AC2.4 | `task_edges` schema correct | integration | `crates/pattern_db/tests/migration_task_block_index.rs` | Verify columns via `PRAGMA table_info`; unique expression index functional |
| AC2.5 | Duplicate edge rejected by unique constraint | integration | `crates/pattern_db/tests/migration_task_block_index.rs` | Two identical inserts -> second returns UNIQUE violation |
| AC2.6 | `priority` column drop does not break queries; indexes dropped before table | integration + compile | `crates/pattern_db/tests/migration_task_block_index.rs` | Post-migration `PRAGMA table_info` shows no `priority`; `cargo check -p pattern-db` clean |
| AC2.7 | Block-level vs item-level targets cannot collide | integration | `crates/pattern_db/tests/migration_task_block_index.rs` | Insert `target_item=NULL` and `target_item="x"` for same source; both succeed (distinct under COALESCE) |

### AC3: Subscriber reconciliation for TaskList blocks

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC3.1 | 5 items + 3 edges reconciled to index tables | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Write TaskList, verify row counts in `tasks` and `task_edges` |
| AC3.2 | Deleted item removes row + edges | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Remove item from loro, re-reconcile, assert row and referencing edges gone |
| AC3.3 | Edge add/remove updates `task_edges` in same tx | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Modify `blocks` field, re-reconcile, verify edge row added/removed |
| AC3.4 | Mid-reconcile failure rolls back full transaction | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Inject CHECK constraint causing edge insert failure; assert both tables show previous state |
| AC3.5 | Subscriber panic restarts worker + metric increments | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Inject panic via malformed LoroMap; assert restart + `metrics-util::debugging` counter |
| AC3.6 | Idempotent reconcile (no-change run) | integration | `crates/pattern_memory/tests/subscriber_task_list.rs` | Run subscriber twice with unchanged loro; final row set identical |
| AC3.7 | Concurrent two-agent edits merge cleanly | integration | `crates/pattern_memory/tests/subscriber_task_list_concurrent.rs` | Two LoroDoc instances, merge changesets, reconcile; both agents' changes reflected |

### AC4: `ctx.tasks.*` SDK surface methods

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC4.1 | `create_task` writes item; listed afterward | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Create via handler, `list_tasks` returns new id |
| AC4.2 | `update_task` patches only specified fields | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Patch `subject` only; assert `description` unchanged, `updated_at` refreshed |
| AC4.3 | `transition_status` to Completed sets `completed_at` | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Transition, inspect item metadata for `completed_at` key |
| AC4.4 | `link(A, B)` creates single edge row | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Link, flush subscriber, assert one `task_edges` row source=A target=B |
| AC4.5 | `unlink(A, B)` removes edge row | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Link then unlink, assert edge row gone after reconcile |
| AC4.5b | Cross-block link is atomic (only source doc modified) | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | A in block L1, B in block L2; link; assert only L1's doc version changed |
| AC4.6 | `add_comment` appends with author + timestamp | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Add 3 comments, list in order, verify agent id on each |
| AC4.7 | `update_task` on nonexistent ref -> `TaskNotFound` | unit | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Bogus `TaskEdgeRef` returns `MemoryError::TaskNotFound` |
| AC4.8 | `link(A, A)` self-edge allowed | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Self-link succeeds; edge row has source == target |

### AC5: `list_tasks` + `query_graph`

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC5.1 | `list_tasks(block=Some(h))` returns block h tasks only | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Seed two blocks, list one, assert correct subset |
| AC5.2 | `list_tasks(block=None)` returns scope-visible tasks | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | Two agents' blocks; agent A sees only own tasks |
| AC5.3 | Filter by status, owner, has_blockers, keyword | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests + `crates/pattern_db/tests/queries_task.rs` | Each filter tested independently; FTS5 keyword via insta snapshot |
| AC5.4 | `query_graph` respects direction + depth + max_nodes | integration | `crates/pattern_db/tests/queries_task_graph.rs` + `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | 5-node chain forward; reverse from leaf; `Both` combines |
| AC5.5 | Scope enforcement hides persona-scope tasks | integration | `crates/pattern_runtime/src/sdk/handlers/tasks.rs` tests | `CoreOnly` session cannot see persona-scope TaskList blocks |
| AC5.6 | `query_graph` depth=0 returns root only | integration | `crates/pattern_db/tests/queries_task_graph.rs` | Zero edges in result |
| AC5.6b | `max_nodes` cap + `truncated: true` | integration | `crates/pattern_db/tests/queries_task_graph.rs` | 10k nodes, cap at 1000, verify truncation flag |
| AC5.7 | Cyclic graph terminates via visited set | integration | `crates/pattern_db/tests/queries_task_graph.rs` | A->B->C->A cycle, depth=10; returns 3 nodes, 3 edges |
| AC5.8 | 10k-node runaway graph bounded by max_nodes + time | integration | `crates/pattern_db/tests/queries_task_graph.rs` | Returns <=1000 nodes; completes in <1s (timed assertion) |

### AC6: Skill schema + md+YAML frontmatter round-trip

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC6.1 | `BlockSchema::Skill { expected_keys }` exists + serde | unit | `crates/pattern_core/src/types/memory_types/schema.rs` tests | Construct, JSON round-trip |
| AC6.2 | Proptest skill md+frontmatter round-trip | property | `crates/pattern_memory/tests/skill_md_roundtrip.rs` | Arbitrary `SkillMetadata` + body -> emit -> parse; 100+ cases |
| AC6.3 | Unknown frontmatter keys preserved in extras LoroMap | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests | Key `author: "@me"` survives parse -> emit -> parse |
| AC6.4 | Nested `hooks` JSON structure preserved | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests | Checklist array + workflow map round-trip via `serde_json::Value` |
| AC6.5 | Malformed YAML -> `SkillParseError` with location | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests | `name: [` syntax error; missing `name` key; each returns typed error |
| AC6.6 | Missing `---` delimiters -> `MissingDelimiters` | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests | File without frontmatter delimiters rejected |
| AC6.7 | Minimal frontmatter (name + trust_tier only) parses | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests | All optional fields default to None/empty |

### AC7: Trust tier assignment

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC7.1 | SDK resource dir -> `FirstParty` | unit | `crates/pattern_memory/src/skill.rs` tests | `SkillSource::SdkResourceDir` -> `FirstParty` |
| AC7.2 | Mount skills dir -> `ProjectLocal` | unit | `crates/pattern_memory/src/skill.rs` tests | `SkillSource::MountSkillsDir` -> `ProjectLocal` |
| AC7.3 | Runtime-created -> `AdHoc` | unit | `crates/pattern_memory/src/skill.rs` tests | `SkillSource::Runtime` -> `AdHoc` |
| AC7.4 | Declared `PluginInstalled` preserved on round-trip | unit | `crates/pattern_memory/src/skill.rs` tests | Source is MountSkillsDir but declared tier is PluginInstalled; output is PluginInstalled |
| AC7.5 | PluginInstalled increments warning metric | unit | `crates/pattern_memory/src/skill.rs` tests | `metrics-util::debugging` recorder captures counter increment |
| AC7.6 | Invalid trust_tier string -> parse error | unit | `crates/pattern_memory/src/fs/markdown_skill/` tests + `crates/pattern_core/src/types/memory_types/skill.rs` tests | `"foo"` -> `InvalidTrustTier`; serde deser also rejects |

### AC8: `ctx.skills.*` SDK surface methods

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC8.1 | `list()` returns all scope-visible Skill blocks as `SkillInfo` | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Seed 3 skills + 2 text blocks; list returns 3 |
| AC8.2 | `get_metadata(handle)` returns typed `SkillMetadata` | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Nested `hooks` preserved through SDK boundary |
| AC8.3 | `get_metadata` on non-Skill block returns `None` | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Text block handle -> `None` |
| AC8.4 | `search(query)` returns FTS5-ranked `SkillInfo` | integration + snapshot | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Name, description, body matches; insta snapshot for BM25 ordering |
| AC8.5 | `load` on non-existent block -> `BlockNotFound` | unit | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Missing handle returns error |
| AC8.6 | `load` on non-Skill block -> `NotASkill` | unit | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Text block handle returns `SkillError::NotASkill` |

### AC9: Skill `load` behavior + metadata updates

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC9.1 | `load` injects `[skill:loaded]` pseudo-message in segment 2 | snapshot | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests + `crates/pattern_provider/src/compose/pseudo_messages.rs` tests | insta snapshot of composed request segment 2 |
| AC9.2 | Loaded skill persists across turns in segment 2 | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Multi-turn advance; segment 2 still shows marker |
| AC9.3 | `load` updates sqlite stats only; `.md` hash unchanged | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | blake3 hash before/after 100 loads; `use_count` incremented |
| AC9.3b | `get_metadata` excludes stats; `get_usage_stats` returns fresh stats | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Post-load metadata has no stat fields; separate query returns `use_count` |
| AC9.4 | Multiple skill loads produce ordered markers | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | Load A then B; markers appear in A-then-B order |
| AC9.5 | Same skill loaded twice produces two markers | integration | `crates/pattern_runtime/src/sdk/handlers/skills.rs` tests | No dedup; two `[skill:loaded]` blocks present |
| AC9.6 | Skill load does not dirty VCS working copy | integration | `crates/pattern_memory/tests/skills_load_mode_a.rs` | Mode-A jj-tracked mount; 100 loads; `jj status` shows no modifications |

### AC10: End-to-end smoke + scope enforcement

| AC | Test | Type | File | Notes |
|----|------|------|------|-------|
| AC10.1 | Smoke test passes deterministically | e2e | `crates/pattern_memory/tests/task_skill_smoke.rs` | Full `ctx.tasks.*` + `ctx.skills.*` surface exercised; 4 test functions |
| AC10.2 | Mock provider produces deterministic output | e2e | `crates/pattern_memory/tests/task_skill_smoke.rs` | `MockProviderClient` with scripted turns; no live model |
| AC10.3 | Scope enforcement: project-scope blocks invisible to persona session | e2e | `crates/pattern_memory/tests/task_skill_smoke.rs` | `smoke_scope_enforcement` test; `Full` isolation TaskList + Skill hidden from persona |
| AC10.4 | Failure in smoke produces clear step-identifying error | e2e | `crates/pattern_memory/tests/task_skill_smoke.rs` | Human-readable assertion messages per step |
| AC10.5 | Smoke tests isolated via per-test `TempDir` | e2e | `crates/pattern_memory/tests/task_skill_smoke.rs` | Run with `--test-threads=8`; no shared-state flakiness |
| AC10.6 | External `.kdl` edit reconciled into index | integration | `crates/pattern_memory/tests/external_kdl_edit_reconcile.rs` | Text-append new item to `.kdl` file; watcher fires; `tasks` row appears |
| AC10.7 | Quiesce + commit cycle preserves index across restart | integration | `crates/pattern_memory/tests/quiesce_commit_cycle.rs` | Quiesce, `jj commit`, drop mount, reopen; task index matches pre-quiesce state |
| AC10.8 | Cross-block-type FTS5 search returns all three types | integration + snapshot | `crates/pattern_memory/tests/cross_schema_fts.rs` | Text + TaskList + Skill blocks seeded with shared keyword; insta snapshot of BM25 ordering |

## Human verification

No ACs require human verification. All 75 sub-items (AC1.1 through AC10.8) are covered by automated tests.

AC10.7 is the closest candidate for manual verification (VCS commit cleanliness after quiesce). The `quiesce_commit_cycle.rs` integration test programmatically verifies: WAL truncation, canonical file hashes, `jj commit` success, file set contents (no WAL/SHM artifacts), and index preservation across mount restart. This covers the AC's functional contract. If VCS-presentation-level confidence is desired (e.g., visually inspecting `jj log` output for clean linear history), that is a post-merge spot check rather than an AC gate.
