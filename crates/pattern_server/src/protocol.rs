//! IRPC service contract for the Pattern daemon.
//!
//! Defines the [`PatternProtocol`] enum that the irpc `#[rpc_requests]` macro
//! expands into a `PatternMessage` enum consumed by the daemon server actor.
//!
//! Transport serialization uses postcard (irpc's wire format). The test suite
//! uses serde_json for round-trip validation — serde_json and postcard both
//! honour the same `Serialize`/`Deserialize` impls, so this is correct.

use std::path::PathBuf;

use irpc::{
    channel::{mpsc, oneshot},
    rpc_requests,
};
use pattern_core::traits::turn_sink::{DisplayKind, TurnEvent};
use pattern_core::types::provider::{ContentPart, ToolOutcome};
use pattern_core::types::turn::StopReason;
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

/// Wire-safe version of [`TurnEvent`].
///
/// The internal `TurnEvent` contains genai types (`ToolCall`, `ToolResult`,
/// `CompletionRequest`) that use `serde_json::Value` fields and
/// `#[serde(skip_serializing_if)]` attributes — both incompatible with
/// postcard's binary wire format. This enum owns only postcard-safe types
/// (strings, simple enums, no `Value`).
///
/// Conversion from `TurnEvent` happens at the bridge boundary
/// ([`TurnSinkBridge::emit`]) so the internal runtime never sees this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireTurnEvent {
    /// Streamed LLM response text.
    Text(String),
    /// LLM reasoning content (thinking/chain-of-thought).
    Thinking(String),
    /// Tool invocation. Arguments are JSON-stringified.
    ToolCall {
        call_id: String,
        function_name: String,
        arguments_json: String,
    },
    /// Tool result. Content is JSON-stringified.
    ToolResult {
        call_id: String,
        success: bool,
        content_json: String,
    },
    /// Agent display output (chunk/final/note).
    Display { kind: DisplayKind, text: String },
    /// Wire turn ended.
    Stop(StopReason),
}

impl WireTurnEvent {
    /// Convert from the internal `TurnEvent`.
    ///
    /// `ComposedRequest` is filtered out (returns `None`) — it's a debug-only
    /// event that contains types incompatible with the wire format.
    pub fn from_turn_event(event: &TurnEvent) -> Option<Self> {
        match event {
            TurnEvent::Text(s) => Some(Self::Text(s.clone())),
            TurnEvent::Thinking(s) => Some(Self::Thinking(s.clone())),
            TurnEvent::ToolCall(tc) => Some(Self::ToolCall {
                call_id: tc.call_id.clone(),
                function_name: tc.fn_name.clone(),
                arguments_json: tc.fn_arguments.to_string(),
            }),
            TurnEvent::ToolResult(tr) => Some(Self::ToolResult {
                call_id: tr.call_id.clone(),
                success: matches!(tr.outcome, ToolOutcome::Success(_)),
                content_json: match &tr.outcome {
                    ToolOutcome::Success(val) => val.to_string(),
                    ToolOutcome::Error(msg) => msg.clone(),
                },
            }),
            TurnEvent::Display { kind, text } => Some(Self::Display {
                kind: *kind,
                text: text.clone(),
            }),
            TurnEvent::Stop(reason) => Some(Self::Stop(*reason)),
            TurnEvent::ComposedRequest(_) => None,
            _ => None, // Forward-compat for future variants.
        }
    }
}

/// A turn event tagged with the batch and agent that produced it.
///
/// Uses [`WireTurnEvent`] (postcard-safe) instead of the internal `TurnEvent`.
/// The daemon's fan-out logic emits one of these per event into every
/// subscriber channel that matches the `agent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedTurnEvent {
    /// Which batch (exchange) this event belongs to.
    pub batch_id: BatchId,
    /// Which agent emitted this event.
    pub agent_id: AgentId,
    /// The wire-safe turn event.
    pub event: WireTurnEvent,
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

/// Request payload for [`PatternProtocol::GetHistory`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetHistoryRequest {
    /// Agent to fetch history for.
    pub agent_id: AgentId,
}

/// A single historical message batch with reconstructed events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalBatch {
    /// Batch ID (snowflake).
    pub batch_id: BatchId,
    /// User's message that initiated this batch, if any.
    pub user_message: Option<String>,
    /// Agent response events as they were emitted during processing.
    pub events: Vec<WireTurnEvent>,
    /// Estimated token count for this batch (user + agent content).
    pub tokens: u64,
}

/// Response to [`GetHistory`](PatternProtocol::GetHistory).
///
/// Contains recent conversation history for an agent, reconstructed from
/// stored messages into the same wire format as live events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResponse {
    /// Historical batches in chronological order (oldest first).
    pub batches: Vec<HistoricalBatch>,
}

/// Request payload for [`PatternProtocol::InitSession`].
///
/// The TUI sends this after connecting to tell the daemon which project it is
/// working in. The daemon mounts the project on demand (or reuses a cached
/// mount) and resolves the requested persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitSessionRequest {
    /// Project root path for memory mount.
    pub project_path: PathBuf,
    /// Preferred agent_id (resolved from config by the client).
    pub default_agent: AgentId,
}

/// Response to [`InitSession`](PatternProtocol::InitSession).
///
/// Contains the daemon-resolved agent identity and available personas for the
/// project. If project mounting failed, `error` is `Some(message)` and the
/// session is in a degraded state (no memory, no LLM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The actual agent_id the daemon resolved.
    pub agent_id: AgentId,
    /// Persona display name.
    pub persona_name: String,
    /// All available personas discovered for this project.
    pub available_agents: Vec<AgentId>,
    /// Set when session initialization failed. The session is in a degraded
    /// state — the TUI should surface this error to the user.
    pub error: Option<String>,
}

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

    /// Fetch conversation history for an agent.
    ///
    /// Returns recent message batches reconstructed from stored messages,
    /// with events in the same wire format as live subscription output.
    #[rpc(tx = oneshot::Sender<HistoryResponse>)]
    GetHistory(GetHistoryRequest),

    /// Execute a slash command and return the result.
    #[rpc(tx = oneshot::Sender<CommandResult>)]
    RunCommand(SlashCommand),

    /// Initialize a session for a project.
    ///
    /// The TUI sends this after connecting. The daemon mounts the project on
    /// demand (or reuses a cached mount), discovers personas, and returns
    /// [`SessionInfo`] with the resolved agent identity and available agents.
    #[rpc(tx = oneshot::Sender<SessionInfo>)]
    InitSession(InitSessionRequest),
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
            event: WireTurnEvent::Text("hello world".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaggedTurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.batch_id, "batch-001");
        assert!(matches!(decoded.event, WireTurnEvent::Text(ref s) if s == "hello world"));
    }

    #[test]
    fn tagged_turn_event_stop_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "batch-002".into(),
            agent_id: "agent-2".into(),
            event: WireTurnEvent::Stop(StopReason::EndTurn),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: TaggedTurnEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded.event,
            WireTurnEvent::Stop(StopReason::EndTurn)
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

    #[test]
    fn init_session_request_roundtrip() {
        let req = InitSessionRequest {
            project_path: std::path::PathBuf::from("/home/user/project"),
            default_agent: "pattern-default".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: InitSessionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded.project_path,
            std::path::PathBuf::from("/home/user/project")
        );
        assert_eq!(decoded.default_agent, "pattern-default");
    }

    #[test]
    fn session_info_roundtrip() {
        let info = SessionInfo {
            agent_id: "pattern-default".into(),
            persona_name: "Pattern Default".into(),
            available_agents: vec!["pattern-default".into(), "supervisor".into()],
            error: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent_id, "pattern-default");
        assert_eq!(decoded.persona_name, "Pattern Default");
        assert_eq!(decoded.available_agents.len(), 2);
    }
}
