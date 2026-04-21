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

use irpc::{Client, WithChannels};
use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::traits::turn_sink::{TurnEvent, TurnSink};
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
pub struct SessionConfig {
    /// SDK location for the Haskell eval worker.
    pub sdk: SdkLocation,
    /// Memory store (typically an `Arc<MemoryCache>` from a mounted store).
    pub memory_store: Arc<dyn MemoryStore>,
    /// LLM provider client (e.g. `PatternGatewayClient`).
    pub provider: Arc<dyn ProviderClient>,
    /// Constellation database handle (memory.db + messages.db).
    pub db: Arc<pattern_db::ConstellationDb>,
    /// Default persona for new sessions. Loaded from a persona KDL file.
    pub persona: PersonaSnapshot,
    /// Optional mount path for scope wiring and lib/ include-path extension.
    pub mount_path: Option<PathBuf>,
}

/// The daemon server actor.
///
/// Receives [`PatternMessage`]s from clients (local or remote) and events from
/// [`TurnSinkBridge`]s. Fans events out to all subscribers that match the
/// event's `agent_id`.
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
    /// Open sessions keyed by agent ID. Each session uses a [`MultiplexSink`]
    /// whose inner sink is swapped to a per-batch [`TurnSinkBridge`] before
    /// each `step_with_agent_loop` call.
    sessions: HashMap<AgentId, (Arc<TidepoolSession>, Arc<MultiplexSink>)>,
    /// Per-agent mutex that serializes the `set_inner` + `spawn` sequence.
    ///
    /// Without this lock, two concurrent `SendMessage` calls for the same
    /// agent could race: the first call's `set_inner` might be overwritten by
    /// the second before the first task begins executing, causing that step's
    /// events to be tagged with the wrong `batch_id`.
    session_locks: HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>,
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
            sessions: HashMap::new(),
            session_locks: HashMap::new(),
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

    /// Get or open a session for the given agent. In real mode, opens a
    /// [`TidepoolSession`] via `open_with_agent_loop` on first use and
    /// caches it. The session is opened with a [`MultiplexSink`] whose
    /// inner sink is swapped per-batch before each step.
    async fn get_or_open_session(
        &mut self,
        agent_id: &AgentId,
    ) -> Result<(Arc<TidepoolSession>, Arc<MultiplexSink>), String> {
        if let Some(entry) = self.sessions.get(agent_id) {
            return Ok(entry.clone());
        }

        let config = self
            .session_config
            .as_ref()
            .ok_or_else(|| "session config not available (echo mode?)".to_string())?;

        let mux_sink = Arc::new(MultiplexSink::new());
        let sink_dyn: Arc<dyn TurnSink> = mux_sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            config.persona.clone(),
            &config.sdk,
            config.memory_store.clone(),
            config.provider.clone(),
            config.db.clone(),
            sink_dyn,
            None, // prelude_dir — SDK bundles the prelude internally.
            config.mount_path.clone(),
        )
        .await
        .map_err(|e| format!("failed to open session for {agent_id}: {e}"))?;

        let session = Arc::new(session);
        self.sessions
            .insert(agent_id.clone(), (session.clone(), mux_sink.clone()));
        info!(agent_id = %agent_id, "opened new session");
        Ok((session, mux_sink))
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
                } else {
                    // Real session mode: open or reuse session, drive step.
                    let event_tx = self.event_tx.clone();
                    let partner_id = self.partner_id.clone();
                    match self.get_or_open_session(&agent_id).await {
                        Ok((session, mux_sink)) => {
                            // Acquire (or create) the per-agent serialization lock.
                            // This serializes the set_inner + spawn sequence so that
                            // two concurrent SendMessage calls for the same agent
                            // cannot interleave their bridge swaps, which would cause
                            // one batch's events to be tagged with the other's batch_id.
                            let agent_lock = self
                                .session_locks
                                .entry(agent_id.clone())
                                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                                .clone();

                            // Build TurnInput using the persona's agent_id for
                            // correct memory block ownership — not the client's
                            // routing key (which may be "default").
                            let persona_agent_id = self.session_config
                                .as_ref()
                                .map(|c| c.persona.agent_id.to_string())
                                .unwrap_or_else(|| agent_id.to_string());
                            let turn_input = build_turn_input(&inner, &partner_id, &persona_agent_id);

                            // Drive step in a background task so the actor
                            // remains responsive to other messages.
                            tokio::spawn(async move {
                                // Hold the per-agent lock for the entire set_inner
                                // + step sequence. This ensures only one batch at a
                                // time drives the agent in phase 1.
                                let _guard = agent_lock.lock().await;

                                // Build a per-batch bridge and swap it into the
                                // session's MultiplexSink so events from this step
                                // are tagged with the correct batch_id.
                                let bridge = Arc::new(TurnSinkBridge::new(
                                    batch_id.clone(),
                                    agent_id.clone(),
                                    event_tx,
                                ));
                                mux_sink.set_inner(bridge.clone());

                                match session.step_with_agent_loop(turn_input).await {
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
                                        bridge.emit(TurnEvent::Text(format!("error: {e}")));
                                        bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            warn!(agent_id = %agent_id, error = %e, "failed to open session");
                            let bridge = TurnSinkBridge::new(batch_id, agent_id, event_tx);
                            bridge.emit(TurnEvent::Text(format!("error: {e}")));
                            bridge.emit(TurnEvent::Stop(StopReason::EndTurn));
                        }
                    }
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
                    .keys()
                    .map(|id| AgentInfo {
                        agent_id: id.clone(),
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
        }
    }
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
        assert!(matches!(ev.event, TurnEvent::Text(ref s) if s.contains("hello")));
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
}
