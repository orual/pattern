//! Daemon server actor.
//!
//! [`DaemonServer`] is a tokio task (actor) that owns the event bus and
//! dispatches incoming [`PatternMessage`]s. It receives messages on a
//! `tokio::sync::mpsc` channel and events on the bridge's unbounded channel,
//! fanning events out to all matching irpc subscriber channels.
//!
//! The server is created via [`DaemonServer::spawn`], which returns a
//! [`DaemonHandle`] containing an [`irpc::Client`] for making requests.
//! In-process tests use `Client::local`; the daemon binary adds a QUIC
//! listener that forwards remote messages into the same channel.
//!
//! ## Echo mode vs real session mode
//!
//! When `echo` is `true` (the default when no runtime config is provided),
//! the server echoes messages back without invoking the LLM. This mode is
//! used by integration tests that run in CI without provider credentials.
//!
//! When real session infrastructure is provided via [`SessionConfig`], the
//! server opens [`TidepoolSession`]s and drives them via
//! [`step_with_agent_loop`](TidepoolSession::step_with_agent_loop).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use irpc::{Client, WithChannels};
use pattern_core::CapabilitySet;
use pattern_core::ProviderClient;
use pattern_core::fronting::{FrontingResolver, ResolveOutcome};
use pattern_core::traits::MemoryStore;
use pattern_core::traits::turn_sink::{DisplayKind, TurnEvent, TurnSink};
use pattern_core::types::ids::{
    AgentId as CoreAgentId, BatchId as CoreBatchId, MessageId, new_id, new_snowflake_id,
};
use pattern_core::types::message::{Message, MessageAttachment, ShellOutputKind};
use pattern_core::types::provider::{ChatMessage, ContentPart};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::{StopReason, TurnInput};
use pattern_runtime::agent_registry::AgentRegistry;
use pattern_runtime::router::RouterRegistry;
use pattern_runtime::router::agent::AgentRouter;
use pattern_runtime::router::cli::CliRouter;
use pattern_runtime::sdk::SdkLocation;
use pattern_runtime::session::{SessionRegistries, TidepoolSession, WakeRegistryExtras};
use serde_json::json;
use smol_str::SmolStr;
use tracing::{info, warn};

use crate::bridge::{EventRx, EventTx, MultiplexSink, TurnSinkBridge, new_event_channel};
use crate::protocol::*;
use pattern_db::queries::get_messages;

/// RAII guard that removes a `batch_id → agent_id` entry from `batch_to_agent`
/// when dropped.
///
/// Held by every spawned session task so that the entry is removed on normal
/// completion, early return, or panic — without relying on `fan_out` to observe
/// a `Stop` event. The `fan_out` cleanup on `Stop` is left as a defensive
/// double-remove; `DashMap::remove` is a no-op when the key is absent.
struct BatchGuard {
    map: Arc<DashMap<BatchId, AgentId>>,
    batch_id: BatchId,
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        self.map.remove(&self.batch_id);
    }
}

/// Configuration for real session mode. When provided to
/// [`DaemonServer::spawn_with_config`], the server opens
/// [`TidepoolSession`]s instead of echoing messages.
///
/// The daemon is persona-agnostic and project-agnostic at startup. Projects
/// are mounted on demand via [`InitSession`](crate::protocol::PatternProtocol::InitSession),
/// and personas are resolved lazily when a session is first opened for a given
/// `agent_id`.
pub struct SessionConfig {
    /// SDK location for the Haskell eval worker.
    pub sdk: SdkLocation,
    /// LLM provider client (e.g. `PatternGatewayClient`).
    pub provider: Arc<dyn ProviderClient>,
    /// Runtime-global port registry. Shared across all sessions opened by
    /// this daemon instance. Plugins register at boot; agents dispatch
    /// through it via `PortHandler`.
    pub port_registry: std::sync::Arc<pattern_runtime::port_registry::PortRegistryImpl>,
}

/// Cached project mount state.
///
/// Wraps the resources needed for sessions within a project. The
/// [`MountedStore`](pattern_memory::mount::MountedStore) is kept alive for
/// RAII (filesystem watcher, backup scheduler).
pub(crate) struct ProjectMount {
    /// The in-memory cache backing the `MemoryStore` trait.
    ///
    /// Stored as `Arc<MemoryCache>` (not `Arc<dyn MemoryStore>`) so the server
    /// can access `block_change_notifier()` for wiring `WakeRegistry` at
    /// session open. The `Arc` coerces to `Arc<dyn MemoryStore>` at call sites
    /// that need the trait object.
    pub cache: Arc<pattern_memory::cache::MemoryCache>,
    /// Constellation database handle (memory.db + messages.db).
    pub db: Arc<pattern_db::ConstellationDb>,
    /// Mount root directory.
    pub mount_path: PathBuf,
    /// Shared `AgentRegistry` for all sessions in this mount. All sessions
    /// within the same project share one registry so they can route messages
    /// to each other via the `agent:` scheme. Created with the mount.
    pub agent_registry: Arc<AgentRegistry>,
    /// Constellation-scoped fronting set. One per mount (a constellation is
    /// a project's set of personas). Loaded from `fronting_set` /
    /// `routing_rules` tables in the mount's `memory.db` at attach time;
    /// mutated through [`Self::update_fronting`] which holds the write lock
    /// across the in-memory mutate + DB save and rolls back on save failure.
    ///
    /// `RoutingTable` deserializes with an empty compiled-regex cache; the
    /// load path calls [`RoutingTable::compile`] to populate it before
    /// publishing the value here.
    ///
    /// Uses `std::sync::RwLock` (sync) rather than `tokio::sync::RwLock`
    /// (async) so the same `Arc` can be shared with `SessionContext.fronting_set`,
    /// which is read by the sync `Pattern.Fronting` handler running on the
    /// eval-worker OS thread (no ambient tokio runtime there).
    pub fronting: Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>>,
    /// Compiled file policy from the mount's `.pattern.kdl` `file_policy {}`
    /// block. Threaded into each session's `FileManager` so `Pattern.File.*`
    /// effects gate correctly.
    ///
    /// **Safe-default contract:** `get_or_mount_project` always populates
    /// this with `Some(policy)` — never `None`. The three populated cases:
    ///
    /// - `Some(rules)` when the block declares at least one allow/deny.
    /// - `Some(empty)` when the block is empty/absent — every File op is
    ///   denied via the policy module's "no matching rule" path. This
    ///   surfaces a clearer error than `None` (which would lie about the
    ///   mount config's existence).
    /// - `Some(empty)` when the block has malformed globs — logged loud
    ///   at error level, then a default-deny FM is wired so File ops
    ///   produce a uniform policy denial instead of a missing-FM error.
    ///
    /// `None` is reserved for callers that build `ProjectMount` outside
    /// the daemon's mount path (test harnesses, future plugin integration).
    pub file_policy: Option<pattern_runtime::file_manager::FilePolicy>,
    /// Shared `PortRegistryImpl` for all sessions in this mount. Built
    /// once at mount time via `PortRegistryImpl::with_runtime_ports` so
    /// `HttpPort` (and any future runtime-provided ports) are always
    /// registered. Threaded through `SessionRegistries` to each session
    /// opened against the mount.
    pub port_registry: Arc<pattern_runtime::port_registry::PortRegistryImpl>,
    /// Constellation persona registry backed by `pattern_db`. Built once at
    /// mount time and shared with every session opened against the mount via
    /// `SessionContext::with_constellation_registry`. The daemon also reaches
    /// for it directly to handle the `PromoteDraft` RPC (Phase 6 T6).
    pub constellation_registry: Arc<dyn pattern_core::ConstellationRegistry>,
    /// Optional human-readable display name for the partner using this mount.
    ///
    /// Loaded from `.pattern.kdl`'s `partner { display-name "..." }` block at
    /// mount time. Surfaced via `SessionInfo.partner_display_name` so TUIs can
    /// render the partner's name. `None` when the block is absent.
    ///
    /// Phase 6 T8.
    pub partner_display_name: Option<String>,
    /// Keeps the `MountedStore` alive for RAII (watcher, backup scheduler).
    _mounted: pattern_memory::mount::MountedStore,
}

impl ProjectMount {
    /// Atomically mutate the fronting set and persist the change. If the
    /// DB save fails, the in-memory state is reverted to its pre-mutation
    /// snapshot before the error is returned, so callers never see a
    /// committed-in-RAM-but-not-on-disk fronting set.
    ///
    /// # Lock release timing
    ///
    /// Phase 1: snapshot + apply mutator under the sync write lock.
    /// The block expression at the end of Phase 1 releases the write guard
    /// before the spawn_blocking await below — sync `RwLockWriteGuard` is
    /// `!Send` and would otherwise prevent the future from being `Send`.
    ///
    /// Returns the new [`FrontingSet`] snapshot on success so the caller
    /// can fan it out as a [`crate::protocol::WireTurnEvent::FrontingChanged`]
    /// without re-reading the lock.
    pub async fn update_fronting<F>(
        &self,
        mutator: F,
    ) -> Result<pattern_core::fronting::FrontingSet, FrontingUpdateError>
    where
        F: FnOnce(
            &mut pattern_core::fronting::FrontingSet,
        ) -> Result<(), pattern_core::fronting::FrontingLoadError>,
    {
        update_fronting_inner(&self.fronting, &self.db, mutator).await
    }
}

/// Three-phase commit for a `FrontingSet` mutation.
///
/// Extracted as a free function so tests can exercise the same code path
/// as production [`ProjectMount::update_fronting`] without constructing a
/// full `ProjectMount` (which requires a live `MountedStore` + RAII
/// watcher / backup scheduler).
///
/// Phase 1: snapshot + apply mutator under sync write lock; revert on
/// mutator rejection.
/// Phase 2: persist on a blocking task (rusqlite is sync).
/// Phase 3: handle the spawn_blocking outcome — revert to pre-mutation
/// snapshot on Join failure or DB error.
pub(crate) async fn update_fronting_inner<F>(
    fronting: &Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>>,
    db: &Arc<pattern_db::ConstellationDb>,
    mutator: F,
) -> Result<pattern_core::fronting::FrontingSet, FrontingUpdateError>
where
    F: FnOnce(
        &mut pattern_core::fronting::FrontingSet,
    ) -> Result<(), pattern_core::fronting::FrontingLoadError>,
{
    // Phase 1: snapshot + apply mutator under the sync write lock.
    // The block expression releases the write guard before the
    // spawn_blocking await below.
    let (snapshot, to_save) = {
        let mut guard = fronting
            .write()
            .map_err(|_| FrontingUpdateError::PoisonedLock)?;
        let snap = guard.clone();
        if let Err(e) = mutator(&mut guard) {
            // Mutator rejected the change (e.g. invalid regex). The
            // mutator's contract is "all-or-nothing" but we can't
            // enforce that, so revert defensively to be safe.
            *guard = snap;
            return Err(FrontingUpdateError::Mutator(e));
        }
        (snap, guard.clone())
    };

    // Phase 2: persist on a blocking task (rusqlite is sync).
    let db_clone = db.clone();
    let join_result =
        tokio::task::spawn_blocking(move || -> Result<(), pattern_db::error::DbError> {
            let mut conn = db_clone.get()?;
            pattern_db::queries::fronting::save_fronting_set(&mut conn, &to_save)
        })
        .await;

    // Phase 3: handle the spawn_blocking outcome. On JoinError or DB
    // error, revert to the pre-mutation snapshot.
    match join_result {
        Err(join_err) => {
            let mut guard = fronting
                .write()
                .map_err(|_| FrontingUpdateError::PoisonedLock)?;
            *guard = snapshot;
            return Err(FrontingUpdateError::Join(join_err));
        }
        Ok(Err(e)) => {
            // DB save failed. Re-take the write lock and revert. Brief
            // window between phase-1 release and phase-3 reacquire where
            // a reader could see the about-to-be-reverted state —
            // acceptable for the rare save-failure path.
            let mut guard = fronting
                .write()
                .map_err(|_| FrontingUpdateError::PoisonedLock)?;
            *guard = snapshot;
            return Err(FrontingUpdateError::Save(e));
        }
        Ok(Ok(())) => {}
    }

    // Read the now-saved state for the caller. Brief read-lock
    // acquire; read poisoning is fatal here too.
    let final_state = fronting
        .read()
        .map_err(|_| FrontingUpdateError::PoisonedLock)?
        .clone();
    Ok(final_state)
}

/// Errors produced by [`ProjectMount::update_fronting`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrontingUpdateError {
    /// The caller-supplied mutator returned an error (e.g. an invalid
    /// regex in a routing rule). In-memory state has been reverted.
    #[error("fronting mutation rejected: {0}")]
    Mutator(#[source] pattern_core::fronting::FrontingLoadError),
    /// The DB save failed. In-memory state has been reverted to match
    /// what's on disk.
    #[error("fronting save failed: {0}")]
    Save(#[source] pattern_db::error::DbError),
    /// The blocking task that ran the save panicked or was cancelled.
    /// In-memory state has been reverted to the pre-mutation snapshot (the
    /// on-disk state is unknown — the blocking task may or may not have
    /// completed its write). Callers that need certainty should call
    /// `GetFronting` after receiving this error to reload from disk.
    #[error("fronting save task failed to join: {0}")]
    Join(#[source] tokio::task::JoinError),
    /// The fronting RwLock was poisoned by a panic in another thread
    /// holding it. Treat as fatal; the daemon should restart.
    #[error("fronting lock poisoned (a thread panicked while holding it)")]
    PoisonedLock,
}

/// A cached agent session: the tidepool session and its multiplexing sink.
///
/// Stored in a shared [`DashMap`] so spawned tasks can look up and insert
/// sessions without going through the actor loop.
///
/// Visibility is `pub(crate)` to match the `DaemonHandle::sessions` field that
/// holds `Arc<DashMap<AgentId, AgentSession>>` in test builds.
#[derive(Clone)]
pub(crate) struct AgentSession {
    session: Arc<TidepoolSession>,
    mux_sink: Arc<MultiplexSink>,
}

/// The daemon server actor.
///
/// Receives [`PatternMessage`]s from clients (local or remote) and events from
/// [`TurnSinkBridge`]s. Fans events out to all subscribers that match the
/// event's `agent_id`.
///
/// Session lifecycle (opening, compilation) happens in spawned tasks using
/// the shared [`DashMap`]-backed caches, so the actor loop never blocks on
/// slow operations like tidepool Haskell compilation.
pub struct DaemonServer {
    recv: tokio::sync::mpsc::Receiver<PatternMessage>,
    event_rx: EventRx,
    event_tx: EventTx,
    /// Active subscribers keyed by `agent_id`. Each entry is a list of irpc
    /// mpsc senders — the server-side half of the streaming RPC. Using a
    /// `HashMap` avoids the O(n) linear scan on every fan-out event.
    subscribers: HashMap<AgentId, Vec<irpc::channel::mpsc::Sender<TaggedTurnEvent>>>,
    /// Mount-scoped subscribers keyed by canonical mount path.
    ///
    /// Phase 6 T8: the TUI subscribes via `SubscribeAll` to receive every
    /// agent's events for a mount plus daemon-level events
    /// (`FrontingChanged`, `ConstellationChanged`). Coexists with the
    /// per-agent `subscribers` map; `fan_out` routes to both.
    mount_subscribers: HashMap<PathBuf, Vec<irpc::channel::mpsc::Sender<TaggedTurnEvent>>>,
    /// Maps `agent_id` → canonical mount path for events that arrive
    /// without an explicit `mount_path` tag (the per-agent emit path
    /// in `TurnSinkBridge` leaves it `None`). Populated on session open.
    /// Shared with spawned tasks so `get_or_open_session` can register
    /// the mapping without a round-trip to the actor.
    ///
    /// Phase 6 T8.
    agent_to_mount: Arc<DashMap<AgentId, PathBuf>>,
    started_at: Instant,
    /// When true, messages are echoed back without invoking the LLM.
    echo: bool,
    /// Session infrastructure for real mode. `None` in echo mode.
    session_config: Option<Arc<SessionConfig>>,
    /// Cached project mounts keyed by canonical project path.
    /// Each mount owns a `MountedStore` (memory cache, DB, watcher).
    project_mounts: Arc<DashMap<PathBuf, Arc<ProjectMount>>>,
    /// The currently active project mount. Set by `InitSession`, used by
    /// `SendMessage` for session creation. One project at a time for now;
    /// multi-project support can be added later by keying sessions on
    /// `(project_path, agent_id)`.
    current_mount: Option<Arc<ProjectMount>>,
    /// Open sessions keyed by agent ID, shared with spawned tasks.
    /// Each session uses a [`MultiplexSink`] whose inner sink is swapped to
    /// a per-batch [`TurnSinkBridge`] before each `step_with_agent_loop` call.
    sessions: Arc<DashMap<AgentId, AgentSession>>,
    /// Per-agent mutex that serializes session opening and the
    /// `set_inner` + `step` sequence. Shared with spawned tasks.
    ///
    /// Without this lock, two concurrent `SendMessage` calls for the same
    /// agent could race: the first call's `set_inner` might be overwritten by
    /// the second before the first task begins executing, causing that step's
    /// events to be tagged with the wrong `batch_id`.
    session_locks: Arc<DashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>,
    /// Stable partner identity for this daemon session.
    ///
    /// Minted once at spawn time and returned in every [`SessionInfo`] response
    /// so that TUI clients have a consistent `user_id` for constructing
    /// `Author::Partner` origins. Daemon-level identity, not per-session.
    /// The SendMessage handler no longer uses this directly — clients carry it
    /// from InitSession and embed it in every `AgentMessage::origin`.
    partner_id: SmolStr,
    /// Number of available personas discovered during the last InitSession.
    /// Updated each time InitSession is called, used by GetStatus to report
    /// agent count to the TUI.
    available_agents: usize,
    /// Maps in-flight batch IDs to their agent ID so that `CancelBatch` can
    /// locate the correct session. Entries are inserted on `SendMessage` and
    /// removed when a `Stop` event arrives for the batch.
    batch_to_agent: Arc<DashMap<BatchId, AgentId>>,
    /// Test-only: when `Some`, `get_or_mount_project` uses this registry
    /// instead of building one from the DB. Lets tests inject a mock that
    /// fails on specific methods (e.g. `set_status`) without requiring
    /// database-level surgery. Gated behind `#[cfg(test)]` so it is
    /// zero-cost in production builds.
    #[cfg(test)]
    constellation_registry_override: Option<Arc<dyn pattern_core::ConstellationRegistry>>,
}

/// Handle returned by [`DaemonServer::spawn`].
///
/// Holds an irpc [`Client`] that can make requests to the running actor.
/// For the daemon binary, this client's local sender is also used to set up
/// the QUIC listener (via `as_local()`).
pub struct DaemonHandle {
    /// The irpc client for making requests to the daemon actor.
    pub client: Client<PatternProtocol>,
    /// Test-only reference to the server's `batch_to_agent` map, so tests can
    /// verify that entries are retired on batch completion without needing a
    /// public accessor on the production server. Kept behind `cfg(test)` so
    /// it cannot leak into normal use.
    #[cfg(test)]
    pub(crate) batch_to_agent: Arc<DashMap<BatchId, AgentId>>,
    /// Test-only reference to the server's open sessions map, so tests can
    /// verify session lifecycle (e.g. that failed PromoteDraft step-6 removes
    /// the entry). Kept `pub(crate)` and `cfg(test)` — only unit tests in
    /// this crate's `#[cfg(test)]` module access it.
    #[cfg(test)]
    pub(crate) sessions: Arc<DashMap<AgentId, AgentSession>>,
    /// Test-only reference to the server's agent-to-mount mapping, so tests
    /// can verify cleanup on PromoteDraft step-6 failure.
    #[cfg(test)]
    pub(crate) agent_to_mount: Arc<DashMap<AgentId, PathBuf>>,
}

impl DaemonServer {
    /// Spawn the daemon server actor in echo mode.
    ///
    /// Messages are echoed back without invoking the LLM. Used by tests
    /// and when the `--echo` flag is passed to the daemon binary.
    pub fn spawn() -> DaemonHandle {
        Self::spawn_inner(true, None)
    }

    /// Spawn the daemon server actor with real session infrastructure.
    ///
    /// The server will open [`TidepoolSession`]s and drive them via
    /// `step_with_agent_loop` when messages arrive.
    pub fn spawn_with_config(config: SessionConfig) -> DaemonHandle {
        Self::spawn_inner(false, Some(Arc::new(config)))
    }

    /// Test-only: spawn with real session infrastructure AND a registry
    /// override. `get_or_mount_project` will use `registry` instead of
    /// building one from the DB, allowing tests to inject a mock that
    /// fails on specific methods (e.g. `set_status` for step-6 testing).
    ///
    /// The override registry is used for all mounts created by this daemon
    /// instance. It replaces the normal `ConstellationRegistryDb` + wrapping
    /// `EventEmittingRegistry` layer, so tests own the full registry logic.
    ///
    /// Marked `pub` (not `pub(crate)`) so integration tests in `tests/` can
    /// call it. The `cfg(test)` gate ensures it never appears in production.
    #[cfg(test)]
    pub fn spawn_with_config_and_registry(
        config: SessionConfig,
        registry: Arc<dyn pattern_core::ConstellationRegistry>,
    ) -> DaemonHandle {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, event_rx) = new_event_channel();
        let batch_to_agent = Arc::new(DashMap::new());
        let sessions: Arc<DashMap<AgentId, AgentSession>> = Arc::new(DashMap::new());
        let agent_to_mount: Arc<DashMap<AgentId, PathBuf>> = Arc::new(DashMap::new());
        let mut server = Self {
            recv: msg_rx,
            event_rx,
            event_tx,
            subscribers: HashMap::new(),
            mount_subscribers: HashMap::new(),
            agent_to_mount: agent_to_mount.clone(),
            started_at: Instant::now(),
            echo: false,
            session_config: Some(Arc::new(config)),
            project_mounts: Arc::new(DashMap::new()),
            current_mount: None,
            sessions: sessions.clone(),
            session_locks: Arc::new(DashMap::new()),
            partner_id: new_id(),
            available_agents: 0,
            batch_to_agent: batch_to_agent.clone(),
            constellation_registry_override: None,
        };
        server.constellation_registry_override = Some(registry);
        tokio::spawn(server.run());
        DaemonHandle {
            client: Client::local(msg_tx),
            batch_to_agent,
            sessions,
            agent_to_mount,
        }
    }

    /// Internal spawn helper.
    fn spawn_inner(echo: bool, session_config: Option<Arc<SessionConfig>>) -> DaemonHandle {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, event_rx) = new_event_channel();
        let batch_to_agent = Arc::new(DashMap::new());
        let sessions: Arc<DashMap<AgentId, AgentSession>> = Arc::new(DashMap::new());
        let agent_to_mount: Arc<DashMap<AgentId, PathBuf>> = Arc::new(DashMap::new());
        let server = Self {
            recv: msg_rx,
            event_rx,
            event_tx,
            subscribers: HashMap::new(),
            mount_subscribers: HashMap::new(),
            agent_to_mount: agent_to_mount.clone(),
            started_at: Instant::now(),
            echo,
            session_config,
            project_mounts: Arc::new(DashMap::new()),
            current_mount: None,
            sessions: sessions.clone(),
            session_locks: Arc::new(DashMap::new()),
            partner_id: new_id(),
            available_agents: 0,
            batch_to_agent: batch_to_agent.clone(),
            // Production builds always use None; test builds may override
            // via `spawn_with_config_and_registry`.
            #[cfg(test)]
            constellation_registry_override: None,
        };
        tokio::spawn(server.run());
        DaemonHandle {
            client: Client::local(msg_tx),
            #[cfg(test)]
            batch_to_agent,
            #[cfg(test)]
            sessions,
            #[cfg(test)]
            agent_to_mount,
        }
    }

    /// Actor main loop. Alternates between receiving messages and events.
    async fn run(mut self) {
        loop {
            tokio::select! {
                msg = self.recv.recv() => {
                    match msg {
                        Some(msg) => self.handle(msg).await,
                        None => break, // All senders dropped — shut down.
                    }
                }
                event = self.event_rx.recv() => {
                    if let Some(event) = event {
                        self.fan_out(event).await;
                    }
                }
            }
        }
    }

    /// Fan out a tagged event to per-agent and per-mount subscribers.
    ///
    /// Routing rules:
    /// - Per-agent subscribers (`SubscribeOutput`) keyed on `event.agent_id`.
    /// - Per-mount subscribers (`SubscribeAll`) keyed on the event's mount.
    ///   The mount is taken from `event.mount_path` if set, otherwise
    ///   resolved by looking up `event.agent_id` in `agent_to_mount`.
    ///
    /// Uses `try_send` so a slow or full subscriber does not block the
    /// actor loop. Full / disconnected subscribers are removed.
    async fn fan_out(&mut self, event: TaggedTurnEvent) {
        tracing::trace!(
            agent_id = %event.agent_id,
            batch_id = %event.batch_id,
            event = ?event.event,
            "fan_out: dispatching event"
        );

        // Retire the `batch_id -> agent_id` mapping once the batch completes
        // so `batch_to_agent` does not grow unboundedly over long-running
        // sessions. CancelBatch removes entries on cancel; this handles the
        // normal-completion path.
        if matches!(event.event, WireTurnEvent::Stop(_)) {
            self.batch_to_agent.remove(&event.batch_id);
        }

        // Resolve the mount path once (used for mount-scoped fan-out below).
        // Per-agent emitters leave mount_path None — look it up in
        // agent_to_mount. Daemon-level emitters set it explicitly.
        let mount_key: Option<PathBuf> =
            event.mount_path.as_deref().map(PathBuf::from).or_else(|| {
                self.agent_to_mount
                    .get(&event.agent_id)
                    .map(|p| p.value().clone())
            });

        // Per-agent subscribers.
        if let Some(senders) = self.subscribers.get_mut(&event.agent_id) {
            Self::deliver_to(senders, &event).await;
        }

        // Per-mount subscribers.
        if let Some(key) = mount_key
            && let Some(senders) = self.mount_subscribers.get_mut(&key)
        {
            Self::deliver_to(senders, &event).await;
        }
    }

    /// Deliver `event` to every sender in `senders`, removing slow or
    /// disconnected subscribers in place.
    async fn deliver_to(
        senders: &mut Vec<irpc::channel::mpsc::Sender<TaggedTurnEvent>>,
        event: &TaggedTurnEvent,
    ) {
        let mut i = 0;
        while i < senders.len() {
            let tx = &senders[i];
            match tx.try_send(event.clone()).await {
                Ok(true) => {
                    i += 1;
                }
                Ok(false) => {
                    warn!(
                        agent_id = %event.agent_id,
                        "subscriber buffer full, removing slow subscriber"
                    );
                    senders.swap_remove(i);
                }
                Err(_) => {
                    warn!(
                        agent_id = %event.agent_id,
                        "subscriber disconnected, removing"
                    );
                    senders.swap_remove(i);
                }
            }
        }
    }

    /// Dispatch a single incoming message.
    async fn handle(&mut self, msg: PatternMessage) {
        match msg {
            PatternMessage::SendMessage(req) => {
                let WithChannels { tx, inner, .. } = req;
                let batch_id = inner.batch_id.clone();

                // Echo mode uses a synthetic agent_id for fan-out keying.
                // Real mode resolves the agent_id from the recipient below.
                if self.echo {
                    let agent_id: AgentId = match &inner.recipient {
                        Recipient::Direct(id) => id.clone(),
                        Recipient::Address(id) => SmolStr::from(id.trim_start_matches('@')),
                        Recipient::Auto => "echo-auto".into(),
                    };

                    // Track batch → agent so CancelBatch can find the right session.
                    self.batch_to_agent
                        .insert(batch_id.clone(), agent_id.clone());

                    // Acknowledge receipt — the client unblocks immediately.
                    let _ = tx.send(()).await;

                    // Echo mode: extract text from parts, emit "echo: {text}" + Stop.
                    // This is instant so it stays inline in the actor loop.
                    let bridge = TurnSinkBridge::new(batch_id, agent_id, self.event_tx.clone());
                    let text = inner
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text(s) => Some(s.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    bridge.emit(TurnEvent::Text(format!("echo: {text}")));
                    bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                    return;
                }

                let Some(mount) = self.current_mount.clone() else {
                    // No mount available — send InitSession first.
                    let _ = tx.send(()).await;
                    let bridge =
                        TurnSinkBridge::new(batch_id, "no-mount".into(), self.event_tx.clone());
                    bridge.emit(TurnEvent::Display {
                        kind: DisplayKind::Note,
                        text: "no project mounted — send InitSession first".into(),
                    });
                    bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                    return;
                };

                // Pre-resolve the target agent_id from the Recipient directive.
                // Done here (synchronously, before ack) so the batch_to_agent
                // entry carries the correct resolved id from the start.
                //
                // Recipient::Auto routes through the FrontingResolver, taking a
                // snapshot of the current FrontingSet (AC8.8: the snapshot is
                // taken at dispatch time; later mutations don't re-route this
                // message).
                let resolved_agent_id: AgentId = match &inner.recipient {
                    Recipient::Direct(id) => id.clone(),
                    Recipient::Address(persona_id) => {
                        SmolStr::from(persona_id.trim_start_matches('@'))
                    }
                    Recipient::Auto => {
                        // Snapshot the fronting set under a short-lived read lock.
                        // `snapshot_fronting_set` takes the lock, clones the set, and
                        // returns, ensuring the RwLockReadGuard (!Send) is fully
                        // dropped before we hit any await point below.
                        let set_snapshot_opt = snapshot_fronting_set(&mount.fronting);
                        let set_snapshot = match set_snapshot_opt {
                            Some(s) => s,
                            None => {
                                // Lock poisoned — emit a user-visible note.
                                let _ = tx.send(()).await;
                                let bridge = TurnSinkBridge::new(
                                    batch_id,
                                    "no-mount".into(),
                                    self.event_tx.clone(),
                                );
                                bridge.emit(TurnEvent::Display {
                                    kind: DisplayKind::Note,
                                    text: "fronting lock poisoned; cannot route message".into(),
                                });
                                bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                                return;
                            }
                        };
                        let body_text = inner
                            .parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text(s) => Some(s.as_str()),
                                _ => None,
                            })
                            .next()
                            .unwrap_or("");
                        // Use the mount's real constellation registry so
                        // Recipient::Auto can fall back to Active personas when
                        // the fronting set is empty (e.g. on first use before
                        // explicit fronting is configured).
                        let resolver = FrontingResolver::new(
                            set_snapshot,
                            mount.constellation_registry.clone(),
                        );
                        let outcome = resolver.resolve(body_text).await;
                        match outcome {
                            ResolveOutcome::Direct(id)
                            | ResolveOutcome::Rule { target: id, .. }
                            | ResolveOutcome::Fallback(id)
                            | ResolveOutcome::DefaultPersona(id) => id,
                            ResolveOutcome::FanOut(ids) => {
                                // Co-fronting fan-out: for TUI input pick the first
                                // (lexicographically lowest after sort). The full
                                // FanOut semantics for SDK messages are handled by
                                // dispatch_to_mailboxes; this path is human input.
                                ids.into_iter().next().expect("FanOut never empty")
                            }
                            ResolveOutcome::SystemDefault => {
                                // No fronting configured. Acknowledge the message but
                                // emit a user-visible note so the partner knows it
                                // wasn't silently dropped.
                                let _ = tx.send(()).await;
                                let bridge = TurnSinkBridge::new(
                                    batch_id,
                                    "daemon".into(),
                                    self.event_tx.clone(),
                                );
                                bridge.emit(TurnEvent::Display {
                                    kind: DisplayKind::Note,
                                    text: "no fronting configured — message was not routed; \
                                           use SetFronting or configure a persona to receive messages"
                                        .into(),
                                });
                                bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                                return;
                            }
                        }
                    }
                };

                // Track batch → agent so CancelBatch can find the right session.
                self.batch_to_agent
                    .insert(batch_id.clone(), resolved_agent_id.clone());

                // Acknowledge receipt — the client unblocks immediately.
                let _ = tx.send(()).await;

                // Real session mode: spawn a task to handle session open
                // and step. The actor loop stays responsive — session open
                // may trigger tidepool Haskell compilation (5-10s).
                let sessions = self.sessions.clone();
                let session_locks = self.session_locks.clone();
                let config = self.session_config.clone().unwrap();
                let event_tx = self.event_tx.clone();
                let agent_to_mount = self.agent_to_mount.clone();
                let agent_id = resolved_agent_id;

                tokio::spawn(async move {
                    // 1. Get or open session (may block during compilation).
                    let agent_session = match get_or_open_session(
                        &agent_id,
                        &sessions,
                        &session_locks,
                        &config,
                        &mount,
                        &event_tx,
                        &agent_to_mount,
                    )
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(agent_id = %agent_id, error = %e, "failed to open session");
                            let bridge = TurnSinkBridge::new(batch_id, agent_id, event_tx);
                            bridge.emit(TurnEvent::Display {
                                kind: DisplayKind::Note,
                                text: format!("error: {e}"),
                            });
                            bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                            return;
                        }
                    };

                    // 2. Set up event bridge on the mux sink.
                    let bridge = Arc::new(TurnSinkBridge::new(
                        batch_id.clone(),
                        agent_id.clone(),
                        event_tx,
                    ));
                    agent_session.mux_sink.set_inner(bridge.clone());

                    // 3. Build message and deliver to mailbox.
                    let session_agent_id = agent_session.session.agent_id().to_string();
                    let turn_input = build_turn_input(&inner, &session_agent_id);
                    let mailbox_input = pattern_runtime::mailbox::MailboxInput::new(
                        turn_input.origin,
                        turn_input.messages.into_iter().next().unwrap(),
                    );

                    if let Err(e) = agent_session
                        .session
                        .context()
                        .mailbox()
                        .send_input(mailbox_input)
                    {
                        warn!(
                            agent_id = %agent_id,
                            batch_id = %batch_id,
                            error = %e,
                            "failed to enqueue message in mailbox"
                        );
                        bridge.emit(TurnEvent::Display {
                            kind: DisplayKind::Note,
                            text: format!("error: {e}"),
                        });
                        bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                    }
                });
            }
            PatternMessage::SubscribeOutput(req) => {
                let WithChannels { tx, inner, .. } = req;
                // Register this subscriber. The actor's `fan_out()` method
                // will forward matching events to this irpc mpsc sender.
                self.subscribers.entry(inner.agent_id).or_default().push(tx);
            }
            PatternMessage::SubscribeAll(req) => {
                let WithChannels { tx, inner, .. } = req;
                // Resolve to the same mount-path the daemon uses internally.
                // Subscribers may pass any path inside the project (e.g. the
                // project root or the mount itself); we walk up via the same
                // `find_mount` the InitSession path uses so the key matches
                // what `EventEmittingRegistry` and `DaemonFrontingCommitter`
                // emit on.
                let canonical = inner
                    .mount_path
                    .canonicalize()
                    .unwrap_or_else(|_| inner.mount_path.clone());
                let key = pattern_memory::mount::find_mount(&canonical).unwrap_or(canonical);
                self.mount_subscribers.entry(key).or_default().push(tx);
            }
            PatternMessage::ListAgents(req) => {
                let WithChannels { tx, .. } = req;
                let agents: Vec<AgentInfo> = self
                    .sessions
                    .iter()
                    .map(|entry| AgentInfo {
                        agent_id: entry.key().clone(),
                        persona_name: String::new(), // Populated when multi-agent lands.
                        active_batches: vec![],
                    })
                    .collect();
                let _ = tx.send(agents).await;
            }
            PatternMessage::ListCommands(req) => {
                let WithChannels { tx, .. } = req;
                // The daemon's command registry starts empty. Plugin commands
                // will be registered here when the plugin system lands.
                // Built-in TUI commands are handled client-side and are not
                // included in this response.
                let _ = tx.send(vec![]).await;
            }
            PatternMessage::GetStatus(req) => {
                let WithChannels { tx, .. } = req;
                let status = RuntimeStatus {
                    agent_count: self.available_agents,
                    active_batch_count: 0,
                    uptime_secs: self.started_at.elapsed().as_secs(),
                };
                let _ = tx.send(status).await;
            }
            PatternMessage::GetHistory(req) => {
                use crate::protocol::{HistoricalBatch, HistoryResponse};
                let WithChannels { tx, inner, .. } = req;

                // Move the blocking DB read + deserialization off the actor loop
                // into a spawn_blocking task so other messages are not delayed
                // while we wait for SQLite I/O.
                let db = self.current_mount.as_ref().map(|m| m.db.clone());
                let agent_id = inner.agent_id.clone();

                let agent_id_for_batches = agent_id.clone();
                tokio::spawn(async move {
                    let batches = tokio::task::spawn_blocking(move || -> Vec<HistoricalBatch> {
                        let Some(db) = db else {
                            return vec![];
                        };
                        let conn = match db.dedicated_connection() {
                            Ok(c) => c,
                            Err(_) => return vec![],
                        };

                        // Fetch messages in DESC order, reverse to get chronological (ASC) order.
                        let mut messages =
                            get_messages(&conn, &agent_id, i64::MAX).unwrap_or_default();
                        messages.reverse();

                        // Group by batch_id and reconstruct events.
                        let mut batch_map: std::collections::HashMap<String, Vec<_>> =
                            std::collections::HashMap::new();
                        for msg in messages {
                            if let Some(batch_id) = &msg.batch_id {
                                batch_map.entry(batch_id.clone()).or_default().push(msg);
                            }
                        }

                        // Convert each batch to HistoricalBatch with WireTurnEvents.
                        let mut batches: Vec<HistoricalBatch> = batch_map
                            .into_iter()
                            .map(|(batch_id, mut msgs)| {
                                use pattern_db::models::MessageRole;
                                msgs.sort_by_key(|m| m.sequence_in_batch.unwrap_or(0));

                                // Extract user message from the first User role message.
                                // Deserialize the full ChatMessage to get complete text content.
                                let user_message = msgs
                                    .iter()
                                    .filter(|m| m.role == MessageRole::User)
                                    .filter_map(|m| {
                                        serde_json::from_value::<ChatMessage>(
                                            m.content_json.0.clone(),
                                        )
                                        .ok()
                                    })
                                    .map(|cm| {
                                        cm.content
                                            .parts()
                                            .iter()
                                            .filter_map(|p| p.as_text())
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                    })
                                    .next();

                                let events: Vec<WireTurnEvent> =
                                    msgs.into_iter().flat_map(message_to_wire_events).collect();
                                tracing::trace!("{:?}", events);

                                let tokens = estimate_batch_tokens(&user_message, &events);

                                HistoricalBatch {
                                    batch_id: batch_id.into(),
                                    agent_id: agent_id_for_batches.clone(),
                                    user_message,
                                    events,
                                    tokens,
                                }
                            })
                            .collect();

                        // Sort batches by batch_id (snowflakes sort chronologically).
                        batches.sort_by(|a, b| a.batch_id.cmp(&b.batch_id));
                        batches
                    })
                    .await
                    .unwrap_or_default();
                    let response = HistoryResponse { batches };
                    let result = tx.send(response).await; // The actual history needs to go back to the agent
                    if result.is_err() {
                        tracing::error!("{:?}", result);
                    }
                });
            }
            PatternMessage::CancelBatch(req) => {
                let WithChannels { tx, inner, .. } = req;
                let batch_id = inner;
                // Look up which agent owns this batch, then signal its CancelState.
                if let Some(agent_id) = self.batch_to_agent.get(&batch_id) {
                    if let Some(session) = self.sessions.get::<SmolStr>(&agent_id) {
                        session.session.cancel_state().request_cancel();
                        tracing::info!(
                            batch_id = %batch_id,
                            agent_id = %*agent_id,
                            "cancel requested for in-flight batch"
                        );
                    }
                } else {
                    tracing::debug!(batch_id = %batch_id, "cancel_batch: no active batch found");
                }
                // Remove the mapping — the batch is no longer in flight.
                self.batch_to_agent.remove(&batch_id);
                let _ = tx.send(()).await;
            }
            PatternMessage::RunCommand(req) => {
                // RunCommand is the transport for plugin-namespaced slash commands
                // (e.g. `/plugin-name:do-thing`). Built-in commands route through
                // dedicated RPCs (ListAgents, GetStatus, Shutdown, ...) rather than
                // here. The plugin system itself is future work; for now every
                // command returns a "not implemented" error.
                let WithChannels { tx, inner, .. } = req;
                let result = CommandResult {
                    success: false,
                    output: format!("plugin command not yet implemented: {}", inner.command),
                };
                let _ = tx.send(result).await;
            }
            PatternMessage::Shutdown(req) => {
                let WithChannels { tx, .. } = req;
                // Respond before exiting so the client's await resolves cleanly.
                // A brief sleep gives the response time to flush over the wire
                // before the process exits.
                let _ = tx.send(crate::protocol::ShutdownResponse).await;
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    std::process::exit(0);
                });
            }
            PatternMessage::GetFronting(req) => {
                let WithChannels { tx, .. } = req;
                // Build the response under a synchronous read lock, then drop
                // the guard before the async `tx.send(...).await` so the guard
                // (which is not `Send`) is never held across an await point.
                let response = if let Some(mount) = &self.current_mount {
                    match mount.fronting.read() {
                        Ok(guard) => {
                            let rules = guard
                                .routing
                                .rules
                                .iter()
                                .map(|r| {
                                    let (pt, pv) = wire_pattern(&r.pattern);
                                    WireRoutingRule {
                                        id: r.id.clone(),
                                        pattern_type: pt.to_string(),
                                        pattern_value: pv,
                                        target: r.target.to_string(),
                                        priority: r.priority,
                                    }
                                })
                                .collect();
                            FrontingGetResponse {
                                set: WireFrontingSet {
                                    active: guard.active.iter().map(|id| id.to_string()).collect(),
                                    fallback: guard.fallback.as_ref().map(|id| id.to_string()),
                                    rules,
                                },
                            }
                            // `guard` drops here — lock released before await.
                        }
                        Err(_) => {
                            warn!("fronting lock poisoned; returning empty set");
                            FrontingGetResponse {
                                set: WireFrontingSet {
                                    active: vec![],
                                    fallback: None,
                                    rules: vec![],
                                },
                            }
                        }
                    }
                } else {
                    FrontingGetResponse {
                        set: WireFrontingSet {
                            active: vec![],
                            fallback: None,
                            rules: vec![],
                        },
                    }
                };
                let _ = tx.send(response).await;
            }
            PatternMessage::SetFronting(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = if let Some(mount) = self.current_mount.clone() {
                    let mount_path_str = mount.mount_path.to_string_lossy().into_owned();
                    let active_ids = inner.active.clone();
                    let fallback_id = inner.fallback.clone();
                    let result = mount
                        .update_fronting(|set| {
                            set.active = active_ids
                                .into_iter()
                                .map(|s| pattern_core::types::ids::PersonaId::new(s.as_str()))
                                .collect();
                            set.fallback = fallback_id
                                .map(|s| pattern_core::types::ids::PersonaId::new(s.as_str()));
                            Ok(())
                        })
                        .await;
                    match result {
                        Ok(new_set) => {
                            self.fan_out_fronting_changed(&new_set, Some(mount_path_str))
                                .await;
                            FrontingSetResponse {
                                success: true,
                                error: None,
                            }
                        }
                        Err(e) => FrontingSetResponse {
                            success: false,
                            error: Some(e.to_string()),
                        },
                    }
                } else {
                    FrontingSetResponse {
                        success: false,
                        error: Some("no project mounted — send InitSession first".to_string()),
                    }
                };
                let _ = tx.send(response).await;
            }
            PatternMessage::UpdateRouting(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = if let Some(mount) = self.current_mount.clone() {
                    let mount_path_str = mount.mount_path.to_string_lossy().into_owned();
                    let wire_rules = inner.rules.clone();
                    let result = mount
                        .update_fronting(|set| {
                            let domain_rules: Vec<pattern_core::fronting::RoutingRule> =
                                wire_rules.into_iter().map(wire_rule_to_domain).collect();
                            // `try_from_rules` returns `FrontingLoadError` on
                            // invalid regex — propagate directly with `?`.
                            let table =
                                pattern_core::fronting::RoutingTable::try_from_rules(domain_rules)?;
                            set.routing = table;
                            Ok(())
                        })
                        .await;
                    match result {
                        Ok(new_set) => {
                            self.fan_out_fronting_changed(&new_set, Some(mount_path_str))
                                .await;
                            UpdateRoutingResponse {
                                success: true,
                                error: None,
                            }
                        }
                        Err(e) => UpdateRoutingResponse {
                            success: false,
                            error: Some(e.to_string()),
                        },
                    }
                } else {
                    UpdateRoutingResponse {
                        success: false,
                        error: Some("no project mounted — send InitSession first".to_string()),
                    }
                };
                let _ = tx.send(response).await;
            }
            PatternMessage::PromoteDraft(req) => {
                let WithChannels { tx, inner, .. } = req;
                let mut warning: Option<String> = None;
                let response = match self.handle_promote_draft(inner, &mut warning).await {
                    Ok(()) => PromoteDraftResponse {
                        success: true,
                        error: None,
                        warning,
                    },
                    Err(e) => PromoteDraftResponse {
                        success: false,
                        error: Some(e),
                        warning,
                    },
                };
                let _ = tx.send(response).await;
            }
            PatternMessage::ListPersonas(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = self.handle_list_personas(inner).await;
                let _ = tx.send(response).await;
            }
            PatternMessage::AddRelationship(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = match self.handle_add_relationship(inner).await {
                    Ok(()) => AddRelationshipResponse {
                        success: true,
                        error: None,
                    },
                    Err(e) => AddRelationshipResponse {
                        success: false,
                        error: Some(e),
                    },
                };
                let _ = tx.send(response).await;
            }
            PatternMessage::ListGroups(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = self.handle_list_groups(inner).await;
                let _ = tx.send(response).await;
            }
            PatternMessage::CreateGroup(req) => {
                let WithChannels { tx, inner, .. } = req;
                let response = self.handle_create_group(inner).await;
                let _ = tx.send(response).await;
            }
            PatternMessage::GetClientCount(req) => {
                let WithChannels { tx, .. } = req;
                // Dead senders are only lazily pruned during fan_out. Since
                // fan_out only runs when events arrive, the count can be stale
                // after a subscriber disconnects but before the next event.
                // Probe each sender's closed() future to prune proactively so
                // that --stop-daemon-on-exit (AC6.7) sees the true count.
                for senders in self.subscribers.values_mut() {
                    let mut alive = Vec::with_capacity(senders.len());
                    for tx in senders.drain(..) {
                        // closed() resolves when the receiver is dropped (including
                        // remote disconnects via QUIC). A zero-duration timeout
                        // lets us probe without blocking the actor loop.
                        let is_closed = tokio::time::timeout(Duration::from_millis(0), tx.closed())
                            .await
                            .is_ok();
                        if !is_closed {
                            alive.push(tx);
                        }
                    }
                    *senders = alive;
                }
                // Remove agent entries that have no live subscribers remaining.
                self.subscribers.retain(|_, senders| !senders.is_empty());

                let count: usize = self.subscribers.values().map(|s| s.len()).sum();
                let _ = tx.send(count).await;
            }
            PatternMessage::InitSession(req) => {
                let WithChannels { tx, inner, .. } = req;

                if self.echo {
                    // Echo mode: still mount the project so that registry-level
                    // RPCs (ListPersonas, AddRelationship, PromoteDraft, etc.)
                    // can access the DB. The session response is synthetic — no
                    // real LLM session is opened.
                    if let Ok(mount) = self.get_or_mount_project(&inner.project_path) {
                        self.current_mount = Some(mount);
                    }
                    let _ = tx
                        .send(SessionInfo {
                            agent_id: inner.default_agent,
                            persona_name: "echo".into(),
                            available_agents: vec![],
                            agent_aliases: vec![],
                            partner_id: self.partner_id.clone(),
                            // Phase 6: read from .pattern.kdl partner { display_name "..." }
                            partner_display_name: None,
                            fronting_snapshot: None,
                            error: None,
                        })
                        .await;
                    return;
                }

                // Mount or reuse the project.
                let mount = match self.get_or_mount_project(&inner.project_path) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(path = %inner.project_path.display(), error = %e, "failed to mount project");
                        let _ = tx
                            .send(SessionInfo {
                                agent_id: inner.default_agent,
                                persona_name: String::new(),
                                available_agents: vec![],
                                agent_aliases: vec![],
                                partner_id: self.partner_id.clone(),
                                // Phase 6: read from .pattern.kdl partner { display_name "..." }
                                partner_display_name: None,
                                fronting_snapshot: None,
                                error: Some(format!(
                                    "failed to mount project at {}: {e}",
                                    inner.project_path.display()
                                )),
                            })
                            .await;
                        return;
                    }
                };

                // Store as the current mount for subsequent SendMessage calls.
                self.current_mount = Some(mount.clone());

                // Discover personas from global + project scopes.
                let paths = pattern_memory::PatternPaths::default_paths();
                let personas = paths
                    .ok()
                    .and_then(|p| {
                        pattern_memory::persona::discover_personas(&p, Some(&mount.mount_path)).ok()
                    })
                    .unwrap_or_default();

                // Resolve the requested agent. The user may have addressed
                // the agent by alias (persona `name` field); resolve to the
                // canonical id and return that to the client so subsequent
                // RPCs can use the canonical form.
                let requested = inner.default_agent.clone();
                let normalized = requested.trim_start_matches('@');
                let canonical = personas
                    .resolve(normalized)
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| normalized.to_owned());
                let agent_id: AgentId = SmolStr::from(canonical.as_str());

                let persona_name = personas
                    .path_for(normalized)
                    .and_then(|p| pattern_runtime::persona_loader::load_persona(p).ok())
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| agent_id.to_string());

                let available: Vec<AgentId> =
                    personas.canonical_ids().map(|k| SmolStr::from(k)).collect();

                let agent_aliases: Vec<crate::protocol::AgentAlias> = personas
                    .iter_aliases()
                    .map(|(alias, canonical)| crate::protocol::AgentAlias {
                        alias: SmolStr::from(alias),
                        canonical_id: SmolStr::from(canonical),
                    })
                    .collect();

                // Update available agents count for GetStatus.
                self.available_agents = available.len();

                info!(
                    agent_id = %agent_id,
                    persona = %persona_name,
                    project = %inner.project_path.display(),
                    agents = ?available,
                    "session initialized"
                );

                // Snapshot the per-mount fronting set for the TUI's initial
                // status bar + constellation panel render. Failure to snapshot
                // (poisoned lock) yields None — the TUI falls back to its
                // empty state.
                let fronting_snapshot = mount
                    .fronting
                    .read()
                    .ok()
                    .map(|set| build_fronting_snapshot(&set));

                let _ = tx
                    .send(SessionInfo {
                        agent_id,
                        persona_name,
                        available_agents: available,
                        agent_aliases,
                        partner_id: self.partner_id.clone(),
                        partner_display_name: mount.partner_display_name.clone(),
                        fronting_snapshot,
                        error: None,
                    })
                    .await;
            }
        }
    }

    /// Get or create a cached project mount for the given path.
    ///
    /// Resolution order:
    /// 1. If the path is already cached, return the cached handle.
    /// 2. Try [`pattern_memory::mount::attach`] on the canonical path.
    ///    Hits InRepo / Sidecar markers via walk-up, or standalone
    ///    mounts via the projects registry.
    /// 3. On [`MountError::NotFound`](pattern_memory::mount::MountError),
    ///    fall back to the global standalone mount at
    ///    `<data_root>/projects/@global/shared/`. Lazy-initializes the
    ///    mount on first use.
    ///
    /// The global fallback exists so `pattern chat` (and similar TUI
    /// flows) launched from a non-project directory yields a working
    /// session instead of erroring. Multiple non-project paths share
    /// the same global mount Arc; cache entries under both the global
    /// mount path and each calling canonical keep the lookup O(1) on
    /// subsequent calls.
    fn get_or_mount_project(
        &self,
        project_path: &std::path::Path,
    ) -> Result<Arc<ProjectMount>, String> {
        const GLOBAL_PROJECT_ID: &str = "@global";

        // Canonicalize for consistent cache keys.
        let canonical = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());

        // Fast path: already mounted under this canonical.
        if let Some(entry) = self.project_mounts.get(&canonical) {
            return Ok(entry.clone());
        }

        // Slow path: try to attach the project mount, falling through
        // to the global mount on NotFound.
        let first_party_skill_dir =
            std::path::PathBuf::from(pattern_runtime::sdk::FIRST_PARTY_SKILL_DIR);

        let (cache_key, mounted) =
            match pattern_memory::mount::attach(&canonical, Some(first_party_skill_dir.clone())) {
                Ok(m) => (canonical.clone(), m),
                Err(pattern_memory::mount::MountError::NotFound { .. }) => {
                    let paths = pattern_memory::PatternPaths::default_paths()
                        .map_err(|e| format!("failed to resolve pattern paths: {e}"))?;
                    let global_path = paths.standalone_mount_path(GLOBAL_PROJECT_ID);

                    // Cache hit on the shared global mount? Stash under the
                    // calling canonical so future calls from the same path
                    // skip straight to fast-path.
                    if let Some(entry) = self.project_mounts.get(&global_path) {
                        self.project_mounts.insert(canonical, entry.clone());
                        return Ok(entry.clone());
                    }

                    // Lazy-init the global standalone mount if it doesn't
                    // exist yet. Standalone mode requires jj — surface a
                    // clear error if jj isn't on PATH.
                    if !global_path.join(".pattern.kdl").is_file() {
                        let jj = pattern_memory::jj::JjAdapter::detect()
                            .map_err(|e| format!("jj detection failed: {e}"))?
                            .ok_or_else(|| {
                                "global fallback mount requires jj on PATH \
                             (or run `pattern mount init` in a project directory)"
                                    .to_owned()
                            })?;
                        pattern_memory::modes::standalone::init(GLOBAL_PROJECT_ID, &jj, &paths)
                            .map_err(|e| format!("global mount init failed: {e}"))?;
                        tracing::info!(
                            mount = %global_path.display(),
                            "lazy-initialized global standalone mount for non-project session"
                        );
                    }

                    let mounted =
                        pattern_memory::mount::attach(&global_path, Some(first_party_skill_dir))
                            .map_err(|e| {
                                format!(
                                    "global mount attach failed at {}: {e}",
                                    global_path.display()
                                )
                            })?;

                    (global_path, mounted)
                }
                Err(other) => {
                    return Err(format!(
                        "failed to attach mount at {}: {other}",
                        canonical.display()
                    ));
                }
            };

        // Load the persisted FrontingSet for this constellation. A missing
        // row is fine (default-empty); a malformed row is logged and treated
        // as default so a corrupt fronting_set never blocks mount-attach —
        // the user can re-set fronting through the SDK or RPC.
        let fronting_loaded = {
            let conn_result = mounted.db.get();
            match conn_result {
                Ok(conn) => match pattern_db::queries::fronting::load_fronting_set(&conn) {
                    Ok(Some(set)) => set,
                    Ok(None) => pattern_core::fronting::FrontingSet::default(),
                    Err(e) => {
                        tracing::warn!(
                            target = "pattern_server::fronting",
                            error = %e,
                            "failed to load FrontingSet from DB; starting with default"
                        );
                        pattern_core::fronting::FrontingSet::default()
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        target = "pattern_server::fronting",
                        error = %e,
                        "failed to acquire DB connection for FrontingSet load; starting with default"
                    );
                    pattern_core::fronting::FrontingSet::default()
                }
            }
        };

        // Build the per-mount port registry. `with_runtime_ports` ships
        // `HttpPort` (and any future runtime-provided ports) so every
        // session opened against this mount has them available without
        // each call site reconstructing the registry.
        let port_registry = Arc::new(
            pattern_runtime::port_registry::PortRegistryImpl::with_runtime_ports(
                &tokio::runtime::Handle::current(),
            ),
        );

        // Compile the mount's `file-policy { }` block once at mount time.
        //
        // Safe-default policy: this branch ALWAYS produces `Some(policy)`
        // — never `None`. A FileManager is always wired so agent File.*
        // effects surface the policy module's "no matching rule" denial,
        // not the generic "no file manager configured" error (which
        // lies about mount config presence). The three cases:
        //
        //   * Block has rules → `Some(rules)`.
        //   * Block is empty (or absent) → `Some(empty)`. Every File op
        //     is denied via the default-deny path. Logged as a warning.
        //   * Block has malformed globs → `Some(empty)` after logging
        //     loud at error level. Surfaces a uniform "no matching rule"
        //     denial instead of breaking the FM wiring entirely.
        let file_policy = {
            let section = mounted.config.file_policy.clone();
            let policy = if section.rules.is_empty() {
                tracing::warn!(
                    mount = %mounted.mount_path.display(),
                    "file-policy block is empty or absent; every agent File.* effect \
                     will be denied by the policy gate until `.pattern.kdl` declares \
                     allow/deny rules"
                );
                pattern_runtime::file_manager::FilePolicy::from_rules(Vec::new())
                    .expect("empty rule list is always valid")
            } else {
                match pattern_runtime::file_manager::FilePolicy::from_section(section) {
                    Ok(policy) => policy,
                    Err(err) => {
                        tracing::error!(
                            mount = %mounted.mount_path.display(),
                            error = %err,
                            "failed to compile file-policy from .pattern.kdl; falling \
                             back to default-deny so agent File.* effects surface a \
                             policy denial instead of a missing-FM error"
                        );
                        pattern_runtime::file_manager::FilePolicy::from_rules(Vec::new())
                            .expect("empty rule list is always valid")
                    }
                }
            };
            Some(policy)
        };

        // Build the rusqlite-backed constellation registry for this mount,
        // wrapped in EventEmittingRegistry so every mutation broadcasts a
        // ConstellationChanged event to mount-scoped subscribers (Phase 6 T8).
        // Shared across every session opened against the mount AND used
        // directly by the daemon for PromoteDraft / draft-flip RPCs.
        //
        // In test builds, a registry override from `spawn_with_config_and_registry`
        // replaces both the DB-backed registry and the EventEmittingRegistry wrapper,
        // giving tests full control over which methods succeed or fail.
        #[cfg(test)]
        let constellation_registry: Arc<dyn pattern_core::ConstellationRegistry> =
            if let Some(ref r) = self.constellation_registry_override {
                r.clone()
            } else {
                let raw: Arc<dyn pattern_core::ConstellationRegistry> =
                    Arc::new(pattern_db::ConstellationRegistryDb::new(mounted.db.clone()));
                Arc::new(EventEmittingRegistry::new(
                    raw,
                    self.event_tx.clone(),
                    mounted.mount_path.clone(),
                ))
            };
        #[cfg(not(test))]
        let constellation_registry: Arc<dyn pattern_core::ConstellationRegistry> = {
            let raw: Arc<dyn pattern_core::ConstellationRegistry> =
                Arc::new(pattern_db::ConstellationRegistryDb::new(mounted.db.clone()));
            Arc::new(EventEmittingRegistry::new(
                raw,
                self.event_tx.clone(),
                mounted.mount_path.clone(),
            ))
        };

        // Resolve the partner display name from `.pattern.kdl`'s
        // `partner { display-name "..." }` block (Phase 6 T8). `None` when the
        // block is absent or has no `display-name` child.
        let partner_display_name = mounted
            .config
            .partner
            .as_ref()
            .and_then(|p| p.display_name.clone());

        let mount = Arc::new(ProjectMount {
            cache: mounted.cache.clone(),
            db: mounted.db.clone(),
            mount_path: mounted.mount_path.clone(),
            // One AgentRegistry per mount: all sessions in this project share
            // it so they can route to each other via the `agent:` scheme.
            agent_registry: Arc::new(AgentRegistry::new()),
            fronting: Arc::new(std::sync::RwLock::new(fronting_loaded)),
            file_policy,
            port_registry,
            constellation_registry,
            partner_display_name,
            _mounted: mounted,
        });

        // Cache under the resolved mount path (the canonical for project
        // mounts, the global mount path for fallbacks). Deliberately do
        // NOT also stash under the input canonical for the fallback case:
        // if the user later runs `pattern mount init` in that directory,
        // a stale cache entry pointing at the global mount would shadow
        // the new project mount. The cost is one re-attach attempt per
        // future call from the same non-project path; correctness wins.
        self.project_mounts.insert(cache_key, mount.clone());
        Ok(mount)
    }

    /// Fan out a `FrontingChanged` event derived from `new_set` to all
    /// subscribers. Uses the `"fronting"` / `"daemon"` sentinel batch/agent IDs
    /// so TUI clients can distinguish fronting events from per-agent turn events.
    ///
    /// `mount_path` is the canonical path of the mount this fronting belongs
    /// to — used by mount-scoped subscribers to filter.
    async fn fan_out_fronting_changed(
        &mut self,
        new_set: &pattern_core::fronting::FrontingSet,
        mount_path: Option<String>,
    ) {
        let event = build_fronting_changed_event(new_set, mount_path);
        self.fan_out(event).await;
    }

    /// Phase 6 T6: handle a `PromoteDraft` RPC.
    ///
    /// Flow:
    /// 1. Resolve the project mount (must be initialised).
    /// 2. Fetch the persona record from the registry; verify Draft.
    /// 3. Move the KDL file from the runtime's `drafts_dir` flat layout
    ///    (`<drafts_dir>/<id>.kdl`) into the project mount's standard
    ///    discovery layout (`<mount>/personas/@<id>/persona.kdl`). Future
    ///    `discover_personas` calls find the persona via the normal path.
    /// 4. Update `config_path` in the registry to the new location.
    /// 5. Load the persona snapshot from the new path and open the session
    ///    (which calls `register_active` and auto-drains the Phase 4 draft
    ///    queue into the new mailbox).
    /// 6. Flip persona registry status to `Active`.
    ///
    /// Seed-cache loading (fork-promote, Phase 3 Task 7) is a separate
    /// follow-up: when the draft was created via `fork.promote()`, the
    /// `<drafts_dir>/<persona_id>.cache/<label>.loro` files also need to
    /// migrate (currently still tracked as a known gap).
    /// Promote a draft persona to Active.
    ///
    /// `out_warning` is populated (regardless of return value) when a
    /// non-fatal sub-step like seed-cache migration fails. Partners
    /// care about memory loss whether the overall promote ultimately
    /// succeeded or failed at a downstream step, so the warning is
    /// surfaced via the RPC response on both branches.
    async fn handle_promote_draft(
        &self,
        req: crate::protocol::PromoteDraftRequest,
        out_warning: &mut Option<String>,
    ) -> Result<(), String> {
        use pattern_core::constellation::PersonaStatus;
        use pattern_core::types::ids::PersonaId;

        let mount = self
            .current_mount
            .clone()
            .ok_or_else(|| "no project mounted — send InitSession first".to_string())?;

        let persona_id: PersonaId = req.persona_id.as_str().into();

        // 1+2: fetch the registry record + status check.
        let record = mount
            .constellation_registry
            .get(&persona_id)
            .await
            .map_err(|e| format!("registry lookup failed: {e}"))?
            .ok_or_else(|| format!("persona {:?} not found in registry", persona_id))?;

        if record.status != PersonaStatus::Draft {
            return Err(format!(
                "persona {:?} is not Draft (current status: {:?})",
                persona_id, record.status
            ));
        }

        let draft_path = record
            .config_path
            .ok_or_else(|| format!("draft persona {:?} has no config_path", persona_id))?;

        // 3a: move the KDL into the mount's discovery layout. The convention
        // is `<mount>/personas/@<id>/persona.kdl` — `discover_personas` finds
        // it after the move via the project-scoped scan path.
        let promoted_path = promote_persona_file(&mount.mount_path, &persona_id, &draft_path)
            .map_err(|e| format!("failed to move draft persona: {e}"))?;

        // 3b: migrate the fork-promote seed cache (if any). Drafts created
        // via `fork.promote()` carry per-block memory state at
        // `<drafts_dir>/<persona_id>.cache/`. Import each block into the
        // mount's MemoryCache under the new persona's id, then persist so
        // it lands in the DB before the session opens.
        //
        // `migrate_seed_cache` is idempotent: blocks that already exist in
        // the cache (from a prior partial import) are skipped at `create_block`
        // and re-applied via `insert_from_snapshot`, so retries converge.
        let seed_cache_dir = draft_path
            .parent()
            .map(|p| p.join(format!("{persona_id}.cache")));
        if let Some(cache_dir) = seed_cache_dir.as_ref()
            && cache_dir.is_dir()
        {
            if let Err(e) = migrate_seed_cache(cache_dir, &persona_id, &mount.cache).await {
                let msg = format!(
                    "seed cache migration failed for persona {persona_id}: {e}; \
                     promoted persona starts with empty memory \
                     (cache dir: {})",
                    cache_dir.display()
                );
                tracing::warn!(
                    persona_id = %persona_id,
                    cache_dir = %cache_dir.display(),
                    error = %e,
                    "seed cache migration failed; promoted persona will start with empty memory"
                );
                *out_warning = Some(msg);
            } else {
                // Best-effort cleanup of the seed cache directory after a
                // successful import. The blocks now live in the mount's
                // MemoryCache + DB.
                if let Err(e) = std::fs::remove_dir_all(cache_dir) {
                    tracing::warn!(
                        cache_dir = %cache_dir.display(),
                        error = %e,
                        "failed to remove seed cache after import; non-fatal"
                    );
                }
            }
        }

        // 4 (deferred): update registry config_path after the session opens
        // successfully (step 5). The path the session loaded from is the
        // source of truth — recording it before we know the session can open
        // would be misleading on failure.

        // 5: load + open via the shared session-open helper.
        let persona =
            pattern_runtime::persona_loader::load_persona(&promoted_path).map_err(|e| {
                format!(
                    "failed to load persona from {}: {e}",
                    promoted_path.display()
                )
            })?;
        let agent_id: pattern_core::types::ids::AgentId = persona.agent_id.as_str().into();

        // Per-agent lock + dedup — same shape as `get_or_open_session`.
        let lock = self
            .session_locks
            .entry(agent_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        if !self.sessions.contains_key(&agent_id) {
            let session_config = self
                .session_config
                .as_ref()
                .ok_or_else(|| {
                    "no SessionConfig — daemon was not spawned with real session infrastructure"
                        .to_string()
                })?
                .clone();
            open_session_with_persona(
                &agent_id,
                persona,
                &self.sessions,
                &session_config,
                &mount,
                &self.event_tx,
                &self.agent_to_mount,
            )
            .await?;
        }

        // 4 (now): session opened successfully — record the config_path.
        // Failure is non-fatal: the file is in the discovery path and future
        // opens via `discover_personas` will still work.
        if let Err(e) = mount
            .constellation_registry
            .set_config_path(&persona_id, Some(promoted_path.clone()))
            .await
        {
            tracing::warn!(
                persona_id = %persona_id,
                error = %e,
                "registry set_config_path failed after file move; \
                 persona still discoverable via mount/personas path"
            );
        }

        // 6: flip registry status to Active. After this, the persona is a
        // first-class member of the constellation and the queue (drained at
        // step 5 inside register_active) is in the live mailbox.
        //
        // If this fails, the session is already open and registered in the
        // agent mailbox. Clean up the session so a retry attempt can succeed
        // without finding a stale open-but-Draft session.
        if let Err(e) = mount
            .constellation_registry
            .set_status(&persona_id, PersonaStatus::Active)
            .await
        {
            // Best-effort session cleanup. We remove the entry from both maps
            // so a retry of PromoteDraft starts from a clean state. The
            // TidepoolSession Drop impl unregisters from AgentRegistry.
            self.sessions.remove(&agent_id);
            self.agent_to_mount.remove(&agent_id);
            tracing::warn!(
                persona_id = %persona_id,
                agent_id = %agent_id,
                error = %e,
                "set_status Active failed after session open; session removed for clean retry"
            );
            return Err(format!(
                "failed to update registry status to Active after session open: {e}; \
                 session has been closed — retry PromoteDraft to recover"
            ));
        }

        tracing::info!(
            persona_id = %persona_id,
            promoted_path = %promoted_path.display(),
            source = "pattern_server.promote_draft",
            "draft promoted to Active"
        );

        Ok(())
    }

    /// Phase 6 T7: handle a `ListPersonas` RPC.
    ///
    /// Filters by the optional project path; returns the slim wire summary
    /// for each matching record.
    async fn handle_list_personas(
        &self,
        req: crate::protocol::ListPersonasRequest,
    ) -> crate::protocol::ListPersonasResponse {
        use pattern_core::constellation::RegistryScope;
        use std::path::PathBuf;

        let mount = match self.current_mount.as_ref() {
            Some(m) => m,
            None => {
                return crate::protocol::ListPersonasResponse {
                    personas: Vec::new(),
                    error: Some("no project mounted — send InitSession first".to_string()),
                };
            }
        };

        let scope = match req.project {
            None => RegistryScope::All,
            Some(p) => RegistryScope::Project(PathBuf::from(p)),
        };

        match mount.constellation_registry.list(scope).await {
            Ok(records) => {
                let personas = records
                    .into_iter()
                    .map(|r| persona_record_to_wire_summary(&r))
                    .collect();
                crate::protocol::ListPersonasResponse {
                    personas,
                    error: None,
                }
            }
            Err(e) => crate::protocol::ListPersonasResponse {
                personas: Vec::new(),
                error: Some(format!("registry list failed: {e}")),
            },
        }
    }

    /// Phase 6 T7: handle an `AddRelationship` RPC.
    async fn handle_add_relationship(
        &self,
        req: crate::protocol::AddRelationshipRequest,
    ) -> Result<(), String> {
        use pattern_core::constellation::RelationshipSpec;
        use pattern_runtime::sdk::requests::constellation::parse_relationship_kind;

        let mount = self
            .current_mount
            .clone()
            .ok_or_else(|| "no project mounted — send InitSession first".to_string())?;

        let kind = parse_relationship_kind(&req.kind).ok_or_else(|| {
            format!(
                "unknown relationship kind {:?}; expected one of \
                 supervisor_of, specialist_for, peer_with, observer_of",
                req.kind
            )
        })?;

        mount
            .constellation_registry
            .add_relationship(RelationshipSpec::new(req.from, req.to, kind))
            .await
            .map(|_inserted| ()) // bool (was-inserted) is not surfaced in the wire response
            .map_err(|e| format!("registry add_relationship failed: {e}"))
    }

    /// Phase 6 T7: handle a `ListGroups` RPC.
    async fn handle_list_groups(
        &self,
        req: crate::protocol::ListGroupsRequest,
    ) -> crate::protocol::ListGroupsResponse {
        use pattern_core::constellation::RegistryScope;
        use std::path::PathBuf;

        let mount = match self.current_mount.as_ref() {
            Some(m) => m,
            None => {
                return crate::protocol::ListGroupsResponse {
                    groups: Vec::new(),
                    error: Some("no project mounted — send InitSession first".to_string()),
                };
            }
        };

        let scope = match req.project {
            None => RegistryScope::All,
            Some(p) => RegistryScope::Project(PathBuf::from(p)),
        };

        match mount.constellation_registry.groups(scope).await {
            Ok(groups) => {
                let groups = groups
                    .into_iter()
                    .map(|g| crate::protocol::WireGroupSummary {
                        id: g.id.to_string(),
                        name: g.name,
                        project_id: g.project_id,
                        members: g.members.into_iter().map(|m| m.to_string()).collect(),
                    })
                    .collect();
                crate::protocol::ListGroupsResponse {
                    groups,
                    error: None,
                }
            }
            Err(e) => crate::protocol::ListGroupsResponse {
                groups: Vec::new(),
                error: Some(format!("registry groups failed: {e}")),
            },
        }
    }

    /// Phase 6 T7: handle a `CreateGroup` RPC.
    async fn handle_create_group(
        &self,
        req: crate::protocol::CreateGroupRequest,
    ) -> crate::protocol::CreateGroupResponse {
        let mount = match self.current_mount.as_ref() {
            Some(m) => m,
            None => {
                return crate::protocol::CreateGroupResponse {
                    group: None,
                    error: Some("no project mounted — send InitSession first".to_string()),
                };
            }
        };

        match mount
            .constellation_registry
            .create_group(req.name, req.project_id)
            .await
        {
            Ok(g) => crate::protocol::CreateGroupResponse {
                group: Some(crate::protocol::WireGroupSummary {
                    id: g.id.to_string(),
                    name: g.name,
                    project_id: g.project_id,
                    members: g.members.into_iter().map(|m| m.to_string()).collect(),
                }),
                error: None,
            },
            Err(e) => crate::protocol::CreateGroupResponse {
                group: None,
                error: Some(format!("registry create_group failed: {e}")),
            },
        }
    }
}

/// Phase 6 T7 helper: convert a domain `PersonaRecord` to the slim wire
/// summary used by `ListPersonas`.
fn persona_record_to_wire_summary(
    r: &pattern_core::constellation::PersonaRecord,
) -> crate::protocol::WirePersonaSummary {
    use pattern_core::constellation::{EdgeDirection, PersonaStatus};
    use pattern_core::spawn::RelationshipKind;
    let outgoing_relationships = r
        .relationships
        .iter()
        .filter(|e| e.direction == EdgeDirection::Outgoing)
        .map(|e| {
            let kind = match e.kind {
                RelationshipKind::SupervisorOf => "supervisor_of",
                RelationshipKind::SpecialistFor => "specialist_for",
                RelationshipKind::PeerWith => "peer_with",
                RelationshipKind::ObserverOf => "observer_of",
            };
            (e.other.to_string(), kind.to_string())
        })
        .collect();
    crate::protocol::WirePersonaSummary {
        id: r.id.to_string(),
        name: r.name.clone(),
        status: match r.status {
            PersonaStatus::Active => "active".to_string(),
            PersonaStatus::Draft => "draft".to_string(),
            PersonaStatus::Inactive => "inactive".to_string(),
        },
        config_path: r
            .config_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        project_attachments: r
            .project_attachments
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        outgoing_relationships,
    }
}

/// Phase 6 T6: import a fork-promote seed cache into the mount's
/// `MemoryCache` under `persona_id`'s ownership.
///
/// The seed cache is a directory written by [`pattern_runtime::spawn::fork::ForkHandle::promote`]
/// containing per-block `<label>.loro` snapshots and a `manifest.json` listing
/// each block's schema and type. We read the manifest, load each snapshot,
/// and call `MemoryCache::insert_from_snapshot` with the recorded metadata.
/// Each block is then persisted so it survives session restart.
async fn migrate_seed_cache(
    cache_dir: &std::path::Path,
    persona_id: &pattern_core::types::ids::PersonaId,
    cache: &pattern_memory::cache::MemoryCache,
) -> Result<u32, String> {
    use pattern_runtime::spawn::fork::{SEED_CACHE_MANIFEST_VERSION, SeedCacheManifest};

    let manifest_path = cache_dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("read manifest {}: {e}", manifest_path.display()))?;
    let manifest: SeedCacheManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("parse manifest {}: {e}", manifest_path.display()))?;

    if manifest.version != SEED_CACHE_MANIFEST_VERSION {
        return Err(format!(
            "seed cache manifest version mismatch: expected {SEED_CACHE_MANIFEST_VERSION}, \
             got {} — refusing to import incompatible format",
            manifest.version
        ));
    }
    if manifest.persona_id != persona_id.as_str() {
        return Err(format!(
            "seed cache manifest persona_id mismatch: expected {:?}, got {:?}",
            persona_id, manifest.persona_id
        ));
    }

    // Seed-cache snapshots come from a persona's draft directory; they
    // belong to that persona's Global scope.
    let scope = pattern_core::types::memory_types::Scope::Global(persona_id.as_str().into());
    let mut imported: u32 = 0;
    let mut skipped: u32 = 0;
    for entry in manifest.entries {
        let snap_path = cache_dir.join(&entry.file);
        let snapshot = std::fs::read(&snap_path)
            .map_err(|e| format!("read seed snapshot {}: {e}", snap_path.display()))?;

        let already_exists = match pattern_core::MemoryStore::get_block(cache, &scope, &entry.label)
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => return Err(format!("get_block check for {:?}: {e}", entry.label)),
        };

        if !already_exists {
            let create = pattern_core::types::block::BlockCreate::new(
                entry.label.clone(),
                entry.block_type,
                entry.schema.clone(),
            );
            pattern_core::MemoryStore::create_block(cache, &scope, create)
                .map_err(|e| format!("create_block for {:?}: {e}", entry.label))?;
        } else {
            skipped += 1;
        }

        cache
            .insert_from_snapshot(
                scope.id(),
                entry.label.clone(),
                snapshot,
                entry.schema,
                entry.block_type,
            )
            .map_err(|e| format!("insert_from_snapshot for {:?}: {e}", entry.label))?;

        pattern_core::MemoryStore::persist_block(cache, &scope, &entry.label)
            .map_err(|e| format!("persist seed block {:?}: {e}", entry.label))?;

        imported += 1;
    }

    if skipped > 0 {
        tracing::info!(
            persona_id = %persona_id,
            imported,
            skipped,
            source = "pattern_server.promote_draft.seed_cache",
            "seed cache imported (some blocks already existed; snapshots re-applied)"
        );
    } else {
        tracing::info!(
            persona_id = %persona_id,
            imported,
            source = "pattern_server.promote_draft.seed_cache",
            "seed cache imported into mount cache"
        );
    }
    Ok(imported)
}

/// Move a draft persona KDL into the project mount's standard discovery
/// layout. Returns the new on-disk path
/// (`<mount>/personas/@<id>/persona.kdl`).
///
/// Falls back to copy + remove when `rename` fails (e.g. cross-filesystem
/// drafts dir vs. project mount) so the move always lands.
fn promote_persona_file(
    mount_path: &std::path::Path,
    persona_id: &pattern_core::types::ids::PersonaId,
    draft_path: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let target_dir = mount_path.join("personas").join(format!("@{persona_id}"));
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("create_dir_all {}: {e}", target_dir.display()))?;
    let target = target_dir.join("persona.kdl");

    // Same-path idempotency: if draft_path and target resolve to the same
    // file, this is a no-op retry (step-6 failed after a prior promote moved
    // the file and updated config_path). `rename(A, A)` is a POSIX no-op but
    // undefined on Windows — the copy+remove fallback would DELETE the file.
    // Check via canonicalized paths to handle relative vs. absolute + symlinks.
    let draft_canonical = draft_path
        .canonicalize()
        .unwrap_or_else(|_| draft_path.to_path_buf());
    let target_canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if draft_canonical == target_canonical && target.exists() {
        tracing::debug!(
            target = %target.display(),
            "promote_persona_file: draft_path == target; treating as idempotent no-op"
        );
        return Ok(target);
    }

    // Idempotency: if the target already exists and the draft path is gone,
    // a prior promote attempt already moved the file. Treat this as success
    // so retries (e.g. after step-5/6 failure) converge rather than error
    // on a missing source file.
    if target.exists() && !draft_path.exists() {
        tracing::debug!(
            target = %target.display(),
            "promote_persona_file: target already exists and draft is gone; treating as idempotent success"
        );
        return Ok(target);
    }

    // Try a fast atomic rename first.
    match std::fs::rename(draft_path, &target) {
        Ok(()) => Ok(target),
        Err(_) => {
            // Cross-FS or other failure: fall back to copy + remove.
            std::fs::copy(draft_path, &target).map_err(|e| {
                format!("copy {} -> {}: {e}", draft_path.display(), target.display())
            })?;
            // Best-effort cleanup of the source. Loud-log on failure but
            // don't fail the promote — the target now has the canonical
            // copy, leaving the draft in place is non-fatal (the registry's
            // config_path will point at the new location).
            if let Err(e) = std::fs::remove_file(draft_path) {
                tracing::warn!(
                    draft_path = %draft_path.display(),
                    error = %e,
                    "failed to remove draft file after copy; manual cleanup may be needed"
                );
            }
            Ok(target)
        }
    }
}

/// Build a [`TaggedTurnEvent`] carrying [`WireTurnEvent::FrontingChanged`] for
/// `new_set`. Shared between the actor's `fan_out_fronting_changed` and the
/// SDK-side [`DaemonFrontingCommitter`] so the wire shape is identical no
/// matter which path triggered the change.
///
/// `mount_path` is the canonical path of the project mount this fronting
/// state belongs to — used by mount-scoped subscribers ([`SubscribeAll`])
/// to filter events.
fn build_fronting_changed_event(
    new_set: &pattern_core::fronting::FrontingSet,
    mount_path: Option<String>,
) -> TaggedTurnEvent {
    let rules = build_wire_routing_rules(new_set);
    TaggedTurnEvent {
        batch_id: "fronting".into(),
        agent_id: "daemon".into(),
        event: WireTurnEvent::FrontingChanged {
            active: new_set.active.iter().map(|id| id.to_string()).collect(),
            fallback: new_set.fallback.as_ref().map(|id| id.to_string()),
            rules,
        },
        mount_path,
    }
}

/// Build a [`TaggedTurnEvent`] carrying [`WireTurnEvent::ConstellationChanged`].
///
/// Phase 6 T8: emitted by [`EventEmittingRegistry`] after each successful
/// registry mutation. `kind` identifies the mutation type for tracing
/// (TUI clients re-fetch on any kind).
pub(crate) fn build_constellation_changed_event(
    kind: &str,
    mount_path: Option<String>,
) -> TaggedTurnEvent {
    TaggedTurnEvent {
        batch_id: "constellation".into(),
        agent_id: "daemon".into(),
        event: WireTurnEvent::ConstellationChanged {
            kind: kind.to_string(),
        },
        mount_path,
    }
}

/// Convert a `FrontingSet`'s routing rules to the wire form.
fn build_wire_routing_rules(set: &pattern_core::fronting::FrontingSet) -> Vec<WireRoutingRule> {
    set.routing
        .rules
        .iter()
        .map(|r| {
            let (pt, pv) = wire_pattern(&r.pattern);
            WireRoutingRule {
                id: r.id.clone(),
                pattern_type: pt.to_string(),
                pattern_value: pv,
                target: r.target.to_string(),
                priority: r.priority,
            }
        })
        .collect()
}

/// Build a wire snapshot of the given fronting set for use in
/// [`SessionInfo::fronting_snapshot`].
pub(crate) fn build_fronting_snapshot(
    set: &pattern_core::fronting::FrontingSet,
) -> crate::protocol::FrontingSnapshot {
    crate::protocol::FrontingSnapshot {
        active: set.active.iter().map(|id| id.to_string()).collect(),
        fallback: set.fallback.as_ref().map(|id| id.to_string()),
        rules: build_wire_routing_rules(set),
    }
}

// ── EventEmittingRegistry (Phase 6 T8) ────────────────────────────────────────

/// Wraps a [`ConstellationRegistry`](pattern_core::ConstellationRegistry) and
/// emits a [`WireTurnEvent::ConstellationChanged`] event after each successful
/// mutation. Read methods pass through verbatim.
///
/// Constructed once at mount time and used everywhere the daemon needs the
/// registry — including the registry handed to each session via
/// `SessionRegistries.constellation_registry`. Sibling auto-registration,
/// PromoteDraft's status flip, AddRelationship RPC, CreateGroup RPC etc.
/// all therefore broadcast `ConstellationChanged` to mount-scoped subscribers
/// for free.
///
/// `kind` strings are stable identifiers describing what mutation happened
/// (`"persona_registered"`, `"status_changed"`, `"config_path_changed"`,
/// `"relationship_added"`, `"group_created"`). TUI clients re-fetch on any
/// kind; `kind` is for tracing/diagnostics only.
#[derive(Debug, Clone)]
pub struct EventEmittingRegistry {
    inner: Arc<dyn pattern_core::ConstellationRegistry>,
    event_tx: crate::bridge::EventTx,
    /// Canonical mount path tagged on every emitted event.
    mount_path: String,
}

impl EventEmittingRegistry {
    pub fn new(
        inner: Arc<dyn pattern_core::ConstellationRegistry>,
        event_tx: crate::bridge::EventTx,
        mount_path: PathBuf,
    ) -> Self {
        Self {
            inner,
            event_tx,
            mount_path: mount_path.to_string_lossy().into_owned(),
        }
    }

    fn emit(&self, kind: &str) {
        let _ = self.event_tx.send(build_constellation_changed_event(
            kind,
            Some(self.mount_path.clone()),
        ));
    }
}

#[async_trait::async_trait]
impl pattern_core::ConstellationRegistry for EventEmittingRegistry {
    async fn list(
        &self,
        scope: pattern_core::constellation::RegistryScope,
    ) -> Result<
        Vec<pattern_core::constellation::PersonaRecord>,
        pattern_core::constellation::RegistryError,
    > {
        self.inner.list(scope).await
    }

    async fn get(
        &self,
        id: &pattern_core::PersonaId,
    ) -> Result<
        Option<pattern_core::constellation::PersonaRecord>,
        pattern_core::constellation::RegistryError,
    > {
        self.inner.get(id).await
    }

    async fn find(
        &self,
        project: Option<&std::path::Path>,
        kind: Option<pattern_core::spawn::RelationshipKind>,
    ) -> Result<
        Vec<pattern_core::constellation::PersonaRecord>,
        pattern_core::constellation::RegistryError,
    > {
        self.inner.find(project, kind).await
    }

    async fn register(
        &self,
        record: pattern_core::constellation::PersonaRecord,
    ) -> Result<(), pattern_core::constellation::RegistryError> {
        let result = self.inner.register(record).await;
        if result.is_ok() {
            self.emit("persona_registered");
        }
        result
    }

    async fn set_status(
        &self,
        id: &pattern_core::PersonaId,
        status: pattern_core::constellation::PersonaStatus,
    ) -> Result<(), pattern_core::constellation::RegistryError> {
        let result = self.inner.set_status(id, status).await;
        if result.is_ok() {
            self.emit("status_changed");
        }
        result
    }

    async fn set_config_path(
        &self,
        id: &pattern_core::PersonaId,
        config_path: Option<PathBuf>,
    ) -> Result<(), pattern_core::constellation::RegistryError> {
        let result = self.inner.set_config_path(id, config_path).await;
        if result.is_ok() {
            self.emit("config_path_changed");
        }
        result
    }

    async fn add_relationship(
        &self,
        edge: pattern_core::constellation::RelationshipSpec,
    ) -> Result<bool, pattern_core::constellation::RegistryError> {
        let result = self.inner.add_relationship(edge).await;
        // Only emit when a row was actually inserted; `ON CONFLICT DO NOTHING`
        // no-ops (false) do not change state so no event is needed.
        if result.as_ref().is_ok_and(|&inserted| inserted) {
            self.emit("relationship_added");
        }
        result
    }

    async fn groups(
        &self,
        scope: pattern_core::constellation::RegistryScope,
    ) -> Result<
        Vec<pattern_core::constellation::PersonaGroup>,
        pattern_core::constellation::RegistryError,
    > {
        self.inner.groups(scope).await
    }

    async fn create_group(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<pattern_core::constellation::PersonaGroup, pattern_core::constellation::RegistryError>
    {
        let result = self.inner.create_group(name, project_id).await;
        if result.is_ok() {
            self.emit("group_created");
        }
        result
    }
}

// ── DaemonFrontingCommitter (Phase 6 T5b) ─────────────────────────────────────

/// Daemon-side [`FrontingCommitter`] for SDK-driven `Pattern.Fronting` mutations.
///
/// Wraps the same [`update_fronting_inner`] three-phase commit used by the
/// `SetFronting` / `UpdateRouting` IRPCs, plus an event-channel send to fan
/// out [`WireTurnEvent::FrontingChanged`] to subscribers.
///
/// The committer is wired into each `SessionContext` via
/// `with_fronting_committer` from `get_or_open_session`. SDK-driven mutations
/// then go through the same atomic snapshot → mutate → DB persist → rollback
/// path as RPC-driven mutations.
#[derive(Debug, Clone)]
pub struct DaemonFrontingCommitter {
    fronting: Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>>,
    db: Arc<pattern_db::ConstellationDb>,
    event_tx: crate::bridge::EventTx,
    tokio_handle: tokio::runtime::Handle,
    /// Canonical mount path this committer belongs to. Tagged on every
    /// emitted `FrontingChanged` event so mount-scoped subscribers can
    /// filter (Phase 6 T8).
    mount_path: Option<String>,
}

impl DaemonFrontingCommitter {
    pub fn new(
        fronting: Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>>,
        db: Arc<pattern_db::ConstellationDb>,
        event_tx: crate::bridge::EventTx,
        tokio_handle: tokio::runtime::Handle,
        mount_path: Option<String>,
    ) -> Self {
        Self {
            fronting,
            db,
            event_tx,
            tokio_handle,
            mount_path,
        }
    }
}

impl pattern_runtime::sdk::handlers::fronting::FrontingCommitter for DaemonFrontingCommitter {
    fn fronting_set(&self) -> &Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>> {
        &self.fronting
    }

    fn commit_sync(
        &self,
        mutator: pattern_runtime::sdk::handlers::fronting::FrontingMutator,
    ) -> Result<pattern_core::fronting::FrontingSet, pattern_runtime::tidepool_effect::EffectError>
    {
        let fronting = self.fronting.clone();
        let db = self.db.clone();
        let new_set = self
            .tokio_handle
            .block_on(async move { update_fronting_inner(&fronting, &db, mutator).await })
            .map_err(|e| {
                pattern_runtime::tidepool_effect::EffectError::Handler(format!(
                    "fronting commit failed: {e}"
                ))
            })?;

        // Send through the actor's event channel; the actor loop fans it out
        // to subscribers. Channel-closed (daemon shutting down) is not an
        // error from the SDK handler's perspective — the mutation already
        // landed in the DB and in-memory state.
        let _ = self.event_tx.send(build_fronting_changed_event(
            &new_set,
            self.mount_path.clone(),
        ));

        Ok(new_set)
    }
}

/// Snapshot the current [`FrontingSet`] from the given lock without holding
/// the guard across any `.await`.
///
/// Returns `None` if the lock is poisoned. The caller is responsible for
/// surfacing the poison case to the user.
fn snapshot_fronting_set(
    lock: &std::sync::RwLock<pattern_core::fronting::FrontingSet>,
) -> Option<pattern_core::fronting::FrontingSet> {
    lock.read().ok().map(|guard| guard.clone())
}

/// Project a [`pattern_core::fronting::MessagePattern`] to its wire representation.
///
/// Returns a `(&'static str, String)` pair of `(pattern_type, pattern_value)` in
/// the same format as the [`WireRoutingRule`] fields.
fn wire_pattern(p: &pattern_core::fronting::MessagePattern) -> (&'static str, String) {
    match p {
        pattern_core::fronting::MessagePattern::Prefix(s) => ("Prefix", s.clone()),
        pattern_core::fronting::MessagePattern::Contains(s) => ("Contains", s.clone()),
        pattern_core::fronting::MessagePattern::TopicTag(s) => ("TopicTag", s.clone()),
        pattern_core::fronting::MessagePattern::Regex(s) => ("Regex", s.clone()),
        // Forward-compat: unknown patterns are preserved as an opaque pair so
        // they survive a round-trip without being silently dropped.
        _ => ("Unknown", String::new()),
    }
}

/// Convert a [`WireRoutingRule`] from the RPC wire format to a domain
/// [`pattern_core::fronting::RoutingRule`].
fn wire_rule_to_domain(w: WireRoutingRule) -> pattern_core::fronting::RoutingRule {
    let pattern = match w.pattern_type.as_str() {
        "Prefix" => pattern_core::fronting::MessagePattern::Prefix(w.pattern_value),
        "Contains" => pattern_core::fronting::MessagePattern::Contains(w.pattern_value),
        "TopicTag" => pattern_core::fronting::MessagePattern::TopicTag(w.pattern_value),
        "Regex" => pattern_core::fronting::MessagePattern::Regex(w.pattern_value),
        // Unknown types round-trip as Prefix with empty value — compilation
        // will succeed and the rule will match nothing meaningful.
        _ => pattern_core::fronting::MessagePattern::Prefix(String::new()),
    };
    pattern_core::fronting::RoutingRule::new(w.id, pattern, w.target.as_str(), w.priority)
}

/// Get or open a session for the given agent.
///
/// Fast path: returns immediately if the session is already cached.
/// Slow path: acquires a per-agent lock, double-checks, then resolves the
/// persona and opens a [`TidepoolSession`] via `open_with_agent_loop`.
/// The lock prevents two concurrent tasks from racing to open the same session.
///
/// Project-specific state (memory store, DB, mount path) comes from the
/// `project_mount` parameter, which is populated by `InitSession`. The
/// `config` provides project-independent state (SDK, provider).
async fn get_or_open_session(
    agent_id: &AgentId,
    sessions: &DashMap<AgentId, AgentSession>,
    session_locks: &DashMap<AgentId, Arc<tokio::sync::Mutex<()>>>,
    config: &SessionConfig,
    project_mount: &ProjectMount,
    event_tx: &crate::bridge::EventTx,
    agent_to_mount: &DashMap<AgentId, PathBuf>,
) -> Result<AgentSession, String> {
    // Fast path: session already exists. Clone immediately, drop ref.
    if let Some(entry) = sessions.get(agent_id) {
        return Ok(entry.clone());
    }

    // Slow path: need to open. Acquire per-agent lock to prevent races.
    let lock = session_locks
        .entry(agent_id.clone())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = lock.lock().await;

    // Double-check after acquiring lock (another task may have opened it).
    if let Some(entry) = sessions.get(agent_id) {
        return Ok(entry.clone());
    }

    // Resolve persona by id, then delegate to the shared open path.
    let persona = resolve_persona(agent_id, Some(&project_mount.mount_path))?;
    open_session_with_persona(
        agent_id,
        persona,
        sessions,
        config,
        project_mount,
        event_tx,
        agent_to_mount,
    )
    .await
}

/// Shared session-open implementation used by both `get_or_open_session`
/// (which resolves the persona by id) and the `PromoteDraft` flow (which
/// loads the persona from a draft KDL on disk). The caller is responsible
/// for any pre-open de-dup / locking.
async fn open_session_with_persona(
    agent_id: &AgentId,
    persona: PersonaSnapshot,
    sessions: &DashMap<AgentId, AgentSession>,
    config: &SessionConfig,
    project_mount: &ProjectMount,
    event_tx: &crate::bridge::EventTx,
    agent_to_mount: &DashMap<AgentId, PathBuf>,
) -> Result<AgentSession, String> {
    let mux_sink = Arc::new(MultiplexSink::new());
    let sink_dyn: Arc<dyn TurnSink> = mux_sink.clone();

    // Build a RouterRegistry with:
    //   - `agent:` scheme → AgentRouter backed by the per-mount registry.
    //   - `cli:` scheme (default) → CliRouter for human-visible output.
    //
    // The CliRouter's receiver is dropped here — CLI/TUI output travels via
    // the TurnSink/MultiplexSink path (WireTurnEvent fan-out), not via the
    // router channel. Registering a CliRouter as the default scheme ensures
    // malformed or unknown-scheme recipients don't return a hard error when
    // a session tries to send to a human-visible recipient without an
    // explicit `cli:` prefix.
    let (cli_router, _cli_rx) = CliRouter::new();
    let mut router_reg = RouterRegistry::new().with_default_scheme("cli");

    // AgentRouter gains fronting-aware dispatch. The FrontingState points at
    // the mount's canonical FrontingSet lock and the mount's real
    // constellation registry. This allows the FrontingResolver to fall back
    // to Active personas from the DB-backed registry when the fronting set is
    // empty rather than always returning SystemDefault ("no fronting
    // configured").
    let fronting_state = pattern_runtime::fronting_dispatch::FrontingState::new(
        project_mount.fronting.clone(),
        project_mount.constellation_registry.clone(),
    );
    router_reg.register(Arc::new(
        AgentRouter::new(project_mount.agent_registry.clone()).with_fronting(fronting_state),
    ));
    router_reg.register(Arc::new(cli_router));
    let router_reg = Arc::new(router_reg);

    // Wake registry extras: wire the mount's memory cache notifier and store
    // so BlockChanged / TaskDependencyResolved evaluators have what they need.
    let wake_extras = WakeRegistryExtras {
        block_change_notifier: Some(project_mount.cache.block_change_notifier().clone()),
        memory_store: Some(project_mount.cache.clone() as Arc<dyn MemoryStore>),
    };

    // The committer carries the project_mount's `fronting` Arc internally
    // — that's the same Arc the RPC `update_fronting` path mutates, so SDK
    // and RPC mutations end up in the same lock by construction.
    let fronting_committer: Arc<dyn pattern_runtime::sdk::handlers::fronting::FrontingCommitter> =
        Arc::new(DaemonFrontingCommitter::new(
            project_mount.fronting.clone(),
            project_mount.db.clone(),
            event_tx.clone(),
            tokio::runtime::Handle::current(),
            Some(project_mount.mount_path.to_string_lossy().into_owned()),
        ));

    // Production sibling resolver: query the per-mount constellation registry
    // for `agent:<id>` lookups. Without this, `ctx.spawn.sibling(Existing(id))`
    // would always fail with `RegistryError::PersonaNotFound` because the
    // default `UnconfiguredSiblingResolver` rejects every lookup.
    let sibling_resolver: Arc<dyn pattern_runtime::spawn::sibling::SiblingPersonaResolver> =
        Arc::new(
            pattern_runtime::spawn::sibling::ConstellationSiblingResolver::new(
                project_mount.constellation_registry.clone(),
            ),
        );

    let registries = SessionRegistries {
        agent_registry: Some(project_mount.agent_registry.clone()),
        router_registry: Some(router_reg),
        wake_registry_extras: Some(wake_extras),
        port_registry: Some(project_mount.port_registry.clone()),
        file_policy: project_mount.file_policy.clone(),
        fronting_committer: Some(fronting_committer),
        constellation_registry: Some(project_mount.constellation_registry.clone()),
        sibling_resolver: Some(sibling_resolver),
        plugin_registry: None, // TODO: wire from DaemonServer when plugin loading lands
    };
    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &config.sdk,
        project_mount.cache.clone(),
        config.provider.clone(),
        project_mount.db.clone(),
        tokio::runtime::Handle::current(),
        sink_dyn,
        None, // prelude_dir — SDK bundles the prelude internally.
        Some(project_mount.mount_path.clone()),
        // Explicitly pass CapabilitySet::all() so sessions have full power
        // (daemon uses full power until per-persona caps land; but pass Some
        // so the wake handler's fail-closed gate passes rather than denying).
        Some(CapabilitySet::all()),
        Some(registries),
    )
    .await
    .map_err(|e| format!("failed to open session for {agent_id}: {e}"))?;

    let agent_session = AgentSession {
        session: Arc::new(session),
        mux_sink,
    };

    sessions.insert(agent_id.clone(), agent_session.clone());
    // Phase 6 T8: register the agent's mount so per-agent events without an
    // explicit mount_path tag can be routed to mount-scoped subscribers.
    agent_to_mount.insert(agent_id.clone(), project_mount.mount_path.clone());
    info!(agent_id = %agent_id, "opened new session");

    Ok(agent_session)
}

/// Resolve a persona by agent_id using `discover_personas`.
///
/// Looks up the normalized agent_id (stripped of `@` prefix) in the
/// discovery map built from global `~/.pattern/personas/` and the
/// project mount's `personas/` directory.
fn resolve_persona(
    agent_id: &AgentId,
    mount_path: Option<&std::path::Path>,
) -> Result<PersonaSnapshot, String> {
    use pattern_memory::PatternPaths;
    use pattern_memory::persona::discover_personas;

    let paths = PatternPaths::default_paths()
        .map_err(|e| format!("failed to resolve pattern home: {e}"))?;

    let personas = discover_personas(&paths, mount_path)
        .map_err(|e| format!("persona discovery failed: {e}"))?;

    // Normalize: strip leading '@' from the requested agent_id.
    let normalized = agent_id.trim_start_matches('@');

    // path_for resolves both canonical agent_id and alias (persona name).
    let persona_path = personas.path_for(normalized).ok_or_else(|| {
        let available: Vec<_> = personas.canonical_ids().collect();
        format!("persona not found for agent_id '{normalized}'; available: {available:?}")
    })?;

    pattern_runtime::persona_loader::load_persona(persona_path).map_err(|e| {
        format!(
            "failed to load persona from {}: {e}",
            persona_path.display()
        )
    })
}

/// Build a [`TurnInput`] from an [`AgentMessage`].
///
/// Mints fresh turn and batch IDs, wraps the client's content parts into a
/// user [`ChatMessage`], and passes the caller-supplied [`MessageOrigin`]
/// through directly to `TurnInput::origin`. The daemon does **not** override
/// or default the author — each RPC client is responsible for supplying its
/// own identity (Partner, Agent, Human, System).
fn build_turn_input(msg: &AgentMessage, session_agent_id: &str) -> TurnInput {
    let batch_id = CoreBatchId::from(msg.batch_id.to_string());
    // Use the session's persona agent_id for message ownership — not the
    // client-sent routing key, which may differ (e.g. "default" vs
    // "pattern-default").
    let agent_id = CoreAgentId::from(session_agent_id.to_string());

    let chat_msg = ChatMessage::user(
        msg.parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    );

    let message = Message {
        chat_message: chat_msg,
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        owner_id: agent_id,
        created_at: jiff::Timestamp::now(),
        batch: batch_id.clone(),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };

    TurnInput {
        turn_id: new_snowflake_id(),
        batch_id,
        // Pass the caller-supplied origin through unchanged. The client is
        // responsible for constructing the appropriate Author (Partner, Agent,
        // Human, System); the daemon must not assume any specific variant.
        origin: msg.origin.clone(),
        messages: vec![message],
    }
}

/// Convert a stored DB message to WireTurnEvents.
///
/// This function deserializes the `content_json` from the DB message and
/// converts it to the appropriate wire events. A single DB message can
/// produce multiple events (e.g., an assistant message with multiple
/// content parts: text, tool calls, thinking).
///
/// User messages return an empty vec - the user_message field of
/// HistoricalBatch handles those separately.
fn message_to_wire_events(
    db_msg: pattern_db::models::Message,
) -> Vec<crate::protocol::WireTurnEvent> {
    use crate::protocol::WireTurnEvent;
    use pattern_db::models::MessageRole;

    let mut events = Vec::new();

    // Deserialize the ChatMessage from content_json.
    let Ok(chat_msg) = serde_json::from_value::<ChatMessage>(db_msg.content_json.0) else {
        return events;
    };

    if let Ok(attachments) = serde_json::from_value::<Vec<MessageAttachment>>(
        db_msg
            .attachments_json
            .unwrap_or(pattern_db::Json(json!({})))
            .0,
    ) {
        events.push(WireTurnEvent::Attachments(attachments_to_wire(
            &attachments,
        )));
    }

    // User messages are handled via the user_message field, not events.
    if db_msg.role == MessageRole::User {
        return events;
    }

    // For System messages, emit text content.
    if db_msg.role == MessageRole::System {
        let text: String = chat_msg
            .content
            .parts()
            .iter()
            .filter_map(|p| p.as_text())
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            events.push(WireTurnEvent::Text(text));
        }
        return events;
    }

    // For Assistant messages, emit events for ALL content parts.
    if db_msg.role == MessageRole::Assistant {
        for part in chat_msg.content.parts() {
            if let Some(text) = part.as_text() {
                events.push(WireTurnEvent::Text(text.to_string()));
            } else if let Some(tc) = part.as_tool_call() {
                events.push(WireTurnEvent::ToolCall {
                    call_id: tc.call_id.clone(),
                    function_name: tc.fn_name.clone(),
                    arguments_json: tc.fn_arguments.to_string(),
                });
            }
            // Note: ThinkingBlock and other parts could be added here as needed
        }
        return events;
    }

    // For Tool messages, emit tool result events.
    if db_msg.role == MessageRole::Tool {
        for part in chat_msg.content.parts() {
            if let Some(tr) = part.as_tool_response() {
                // Determine success based on content structure.
                let (success, content_json) = if tr.content.is_string() {
                    (true, tr.content.to_string())
                } else {
                    // Check if this is an error response (has "error" key).
                    if let Some(obj) = tr.content.as_object() {
                        if obj.contains_key("error") {
                            (false, tr.content.to_string())
                        } else {
                            (true, tr.content.to_string())
                        }
                    } else {
                        (true, tr.content.to_string())
                    }
                };
                events.push(WireTurnEvent::ToolResult {
                    call_id: tr.call_id.clone(),
                    success,
                    content_json,
                });
            }
        }
    }

    events
}

/// Estimate token count for a historical batch.
///
/// Uses the same heuristic as the runtime: ~4 chars per token + flat overhead.
/// For tool calls/results, counts the full JSON string length including structure.
fn estimate_batch_tokens(user_message: &Option<String>, events: &[WireTurnEvent]) -> u64 {
    let mut total_chars = 0;

    // User message characters.
    if let Some(msg) = user_message {
        total_chars += msg.len();
    }

    // Event characters.
    for ev in events {
        match ev {
            WireTurnEvent::Text(s) => total_chars += s.len(),
            WireTurnEvent::Thinking(s) => total_chars += s.len(),
            // ToolCall: count function name + full JSON arguments
            WireTurnEvent::ToolCall {
                function_name,
                arguments_json,
                ..
            } => {
                total_chars += function_name.len();
                total_chars += arguments_json.len();
            }
            // ToolResult: count the full JSON content string
            WireTurnEvent::ToolResult { content_json, .. } => {
                total_chars += content_json.len();
            }
            WireTurnEvent::Display { text, .. } => total_chars += text.len(),
            WireTurnEvent::MessageSent {
                recipient, body, ..
            } => {
                total_chars += recipient.len();
                total_chars += body.len();
            }
            WireTurnEvent::Stop(_) => {}
            // FrontingChanged + ConstellationChanged are notification events
            // (no agent-side text); do not contribute to token estimates.
            WireTurnEvent::FrontingChanged { .. } => {}
            WireTurnEvent::ConstellationChanged { .. } => {}
            WireTurnEvent::Attachments(a) => {
                for attachment in a.iter() {
                    total_chars += match attachment {
                        WireMessageAttachment::BatchOpeningSnapshot { blocks, .. } => blocks
                            .iter()
                            .map(|b| b.rendered.as_deref().unwrap_or_default().len())
                            .sum::<usize>(),
                        WireMessageAttachment::SkillAvailable {
                            name,
                            description,
                            keywords,
                            ..
                        } => {
                            name.len()
                                + description.as_deref().unwrap_or_default().len()
                                + keywords.iter().map(|k| k.len()).sum::<usize>()
                        }
                        WireMessageAttachment::Custom { content } => content.len(),
                        WireMessageAttachment::FileEdit { diff, .. } => {
                            diff.as_deref().unwrap_or_default().len()
                        }
                        WireMessageAttachment::FileConflict { .. } => 5,
                        WireMessageAttachment::BlockWriteNotifications { writes } => writes
                            .iter()
                            .map(|w| w.rendered_content.len())
                            .sum::<usize>(),
                        WireMessageAttachment::ShellOutput { kind, .. } => match kind {
                            ShellOutputKind::Output(output) => output.len(),
                            ShellOutputKind::Exit { .. } => 0,
                            ShellOutputKind::Backgrounded { .. } => 0,
                        },
                        WireMessageAttachment::PortEvent { payload, .. } => payload.len(),
                    }
                }
            }
        }
    }

    // Heuristic: ~4 chars per token + 32 token overhead per batch.
    (total_chars / 4) as u64 + 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DaemonClient;
    use pattern_core::types::ids::{new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Partner, Sphere};
    use smol_str::SmolStr;

    fn test_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Partner(Partner {
                user_id: new_id(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    #[tokio::test]
    async fn send_message_returns_batch_id_and_emits_events() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        // Subscribe before sending so we don't miss events.
        let mut events = client.subscribe_output("test-agent".into()).await.unwrap();

        // Send a message (client mints the batch_id). Use Direct to match
        // the subscribe target agent so events fan-out to our subscriber.
        let batch_id: SmolStr = new_snowflake_id();
        client
            .send_message(
                batch_id.clone(),
                Recipient::Direct("test-agent".into()),
                vec![ContentPart::Text("hello".into())],
                test_origin(),
            )
            .await
            .unwrap();

        // Receive events — tagged with our batch_id.
        let ev = events.recv().await.unwrap().unwrap();
        assert_eq!(ev.batch_id, batch_id);
        assert!(matches!(ev.event, WireTurnEvent::Text(ref s) if s.contains("hello")));
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_events() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        let mut rx1 = client.subscribe_output("test-agent".into()).await.unwrap();
        let mut rx2 = client.subscribe_output("test-agent".into()).await.unwrap();

        let batch_id: SmolStr = new_snowflake_id();
        client
            .send_message(
                batch_id.clone(),
                Recipient::Direct("test-agent".into()),
                vec![ContentPart::Text("shared".into())],
                test_origin(),
            )
            .await
            .unwrap();

        let ev1 = rx1.recv().await.unwrap().unwrap();
        let ev2 = rx2.recv().await.unwrap().unwrap();
        assert_eq!(ev1.batch_id, batch_id);
        assert_eq!(ev2.batch_id, batch_id);
    }

    #[tokio::test]
    async fn get_status_returns_uptime() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        let status = client.get_status().await.unwrap();
        assert_eq!(status.agent_count, 0);
        // Uptime should be very small but non-negative.
        assert!(status.uptime_secs < 5);
    }

    #[tokio::test]
    async fn init_session_echo_mode_returns_requested_agent() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        let info = client
            .init_session(
                std::path::PathBuf::from("/tmp/test-project"),
                "my-agent".into(),
            )
            .await
            .unwrap();

        assert_eq!(info.agent_id, "my-agent");
        assert_eq!(info.persona_name, "echo");
        assert!(info.available_agents.is_empty());
        assert!(!info.partner_id.is_empty(), "partner_id must be non-empty");
        assert!(info.error.is_none());
    }

    /// Verifies that `GetClientCount` prunes senders whose receiver has been
    /// dropped, rather than returning a stale count.
    ///
    /// Without the proactive closed() probe in the handler, dropping the
    /// receiver does not remove the sender from `self.subscribers` until
    /// the next `fan_out`. This means `client_count()` would return 1 even
    /// after the last subscriber exits, and `--stop-daemon-on-exit` would
    /// never trigger.
    #[tokio::test]
    async fn get_client_count_prunes_dropped_subscribers() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        // Subscribe to a specific agent to register a sender in subscribers.
        let rx = client
            .subscribe_output("prune-test-agent".into())
            .await
            .unwrap();

        // The server must see one live subscriber.
        let count = client.client_count().await.unwrap();
        assert_eq!(count, 1, "expected 1 subscriber after subscribing");

        // Drop the receiver — this causes the sender's closed() to resolve.
        drop(rx);

        // Give the drop a moment to propagate through the channel machinery.
        tokio::task::yield_now().await;

        // GetClientCount must probe and prune the dead sender, returning 0.
        let count = client.client_count().await.unwrap();
        assert_eq!(
            count, 0,
            "expected 0 subscribers after receiver was dropped"
        );
    }

    /// `batch_to_agent` must drop an entry once its batch finishes (Stop event),
    /// not only on `CancelBatch`. Otherwise the map grows without bound over
    /// the life of the daemon.
    #[tokio::test]
    async fn batch_to_agent_drops_entry_on_stop_event() {
        let handle = DaemonServer::spawn();
        let batch_map = handle.batch_to_agent.clone();
        let client = DaemonClient::from_local(handle.client);

        let mut events = client
            .subscribe_output("retire-test-agent".into())
            .await
            .unwrap();

        // Send several batches and drain each to completion; after every
        // Stop the corresponding entry must be gone.
        for _ in 0..3 {
            let batch_id: SmolStr = new_snowflake_id();
            client
                .send_message(
                    batch_id.clone(),
                    Recipient::Direct("retire-test-agent".into()),
                    vec![ContentPart::Text("ping".into())],
                    test_origin(),
                )
                .await
                .unwrap();

            // Drain until we see the batch's Stop event.
            loop {
                let ev = events.recv().await.unwrap().unwrap();
                if ev.batch_id == batch_id && matches!(ev.event, WireTurnEvent::Stop(_)) {
                    break;
                }
            }
        }

        // Yield to let fan_out finalise any trailing removals before we peek.
        tokio::task::yield_now().await;

        assert!(
            batch_map.is_empty(),
            "batch_to_agent must be empty after all batches complete; has {} entries",
            batch_map.len()
        );
    }

    /// `BatchGuard` removes the entry when a spawned task exits early, even
    /// without emitting a `Stop` event. This tests the guard directly rather
    /// than through the full send path: spawn a minimal task that inserts an
    /// entry, holds a guard, then returns early. The entry must be gone.
    #[tokio::test]
    async fn batch_to_agent_removes_entry_when_task_exits_early() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let batch_to_agent: Arc<DashMap<BatchId, AgentId>> = Arc::new(DashMap::new());
        let batch_id: BatchId = "early-exit-batch".into();
        let agent_id: AgentId = "test-agent".into();

        // Simulate the actor inserting the entry before spawning the task.
        batch_to_agent.insert(batch_id.clone(), agent_id.clone());
        assert!(
            batch_to_agent.contains_key(&batch_id),
            "entry should be present after insert"
        );

        // Spawn a task that holds the guard and returns early (without emitting Stop).
        let map_clone = batch_to_agent.clone();
        let bid_clone = batch_id.clone();
        tokio::spawn(async move {
            let _guard = BatchGuard {
                map: map_clone,
                batch_id: bid_clone,
            };
            // Exit without emitting Stop — guard's Drop should clean up.
        })
        .await
        .unwrap();

        // Yield to ensure Drop has run and the entry is removed.
        tokio::task::yield_now().await;

        assert!(
            !batch_to_agent.contains_key(&batch_id),
            "BatchGuard must remove the entry on task exit; entry still present"
        );
    }

    /// `update_fronting` reverts in-memory state when the DB save fails.
    ///
    /// This test:
    /// 1. Creates a `ProjectMount` backed by an in-memory `ConstellationDb`.
    /// 2. Drops the `fronting_set` table to force the SQL write to fail.
    /// 3. Calls `update_fronting` with a mutator that changes the active set.
    /// 4. Asserts `Err(FrontingUpdateError::Save(_))` is returned.
    /// 5. Re-reads the fronting lock and asserts it matches the pre-mutation
    ///    state — verifying the rollback invariant.
    #[tokio::test]
    async fn update_fronting_reverts_on_save_failure() {
        use pattern_core::fronting::FrontingSet;
        use pattern_core::types::ids::PersonaId;
        use smol_str::SmolStr;

        // Open an in-memory DB and seed a FrontingSet we can observe.
        let db = Arc::new(
            pattern_db::ConstellationDb::open_in_memory().expect("in-memory DB must open"),
        );

        // Build the original fronting set (fallback = "alice").
        let original = FrontingSet::from_parts(
            vec![SmolStr::from("alice")],
            Some(SmolStr::from("alice")),
            pattern_core::fronting::RoutingTable::default(),
        );

        // Save it so the row exists before we break the table.
        {
            let mut conn = db.get().expect("pool connection must be available");
            pattern_db::queries::fronting::save_fronting_set(&mut conn, &original)
                .expect("initial save must succeed");
        }

        // Now drop the `fronting_set` table to make future writes fail.
        {
            let conn = db.get().expect("pool connection must be available");
            conn.execute_batch(
                "DROP TABLE IF EXISTS fronting_set; DROP TABLE IF EXISTS routing_rules;",
            )
            .expect("DROP TABLE must succeed");
        }

        // Build the fronting lock for the test. Calls the SAME free
        // function that `ProjectMount::update_fronting` uses in
        // production — no copy-paste — so a refactor of the
        // three-phase commit logic gets caught by this test.
        let fronting_lock = Arc::new(std::sync::RwLock::new(original.clone()));

        let update_result = super::update_fronting_inner(&fronting_lock, &db, |set| {
            set.active = vec![PersonaId::new("bob")];
            Ok(())
        })
        .await;

        // The save should have failed because the table was dropped.
        assert!(
            matches!(update_result, Err(FrontingUpdateError::Save(_))),
            "expected Save error after table drop; got: {update_result:?}"
        );

        // The in-memory state must be reverted to the original.
        let after_failure = fronting_lock
            .read()
            .expect("lock must not be poisoned")
            .clone();
        assert_eq!(
            after_failure.active, original.active,
            "in-memory active set must revert to pre-mutation state after save failure"
        );
        assert_eq!(
            after_failure.fallback, original.fallback,
            "in-memory fallback must revert to pre-mutation state after save failure"
        );
    }

    // ── DaemonFrontingCommitter (Phase 6 T5b) ─────────────────────────────────

    /// SDK-driven `Set` via `DaemonFrontingCommitter` persists to the DB AND
    /// emits a `FrontingChanged` event on the actor's event channel. Verifies
    /// AC8.* persistence + emission carryover from Phase 5.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_committer_persists_and_emits_on_set() {
        use pattern_runtime::sdk::handlers::fronting::FrontingCommitter;

        let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let fronting = Arc::new(std::sync::RwLock::new(
            pattern_core::fronting::FrontingSet::default(),
        ));
        let (event_tx, mut event_rx) = crate::bridge::new_event_channel();

        let committer = DaemonFrontingCommitter::new(
            fronting.clone(),
            db.clone(),
            event_tx,
            tokio::runtime::Handle::current(),
            None,
        );

        // Mutator: set active = [alice], fallback = Some(alice).
        let mutator: pattern_runtime::sdk::handlers::fronting::FrontingMutator =
            Box::new(|set: &mut pattern_core::fronting::FrontingSet| {
                set.active = vec![pattern_core::types::ids::PersonaId::new("alice")];
                set.fallback = Some(pattern_core::types::ids::PersonaId::new("alice"));
                Ok(())
            });

        // Run on a blocking thread because commit_sync calls block_on.
        let committer_clone = committer.clone();
        let new_set = tokio::task::spawn_blocking(move || {
            committer_clone
                .commit_sync(mutator)
                .expect("commit must succeed")
        })
        .await
        .unwrap();
        assert_eq!(new_set.active.len(), 1);
        assert_eq!(new_set.active[0].as_str(), "alice");

        // Verify in-memory state landed. The guard is scoped so it drops
        // before the subsequent `.await` point (clippy::await_holding_lock).
        {
            let after = fronting.read().unwrap();
            assert_eq!(after.active.len(), 1);
            assert_eq!(after.active[0].as_str(), "alice");
            assert_eq!(after.fallback.as_ref().map(|s| s.as_str()), Some("alice"));
        }

        // Verify the row landed in the DB.
        let conn = db.get().unwrap();
        let loaded = pattern_db::queries::fronting::load_fronting_set(&conn)
            .unwrap()
            .expect("DB must have a saved fronting row after commit");
        assert_eq!(loaded.active.len(), 1);
        assert_eq!(loaded.active[0].as_str(), "alice");

        // Verify a FrontingChanged event was sent on the channel.
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("must receive event within timeout")
            .expect("event channel must not be closed");
        assert_eq!(event.batch_id, "fronting");
        assert_eq!(event.agent_id, "daemon");
        match event.event {
            crate::protocol::WireTurnEvent::FrontingChanged {
                active, fallback, ..
            } => {
                assert_eq!(active, vec!["alice".to_string()]);
                assert_eq!(fallback, Some("alice".to_string()));
            }
            other => panic!("expected FrontingChanged event, got: {other:?}"),
        }
    }

    /// Phase 6 T6 followup — `migrate_seed_cache` ingests a fork-promote
    /// seed cache (snapshots + manifest.json) into a fresh `MemoryCache`
    /// under the new persona's id. Verifies:
    /// - manifest.json is read + parsed
    /// - each .loro snapshot lands as a block in the cache
    /// - blocks are owned by `persona_id` and persist to the DB
    /// - version mismatch surfaces a clear error
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migrate_seed_cache_imports_blocks_and_metadata() {
        use pattern_core::types::ids::PersonaId;
        use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
        use pattern_runtime::spawn::fork::{
            SEED_CACHE_MANIFEST_VERSION, SeedCacheManifest, SeedCacheManifestEntry,
        };

        // Build a source cache, create one block, export its snapshot.
        let src_db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let src_cache = pattern_memory::cache::MemoryCache::new(src_db);
        let src_agent = "fork-source";
        let label = "notes";
        let src_scope = Scope::global(src_agent);
        let create = pattern_core::types::block::BlockCreate::new(
            label,
            MemoryBlockType::Working,
            BlockSchema::text(),
        );
        let _doc = pattern_core::MemoryStore::create_block(&src_cache, &src_scope, create).unwrap();
        // Persist so the cached doc is committed.
        pattern_core::MemoryStore::persist_block(&src_cache, &src_scope, label).unwrap();

        let docs = src_cache.snapshot_cached_docs();
        assert_eq!(docs.len(), 1, "source cache should have one block");
        let snapshot = docs[0].export_snapshot().expect("export snapshot");

        // Lay out the seed cache directory + manifest.
        let tmp = tempfile::TempDir::new().unwrap();
        let persona_id_str = "promoted-persona";
        let cache_dir = tmp.path().join(format!("{persona_id_str}.cache"));
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("notes.loro"), &snapshot).unwrap();

        let manifest = SeedCacheManifest {
            version: SEED_CACHE_MANIFEST_VERSION,
            persona_id: persona_id_str.to_string(),
            entries: vec![SeedCacheManifestEntry {
                file: "notes.loro".to_string(),
                label: label.to_string(),
                schema: BlockSchema::text(),
                block_type: MemoryBlockType::Working,
            }],
        };
        std::fs::write(
            cache_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        // Target cache (mount cache).
        let target_db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let target_cache = pattern_memory::cache::MemoryCache::new(target_db);

        let persona_id: PersonaId = persona_id_str.into();
        let imported = super::migrate_seed_cache(&cache_dir, &persona_id, &target_cache)
            .await
            .expect("migrate_seed_cache must succeed");
        assert_eq!(imported, 1);

        // The block should now be queryable under the new agent_id.
        let loaded = pattern_core::MemoryStore::get_block(
            &target_cache,
            &Scope::global(persona_id_str),
            label,
        )
        .unwrap()
        .expect("imported block must be retrievable");
        assert_eq!(loaded.label(), label);
        // Doc's stored agent_id is the encoded scope db_key
        // (`global:<persona_id>`), per the Phase-1 Scope redesign.
        assert_eq!(loaded.agent_id(), Scope::global(persona_id_str).to_db_key());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migrate_seed_cache_rejects_version_mismatch() {
        use pattern_core::types::ids::PersonaId;
        use pattern_runtime::spawn::fork::SeedCacheManifest;

        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().join("p.cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let manifest = SeedCacheManifest {
            version: 999, // future
            persona_id: "p".to_string(),
            entries: vec![],
        };
        std::fs::write(
            cache_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let target_db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let target_cache = pattern_memory::cache::MemoryCache::new(target_db);
        let persona_id: PersonaId = "p".into();
        let err = super::migrate_seed_cache(&cache_dir, &persona_id, &target_cache)
            .await
            .expect_err("future version must error");
        assert!(err.contains("version mismatch"), "got: {err}");
    }

    /// `promote_persona_file` must return `Ok(target)` without calling
    /// `rename` or `copy`+`remove` when `draft_path == target` and the
    /// file exists. This is the step-6-failure + retry scenario: after a
    /// prior successful promote the registry's `config_path` points at the
    /// moved file, so `draft_path` passed on retry equals `target`. Calling
    /// `rename(A, A)` is a POSIX no-op but undefined on Windows; the
    /// copy+remove fallback would DELETE the file. The early-return guard
    /// prevents both.
    #[test]
    fn promote_persona_file_same_path_is_no_op() {
        use pattern_core::types::ids::PersonaId;

        let tmp = tempfile::TempDir::new().unwrap();
        let persona_id: PersonaId = "my-persona".into();

        // Write the file at the already-promoted location.
        let mount_path = tmp.path();
        let target_dir = mount_path.join("personas").join("@my-persona");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("persona.kdl");
        std::fs::write(&target, "name \"My Persona\"\n").unwrap();

        // Call promote_persona_file with draft_path == target.
        // Must succeed without touching the file content.
        let result = super::promote_persona_file(mount_path, &persona_id, &target);
        assert!(
            result.is_ok(),
            "same-path promote must succeed; got: {result:?}"
        );
        assert_eq!(result.unwrap(), target);

        // File must still exist with original content.
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content, "name \"My Persona\"\n");
    }

    /// `migrate_seed_cache` must be idempotent: calling it twice on the
    /// same input directory and target cache must succeed both times, and
    /// the block must be in the expected final state after both calls.
    ///
    /// This verifies the "skip `create_block`, re-apply `insert_from_snapshot`"
    /// logic described in the function comment: a partial-failure retry that
    /// finds the block already in the cache must not abort on a UNIQUE
    /// constraint violation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migrate_seed_cache_is_idempotent() {
        use pattern_core::types::ids::PersonaId;
        use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
        use pattern_runtime::spawn::fork::{
            SEED_CACHE_MANIFEST_VERSION, SeedCacheManifest, SeedCacheManifestEntry,
        };

        // Build a source block and export its snapshot.
        let src_db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let src_cache = pattern_memory::cache::MemoryCache::new(src_db);
        let label = "idempotent-notes";
        let src_scope = Scope::global("src-agent");
        let create = pattern_core::types::block::BlockCreate::new(
            label,
            MemoryBlockType::Working,
            BlockSchema::text(),
        );
        let _doc = pattern_core::MemoryStore::create_block(&src_cache, &src_scope, create).unwrap();
        pattern_core::MemoryStore::persist_block(&src_cache, &src_scope, label).unwrap();

        let docs = src_cache.snapshot_cached_docs();
        let snapshot = docs[0].export_snapshot().expect("export snapshot");

        // Lay out the seed cache directory.
        let tmp = tempfile::TempDir::new().unwrap();
        let persona_id_str = "idempotent-persona";
        let cache_dir = tmp.path().join(format!("{persona_id_str}.cache"));
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join(format!("{label}.loro")), &snapshot).unwrap();

        let manifest = SeedCacheManifest {
            version: SEED_CACHE_MANIFEST_VERSION,
            persona_id: persona_id_str.to_string(),
            entries: vec![SeedCacheManifestEntry {
                file: format!("{label}.loro"),
                label: label.to_string(),
                schema: BlockSchema::text(),
                block_type: MemoryBlockType::Working,
            }],
        };
        std::fs::write(
            cache_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let target_db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let target_cache = pattern_memory::cache::MemoryCache::new(target_db);
        let persona_id: PersonaId = persona_id_str.into();

        // First import.
        let first = super::migrate_seed_cache(&cache_dir, &persona_id, &target_cache)
            .await
            .expect("first migrate_seed_cache must succeed");
        assert_eq!(first, 1, "first import must import 1 block");

        // Second import on the same input — must not fail on duplicate block.
        let second = super::migrate_seed_cache(&cache_dir, &persona_id, &target_cache)
            .await
            .expect("second migrate_seed_cache must succeed (idempotency)");
        assert_eq!(second, 1, "second import must report 1 block processed");

        // Block must be accessible in the expected final state.
        let loaded = pattern_core::MemoryStore::get_block(
            &target_cache,
            &Scope::global(persona_id_str),
            label,
        )
        .unwrap()
        .expect("block must be retrievable after idempotent import");
        assert_eq!(loaded.label(), label);
    }

    // ── IMP-A: PromoteDraft step-6 (set_status Active) failure cleanup ─────────

    /// A `ConstellationRegistry` wrapper that delegates all methods except
    /// `set_status`, which always returns `Err(RegistryError::BackendUnavailable)`.
    ///
    /// Used by `promote_draft_step6_failure_cleans_up_sessions` to exercise
    /// the cleanup path after step-6 fails, without needing to corrupt the DB.
    #[derive(Debug)]
    struct FailSetStatusRegistry {
        inner: Arc<dyn pattern_core::ConstellationRegistry>,
    }

    #[async_trait::async_trait]
    impl pattern_core::ConstellationRegistry for FailSetStatusRegistry {
        async fn list(
            &self,
            scope: pattern_core::constellation::RegistryScope,
        ) -> Result<
            Vec<pattern_core::constellation::PersonaRecord>,
            pattern_core::constellation::RegistryError,
        > {
            self.inner.list(scope).await
        }

        async fn get(
            &self,
            id: &pattern_core::PersonaId,
        ) -> Result<
            Option<pattern_core::constellation::PersonaRecord>,
            pattern_core::constellation::RegistryError,
        > {
            self.inner.get(id).await
        }

        async fn find(
            &self,
            project: Option<&std::path::Path>,
            kind: Option<pattern_core::spawn::RelationshipKind>,
        ) -> Result<
            Vec<pattern_core::constellation::PersonaRecord>,
            pattern_core::constellation::RegistryError,
        > {
            self.inner.find(project, kind).await
        }

        async fn register(
            &self,
            record: pattern_core::constellation::PersonaRecord,
        ) -> Result<(), pattern_core::constellation::RegistryError> {
            self.inner.register(record).await
        }

        /// Always fails — simulates a transient registry backend failure
        /// after the session has been successfully opened (step-6 failure path).
        async fn set_status(
            &self,
            _id: &pattern_core::PersonaId,
            _status: pattern_core::constellation::PersonaStatus,
        ) -> Result<(), pattern_core::constellation::RegistryError> {
            Err(pattern_core::constellation::RegistryError::BackendUnavailable)
        }

        async fn set_config_path(
            &self,
            id: &pattern_core::PersonaId,
            config_path: Option<std::path::PathBuf>,
        ) -> Result<(), pattern_core::constellation::RegistryError> {
            self.inner.set_config_path(id, config_path).await
        }

        async fn add_relationship(
            &self,
            edge: pattern_core::constellation::RelationshipSpec,
        ) -> Result<bool, pattern_core::constellation::RegistryError> {
            self.inner.add_relationship(edge).await
        }

        async fn groups(
            &self,
            scope: pattern_core::constellation::RegistryScope,
        ) -> Result<
            Vec<pattern_core::constellation::PersonaGroup>,
            pattern_core::constellation::RegistryError,
        > {
            self.inner.groups(scope).await
        }

        async fn create_group(
            &self,
            name: String,
            project_id: Option<String>,
        ) -> Result<
            pattern_core::constellation::PersonaGroup,
            pattern_core::constellation::RegistryError,
        > {
            self.inner.create_group(name, project_id).await
        }
    }

    /// Verify that when step-6 (`set_status(Active)`) fails after a session is
    /// successfully opened, `PromoteDraft` cleans up all session state:
    ///
    /// 1. `PromoteDraft` returns `Err` with the documented user-facing message.
    /// 2. `self.sessions` no longer contains the agent_id.
    /// 3. `self.agent_to_mount` no longer contains the agent_id.
    /// 4. A second promote attempt (with a working registry) converges to Active.
    ///
    /// Requires tidepool-extract; skips when not available.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn promote_draft_step6_failure_cleans_up_sessions() {
        // Tidepool-extract is required to open a real session.
        if pattern_runtime::preflight::check().is_err() {
            return;
        }

        use crate::client::DaemonClient;
        use pattern_core::constellation::PersonaStatus;
        use pattern_memory::modes::in_repo;

        // Set up a real project mount.
        let tmp = tempfile::TempDir::new().unwrap();
        in_repo::init(tmp.path(), "test").expect("in_repo::init must succeed");

        let db = {
            let mounted =
                pattern_memory::mount::attach(tmp.path(), None).expect("test mount attach");
            let db = mounted.db.clone();
            drop(mounted);
            db
        };

        // Seed a draft persona.
        let agent_id = format!("step6-{}", new_id());
        let persona_id_str = "step6-test-persona";
        let drafts_dir = tmp.path().join("drafts");
        std::fs::create_dir_all(&drafts_dir).unwrap();
        let kdl_content = format!(
            r#"name "step6-test-{agent_id}"
agent-id "{agent_id}"
system-prompt "Step-6 test persona."
model provider="anthropic" model-id="claude-sonnet-4-6" {{
    temperature 0.0
    max-tokens 256
}}
context {{
    compress-check-message-floor 100
}}
"#
        );
        let kdl_path = drafts_dir.join(format!("{persona_id_str}.kdl"));
        std::fs::write(&kdl_path, kdl_content).unwrap();

        let raw_registry: Arc<dyn pattern_core::ConstellationRegistry> =
            Arc::new(pattern_db::ConstellationRegistryDb::new(db.clone()));
        let mut record = pattern_core::constellation::PersonaRecord::new(
            persona_id_str,
            format!("step6-test-{persona_id_str}"),
            PersonaStatus::Draft,
        );
        record.config_path = Some(kdl_path.clone());
        raw_registry
            .register(record)
            .await
            .expect("seed register must succeed");

        // Wrap in FailSetStatusRegistry so step-6 fails after the session opens.
        let failing_registry: Arc<dyn pattern_core::ConstellationRegistry> =
            Arc::new(FailSetStatusRegistry {
                inner: raw_registry.clone(),
            });

        let session_config = {
            let port_registry = Arc::new(
                pattern_runtime::port_registry::PortRegistryImpl::with_runtime_ports(
                    &tokio::runtime::Handle::current(),
                ),
            );
            SessionConfig {
                sdk: pattern_runtime::SdkLocation::default(),
                provider: Arc::new(pattern_runtime::NopProviderClient),
                port_registry,
            }
        };

        let handle = DaemonServer::spawn_with_config_and_registry(session_config, failing_registry);
        let client = DaemonClient::from_local(handle.client.clone());

        // InitSession to wire the mount.
        let _ = client
            .init_session(tmp.path().to_path_buf(), "default".into())
            .await
            .expect("InitSession must succeed");

        // Promote — must fail with step-6 error.
        let resp = client.promote_draft(persona_id_str.into()).await.unwrap();
        assert!(
            !resp.success,
            "step-6 failure must surface as promote failure"
        );
        assert!(
            resp.error
                .as_deref()
                .map(|e| e.contains("failed to update registry status to Active")
                    || e.contains("session has been closed"))
                .unwrap_or(false),
            "expected step-6 error message; got: {:?}",
            resp.error
        );

        // Give the cleanup a moment to propagate.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // `sessions` must not contain the agent_id after cleanup.
        let agent_key: AgentId = agent_id.as_str().into();
        assert!(
            !handle.sessions.contains_key(&agent_key),
            "sessions must be cleaned up after step-6 failure"
        );

        // `agent_to_mount` must not contain the agent_id after cleanup.
        assert!(
            !handle.agent_to_mount.contains_key(&agent_key),
            "agent_to_mount must be cleaned up after step-6 failure"
        );

        // A second promote with a working registry must succeed.
        let session_config2 = {
            let port_registry = Arc::new(
                pattern_runtime::port_registry::PortRegistryImpl::with_runtime_ports(
                    &tokio::runtime::Handle::current(),
                ),
            );
            SessionConfig {
                sdk: pattern_runtime::SdkLocation::default(),
                provider: Arc::new(pattern_runtime::NopProviderClient),
                port_registry,
            }
        };
        let handle2 =
            DaemonServer::spawn_with_config_and_registry(session_config2, raw_registry.clone());
        let client2 = DaemonClient::from_local(handle2.client);
        let _ = client2
            .init_session(tmp.path().to_path_buf(), "default".into())
            .await
            .expect("second InitSession must succeed");

        let second = client2.promote_draft(persona_id_str.into()).await.unwrap();
        assert!(
            second.success,
            "second promote with working registry must succeed; got: {:?}",
            second.error
        );

        // Persona must be Active.
        let after = raw_registry
            .get(&persona_id_str.into())
            .await
            .unwrap()
            .expect("persona must exist after successful retry");
        assert_eq!(
            after.status,
            PersonaStatus::Active,
            "persona must be Active after successful retry"
        );
    }

    /// `fronting_set()` exposed by the committer is the SAME `Arc` it persists
    /// against — read and write paths cannot drift.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_committer_exposes_same_arc_it_persists() {
        use pattern_runtime::sdk::handlers::fronting::FrontingCommitter;

        let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let fronting = Arc::new(std::sync::RwLock::new(
            pattern_core::fronting::FrontingSet::default(),
        ));
        let (event_tx, _event_rx) = crate::bridge::new_event_channel();
        let committer = DaemonFrontingCommitter::new(
            fronting.clone(),
            db,
            event_tx,
            tokio::runtime::Handle::current(),
            None,
        );

        // Pointer-equality on the Arc backing storage: the committer's
        // `fronting_set()` accessor and the externally-held lock must be
        // the SAME allocation.
        assert!(
            Arc::ptr_eq(committer.fronting_set(), &fronting),
            "committer's fronting_set() must be the same Arc as the externally-held lock"
        );
    }
}
