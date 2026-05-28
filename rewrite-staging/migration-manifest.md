# Phase 2 Migration Manifest

Generated 2026-04-16. Authoritative until this phase completes; archived after.

## Columns

- **Origin path** — path inside `crates/pattern_core/src/` as of Phase 1 end.
- **Disposition** — `keep` / `stage` / `split` / `delete` / `extract-then-stage`.
- **Destination** — final home (crate + path), `/dev/null` for deletes, `rewrite-staging/<subdir>` for stages.
- **Phase** — consuming phase.
- **Notes** — any reshape guidance.

## Manifest

| Origin path | Disposition | Destination | Phase | Notes |
|---|---|---|---|---|
| `agent/mod.rs` | stage | `rewrite-staging/agent_runtime/agent/mod.rs` | 3 | Kept verbatim; reshape during runtime integration |
| `agent/traits.rs` | extract-then-stage | `pattern_core/src/traits/agent_runtime.rs` + `traits/session.rs`; legacy shape → `rewrite-staging/agent_runtime/legacy_agent_traits.rs` | 2 (this phase) + 3 | Agent trait body decomposes into AgentRuntime + Session |
| `agent/collect.rs` | stage | `rewrite-staging/agent_runtime/agent/collect.rs` | 3 | Helper for agent message collection; reshape during runtime integration |
| `agent/db_agent.rs` | stage | `rewrite-staging/agent_runtime/agent/db_agent.rs` | 3 | DB-backed agent impl; reshape during runtime integration |
| `agent/processing/loop_impl.rs` | stage | `rewrite-staging/agent_runtime/agent/processing/loop_impl.rs` | 3 | Gutting required: Tidepool substrate replaces direct async loop |
| `agent/processing/mod.rs` | stage | `rewrite-staging/agent_runtime/agent/processing/mod.rs` | 3 | See Phase 3 for decomposition |
| `agent/processing/content.rs` | stage | `rewrite-staging/agent_runtime/agent/processing/content.rs` | 3 | Content processing helpers; stage with remainder |
| `agent/processing/errors.rs` | stage | `rewrite-staging/agent_runtime/agent/processing/errors.rs` | 3 | Processing error types; stage with remainder |
| `agent/processing/retry.rs` | stage | `rewrite-staging/agent_runtime/agent/processing/retry.rs` | 3 | Retry logic; stage with remainder |
| `config.rs` | keep | `pattern_core` | — | Fate comment: breakup needed in future config-cleanup plan |
| `context/mod.rs` | split | `DEFAULT_BASE_INSTRUCTIONS` → `pattern_core/src/base_instructions.rs`; rest → `rewrite-staging/context/mod.rs` | 2 + 5 | Constant extracted; builder logic stages |
| `context/builder.rs` | stage | `rewrite-staging/context/builder.rs` | 5 | Lines 226–316 (block render) become composer input |
| `context/compression.rs` | stage | `rewrite-staging/context/compression.rs` | 5 | Four strategies retained; callsites swap to provider-reported token counts |
| `context/activity.rs` | stage | `rewrite-staging/context/activity.rs` | 5 | Activity tracking; stages with context remainder |
| `context/heartbeat.rs` | stage | `rewrite-staging/context/heartbeat.rs` | 5 | Heartbeat logic; stages with context remainder |
| `context/types.rs` | stage | `rewrite-staging/context/types.rs` | 5 | Context type definitions; stages with context remainder |
| `coordination/mod.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/mod.rs` | future-subagent | "Left intact" per design; reshape during subagent plan |
| `coordination/groups.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/groups.rs` | future-subagent | — |
| `coordination/types.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/types.rs` | future-subagent | — |
| `coordination/utils.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/utils.rs` | future-subagent | — |
| `coordination/test_utils.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/test_utils.rs` | future-subagent | — |
| `coordination/patterns/mod.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/mod.rs` | future-subagent | — |
| `coordination/patterns/dynamic.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/dynamic.rs` | future-subagent | — |
| `coordination/patterns/pipeline.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/pipeline.rs` | future-subagent | — |
| `coordination/patterns/round_robin.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/round_robin.rs` | future-subagent | — |
| `coordination/patterns/sleeptime.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/sleeptime.rs` | future-subagent | — |
| `coordination/patterns/supervisor.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/supervisor.rs` | future-subagent | — |
| `coordination/patterns/voting.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/patterns/voting.rs` | future-subagent | — |
| `coordination/selectors/mod.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/selectors/mod.rs` | future-subagent | — |
| `coordination/selectors/capability.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/selectors/capability.rs` | future-subagent | — |
| `coordination/selectors/load_balancing.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/selectors/load_balancing.rs` | future-subagent | — |
| `coordination/selectors/random.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/selectors/random.rs` | future-subagent | — |
| `coordination/selectors/supervisor.rs` | stage | `rewrite-staging/runtime_subsystems/coordination/selectors/supervisor.rs` | future-subagent | — |
| `data_source/mod.rs` | split | traits → `pattern_core/src/traits/{data_stream,source_manager}.rs`; impls → `rewrite-staging/runtime_subsystems/data_source/mod.rs` | 2 + 3 | Trait shapes refined; concrete sources stage |
| `data_source/block.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/block.rs` | 3 | — |
| `data_source/file_source.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/file_source.rs` | 3 | — |
| `data_source/helpers.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/helpers.rs` | 3 | — |
| `data_source/manager.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/manager.rs` | 3 | — |
| `data_source/registry.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/registry.rs` | 3 | — |
| `data_source/stream.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/stream.rs` | 3 | — |
| `data_source/tests.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/tests.rs` | 3 | — |
| `data_source/types.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/types.rs` | 3 | — |
| `data_source/bluesky/mod.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/mod.rs` | 3 | — |
| `data_source/bluesky/batch.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/batch.rs` | 3 | — |
| `data_source/bluesky/blocks.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/blocks.rs` | 3 | — |
| `data_source/bluesky/embed.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/embed.rs` | 3 | — |
| `data_source/bluesky/firehose.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/firehose.rs` | 3 | — |
| `data_source/bluesky/inner.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/inner.rs` | 3 | — |
| `data_source/bluesky/thread.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/bluesky/thread.rs` | 3 | — |
| `data_source/process/mod.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/mod.rs` | 3 | — |
| `data_source/process/backend.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/backend.rs` | 3 | — |
| `data_source/process/error.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/error.rs` | 3 | — |
| `data_source/process/local_pty.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/local_pty.rs` | 3 | — |
| `data_source/process/permission.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/permission.rs` | 3 | — |
| `data_source/process/source.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/source.rs` | 3 | — |
| `data_source/process/tests.rs` | stage | `rewrite-staging/runtime_subsystems/data_source/process/tests.rs` | 3 | — |
| `db/mod.rs` | extract-then-stage | trait shapes (if any) → `pattern_core/src/traits/`; rest → `rewrite-staging/` | 2 | Audit contents in Task 9 |
| `db/combined.rs` | extract-then-stage | trait shapes (if any) → `pattern_core/src/traits/`; rest → `rewrite-staging/` | 2 | Audit contents in Task 9 |
| `embeddings/mod.rs` | split | trait → `pattern_core/src/traits/embedder.rs` (if it has one); impls → `rewrite-staging/provider/embeddings/mod.rs` | 2 + future | — |
| `embeddings/candle.rs` | stage | `rewrite-staging/provider/embeddings/candle.rs` | future | — |
| `embeddings/cloud.rs` | stage | `rewrite-staging/provider/embeddings/cloud.rs` | future | — |
| `embeddings/ollama.rs` | stage | `rewrite-staging/provider/embeddings/ollama.rs` | future | — |
| `embeddings/simple.rs` | stage | `rewrite-staging/provider/embeddings/simple.rs` | future | — |
| `error.rs` | rewrite-in-place | `pattern_core/src/error/` (split into CoreError/RuntimeError/ProviderError/MemoryError) | 2 | See Task 13 |
| `export/mod.rs` | keep | `pattern_core` | — | Fate comment: may reshape if file-format plan lands |
| `export/car.rs` | keep | `pattern_core` | — | — |
| `export/exporter.rs` | keep | `pattern_core` | — | — |
| `export/importer.rs` | keep | `pattern_core` | — | — |
| `export/letta_convert.rs` | keep | `pattern_core` | — | — |
| `export/letta_types.rs` | keep | `pattern_core` | — | — |
| `export/tests.rs` | keep | `pattern_core` | — | — |
| `export/types.rs` | keep | `pattern_core` | — | — |
| `id.rs` | absorb | `pattern_core/src/types/ids.rs` | 2 | Merge existing define_id_type! newtypes with new WorkspaceId/ProjectId |
| `lib.rs` | rewrite | `pattern_core/src/lib.rs` (replace exports with traits/types/error/memory surface) | 2 | — |
| `memory/mod.rs` | keep | `pattern_core` | — | Preserved verbatim per design |
| `memory/cache.rs` | keep | `pattern_core` | — | Preserved verbatim per design |
| `memory/document.rs` | keep | `pattern_core` | — | Preserved |
| `memory/schema.rs` | keep | `pattern_core` | — | Preserved |
| `memory/sharing.rs` | keep | `pattern_core` | — | Preserved; sharing semantics stable across rewrite |
| `memory/store.rs` | keep (refined) | `pattern_core/src/traits/memory_store.rs` (trait) + `memory/store.rs` (impl) | 2 | Trait split from impl per Task 16 |
| `memory/types.rs` | keep | `pattern_core` | — | Preserved; memory type definitions stable |
| `memory_acl.rs` | keep | `pattern_core` | — | — |
| `messages/mod.rs` | split | value types → `pattern_core/src/types/message.rs`; storage helpers → `rewrite-staging/runtime_subsystems/messages/mod.rs` | 2 | Audit per Task 9 |
| `messages/batch.rs` | stage | `rewrite-staging/runtime_subsystems/messages/batch.rs` | 2 | — |
| `messages/conversions.rs` | stage | `rewrite-staging/runtime_subsystems/messages/conversions.rs` | 2 | — |
| `messages/queue.rs` | stage | `rewrite-staging/runtime_subsystems/messages/queue.rs` | 2 | — |
| `messages/response.rs` | stage | `rewrite-staging/runtime_subsystems/messages/response.rs` | 2 | — |
| `messages/store.rs` | stage | `rewrite-staging/runtime_subsystems/messages/store.rs` | 2 | — |
| `messages/tests.rs` | stage | `rewrite-staging/runtime_subsystems/messages/tests.rs` | 2 | — |
| `messages/types.rs` | split | value types → `pattern_core/src/types/message.rs`; rest → `rewrite-staging/runtime_subsystems/messages/types.rs` | 2 | — |
| `model.rs` | split | trait shape → `pattern_core/src/traits/provider_client.rs`; impls → `rewrite-staging/provider/model/model.rs` | 2 + 4 | See Task 17 for ProviderClient design; note: flat file (not model/mod.rs) |
| `model/defaults.rs` | stage | `rewrite-staging/provider/model/defaults.rs` | 4 | — |
| `oauth.rs` | stage | `rewrite-staging/provider/oauth/oauth.rs` | 4 | Flat file wrapping the oauth/ submodule; absorbs into pattern_provider/auth |
| `oauth/auth_flow.rs` | stage | `rewrite-staging/provider/oauth/auth_flow.rs` | 4 | — |
| `oauth/integration.rs` | stage | `rewrite-staging/provider/oauth/integration.rs` | 4 | — |
| `oauth/middleware.rs` | stage | `rewrite-staging/provider/oauth/middleware.rs` | 4 | — |
| `oauth/resolver.rs` | stage | `rewrite-staging/provider/oauth/resolver.rs` | 4 | — |
| `permission.rs` | keep | `pattern_core` | — | Flat file; core primitive |
| `prompt_template.rs` | delete | `/dev/null` | 2 | Unused per user directive. Git history preserves the pre-v3 implementation for reference when a future adaptive-base-prompt-templating plan revisits the concept |
| `queue/mod.rs` | split | trait/types → `pattern_core/src/{traits,types}/`; impls → `rewrite-staging/runtime_subsystems/queue/mod.rs` | 2 + future | Rework expected |
| `queue/processor.rs` | stage | `rewrite-staging/runtime_subsystems/queue/processor.rs` | future | — |
| `realtime.rs` | split | trait/types → `pattern_core/src/{traits,types}/`; impls → `rewrite-staging/runtime_subsystems/realtime.rs` | 2 + future | Flat file; rework expected |
| `runtime/mod.rs` | stage | `rewrite-staging/agent_runtime/runtime/mod.rs` | 3 | — |
| `runtime/router.rs` | stage | `rewrite-staging/agent_runtime/runtime/router.rs` | 3 | MessageRouter trait extracted in Task 19 |
| `runtime/context.rs` | stage | `rewrite-staging/agent_runtime/runtime/context.rs` | 3 | — |
| `runtime/executor.rs` | stage | `rewrite-staging/agent_runtime/runtime/executor.rs` | 3 | — |
| `runtime/tool_context.rs` | stage | `rewrite-staging/agent_runtime/runtime/tool_context.rs` | 3 | — |
| `runtime/types.rs` | stage | `rewrite-staging/agent_runtime/runtime/types.rs` | 3 | — |
| `runtime/endpoints/mod.rs` | stage | `rewrite-staging/agent_runtime/runtime/endpoints/mod.rs` | 3 | — |
| `runtime/endpoints/group.rs` | stage | `rewrite-staging/agent_runtime/runtime/endpoints/group.rs` | 3 | — |
| `test_helpers.rs` | keep | `pattern_core` | — | Test utility module; retained for use in unit tests of kept modules |
| `tool/mod.rs` | stage | `rewrite-staging/runtime_subsystems/tool/mod.rs` | 3 + plugin-system | Registry + DynamicTool reshape in plugin plan |
| `tool/mod_utils.rs` | stage | `rewrite-staging/runtime_subsystems/tool/mod_utils.rs` | 3 + plugin-system | — |
| `tool/registry.rs` | stage | `rewrite-staging/runtime_subsystems/tool/registry.rs` | 3 + plugin-system | — |
| `tool/schema_filter.rs` | stage | `rewrite-staging/runtime_subsystems/tool/schema_filter.rs` | 3 + plugin-system | — |
| `tool/schema_simplifier.rs` | stage | `rewrite-staging/runtime_subsystems/tool/schema_simplifier.rs` | 3 + plugin-system | — |
| `tool/builtin/mod.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/mod.rs` | 3 + plugin-system | — |
| `tool/builtin/block.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/block.rs` | 3 + plugin-system | — |
| `tool/builtin/block_edit.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/block_edit.rs` | 3 + plugin-system | — |
| `tool/builtin/calculator.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/calculator.rs` | 3 + plugin-system | — |
| `tool/builtin/constellation_search.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/constellation_search.rs` | 3 + plugin-system | — |
| `tool/builtin/file.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/file.rs` | 3 + plugin-system | — |
| `tool/builtin/recall.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/recall.rs` | 3 + plugin-system | — |
| `tool/builtin/search.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/search.rs` | 3 + plugin-system | — |
| `tool/builtin/search_utils.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/search_utils.rs` | 3 + plugin-system | — |
| `tool/builtin/send_message.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/send_message.rs` | 3 + plugin-system | — |
| `tool/builtin/shell.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/shell.rs` | 3 + plugin-system | — |
| `tool/builtin/shell_types.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/shell_types.rs` | 3 + plugin-system | — |
| `tool/builtin/source.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/source.rs` | 3 + plugin-system | — |
| `tool/builtin/system_integrity.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/system_integrity.rs` | 3 + plugin-system | — |
| `tool/builtin/test_schemas.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/test_schemas.rs` | 3 + plugin-system | — |
| `tool/builtin/test_utils.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/test_utils.rs` | 3 + plugin-system | — |
| `tool/builtin/tests.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/tests.rs` | 3 + plugin-system | — |
| `tool/builtin/types.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/types.rs` | 3 + plugin-system | — |
| `tool/builtin/web.rs` | stage | `rewrite-staging/runtime_subsystems/tool/builtin/web.rs` | 3 + plugin-system | — |
| `tool/rules/mod.rs` | stage | `rewrite-staging/runtime_subsystems/tool/rules/mod.rs` | 3 + plugin-system | — |
| `tool/rules/engine.rs` | stage | `rewrite-staging/runtime_subsystems/tool/rules/engine.rs` | 3 + plugin-system | — |
| `tool/rules/integration_tests.rs` | stage | `rewrite-staging/runtime_subsystems/tool/rules/integration_tests.rs` | 3 + plugin-system | — |
| `users.rs` | delete | `/dev/null` | 2 | Vestigial per user directive |
| `utils/mod.rs` | audit | `pattern_core/src/utils/` for reusable helpers; rest → `rewrite-staging/runtime_subsystems/utils/` | 2 | Task 9 audits contents |
| `utils/debug.rs` | audit | `pattern_core/src/utils/debug.rs` for reusable helpers; rest → `rewrite-staging/runtime_subsystems/utils/` | 2 | Task 9 audits contents |
| `utils/error_logging.rs` | audit | `pattern_core/src/utils/error_logging.rs` for reusable helpers; rest → `rewrite-staging/runtime_subsystems/utils/` | 2 | Task 9 audits contents |

## Post-hoc additions (after 2026-04-16 cutoff)

| Origin path | Disposition | Destination | Phase | Notes |
|---|---|---|---|---|
| `export.rs` (+ `export/`) | stage | `rewrite-staging/pattern_core_export/` | done post-hoc (v3-task-skill-blocks circular-dep fix, 2026-04-23) | Module relocated to `pattern_memory/src/export/` as part of breaking the `pattern_core → pattern_db` dep cycle (commit `b29738a1`). Staged copies are obsolete duplicates kept for reference; live copies in pattern_memory already absorbed the changes. Delete once confirmed no archaeology value remains. |
