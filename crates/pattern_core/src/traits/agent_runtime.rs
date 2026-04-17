//! Factory trait for opening and shutting down per-agent sessions.
//!
//! An [`AgentRuntime`] owns the dependencies common to every agent session it
//! spawns: the memory store, the provider client, the endpoint registry, and
//! any router / data-source wiring. A concrete runtime (Phase 3) is typically
//! a long-lived object; sessions are short-lived per-turn executors created
//! via [`AgentRuntime::open_session`].
//!
//! # Forward-compatibility
//!
//! Per v3-foundation §Forward-compatibility, this trait is designed around
//! cosa-like semantics — per-statement observability, cheap session fork,
//! reifiable environment — so a future cosa-native runtime plan can slot in
//! without changing the trait surface.
//!
//! # Session restoration
//!
//! `open_session` accepts an optional [`SessionSnapshot`]. When `Some`, the
//! returned session is restored from the snapshot in a *nondestructive*
//! fashion: the snapshot must not mutate any persistent store (DB, disk,
//! CRDT state that the live session observes). Instead, it seeds the
//! in-memory working state only. This makes snapshot restore safe mid-turn
//! (for checkpoint-and-replay debugging) and safe to use from a forked
//! analysis session without corrupting the live state.
//!
//! When `None`, a fresh session is opened using [`PersonaConfig`] as the
//! starting configuration.

use async_trait::async_trait;

use crate::error::RuntimeError;
use crate::traits::session::Session;
use crate::types::snapshot::{PersonaConfig, SessionSnapshot};

/// Runtime supervisor that spawns per-agent sessions.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use pattern_core::error::RuntimeError;
/// use pattern_core::traits::{AgentRuntime, Session};
/// use pattern_core::types::snapshot::{PersonaConfig, SessionSnapshot};
/// use pattern_core::types::turn::{TurnInput, TurnOutput};
///
/// struct DummySession;
///
/// #[async_trait]
/// impl Session for DummySession {
///     async fn step(&mut self, _i: TurnInput) -> Result<TurnOutput, RuntimeError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn restore(&mut self, _s: SessionSnapshot) -> Result<(), RuntimeError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
///
/// struct Dummy;
///
/// #[async_trait]
/// impl AgentRuntime for Dummy {
///     type Session = DummySession;
///
///     async fn open_session(
///         &self,
///         _persona: PersonaConfig,
///         _snapshot: Option<SessionSnapshot>,
///     ) -> Result<Self::Session, RuntimeError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     async fn shutdown(&self) -> Result<(), RuntimeError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// The session type this runtime produces.
    ///
    /// Using an associated type enables zero-cost dispatch. If Phase 3 needs
    /// heterogeneous sessions behind a trait object, an erased wrapper can
    /// be exposed without changing this trait.
    type Session: Session;

    /// Open a new session for the given persona.
    ///
    /// When `snapshot` is `Some`, the returned session is restored from it
    /// in a nondestructive fashion — the snapshot seeds in-memory working
    /// state only and does not mutate any persistent store. This makes
    /// restoration safe for mid-turn replay and for forked analysis sessions
    /// that must not affect the live state.
    async fn open_session(
        &self,
        persona: PersonaConfig,
        snapshot: Option<SessionSnapshot>,
    ) -> Result<Self::Session, RuntimeError>;

    /// Shut the runtime down, releasing owned resources.
    async fn shutdown(&self) -> Result<(), RuntimeError>;
}
