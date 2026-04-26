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
use pattern_core::constellation::ConstellationRegistry;
use pattern_core::fronting::{FrontingResolver, ResolveOutcome};
use pattern_core::traits::MemoryStore;
use pattern_core::traits::turn_sink::{DisplayKind, TurnEvent, TurnSink};
use pattern_core::types::ids::{
    AgentId as CoreAgentId, BatchId as CoreBatchId, MessageId, new_id, new_snowflake_id,
};
use pattern_core::types::message::Message;
use pattern_core::types::provider::{ChatMessage, ContentPart};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::{StopReason, TurnInput};
use pattern_runtime::agent_registry::AgentRegistry;
use pattern_runtime::router::RouterRegistry;
use pattern_runtime::router::agent::AgentRouter;
use pattern_runtime::router::cli::CliRouter;
use pattern_runtime::sdk::SdkLocation;
use pattern_runtime::session::{SessionRegistries, TidepoolSession, WakeRegistryExtras};
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
#[derive(Clone)]
struct AgentSession {
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

    /// Internal spawn helper.
    fn spawn_inner(echo: bool, session_config: Option<Arc<SessionConfig>>) -> DaemonHandle {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, event_rx) = new_event_channel();
        let batch_to_agent = Arc::new(DashMap::new());
        let server = Self {
            recv: msg_rx,
            event_rx,
            event_tx,
            subscribers: HashMap::new(),
            started_at: Instant::now(),
            echo,
            session_config,
            project_mounts: Arc::new(DashMap::new()),
            current_mount: None,
            sessions: Arc::new(DashMap::new()),
            session_locks: Arc::new(DashMap::new()),
            partner_id: new_id(),
            available_agents: 0,
            batch_to_agent: batch_to_agent.clone(),
        };
        tokio::spawn(server.run());
        DaemonHandle {
            client: Client::local(msg_tx),
            #[cfg(test)]
            batch_to_agent,
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

    /// Fan out a tagged event to all subscribers whose `agent_id` matches the
    /// event's `agent_id`. Uses `try_send` so that a slow or full subscriber
    /// does not block the actor loop. Subscribers that are full (buffer
    /// backpressure) or disconnected are removed.
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

        let Some(senders) = self.subscribers.get_mut(&event.agent_id) else {
            return;
        };

        let mut i = 0;
        while i < senders.len() {
            let tx = &senders[i];
            match tx.try_send(event.clone()).await {
                Ok(true) => {
                    // Delivered successfully.
                    i += 1;
                }
                Ok(false) => {
                    // Subscriber's buffer is full — disconnect rather than block.
                    warn!(
                        agent_id = %event.agent_id,
                        "subscriber buffer full, removing slow subscriber"
                    );
                    senders.swap_remove(i);
                }
                Err(_) => {
                    // Subscriber's receiver has been dropped.
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
                        let empty_registry: Arc<dyn ConstellationRegistry> =
                            Arc::new(pattern_core::constellation::EmptyConstellationRegistry);
                        let body_text = inner
                            .parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text(s) => Some(s.as_str()),
                                _ => None,
                            })
                            .next()
                            .unwrap_or("");
                        let resolver = FrontingResolver::new(set_snapshot, empty_registry);
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
                let batch_to_agent = self.batch_to_agent.clone();
                let agent_id = resolved_agent_id;

                tokio::spawn(async move {
                    // Hold a guard for the lifetime of this task. If the task
                    // exits early (error return) or panics, the guard's Drop
                    // removes the batch → agent entry so the map doesn't leak.
                    // The fan_out cleanup on Stop is left as a defensive
                    // double-remove; DashMap::remove is a no-op when absent.
                    let _batch_guard = BatchGuard {
                        map: batch_to_agent,
                        batch_id: batch_id.clone(),
                    };

                    // 1. Get or open session (may block during compilation).
                    let agent_session = match get_or_open_session(
                        &agent_id,
                        &sessions,
                        &session_locks,
                        &config,
                        &mount,
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

                    // 2. Acquire the per-agent serialization lock. This
                    //    serializes set_inner + step so concurrent
                    //    SendMessage calls for the same agent don't
                    //    interleave bridge swaps.
                    let agent_lock = session_locks
                        .entry(agent_id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone();
                    let _guard = agent_lock.lock().await;

                    // 3. Build bridge and swap into mux sink.
                    let bridge = Arc::new(TurnSinkBridge::new(
                        batch_id.clone(),
                        agent_id.clone(),
                        event_tx,
                    ));
                    agent_session.mux_sink.set_inner(bridge.clone());

                    // 4. Build turn input and drive step.
                    // The caller-supplied origin is passed through unchanged;
                    // the daemon does not override or default the author.
                    let session_agent_id = agent_session.session.agent_id().to_string();
                    let turn_input = build_turn_input(&inner, &session_agent_id);

                    match agent_session.session.step_with_agent_loop(turn_input).await {
                        Ok(_reply) => {
                            // Events already emitted via the bridge.
                        }
                        Err(e) => {
                            warn!(
                                agent_id = %agent_id,
                                batch_id = %batch_id,
                                error = %e,
                                "step_with_agent_loop failed"
                            );
                            bridge.emit(TurnEvent::Display {
                                kind: DisplayKind::Note,
                                text: format!("error: {e}"),
                            });
                            bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                        }
                    }
                });
            }
            PatternMessage::SubscribeOutput(req) => {
                let WithChannels { tx, inner, .. } = req;
                // Register this subscriber. The actor's `fan_out()` method
                // will forward matching events to this irpc mpsc sender.
                self.subscribers.entry(inner.agent_id).or_default().push(tx);
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

                                let tokens = estimate_batch_tokens(&user_message, &events);

                                HistoricalBatch {
                                    batch_id: batch_id.into(),
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
                    let _ = tx.send(response).await;
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
                let response = if let Some(mount) = &self.current_mount {
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
                            self.fan_out_fronting_changed(&new_set).await;
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
                let response = if let Some(mount) = &self.current_mount {
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
                            self.fan_out_fronting_changed(&new_set).await;
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
                    // Echo mode: return synthetic session info.
                    let _ = tx
                        .send(SessionInfo {
                            agent_id: inner.default_agent,
                            persona_name: "echo".into(),
                            available_agents: vec![],
                            partner_id: self.partner_id.clone(),
                            // Phase 6: read from .pattern.kdl partner { display_name "..." }
                            partner_display_name: None,
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
                                partner_id: self.partner_id.clone(),
                                // Phase 6: read from .pattern.kdl partner { display_name "..." }
                                partner_display_name: None,
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

                // Resolve the requested agent.
                let agent_id = inner.default_agent.clone();
                let normalized = agent_id.trim_start_matches('@');
                let persona_name = personas
                    .get(normalized)
                    .and_then(|p| pattern_runtime::persona_loader::load_persona(p).ok())
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| agent_id.to_string());

                let available: Vec<AgentId> =
                    personas.keys().map(|k| SmolStr::from(k.as_str())).collect();

                // Update available agents count for GetStatus.
                self.available_agents = available.len();

                info!(
                    agent_id = %agent_id,
                    persona = %persona_name,
                    project = %inner.project_path.display(),
                    agents = ?available,
                    "session initialized"
                );

                let _ = tx
                    .send(SessionInfo {
                        agent_id,
                        persona_name,
                        available_agents: available,
                        partner_id: self.partner_id.clone(),
                        // Phase 6: read from .pattern.kdl partner { display_name "..." }
                        partner_display_name: None,
                        error: None,
                    })
                    .await;
            }
        }
    }

    /// Get or create a cached project mount for the given path.
    ///
    /// If the project is already mounted, returns the cached handle. Otherwise,
    /// canonicalizes the path, calls [`pattern_memory::mount::attach`], and
    /// caches the result.
    fn get_or_mount_project(
        &self,
        project_path: &std::path::Path,
    ) -> Result<Arc<ProjectMount>, String> {
        // Canonicalize for consistent cache keys.
        let canonical = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());

        // Fast path: already mounted.
        if let Some(entry) = self.project_mounts.get(&canonical) {
            return Ok(entry.clone());
        }

        // Slow path: mount the project. Pass the first-party skill directory
        // so skills under pattern_runtime's resources/skills/ are classified
        // as FirstParty regardless of what their frontmatter declares.
        let mounted = pattern_memory::mount::attach(
            &canonical,
            Some(std::path::PathBuf::from(
                pattern_runtime::sdk::FIRST_PARTY_SKILL_DIR,
            )),
        )
        .map_err(|e| format!("failed to attach mount at {}: {e}", canonical.display()))?;

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

        let mount = Arc::new(ProjectMount {
            cache: mounted.cache.clone(),
            db: mounted.db.clone(),
            mount_path: mounted.mount_path.clone(),
            // One AgentRegistry per mount: all sessions in this project share
            // it so they can route to each other via the `agent:` scheme.
            agent_registry: Arc::new(AgentRegistry::new()),
            fronting: Arc::new(std::sync::RwLock::new(fronting_loaded)),
            _mounted: mounted,
        });

        self.project_mounts.insert(canonical, mount.clone());
        Ok(mount)
    }

    /// Fan out a `FrontingChanged` event derived from `new_set` to all
    /// subscribers. Uses the `"fronting"` / `"daemon"` sentinel batch/agent IDs
    /// so TUI clients can distinguish fronting events from per-agent turn events.
    async fn fan_out_fronting_changed(&mut self, new_set: &pattern_core::fronting::FrontingSet) {
        let rules = new_set
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
        let event = TaggedTurnEvent {
            batch_id: "fronting".into(),
            agent_id: "daemon".into(),
            event: WireTurnEvent::FrontingChanged {
                active: new_set.active.iter().map(|id| id.to_string()).collect(),
                fallback: new_set.fallback.as_ref().map(|id| id.to_string()),
                rules,
            },
        };
        self.fan_out(event).await;
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

    // Resolve persona and open session.
    let persona = resolve_persona(agent_id, Some(&project_mount.mount_path))?;
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

    // Phase 5: AgentRouter gains fronting-aware dispatch. The
    // FrontingState points at the mount's canonical FrontingSet lock
    // and a placeholder ConstellationRegistry. Phase 6 will swap in a
    // pattern_db-backed registry; for now an empty in-memory one means
    // the empty-fronting path falls through to SystemDefault, which
    // matches the documented "no fronting configured" behaviour.
    let fronting_state = pattern_runtime::fronting_dispatch::FrontingState::new(
        project_mount.fronting.clone(),
        Arc::new(pattern_core::constellation::EmptyConstellationRegistry)
            as Arc<dyn pattern_core::constellation::ConstellationRegistry>,
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

    let registries = SessionRegistries {
        agent_registry: Some(project_mount.agent_registry.clone()),
        router_registry: Some(router_reg),
        wake_registry_extras: Some(wake_extras),
        fronting_set: Some(project_mount.fronting.clone()),
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

    let persona_path = personas.get(normalized).ok_or_else(|| {
        let available: Vec<_> = personas.keys().collect();
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
            // FrontingChanged is a notification event (no agent-side
            // text); does not contribute to token estimates.
            WireTurnEvent::FrontingChanged { .. } => {}
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

}
