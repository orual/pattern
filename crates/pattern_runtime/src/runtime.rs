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
}

impl TidepoolRuntime {
    /// Construct with an explicit SDK location and memory store.
    pub fn new(
        sdk: SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Self {
        Self {
            sdk,
            memory_store,
            provider,
        }
    }

    /// Construct using [`SdkLocation::default`] (respects `$PATTERN_SDK_DIR`).
    pub fn with_default_sdk(
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
    ) -> Self {
        Self::new(SdkLocation::default(), memory_store, provider)
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
        let mut session = tokio::task::spawn_blocking(move || {
            TidepoolSession::open(persona, &sdk, memory_store, provider)
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
