//! Fully-wired handler for `Pattern.Memory`.
//!
//! Dispatches reads / writes / appends against an `Arc<dyn MemoryStore>`
//! obtained from [`crate::session::SessionContext::memory_store`]. The
//! store trait is defined in `pattern_core::traits::memory_store` and is
//! dyn-compatible (its methods are `async_trait` + `Send + Sync + Debug`).
//!
//! Not wired in Phase 3 (returns
//! `EffectError::Handler("vector search not yet available in phase 3")`):
//! - [`MemoryReq::Search`] (semantic / vector search)
//! - [`MemoryReq::Recall`] (vector recall)
//! - [`MemoryReq::Archive`] is wired: it sets block type to Archival.
//!
//! The handler's `handle` runs inside `tokio::task::spawn_blocking` (the
//! JIT is blocking), so we can `block_on` an async call via
//! `tokio::runtime::Handle::current().block_on(...)` without deadlocking
//! the runtime's executor threads.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pattern_core::memory::{BlockSchema, BlockType, StructuredDocument};
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockWrite, BlockWriteKind};
use pattern_core::types::origin::{AgentAuthor, Author};
use smol_str::SmolStr;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::MemoryReq;
use crate::session::{SessionContext, record_exchange};
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

/// Handler position of `MemoryHandler` in the canonical [`crate::sdk::bundle::SdkBundle`]
/// HList. Used as the effect tag when recording exchanges into the
/// checkpoint log. Keep in sync with `bundle::SdkBundle`'s ordering.
const MEMORY_HANDLER_TAG: u32 = 0;

/// Handler for `Pattern.Memory`. Holds an `Arc<dyn MemoryStore>` handed
/// over by `TidepoolSession::open`; cheap to clone (Arc-share).
#[derive(Clone)]
pub struct MemoryHandler {
    store: Arc<dyn MemoryStore>,
}

impl std::fmt::Debug for MemoryHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryHandler").finish_non_exhaustive()
    }
}

impl MemoryHandler {
    /// Construct a handler bound to the given store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

impl DescribeEffect for MemoryHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Memory",
            description: "Persistent memory-block operations (Get/Put/Create/Append/Replace/Search/Recall/Archive)",
            constructors: &[
                "Get     :: BlockHandle -> Memory Content",
                "Put     :: BlockHandle -> Content -> Maybe Text -> Memory ()",
                "Create  :: BlockHandle -> Text -> BlockType -> SchemaKind -> Maybe Int -> Content -> Memory ()",
                "Append  :: BlockHandle -> Content -> Memory ()",
                "Replace :: BlockHandle -> Text -> Text -> Memory ()",
                "Search  :: Query -> Memory [BlockHandle]",
                "Recall  :: BlockHandle -> Memory Content",
                "Archive :: BlockHandle -> Memory ()",
            ],
            type_defs: &[
                "type BlockHandle = Text",
                "type Content = Text",
                "type Query = Text",
                "data BlockType = BlockCore | BlockWorking | BlockArchival | BlockLog",
                "data SchemaKind = SchemaText | SchemaMap | SchemaList | SchemaLog",
            ],
            helpers: &[
                "get :: Member Memory effs => BlockHandle -> Eff effs Content\nget h = send (Get h)",
                "put :: Member Memory effs => BlockHandle -> Content -> Eff effs ()\nput h c = send (Put h c Nothing)",
                "putWithDesc :: Member Memory effs => BlockHandle -> Content -> Text -> Eff effs ()\nputWithDesc h c d = send (Put h c (Just d))",
                "create :: Member Memory effs => BlockHandle -> Text -> BlockType -> SchemaKind -> Maybe Int -> Content -> Eff effs ()\ncreate h d bt sk cl ic = send (Create h d bt sk cl ic)",
                "append :: Member Memory effs => BlockHandle -> Content -> Eff effs ()\nappend h c = send (Append h c)",
                "replace :: Member Memory effs => BlockHandle -> Text -> Text -> Eff effs ()\nreplace h old new = send (Replace h old new)",
                "search :: Member Memory effs => Query -> Eff effs [BlockHandle]\nsearch q = send (Search q)",
                "recall :: Member Memory effs => BlockHandle -> Eff effs Content\nrecall h = send (Recall h)",
                "archive :: Member Memory effs => BlockHandle -> Eff effs ()\narchive h = send (Archive h)",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for MemoryHandler {
    type Request = MemoryReq;

    fn handle(
        &mut self,
        req: MemoryReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        // Soft-cancel check — if the watchdog has set the flag, return
        // the sentinel error and let the JIT unwind.
        let state = cx.user().cancel_state();
        if state.cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: memory handler cancelled at entry"
            )));
        }

        // Gate entry: pauses the watchdog's budget accumulation while we
        // do I/O-bound work. RAII guarantees exit on error / panic.
        let _guard = HandlerGuard::enter(&state.gate);

        let agent_id = cx.user().agent_id().to_string();
        let store = self.store.clone();

        // Capture the typed request's Debug form up front — we consume
        // `req` below, so we need the string before the match arms move
        // its fields.
        let request_repr = format!("{req:?}");

        // `handle` is synchronous but the trait is async. We're inside
        // a `spawn_blocking` task (the JIT loop); `block_on` here does
        // not deadlock the tokio runtime's executor threads.
        let handle = tokio::runtime::Handle::current();

        let adapter = cx.user().adapter().clone();

        let result = (|| match req {
            MemoryReq::Get(label) => {
                let text = handle
                    .block_on(store.get_rendered_content(&agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Get: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Get: no block named {label:?} for agent {agent_id:?}"
                        ))
                    })?;
                cx.respond(text)
            }
            MemoryReq::Put(label, content, description) => {
                // Capture pre-write state for BlockWrite record.
                let pre = handle
                    .block_on(pre_write_state(&*store, &agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Put: {e}")))?;
                handle
                    .block_on(upsert_block_content(
                        &*store,
                        &agent_id,
                        &label,
                        &content,
                        description.as_deref(),
                    ))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Put: {e}")))?;

                // Record the write.
                let kind = if pre.existed {
                    BlockWriteKind::Replaced
                } else {
                    BlockWriteKind::Created
                };
                record_block_write(
                    RecordBlockWriteParams {
                        adapter: &adapter,
                        agent_id: &agent_id,
                        label: &label,
                        post_content: &content,
                        kind,
                        pre: &pre,
                    },
                    &handle,
                    &*store,
                );
                cx.respond(())
            }
            MemoryReq::Create(label, description, block_type, schema_kind, char_limit, initial) => {
                let bt: BlockType = block_type.into();
                let schema: BlockSchema = schema_kind.into();
                let limit = char_limit
                    .map(|n| n.max(0) as usize)
                    .unwrap_or(DEFAULT_CHAR_LIMIT);
                let create =
                    pattern_core::types::block::BlockCreate::new(label.clone(), bt, schema)
                        .with_description(description)
                        .with_char_limit(limit);
                let doc = handle
                    .block_on(store.create_block(&agent_id, create))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;
                write_text_into(&doc, &initial)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;
                store.mark_dirty(&agent_id, &label);
                handle
                    .block_on(store.persist_block(&agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;

                // Record the write. Freshly created — no pre-content.
                let memory_id = SmolStr::new(doc.id());
                adapter.record_write(BlockWrite {
                    handle: SmolStr::new(&label),
                    memory_id,
                    block_type: doc.block_type(),
                    rendered_content: initial,
                    kind: BlockWriteKind::Created,
                    previous_content_hash: None,
                    previous_rendered_content: None,
                    at: jiff::Timestamp::now(),
                    author: Author::Agent(AgentAuthor {
                        agent_id: SmolStr::new(&agent_id),
                    }),
                });
                cx.respond(())
            }
            MemoryReq::Append(label, content) => {
                // Capture pre-write state.
                let pre = handle
                    .block_on(pre_write_state(&*store, &agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;
                let existing = pre
                    .rendered_content
                    .as_deref()
                    .unwrap_or_default()
                    .to_string();
                let combined = if existing.is_empty() {
                    content.clone()
                } else {
                    format!("{existing}{content}")
                };
                handle
                    .block_on(upsert_block_content(
                        &*store, &agent_id, &label, &combined, None,
                    ))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;

                record_block_write(
                    RecordBlockWriteParams {
                        adapter: &adapter,
                        agent_id: &agent_id,
                        label: &label,
                        post_content: &combined,
                        kind: BlockWriteKind::Appended,
                        pre: &pre,
                    },
                    &handle,
                    &*store,
                );
                cx.respond(())
            }
            MemoryReq::Replace(label, old, new) => {
                // Capture pre-write state (also validates existence).
                let existing = handle
                    .block_on(store.get_rendered_content(&agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Replace: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Replace: no block named {label:?} for agent {agent_id:?}"
                        ))
                    })?;
                let pre_hash = content_hash(&existing);
                let replaced = existing.replace(&old, &new);
                handle
                    .block_on(upsert_block_content(
                        &*store, &agent_id, &label, &replaced, None,
                    ))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Replace: {e}")))?;

                // We already have the pre-content from the existence check.
                let pre = PreWriteState {
                    existed: true,
                    rendered_content: Some(existing),
                    content_hash: Some(pre_hash),
                    memory_id: None,
                    block_type: None,
                };
                record_block_write(
                    RecordBlockWriteParams {
                        adapter: &adapter,
                        agent_id: &agent_id,
                        label: &label,
                        post_content: &replaced,
                        kind: BlockWriteKind::Replaced,
                        pre: &pre,
                    },
                    &handle,
                    &*store,
                );
                cx.respond(())
            }
            MemoryReq::Search(_query) => Err(EffectError::Handler(
                "vector search not yet available in phase 3".to_string(),
            )),
            MemoryReq::Recall(_handle) => Err(EffectError::Handler(
                "vector search not yet available in phase 3".to_string(),
            )),
            MemoryReq::Archive(label) => {
                handle
                    .block_on(store.set_block_type(&agent_id, &label, BlockType::Archival))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Archive: {e}")))?;
                cx.respond(())
            }
        })();

        // Record the exchange on success. We don't record failures:
        // replay re-drives the JIT against recorded responses, so a
        // failed exchange has no stable response to replay. The JIT
        // will re-encounter the same failure on reach. See
        // crates/pattern_runtime/src/checkpoint.rs for the full
        // replay-shape rationale.
        if let Ok(ref value) = result {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            record_exchange(&log, MEMORY_HANDLER_TAG, request_repr, value, turn);
        }
        result
    }
}

/// Upsert a block's content. If the block does not exist, create it as a
/// Working block with a Text schema; otherwise replace its rendered text
/// and persist.
///
/// - `description = Some(d)`: update (or set on auto-create) the block's
///   description metadata.
/// - `description = None`: leave existing metadata untouched. When the
///   block is missing and must be auto-created, falls back to
///   `DEFAULT_AUTO_CREATE_DESCRIPTION` — which is itself a narrow
///   fallback, not the previous pervasive magic string.
///
/// The StructuredDocument sharing contract documented in
/// `crates/pattern_core/CLAUDE.md` states that the returned document's
/// internal LoroDoc is Arc-shared with the cache, so content mutations
/// propagate. Metadata fields are *not* Arc-shared, so description
/// updates go through the store trait (`update_block_description`).
/// After mutating we call `mark_dirty` + `persist_block` per the
/// contract.
async fn upsert_block_content(
    store: &dyn MemoryStore,
    agent_id: &str,
    label: &str,
    content: &str,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let existing = store.get_block(agent_id, label).await?;
    let (doc, is_new) = match existing {
        Some(doc) => (doc, false),
        None => {
            let desc = description.unwrap_or(DEFAULT_AUTO_CREATE_DESCRIPTION);
            let create = pattern_core::types::block::BlockCreate::new(
                label.to_owned(),
                BlockType::Working,
                BlockSchema::text(),
            )
            .with_description(desc)
            .with_char_limit(DEFAULT_CHAR_LIMIT);
            let doc = store.create_block(agent_id, create).await?;
            (doc, true)
        }
    };
    write_text_into(&doc, content)?;
    // For an existing block, a Some-description updates metadata. For a
    // freshly created block, the description is already set at creation
    // time so we skip the redundant trait call.
    if let (false, Some(desc)) = (is_new, description) {
        store
            .update_block_description(agent_id, label, desc)
            .await?;
    }
    store.mark_dirty(agent_id, label);
    store.persist_block(agent_id, label).await?;
    Ok(())
}

/// Fallback description applied only when an agent calls
/// `Pattern.Memory.write` on a label that doesn't exist *and* supplies
/// no description. Agents wanting meaningful metadata should call
/// `Pattern.Memory.create` (or `writeWithDesc`) explicitly.
const DEFAULT_AUTO_CREATE_DESCRIPTION: &str =
    "auto-created by Pattern.Memory.write (no description supplied)";

/// Replace the rendered text of a document. Delegates to
/// [`StructuredDocument::set_text`] if available; otherwise we fall
/// through to the generic JSON import the document supports.
fn write_text_into(
    doc: &StructuredDocument,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // `StructuredDocument::set_text` takes (content, is_system). We
    // pass `false` — writes driven by agent effects are agent-authored,
    // not system-authored.
    doc.set_text(content, false)?;
    Ok(())
}

/// Default character limit for auto-created blocks. Matches the pattern-db
/// default for Working blocks.
const DEFAULT_CHAR_LIMIT: usize = 4096;

/// Snapshot of a block's state before a mutation, used to populate
/// `BlockWrite.previous_*` fields.
struct PreWriteState {
    existed: bool,
    rendered_content: Option<String>,
    content_hash: Option<u64>,
    memory_id: Option<SmolStr>,
    block_type: Option<BlockType>,
}

/// Capture pre-write state for a block. If the block doesn't exist,
/// returns a state with `existed = false` and `None` fields.
async fn pre_write_state(
    store: &dyn MemoryStore,
    agent_id: &str,
    label: &str,
) -> Result<PreWriteState, Box<dyn std::error::Error + Send + Sync>> {
    match store.get_block(agent_id, label).await? {
        Some(doc) => {
            let rendered = doc.text_content();
            let hash = content_hash(&rendered);
            Ok(PreWriteState {
                existed: true,
                rendered_content: Some(rendered),
                content_hash: Some(hash),
                memory_id: Some(SmolStr::new(doc.id())),
                block_type: Some(doc.block_type()),
            })
        }
        None => Ok(PreWriteState {
            existed: false,
            rendered_content: None,
            content_hash: None,
            memory_id: None,
            block_type: None,
        }),
    }
}

/// Compute a simple hash of content for `BlockWrite.previous_content_hash`.
fn content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Parameters for recording a block write via the adapter.
struct RecordBlockWriteParams<'a> {
    adapter: &'a crate::memory::MemoryStoreAdapter,
    agent_id: &'a str,
    label: &'a str,
    post_content: &'a str,
    kind: BlockWriteKind,
    pre: &'a PreWriteState,
}

/// Record a BlockWrite on the adapter after a successful mutation.
/// Resolves memory_id and block_type from the store if not already
/// captured in the pre-write state (e.g. for newly-created blocks via
/// upsert auto-create).
fn record_block_write(
    params: RecordBlockWriteParams<'_>,
    handle: &tokio::runtime::Handle,
    store: &dyn MemoryStore,
) {
    let RecordBlockWriteParams {
        adapter,
        agent_id,
        label,
        post_content,
        kind,
        pre,
    } = params;

    // Resolve memory_id and block_type. If the pre-write state has them,
    // use those; otherwise fetch from the store (the block exists now
    // since the mutation succeeded).
    let (memory_id, block_type) = match (&pre.memory_id, &pre.block_type) {
        (Some(mid), Some(bt)) => (mid.clone(), *bt),
        _ => {
            // Post-mutation fetch for metadata. Best-effort: if this
            // fails we still record the write with placeholder values.
            match handle.block_on(store.get_block(agent_id, label)) {
                Ok(Some(doc)) => (SmolStr::new(doc.id()), doc.block_type()),
                _ => (SmolStr::new("unknown"), BlockType::Working),
            }
        }
    };

    adapter.record_write(BlockWrite {
        handle: SmolStr::new(label),
        memory_id,
        block_type,
        rendered_content: post_content.to_string(),
        kind,
        previous_content_hash: pre.content_hash,
        previous_rendered_content: pre.rendered_content.clone(),
        at: jiff::Timestamp::now(),
        author: Author::Agent(AgentAuthor {
            agent_id: SmolStr::new(agent_id),
        }),
    });
}

#[cfg(test)]
mod tests {
    //! Unit tests for MemoryHandler's not-yet-wired paths.
    //!
    //! End-to-end round-trip tests live in
    //! `tests/session_lifecycle.rs::memory_write_then_read_roundtrips` —
    //! they exercise real agent programs through the JIT. Here we only
    //! verify that the vector-search paths produce the documented
    //! Phase-3 stub error.

    use super::*;
    use crate::NopProviderClient;
    use crate::testing::standard_datacon_table;
    use crate::timeout::CancelState;
    use pattern_core::ProviderClient;
    use pattern_core::types::snapshot::PersonaConfig;

    /// Minimal in-memory store that errors on any call. Sufficient for
    /// vector-search path tests because those fail before touching the
    /// store.
    #[derive(Debug)]
    struct NeverStore;

    #[async_trait::async_trait]
    impl MemoryStore for NeverStore {
        async fn create_block(
            &self,
            _a: &str,
            _create: pattern_core::types::block::BlockCreate,
        ) -> pattern_core::memory::MemoryResult<pattern_core::memory::StructuredDocument> {
            panic!("NeverStore should not be called in this test")
        }
        async fn get_block(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::StructuredDocument>>
        {
            panic!("NeverStore should not be called in this test")
        }
        async fn get_block_metadata(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::BlockMetadata>>
        {
            panic!()
        }
        async fn list_blocks(
            &self,
            _a: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn list_blocks_by_type(
            &self,
            _a: &str,
            _t: pattern_core::memory::BlockType,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn list_all_blocks_by_label_prefix(
            &self,
            _p: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::BlockMetadata>> {
            panic!()
        }
        async fn delete_block(&self, _a: &str, _l: &str) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn get_rendered_content(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<Option<String>> {
            panic!()
        }
        async fn persist_block(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(&self, _a: &str, _l: &str) {}
        async fn insert_archival(
            &self,
            _a: &str,
            _c: &str,
            _m: Option<serde_json::Value>,
        ) -> pattern_core::memory::MemoryResult<String> {
            panic!()
        }
        async fn search_archival(
            &self,
            _a: &str,
            _q: &str,
            _n: usize,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::ArchivalEntry>> {
            panic!()
        }
        async fn delete_archival(&self, _id: &str) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn search(
            &self,
            _a: &str,
            _q: &str,
            _o: pattern_core::memory::SearchOptions,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>>
        {
            panic!()
        }
        async fn search_all(
            &self,
            _q: &str,
            _o: pattern_core::memory::SearchOptions,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>>
        {
            panic!()
        }
        async fn list_shared_blocks(
            &self,
            _a: &str,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::SharedBlockInfo>>
        {
            panic!()
        }
        async fn get_shared_block(
            &self,
            _r: &str,
            _o: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<Option<pattern_core::memory::StructuredDocument>>
        {
            panic!()
        }
        async fn set_block_pinned(
            &self,
            _a: &str,
            _l: &str,
            _p: bool,
        ) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn set_block_type(
            &self,
            _a: &str,
            _l: &str,
            _t: pattern_core::memory::BlockType,
        ) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn update_block_schema(
            &self,
            _a: &str,
            _l: &str,
            _s: pattern_core::memory::BlockSchema,
        ) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn update_block_description(
            &self,
            _a: &str,
            _l: &str,
            _d: &str,
        ) -> pattern_core::memory::MemoryResult<()> {
            panic!()
        }
        async fn undo_block(&self, _a: &str, _l: &str) -> pattern_core::memory::MemoryResult<bool> {
            panic!()
        }
        async fn redo_block(&self, _a: &str, _l: &str) -> pattern_core::memory::MemoryResult<bool> {
            panic!()
        }
        async fn undo_depth(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<usize> {
            panic!()
        }
        async fn redo_depth(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<usize> {
            panic!()
        }
    }

    fn sctx() -> SessionContext {
        let persona = PersonaConfig::new("agent-a", "A", "module X where\nx = pure ()");
        SessionContext::from_persona(&persona, Arc::new(NeverStore), Arc::new(NopProviderClient))
    }

    #[tokio::test]
    async fn search_returns_phase3_stub_error() {
        let table = standard_datacon_table();
        let ctx = sctx();
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new(Arc::new(NeverStore));
        let err = h
            .handle(MemoryReq::Search("anything".into()), &cx)
            .unwrap_err();
        assert!(err.to_string().contains("vector search"), "got: {err}");
        assert!(err.to_string().contains("phase 3"), "got: {err}");
    }

    #[tokio::test]
    async fn recall_returns_phase3_stub_error() {
        let table = standard_datacon_table();
        let ctx = sctx();
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new(Arc::new(NeverStore));
        let err = h
            .handle(MemoryReq::Recall("block".into()), &cx)
            .unwrap_err();
        assert!(err.to_string().contains("vector search"), "got: {err}");
    }

    /// Replace on a block that does not exist surfaces a handler error
    /// rather than silently auto-creating. The handler uses
    /// `Handle::current().block_on(..)` internally — it expects to be
    /// invoked from a blocking worker, so we dispatch the call through
    /// `spawn_blocking`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_on_missing_block_returns_handler_error() {
        use crate::testing::InMemoryMemoryStore;
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let store_for_ctx = store.clone();
        let provider_for_ctx = provider.clone();
        let err_msg = tokio::task::spawn_blocking(move || {
            let table = standard_datacon_table();
            let persona = PersonaConfig::new("agent-a", "A", "module X where\nx = pure ()");
            let ctx = SessionContext::from_persona(&persona, store_for_ctx, provider_for_ctx);
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MemoryHandler::new(store);
            let err = h
                .handle(
                    MemoryReq::Replace("ghost".into(), "a".into(), "b".into()),
                    &cx,
                )
                .unwrap_err();
            err.to_string()
        })
        .await
        .expect("spawn_blocking task panicked");
        assert!(
            err_msg.contains("Pattern.Memory.Replace"),
            "error should identify op; got: {err_msg}"
        );
        assert!(
            err_msg.contains("ghost"),
            "error should identify missing label; got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn cancelled_flag_short_circuits_at_entry() {
        let table = standard_datacon_table();
        let ctx = sctx();
        ctx.cancel_state()
            .cancellation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new(Arc::new(NeverStore));
        // Even though NeverStore panics on any call, this should not
        // reach the store — the sentinel short-circuits at entry.
        let err = h.handle(MemoryReq::Get("any".into()), &cx).unwrap_err();
        assert!(err.to_string().contains(CANCELLED_SENTINEL), "got: {err}");
        let _ = CancelState::new(); // suppress unused import warning if any
    }
}
