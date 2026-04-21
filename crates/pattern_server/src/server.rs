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
use std::time::Instant;

use dashmap::DashMap;
use irpc::{Client, WithChannels};
use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::traits::turn_sink::{DisplayKind, TurnEvent, TurnSink};
use pattern_core::types::ids::{
    AgentId as CoreAgentId, BatchId as CoreBatchId, MessageId, new_id, new_snowflake_id,
};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Partner, Sphere};
use pattern_core::types::provider::{ChatMessage, ContentPart};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::types::turn::{StopReason, TurnInput};
use pattern_runtime::sdk::SdkLocation;
use pattern_runtime::session::TidepoolSession;
use smol_str::SmolStr;
use tracing::{info, warn};

use crate::bridge::{EventRx, EventTx, MultiplexSink, TurnSinkBridge, new_event_channel};
use crate::protocol::*;

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
    pub cache: Arc<dyn MemoryStore>,
    /// Constellation database handle (memory.db + messages.db).
    pub db: Arc<pattern_db::ConstellationDb>,
    /// Mount root directory.
    pub mount_path: PathBuf,
    /// Keeps the `MountedStore` alive for RAII (watcher, backup scheduler).
    _mounted: pattern_memory::mount::MountedStore,
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
    /// Minted once at spawn time so all messages from this session carry the
    /// same `Author::Partner` identity, rather than minting a fresh ID per message.
    partner_id: SmolStr,
}

/// Handle returned by [`DaemonServer::spawn`].
///
/// Holds an irpc [`Client`] that can make requests to the running actor.
/// For the daemon binary, this client's local sender is also used to set up
/// the QUIC listener (via `as_local()`).
pub struct DaemonHandle {
    /// The irpc client for making requests to the daemon actor.
    pub client: Client<PatternProtocol>,
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
        };
        tokio::spawn(server.run());
        DaemonHandle {
            client: Client::local(msg_tx),
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
        tracing::debug!(
            agent_id = %event.agent_id,
            batch_id = %event.batch_id,
            event = ?event.event,
            "fan_out: dispatching event"
        );
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
                let agent_id = inner.agent_id.clone();

                // Acknowledge receipt — the client unblocks immediately.
                let _ = tx.send(()).await;

                if self.echo {
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
                } else if let Some(mount) = &self.current_mount {
                    // Real session mode: spawn a task to handle session open
                    // and step. The actor loop stays responsive — session open
                    // may trigger tidepool Haskell compilation (5-10s).
                    let sessions = self.sessions.clone();
                    let session_locks = self.session_locks.clone();
                    let config = self.session_config.clone().unwrap();
                    let event_tx = self.event_tx.clone();
                    let partner_id = self.partner_id.clone();
                    let mount = mount.clone();

                    tokio::spawn(async move {
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
                        let session_agent_id = agent_session.session.agent_id().to_string();
                        let turn_input = build_turn_input(&inner, &partner_id, &session_agent_id);

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
                } else {
                    // No mount available — send InitSession first.
                    let bridge = TurnSinkBridge::new(batch_id, agent_id, self.event_tx.clone());
                    bridge.emit(TurnEvent::Display {
                        kind: DisplayKind::Note,
                        text: "no project mounted — send InitSession first".into(),
                    });
                    bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                }
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
            PatternMessage::GetStatus(req) => {
                let WithChannels { tx, .. } = req;
                let status = RuntimeStatus {
                    agent_count: self.sessions.len(),
                    active_batch_count: 0,
                    uptime_secs: self.started_at.elapsed().as_secs(),
                };
                let _ = tx.send(status).await;
            }
            PatternMessage::CancelBatch(req) => {
                let WithChannels { tx, .. } = req;
                // TODO(phase-2): wire cancellation — call session.cancel_batch(inner.batch_id)
                // using TidepoolSession's CancelState so the TUI's Esc key can stop a running
                // step. For phase 1, we acknowledge immediately and take no other action.
                let _ = tx.send(()).await;
            }
            PatternMessage::RunCommand(req) => {
                let WithChannels { tx, inner, .. } = req;
                let result = CommandResult {
                    success: false,
                    output: format!("command not yet implemented: {}", inner.command),
                };
                let _ = tx.send(result).await;
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

        // Slow path: mount the project.
        let mounted = pattern_memory::mount::attach(&canonical)
            .map_err(|e| format!("failed to attach mount at {}: {e}", canonical.display()))?;

        let mount = Arc::new(ProjectMount {
            cache: mounted.cache.clone(),
            db: mounted.db.clone(),
            mount_path: mounted.mount_path.clone(),
            _mounted: mounted,
        });

        self.project_mounts.insert(canonical, mount.clone());
        Ok(mount)
    }
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

    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &config.sdk,
        project_mount.cache.clone(),
        config.provider.clone(),
        project_mount.db.clone(),
        sink_dyn,
        None, // prelude_dir — SDK bundles the prelude internally.
        Some(project_mount.mount_path.clone()),
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
/// Mints fresh turn and batch IDs, wraps the client's content parts into
/// a user [`ChatMessage`], and sets the origin to `Author::Partner` using
/// the stable `partner_id` minted once at server spawn time.
fn build_turn_input(msg: &AgentMessage, partner_id: &SmolStr, session_agent_id: &str) -> TurnInput {
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
        origin: MessageOrigin::new(
            Author::Partner(Partner {
                user_id: partner_id.clone(),
            }),
            Sphere::Private,
        ),
        messages: vec![message],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DaemonClient;
    use pattern_core::types::ids::new_snowflake_id;
    use smol_str::SmolStr;

    #[tokio::test]
    async fn send_message_returns_batch_id_and_emits_events() {
        let handle = DaemonServer::spawn();
        let client = DaemonClient::from_local(handle.client);

        // Subscribe before sending so we don't miss events.
        let mut events = client.subscribe_output("test-agent".into()).await.unwrap();

        // Send a message (client mints the batch_id).
        let batch_id: SmolStr = new_snowflake_id();
        client
            .send_message(
                batch_id.clone(),
                "test-agent".into(),
                vec![ContentPart::Text("hello".into())],
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
                "test-agent".into(),
                vec![ContentPart::Text("shared".into())],
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
        assert!(info.error.is_none());
    }
}
