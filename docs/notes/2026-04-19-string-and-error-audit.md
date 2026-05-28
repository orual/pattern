# String and error audit

**Date:** 2026-04-19
**Status:** reference notes for opportunistic cleanup

---

## Error enum issues

### High severity (typed errors being stringified)

1. `RuntimeError::ProviderError { reason: String }` — `ProviderError` type exists and is in scope. should carry `#[source] ProviderError`.
2. `ConfigError::Io(String)` — should be `Io(#[from] std::io::Error)`.

### Medium severity (cause fields discarding source errors)

3. `CoreError::CoordinationFailed { cause: String }` — should carry typed source.
4. `CoreError::AgentGroupError { cause: String }` — same.
5. `CoreError::DataSourceError { cause: String }` — same.
6. `CoreError::ExportError { cause: String }` — same.
7. `CoreError::DagCborDecodingError { details: String }` — should carry `#[source]` decode error.
8. `RuntimeError::CheckpointFailed { reason: String }` — should carry `#[source] std::io::Error`.
9. `RuntimeError::PreflightFailed { reason: String }` — should carry source.
10. `RuntimeError::JoinError { reason: String }` — should carry `#[source] tokio::task::JoinError`.
11. `RuntimeError::DatabasePersistenceFailed { reason: String }` — should carry `#[source] pattern_db::DbError`.
12. `RouterError::RouteFailed(String)` — should carry `#[source] Box<dyn Error + Send + Sync>`.
13. `DocumentError::ImportFailed(String)` / `ExportFailed(String)` — no source preserved.
14. `PersonaLoadError::Parse { message: String }` — stringifies `toml::Error`.
15. `ConfigError::TomlParse(String)` / `TomlSerialize(String)` — stringify source errors.

### Low severity (String fields that should use domain types)

16. `CoreError::AgentProcessing { agent_id: String }` — should use `AgentId`.
17. `CoreError::AgentNotFound { identifier: String }` — should use `AgentId`.
18. `CoreError::AgentInitFailed { cause: String }` — should use `#[source]`.
19. `MemoryError::NotFound { agent_id: String }` (core_types.rs) — should use `AgentId`.
20. `RuntimeError::MissingRuntimePrimitive { name: String }` — `name` is `&'static str` from tidepool.

### Missing `#[non_exhaustive]`

21. `LettaConversionError` (`pattern_core/src/export/letta_convert.rs`)
22. `DocumentError` (`pattern_core/src/types/memory_types/core_types.rs`)
23. `MemoryError` (core_types.rs)
24. `DiscordError` (`pattern_discord`)
25. `McpError` (`pattern_mcp`)

### Structural

26. **Two public `MemoryError` types** — `pattern_core::error::MemoryError` and `pattern_core::types::memory_types::MemoryError`. overlapping semantics. needs unification or renaming.
27. **`MemoryPermission::from_str` returns `Err(String)`** — needs a `MemoryPermissionParseError` type.

---

## String → SmolStr candidates

### Approach notes

- SmolStr inlines ≤22 bytes, Arc-shares beyond. even large strings benefit from cheaper cloning vs String's heap clone.
- **orphan rule:** can't impl `FromSql for SmolStr` directly (both foreign). use a helper function `fn smol(row: &Row, idx: usize) -> SmolStr` or manual conversion in `from_row`. avoid newtype wrapper overhead.
- ID fields in `pattern_core::types::ids` are already SmolStr aliases. the DB layer should match.

### Highest-value targets

**pattern_core types (cloned into handlers):**
- `BlockCreate`: `label`, `description`
- `BlockWrite`: `rendered_content`, `previous_rendered_content`
- `BlockRef`: `label`, `block_id`, `agent_id`
- `Embedding`: `model`
- `CompletionRequest`: `model`
- `ToolResult`: `call_id`
- `ProviderCredential`: `provider`
- `CompressionStrategy::RecursiveSummarization`: `summarization_model`
- `CompositeSection`: `name`, `description`
- `FieldDef`: `name`, `description`
- `BlockFilter`: `agent_id`, `label_prefix`
- `BlockMetadataPatch`: `description`
- `BlockMetadata`: `id`, `agent_id`, `label`, `description`
- `ArchivalEntry` (core): `id`, `agent_id` (NOT `content` — large)
- `SharedBlockInfo`: `block_id`, `owner_agent_id`, `owner_agent_name`, `label`, `description`
- `MemorySearchResult`: `id`

**pattern_runtime (cloned per handler call):**
- `SessionContext`: `agent_id`, `model_id`
- `ParsedConstructor`: `name`

**pattern_db models (every DB row deserialized):**
- `Agent`: `id`, `name`, `model_provider`, `model_name` (NOT `system_prompt` — large)
- `ModelRoutingRule`: `model`
- `AgentGroup`: `id`, `name`
- `AgentAtprotoEndpoint`: `agent_id`, `did`, `endpoint_type`
- `GroupMemberRole::Specialist`: `domain`
- `MemoryBlock`: `id`, `agent_id`, `label`, `embedding_model`
- `ArchivalEntry` (db): `id`, `agent_id` (NOT `content`)
- `MemoryBlockCheckpoint`: `block_id`
- `SharedBlockAttachment`: `block_id`, `agent_id`
- `MemoryBlockUpdate`: `block_id`
- `Message`: `id`, `agent_id`, `position`, `batch_id`, `source`
- `ArchiveSummary`: `id`, `agent_id`, `start_position`, `end_position`, `previous_summary_id` (NOT `summary`)
- `MessageSummary`: `id`, `position`, `source`
- `QueuedMessage`: `id`, `target_agent_id`, `source_agent_id`, `role`, `batch_id`
- `Task`: `id`, `agent_id`, `title`, `parent_task_id`
- `TaskSummary`: `id`, `title`, `parent_task_id`
- `Event`: `id`
- `EventOccurrence`: `id`, `event_id`
- `ActivityEvent`: `id`, `agent_id`
- `AgentSummary`: `agent_id`
- `ConstellationSummary`: `id`
- `NotableEvent`: `id`, `event_type`
- `CoordinationTask`: `id`, `assigned_to`
- `HandoffNote`: `id`, `from_agent`, `to_agent`
- `CoordinationState`: `key`, `updated_by`
- `Folder`: `id`, `name`, `embedding_model`
- `FolderFile`: `id`, `folder_id`, `name`, `content_type`
- `FilePassage`: `id`, `file_id`
- `FolderAttachment`: `folder_id`, `agent_id`
- `DataSource`: `id`, `name`
- `AgentDataSource`: `agent_id`, `source_id`
- `SearchResult` (search.rs): `id`

**pattern_memory:**
- `ChangeSource::Agent`, `ChangeSource::Human`, `ChangeSource::Integration` — all carry short identifier strings.

### Skip (too large / no benefit)

- `PersonaSnapshot::system_prompt` — arbitrarily large
- `SessionContext::system_prompt` — arbitrarily large
- `Agent::system_prompt` — large
- `ArchiveSummary::summary` — LLM-generated text
- `AgentSummary::summary` — LLM-generated text
- `ArchivalEntry::content` — large
- `CheckpointEvent::request_repr` / `response_repr` — debug dumps
- `CodeToolInput::code` — Haskell snippets
- `QueuedMessage::content` / JSON blob fields — serialized data

### Total: ~85 candidate fields
