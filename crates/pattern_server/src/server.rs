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

use std::time::Instant;

use irpc::{Client, WithChannels};
use pattern_core::traits::turn_sink::{TurnEvent, TurnSink};
use pattern_core::types::provider::ContentPart;
use pattern_core::types::turn::StopReason;
use tracing::warn;

use crate::bridge::{EventRx, EventTx, TurnSinkBridge, new_event_channel};
use crate::protocol::*;

/// The daemon server actor.
///
/// Receives [`PatternMessage`]s from clients (local or remote) and events from
/// [`TurnSinkBridge`]s. Fans events out to all subscribers that match the
/// event's `agent_id`.
pub struct DaemonServer {
    recv: tokio::sync::mpsc::Receiver<PatternMessage>,
    event_rx: EventRx,
    event_tx: EventTx,
    /// Active subscribers: `(agent_id_filter, irpc_mpsc_sender)`.
    /// The `irpc::channel::mpsc::Sender` is the server-side half of the
    /// streaming RPC — the client holds the corresponding `Receiver`.
    subscribers: Vec<(AgentId, irpc::channel::mpsc::Sender<TaggedTurnEvent>)>,
    started_at: Instant,
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
    /// Spawn the daemon server actor on the tokio runtime.
    ///
    /// Returns a [`DaemonHandle`] with an irpc [`Client`] connected to the
    /// actor via an in-process channel. The caller may extract the local
    /// sender from the client (via `as_local()`) to set up a QUIC listener.
    pub fn spawn() -> DaemonHandle {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(64);
        let (event_tx, event_rx) = new_event_channel();
        let server = Self {
            recv: msg_rx,
            event_rx,
            event_tx,
            subscribers: Vec::new(),
            started_at: Instant::now(),
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

    /// Fan out a tagged event to all subscribers whose `agent_id` filter
    /// matches the event's `agent_id`. Disconnected subscribers (send
    /// returns error) are removed in-place.
    async fn fan_out(&mut self, event: TaggedTurnEvent) {
        let mut i = 0;
        while i < self.subscribers.len() {
            let (ref agent_filter, ref tx) = self.subscribers[i];
            if *agent_filter == event.agent_id && tx.send(event.clone()).await.is_err() {
                // Subscriber disconnected — remove.
                warn!(agent_id = %agent_filter, "subscriber disconnected, removing");
                self.subscribers.swap_remove(i);
                continue;
            }
            i += 1;
        }
    }

    /// Dispatch a single incoming message.
    async fn handle(&mut self, msg: PatternMessage) {
        match msg {
            PatternMessage::SendMessage(req) => {
                let WithChannels { tx, inner, .. } = req;
                let batch_id = inner.batch_id.clone();

                // Acknowledge receipt — the client unblocks immediately.
                let _ = tx.send(()).await;

                // Build a TurnSinkBridge for this batch to route events
                // back through the actor's fan-out mechanism.
                let bridge = TurnSinkBridge::new(batch_id, inner.agent_id, self.event_tx.clone());

                // Echo stub: extract text from parts, emit "echo: {text}" + Stop.
                // Real session integration is wired in Task 9.
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
            }
            PatternMessage::SubscribeOutput(req) => {
                let WithChannels { tx, inner, .. } = req;
                // Register this subscriber. The actor's `fan_out()` method
                // will forward matching events to this irpc mpsc sender.
                self.subscribers.push((inner.agent_id, tx));
            }
            PatternMessage::ListAgents(req) => {
                let WithChannels { tx, .. } = req;
                // Stub: return empty agent list. Wired to runtime in Task 9.
                let _ = tx.send(vec![]).await;
            }
            PatternMessage::GetStatus(req) => {
                let WithChannels { tx, .. } = req;
                let status = RuntimeStatus {
                    agent_count: 0,
                    active_batch_count: 0,
                    uptime_secs: self.started_at.elapsed().as_secs(),
                };
                let _ = tx.send(status).await;
            }
            PatternMessage::CancelBatch(req) => {
                let WithChannels { tx, .. } = req;
                // Stub: acknowledge but do nothing. Wired in Task 9.
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
