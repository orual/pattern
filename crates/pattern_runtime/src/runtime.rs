//! Concrete [`pattern_core::traits::AgentRuntime`] implementation (Phase 3).
//!
//! Owns:
//! - An [`SdkLocation`] pointing at the Haskell SDK modules.
//! - An `Arc<dyn MemoryStore>` handed to every session's MemoryHandler.
//! - A `tokio::runtime::Handle` supplied explicitly by the caller — the handle
//!   is used by Phase 4's PortRegistry actor and any future async-needing
//!   subsystem. Explicit rather than `Handle::current()` magic-capture because
//!   magic-capture is brittle: callers must be in async context at construction
//!   time, and single-threaded runtimes silently change semantics. The explicit
//!   parameter surfaces the contract in the type signature.
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
//!
//! # ProcessManager is per-session
//!
//! Per the Q4 resolution (Amendment 2026-04-26), `ProcessManager` is NOT on
//! `TidepoolRuntime`. It lives on `SessionContext` so that each session has
//! its own shell state (cwd, env). `TidepoolRuntime` holds only the
//! `tokio_handle` — needed by Phase 4's PortRegistry actor; not needed by
//! the sync `ProcessManager`.

use std::sync::Arc;

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::RuntimeError;
use pattern_core::traits::{AgentRuntime, MemoryStore};
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};

use crate::port_registry::PortRegistryImpl;
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
    /// Caller-supplied tokio runtime handle. Exposed for Phase 4's
    /// PortRegistry actor and any future async-needing subsystem.
    ///
    /// **Why explicit?** `Handle::current()` magic-capture is brittle — callers
    /// must be in async context at construction time, and single-threaded
    /// runtimes silently change semantics. Explicit parameter surfaces the
    /// contract.
    ///
    /// **Why not on ProcessManager?** `ProcessManager` is sync (std::thread +
    /// crossbeam) and never needs a runtime. The handle here is for subsystems
    /// that run async work — currently Phase 4's PortRegistry; potentially
    /// future event-broadcast infrastructure.
    tokio_handle: tokio::runtime::Handle,
    /// Runtime-global port registry. One per `TidepoolRuntime`, shared
    /// across all sessions via `Arc`. The dispatcher actor task is spawned
    /// on `tokio_handle` at construction time and aborted on Drop.
    ///
    /// Plugins register ports at boot via `port_registry().register_sync()`;
    /// agents access them via `SessionContext::port_registry()` (cloned Arc).
    port_registry: Arc<PortRegistryImpl>,
}

impl TidepoolRuntime {
    /// Construct with an explicit SDK location and tokio runtime handle.
    ///
    /// `tokio_handle` must be the handle of the tokio runtime that should own
    /// async work spawned by Phase 4+ subsystems. Pass
    /// `tokio::runtime::Handle::current()` from within an async context (e.g.
    /// `#[tokio::main]` or `#[tokio::test]`).
    pub fn new(
        sdk: SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        let port_registry = Arc::new(PortRegistryImpl::new(&tokio_handle));
        Self {
            sdk,
            memory_store,
            provider,
            db,
            tokio_handle,
            port_registry,
        }
    }

    /// Construct using [`SdkLocation::default`] (respects `$PATTERN_SDK_DIR`).
    ///
    /// `tokio_handle` is required — see [`Self::new`] for rationale.
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

    /// The tokio runtime handle this supervisor was constructed with.
    ///
    /// Phase 4's PortRegistry actor uses this to spawn the actor task without
    /// needing `Handle::current()` (which would require a tokio context at the
    /// call site).
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// The runtime-global port registry.
    ///
    /// Plugins (Plan 4) and runtime-provided ports (Phase 5's `HttpPort`)
    /// register at startup via `registry.register_sync()`. Sessions get a
    /// cloned `Arc` so they can reach the registry and dispatcher without
    /// touching the runtime directly.
    pub fn port_registry(&self) -> &Arc<PortRegistryImpl> {
        &self.port_registry
    }
}

impl Drop for TidepoolRuntime {
    fn drop(&mut self) {
        // Best-effort shutdown of the dispatcher actor. `try_send` is
        // non-blocking and safe to call from Drop. Failure is acceptable:
        // if the runtime's tokio runtime is already gone the task will be
        // leaked at process exit, which is fine. If the 256-bound channel
        // is somehow full at runtime drop (extreme corner case during
        // shutdown) we log at debug and move on.
        if let Err(e) = self
            .port_registry
            .dispatcher_tx
            .try_send(crate::port_registry::dispatcher::Op::Shutdown)
        {
            tracing::debug!(
                error = %e,
                "TidepoolRuntime drop: dispatcher shutdown send skipped (actor may already be gone)"
            );
        }
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
        let port_registry = self.port_registry.clone();
        let mut session = tokio::task::spawn_blocking(move || {
            TidepoolSession::open(persona, &sdk, memory_store, provider, db, port_registry)
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
