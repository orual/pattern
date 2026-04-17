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

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pattern_core::memory::{BlockSchema, BlockType, StructuredDocument};
use pattern_core::traits::MemoryStore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::MemoryReq;
use crate::session::SessionContext;
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

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

impl EffectHandler<SessionContext> for MemoryHandler {
    type Request = MemoryReq;

    fn handle(
        &mut self,
        req: MemoryReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        // Soft-cancel check — if the watchdog has set the flag, return
        // the sentinel error and let the JIT unwind.
        if cx.user().cancel_state().cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: memory handler cancelled at entry"
            )));
        }

        // Gate entry: pauses the watchdog's budget accumulation while we
        // do I/O-bound work. RAII guarantees exit on error / panic.
        let gate = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&gate.gate);

        let agent_id = cx.user().agent_id().to_string();
        let store = self.store.clone();

        // `handle` is synchronous but the trait is async. We're inside
        // a `spawn_blocking` task (the JIT loop); `block_on` here does
        // not deadlock the tokio runtime's executor threads.
        let handle = tokio::runtime::Handle::current();

        match req {
            MemoryReq::Read(label) => {
                let text = handle
                    .block_on(store.get_rendered_content(&agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Read: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Read: no block named {label:?} for agent {agent_id:?}"
                        ))
                    })?;
                cx.respond(text)
            }
            MemoryReq::Write(label, content) => {
                handle
                    .block_on(upsert_block_content(&*store, &agent_id, &label, &content))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Write: {e}")))?;
                cx.respond(())
            }
            MemoryReq::Append(label, content) => {
                let existing = handle
                    .block_on(store.get_rendered_content(&agent_id, &label))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?
                    .unwrap_or_default();
                let combined = if existing.is_empty() {
                    content
                } else {
                    format!("{existing}{content}")
                };
                handle
                    .block_on(upsert_block_content(&*store, &agent_id, &label, &combined))
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;
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
        }
    }
}

/// Upsert a block's content. If the block does not exist, create it as a
/// Working block with a Text schema; otherwise replace its rendered text
/// and persist.
///
/// The StructuredDocument sharing contract documented in
/// `crates/pattern_core/CLAUDE.md` states that the returned document's
/// internal LoroDoc is Arc-shared with the cache, so mutations propagate.
/// After mutating we call `mark_dirty` + `persist_block` per that
/// contract.
async fn upsert_block_content(
    store: &dyn MemoryStore,
    agent_id: &str,
    label: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let existing = store.get_block(agent_id, label).await?;
    let doc = match existing {
        Some(doc) => doc,
        None => {
            store
                .create_block(
                    agent_id,
                    label,
                    "auto-created by Pattern.Memory handler",
                    BlockType::Working,
                    BlockSchema::text(),
                    DEFAULT_CHAR_LIMIT,
                )
                .await?
        }
    };
    write_text_into(&doc, content)?;
    store.mark_dirty(agent_id, label);
    store.persist_block(agent_id, label).await?;
    Ok(())
}

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
    use crate::testing::standard_datacon_table;
    use crate::timeout::CancelState;
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
            _l: &str,
            _d: &str,
            _t: pattern_core::memory::BlockType,
            _s: pattern_core::memory::BlockSchema,
            _c: usize,
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
        async fn persist_block(&self, _a: &str, _l: &str) -> pattern_core::memory::MemoryResult<()> {
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
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>> {
            panic!()
        }
        async fn search_all(
            &self,
            _q: &str,
            _o: pattern_core::memory::SearchOptions,
        ) -> pattern_core::memory::MemoryResult<Vec<pattern_core::memory::MemorySearchResult>> {
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
        async fn undo_block(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<bool> {
            panic!()
        }
        async fn redo_block(
            &self,
            _a: &str,
            _l: &str,
        ) -> pattern_core::memory::MemoryResult<bool> {
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
        SessionContext::from_persona(&persona, Arc::new(NeverStore))
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
        let err = h
            .handle(MemoryReq::Read("any".into()), &cx)
            .unwrap_err();
        assert!(
            err.to_string().contains(CANCELLED_SENTINEL),
            "got: {err}"
        );
        let _ = CancelState::new(); // suppress unused import warning if any
    }
}
