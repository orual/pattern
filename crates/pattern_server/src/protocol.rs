//! IRPC service contract for the Pattern daemon.
//!
//! Defines the [`PatternProtocol`] enum that the irpc `#[rpc_requests]` macro
//! expands into a `PatternMessage` enum consumed by the daemon server actor.
//!
//! Transport serialization uses postcard (irpc's wire format). The test suite
//! uses serde_json for round-trip validation — serde_json and postcard both
//! honour the same `Serialize`/`Deserialize` impls, so this is correct.

use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use pattern_core::traits::turn_sink::TurnEvent;
use pattern_core::types::provider::ContentPart;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Unique identifier for a batch of turn events.
///
/// Client-minted using [`pattern_core::types::ids::new_snowflake_id`].
/// The daemon tags all [`TaggedTurnEvent`]s for a given exchange with this
/// ID so that concurrent batches can be rendered independently in the TUI.
pub type BatchId = SmolStr;

/// Identifier for a running agent.
pub type AgentId = SmolStr;

/// A message from a TUI client to an agent.
///
/// The client mints the `batch_id` (a snowflake) before sending. The daemon
/// uses it to correlate every [`TaggedTurnEvent`] emitted during this exchange
/// back to the originating batch, enabling concurrent rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Client-minted batch ID (snowflake). The daemon uses this to tag all
    /// TurnEvents for this exchange, enabling concurrent batch rendering.
    pub batch_id: BatchId,
    /// Target agent.
    pub agent_id: AgentId,
    /// Message content parts — text, images, binary attachments.
    /// The daemon wraps these into a `ChatMessage::user()` when constructing `TurnInput`.
    pub parts: Vec<ContentPart>,
}

/// Request to subscribe to an agent's turn event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSubscription {
    /// Agent whose events the subscriber wants to receive.
    pub agent_id: AgentId,
}

/// A [`TurnEvent`] tagged with the batch and agent that produced it.
///
/// The daemon's fan-out logic emits one of these per event into every
/// subscriber channel that matches the `agent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedTurnEvent {
    /// Which batch (exchange) this event belongs to.
    pub batch_id: BatchId,
    /// Which agent emitted this event.
    pub agent_id: AgentId,
    /// The underlying turn event.
    pub event: TurnEvent,
}

/// Static metadata about a running agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: AgentId,
    pub persona_name: String,
    /// Batch IDs for exchanges currently in progress.
    pub active_batches: Vec<BatchId>,
}

/// Snapshot of overall daemon runtime health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub agent_count: usize,
    pub active_batch_count: usize,
    pub uptime_secs: u64,
}

/// Request payload for [`PatternProtocol::ListAgents`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAgentsRequest;

/// Request payload for [`PatternProtocol::GetStatus`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStatusRequest;

/// A slash-command invocation forwarded from the TUI.
///
/// Full typed command dispatch (e.g. `/switch-persona`) will be added when
/// multi-agent fronting is implemented. For now all commands route through
/// this generic RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommand {
    pub command: String,
    pub args: Vec<String>,
}

/// Result of a [`SlashCommand`] execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
}

/// The Pattern daemon IRPC service contract.
///
/// The `#[rpc_requests]` macro generates a `PatternMessage` enum and the
/// required [`irpc::Service`] / [`irpc::RemoteService`] trait impls.
/// The daemon server actor receives `PatternMessage` values and pattern-matches
/// on them to dispatch work.
///
/// Design note: `set_fronting(Vec<PersonaId>)` is intentionally not a
/// separate typed variant here. It routes through [`RunCommand`] until
/// multi-agent fronting is implemented in a later phase. A dedicated
/// `SetFronting` variant can be added at that time without breaking the
/// existing wire contract (irpc is forward-extensible via non-exhaustive
/// matching on the generated enum).
#[rpc_requests(message = PatternMessage)]
#[derive(Serialize, Deserialize, Debug)]
pub enum PatternProtocol {
    /// Send a user message to an agent. Returns `()` once the daemon has
    /// accepted the batch and begun processing (acknowledgement, not
    /// completion). Events are delivered via [`SubscribeOutput`].
    #[rpc(tx = oneshot::Sender<()>)]
    SendMessage(AgentMessage),

    /// Cancel an in-flight batch by ID. Returns `()` when the cancellation
    /// signal has been delivered (the batch may still be winding down).
    #[rpc(tx = oneshot::Sender<()>)]
    CancelBatch(BatchId),

    /// Subscribe to all [`TaggedTurnEvent`]s emitted by a given agent.
    /// The server streams events until the client drops its receiver.
    #[rpc(tx = mpsc::Sender<TaggedTurnEvent>)]
    SubscribeOutput(AgentSubscription),

    /// List all agents currently registered with the daemon.
    #[rpc(tx = oneshot::Sender<Vec<AgentInfo>>)]
    ListAgents(ListAgentsRequest),

    /// Get a health snapshot of the daemon runtime.
    #[rpc(tx = oneshot::Sender<RuntimeStatus>)]
    GetStatus(GetStatusRequest),

    /// Execute a slash command and return the result.
    #[rpc(tx = oneshot::Sender<CommandResult>)]
    RunCommand(SlashCommand),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::turn::StopReason;

    #[test]
    fn agent_message_roundtrip() {
        let msg = AgentMessage {
            batch_id: "batch-001".into(),
            agent_id: "agent-1".into(),
            parts: vec![ContentPart::Text("hello".into())],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent_id, "agent-1");
        assert_eq!(decoded.batch_id, "batch-001");
    }

    #[test]
    fn agent_message_roundtrip_preserves_parts() {
        let msg = AgentMessage {
            batch_id: "b".into(),
            agent_id: "a".into(),
            parts: vec![
                ContentPart::Text("first".into()),
                ContentPart::Text("second".into()),
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.parts.len(), 2);
    }

    #[test]
    fn tagged_turn_event_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "batch-001".into(),
            agent_id: "agent-1".into(),
            event: TurnEvent::Text("hello world".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaggedTurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.batch_id, "batch-001");
        assert!(matches!(decoded.event, TurnEvent::Text(ref s) if s == "hello world"));
    }

    #[test]
    fn tagged_turn_event_stop_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "batch-002".into(),
            agent_id: "agent-2".into(),
            event: TurnEvent::Stop(StopReason::EndTurn),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaggedTurnEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded.event,
            TurnEvent::Stop(StopReason::EndTurn)
        ));
    }

    #[test]
    fn runtime_status_roundtrip() {
        let status = RuntimeStatus {
            agent_count: 3,
            active_batch_count: 1,
            uptime_secs: 42,
        };
        let json = serde_json::to_string(&status).unwrap();
        let decoded: RuntimeStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent_count, 3);
        assert_eq!(decoded.uptime_secs, 42);
    }

    #[test]
    fn slash_command_roundtrip() {
        let cmd = SlashCommand {
            command: "switch-persona".into(),
            args: vec!["orual".into()],
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: SlashCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.command, "switch-persona");
        assert_eq!(decoded.args, ["orual"]);
    }
}
