//! Concrete [`pattern_core::traits::AgentRuntime`] implementation (Phase 3).
//!
//! Owns:
//! - An [`SdkLocation`] pointing at the Haskell SDK modules.
//! - An `Arc<dyn MemoryStore>` handed to every session's MemoryHandler.
//!
//! Spawns [`TidepoolSession`] instances on `open_session`, delegating
//! compile + JIT warm to a `tokio::task::spawn_blocking` so the runtime's
//! executor threads stay unblocked.
//!
//! # Trait-object dispatch
//!
//! The memory store and (Phase 4) provider are held as trait objects.
//! `pattern_runtime` must NOT compile-depend on any concrete backend; the
//! Phase 2 architecture forbids that coupling. Adding provider support in
//! Phase 4 means adding another `Arc<dyn ProviderClient>` field and
//! threading it through `TidepoolSession::open` — no cross-crate re-wire.

use std::sync::Arc;

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::RuntimeError;
use pattern_core::traits::{AgentRuntime, MemoryStore};
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};

use crate::sdk::SdkLocation;
use crate::session::TidepoolSession;

/// Runtime supervisor that spawns Tidepool-backed sessions.
#[derive(Debug)]
pub struct TidepoolRuntime {
    sdk: SdkLocation,
    memory_store: Arc<dyn MemoryStore>,
    /// Provider-client handle. Phase 4 wires it in; Phase 5 consumes it
    /// from agent-side model calls. Held here so the runtime's construction
    /// signature is stable across phase boundaries.
    #[allow(dead_code)]
    provider: Arc<dyn ProviderClient>,
    /// Constellation database handle. Threaded to every session opened
    /// by this runtime. Required for message persistence + compaction.
    db: Arc<pattern_db::ConstellationDb>,
    /// Caller-supplied tokio runtime handle. Threaded to every session so
    /// sync handler paths (e.g. the eval-worker thread) can `block_on` an
    /// async future against an explicit, stable runtime instead of
    /// magic-capturing via `Handle::current()` from arbitrary context.
    ///
    /// Explicit-param rationale: `Handle::current()` only resolves inside
    /// an async context, single-threaded runtimes silently change blocking
    /// semantics, and capture-at-use makes the dependency invisible in the
    /// type signature. Surfacing this as a constructor param documents
    /// that the runtime borrows the caller's tokio runtime.
    ///
    /// First consumer: the v3-multi-agent spawn handler (Ephemeral /
    /// AwaitSpawn / AwaitAll arms `block_on` the registry's
    /// `Shared<BoxFuture<SpawnResult>>` from the eval-worker thread).
    /// The sandbox-io Phase 3 PortRegistry actor will share this same
    /// handle when it lands.
    tokio_handle: tokio::runtime::Handle,
}

impl TidepoolRuntime {
    /// Construct with an explicit SDK location, memory store, provider, db,
    /// and tokio handle.
    ///
    /// `tokio_handle` is borrowed from the caller's tokio runtime; see the
    /// field-level docs on [`TidepoolRuntime::tokio_handle`] for the
    /// rationale.
    pub fn new(
        sdk: SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            sdk,
            memory_store,
            provider,
            db,
            tokio_handle,
        }
    }

    /// Construct using [`SdkLocation::default`] (respects `$PATTERN_SDK_DIR`).
    pub fn with_default_sdk(
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self::new(
            SdkLocation::default(),
            memory_store,
            provider,
            db,
            tokio_handle,
        )
    }

    /// Caller-supplied tokio runtime handle. Borrowed by sessions opened
    /// from this runtime; consumed by sync handler paths that need to
    /// `block_on` an async future without depending on `Handle::current()`.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }
}

#[async_trait]
impl AgentRuntime for TidepoolRuntime {
    type Session = TidepoolSession;

    async fn open_session(
        &self,
        persona: PersonaSnapshot,
        snapshot: Option<SessionSnapshot>,
    ) -> Result<Self::Session, RuntimeError> {
        let sdk = self.sdk.clone();
        let memory_store = self.memory_store.clone();
        let provider = self.provider.clone();
        let db = self.db.clone();
        let tokio_handle = self.tokio_handle.clone();
        let mut session = tokio::task::spawn_blocking(move || {
            TidepoolSession::open(persona, &sdk, memory_store, provider, db, tokio_handle)
        })
        .await
        .map_err(|e| RuntimeError::JoinError {
            reason: e.to_string(),
        })??;

        if let Some(snap) = snapshot {
            // Restore seeds the checkpoint log for replay-then-continue.
            // See `TidepoolSession::restore`.
            pattern_core::traits::Session::restore(&mut session, snap).await?;
        }
        Ok(session)
    }

    async fn shutdown(&self) -> Result<(), RuntimeError> {
        // No runtime-level resources to release. Sessions own their JIT
        // machines and drop them in their own `Drop` impl (via SessionMachine).
        Ok(())
    }
}
