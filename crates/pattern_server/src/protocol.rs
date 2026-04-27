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
use pattern_core::types::origin::{Author, MessageOrigin};
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

/// Identifier for a persona by name (used in direct `@persona` addressing).
pub type PersonaId = SmolStr;

/// Routing directive for an [`AgentMessage`].
///
/// Controls how the daemon routes the message:
///
/// - [`Recipient::Direct`] — deliver to the named agent's mailbox, bypassing
///   the fronting resolver entirely.
/// - [`Recipient::Auto`] — let the fronting resolver pick a target based on
///   the current [`FrontingSet`] rules and the message body.
/// - [`Recipient::Address`] — `@persona-name` direct addressing; always
///   delivers to the named persona regardless of the routing rules.
///
/// TUI callers that had a fixed `agent_id` before Phase 5 should use
/// `Recipient::Direct(agent_id)` to preserve the old semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Recipient {
    /// Deliver directly to the named agent's session, bypassing the resolver.
    Direct(AgentId),
    /// Route through the fronting resolver: rules → fallback → fan-out →
    /// default-persona → system-default. The daemon pre-resolves to a single
    /// agent before opening/driving the session.
    Auto,
    /// Direct `@persona-name` addressing. The leading `@` is stripped
    /// (or may be absent) before resolving the persona.
    Address(PersonaId),
}

/// A message from any RPC caller to an agent.
///
/// The client mints the `batch_id` (a snowflake) before sending. The daemon
/// uses it to correlate every [`TaggedTurnEvent`] emitted during this exchange
/// back to the originating batch, enabling concurrent rendering.
///
/// The `origin` field carries full caller attribution. The daemon does **not**
/// assume `Author::Partner` — each caller provides its own [`MessageOrigin`]:
///
/// - TUI callers construct `Author::Partner` using the `partner_id` received
///   at `InitSession` time (or stored from a prior session).
/// - Agent-to-agent callers construct `Author::Agent { agent_id }`.
/// - System/scheduler callers construct `Author::System { reason }`.
/// - Third-party human callers construct `Author::Human { user_id, display_name }`.
///
/// This makes the RPC layer symmetric: any client that can connect to the
/// daemon can supply its own identity rather than having the daemon guess.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Client-minted batch ID (snowflake). The daemon uses this to tag all
    /// TurnEvents for this exchange, enabling concurrent batch rendering.
    pub batch_id: BatchId,
    /// Routing directive. Specifies how the daemon should resolve the target
    /// agent for this message. Use [`Recipient::Direct`] to preserve
    /// pre-Phase-5 behaviour (fixed agent_id).
    ///
    /// When `Recipient::Auto`, the daemon calls the fronting resolver on the
    /// active mount's `FrontingSet` and routes to the resolved persona.
    pub recipient: Recipient,
    /// Message content parts — text, images, binary attachments.
    /// The daemon wraps these into a `ChatMessage::user()` when constructing
    /// [`pattern_core::types::turn::TurnInput`].
    pub parts: Vec<ContentPart>,
    /// Caller-supplied origin attribution. The daemon passes this through
    /// directly to [`pattern_core::types::turn::TurnInput::origin`] — it does
    /// **not** override or default the author. Each RPC client is responsible
    /// for constructing the appropriate [`MessageOrigin`] for its identity.
    pub origin: MessageOrigin,
}

/// Request to subscribe to an agent's turn event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSubscription {
    /// Agent whose events the subscriber wants to receive.
    pub agent_id: AgentId,
}

/// Request to subscribe to ALL events for a project mount.
///
/// Phase 6 T8: the TUI is now mount-scoped (not agent-scoped). Subscribing
/// via `SubscribeAll` returns every `TaggedTurnEvent` for any agent in the
/// mount, plus daemon-level events (`FrontingChanged`, `ConstellationChanged`)
/// fanned out under the `"daemon"` agent_id sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSubscription {
    /// Canonical project mount path. Subscribers are matched on canonical
    /// path so callers do not need to canonicalize before subscribing —
    /// the daemon does it.
    pub mount_path: std::path::PathBuf,
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
    /// An agent sent a message via `Pattern.Message.Send`/`Reply`/`Notify`
    /// or, in Phase 4+, `Delegate`. Routed through the daemon's
    /// `CliRouter` and fanned out to subscribed TUI clients so the
    /// recipient's outbound traffic can be rendered with sender
    /// attribution.
    ///
    /// Phase 4 (v3-multi-agent) introduces this variant. Older clients
    /// that don't understand `MessageSent` should treat it as an
    /// unknown event and skip rather than fail-closed.
    MessageSent {
        /// Recipient address as the agent supplied it (post-scheme-
        /// strip in the runtime, e.g. `"user"` or `"agent:entropy"`).
        recipient: String,
        /// Message body text. The on-wire structured `Message` would
        /// drag genai types into the postcard surface, so we project
        /// to plain text here.
        body: String,
        /// Sender attribution.
        from: Author,
    },
    /// The daemon's active fronting set changed.
    ///
    /// Emitted after a successful `SetFronting` or `UpdateRouting` RPC, or
    /// after an agent with `FrontingControl` mutates the set via the SDK.
    /// Subscribed TUI clients should re-render the fronting status line.
    ///
    /// Phase 5 (v3-multi-agent) introduces this variant. Older clients
    /// that don't understand `FrontingChanged` should skip it.
    ///
    /// TODO(T3): `DaemonServer` emits this after each successful
    /// `update_fronting` call when Block B wiring lands.
    FrontingChanged {
        /// Currently active persona IDs (stable `String` for wire stability;
        /// `PersonaId` is a `SmolStr` alias that serializes identically).
        active: Vec<String>,
        /// Fallback persona ID, if configured.
        fallback: Option<String>,
        /// Updated routing rules.
        rules: Vec<WireRoutingRule>,
    },
    /// The constellation persona registry changed.
    ///
    /// Emitted by [`EventEmittingRegistry`](crate::server::EventEmittingRegistry)
    /// after a mutation lands. The `kind` field is a stable identifier
    /// describing what changed (for tracing/diagnostics); TUI clients
    /// generally treat any change as "re-fetch the registry" and ignore
    /// the kind.
    ///
    /// Possible kind values: `"persona_registered"`, `"status_changed"`,
    /// `"config_path_changed"`, `"relationship_added"`, `"group_created"`.
    ///
    /// Phase 6 T8 introduces this variant.
    ConstellationChanged {
        /// Identifier describing what changed (stable across versions).
        kind: String,
    },
    /// Wire turn ended.
    Stop(StopReason),
}

/// Wire mirror of a routing rule, used in [`WireTurnEvent::FrontingChanged`]
/// and in the `GetFronting` / `SetFronting` RPCs.
///
/// `PersonaId` is represented as `String` on the wire for stability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRoutingRule {
    /// Stable identifier for this rule.
    pub id: String,
    /// Pattern type: `"Prefix"`, `"Contains"`, `"TopicTag"`, or `"Regex"`.
    pub pattern_type: String,
    /// Pattern value (the prefix string, search term, tag, or regex source).
    pub pattern_value: String,
    /// Delivery target persona ID.
    pub target: String,
    /// Priority: higher values are evaluated first.
    pub priority: u32,
}

/// Wire mirror of [`pattern_core::fronting::FrontingSet`].
///
/// Used in `FrontingGetResponse` and `FrontingSetRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireFrontingSet {
    /// Currently active persona IDs.
    pub active: Vec<String>,
    /// Fallback persona ID, if configured.
    pub fallback: Option<String>,
    /// Routing rules.
    pub rules: Vec<WireRoutingRule>,
}

/// Request payload for [`PatternProtocol::GetFronting`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontingGetRequest {}

/// Response to [`PatternProtocol::GetFronting`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontingGetResponse {
    /// Current fronting state.
    pub set: WireFrontingSet,
}

/// Request payload for [`PatternProtocol::SetFronting`].
///
/// Replaces the active personas and fallback. Use `UpdateRouting` to
/// modify routing rules independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontingSetRequest {
    /// New active persona IDs.
    pub active: Vec<String>,
    /// New fallback persona ID, or `None` to enable fan-out mode.
    pub fallback: Option<String>,
}

/// Response to [`PatternProtocol::SetFronting`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontingSetResponse {
    /// Whether the update was applied successfully.
    pub success: bool,
    /// Error message if `success == false`.
    pub error: Option<String>,
}

/// Request payload for [`PatternProtocol::UpdateRouting`].
///
/// Replaces the routing rules independently of the active persona set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRoutingRequest {
    /// New routing rules (replaces all existing rules).
    pub rules: Vec<WireRoutingRule>,
}

/// Response to [`PatternProtocol::UpdateRouting`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRoutingResponse {
    /// Whether the rules were compiled and applied successfully.
    pub success: bool,
    /// Error message if `success == false` (e.g. invalid regex in a rule).
    pub error: Option<String>,
}

/// Request payload for [`PatternProtocol::PromoteDraft`].
///
/// Phase 6 T6: flip a draft persona to `Active`. The daemon loads the
/// persona from `record.config_path`, opens its session via the normal
/// path (which auto-drains any messages queued against the draft via
/// `AgentRegistry::register_active`), and updates the registry status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoteDraftRequest {
    /// The persona id to promote. Must currently be in `Draft` status.
    pub persona_id: String,
}

/// Response to [`PatternProtocol::PromoteDraft`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoteDraftResponse {
    pub success: bool,
    pub error: Option<String>,
}

// ── Phase 6 T7: constellation registry RPCs ──────────────────────────────────

/// Request payload for [`PatternProtocol::ListPersonas`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ListPersonasRequest {
    /// Optional project-path filter. `None` returns every persona; `Some(p)`
    /// returns only those whose `project_attachments` include `p`.
    pub project: Option<String>,
}

/// Slim wire representation of a persona record for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePersonaSummary {
    pub id: String,
    pub name: String,
    /// "active" / "draft" / "inactive".
    pub status: String,
    pub config_path: Option<String>,
    pub project_attachments: Vec<String>,
}

/// Response to [`PatternProtocol::ListPersonas`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPersonasResponse {
    pub personas: Vec<WirePersonaSummary>,
    pub error: Option<String>,
}

/// Request payload for [`PatternProtocol::AddRelationship`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddRelationshipRequest {
    pub from: String,
    pub to: String,
    /// snake_case relationship kind: `supervisor_of`, `specialist_for`,
    /// `peer_with`, or `observer_of`.
    pub kind: String,
}

/// Response to [`PatternProtocol::AddRelationship`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddRelationshipResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// Request payload for [`PatternProtocol::ListGroups`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ListGroupsRequest {
    pub project: Option<String>,
}

/// Slim wire representation of a persona group for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireGroupSummary {
    pub id: String,
    pub name: String,
    pub project_id: Option<String>,
    pub members: Vec<String>,
}

/// Response to [`PatternProtocol::ListGroups`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListGroupsResponse {
    pub groups: Vec<WireGroupSummary>,
    pub error: Option<String>,
}

/// Request payload for [`PatternProtocol::CreateGroup`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub project_id: Option<String>,
}

/// Response to [`PatternProtocol::CreateGroup`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupResponse {
    pub group: Option<WireGroupSummary>,
    pub error: Option<String>,
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
    /// Optional mount path identifying which project mount this event
    /// belongs to. Used by mount-scoped subscribers ([`SubscribeAll`])
    /// to filter events; per-agent subscribers ignore it.
    ///
    /// `None` for legacy emitters (the daemon-side `fan_out` resolves
    /// agent → mount via the `agent_to_mount` map for per-agent events).
    /// `Some(path)` for daemon-level events (`FrontingChanged`,
    /// `ConstellationChanged`) where the emitter knows the mount directly.
    ///
    /// Phase 6 T8 introduces this field.
    #[serde(default)]
    pub mount_path: Option<String>,
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

/// Request payload for [`PatternProtocol::ListCommands`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListCommandsRequest;

/// Metadata about a daemon-registered slash command.
///
/// Returned by [`PatternProtocol::ListCommands`]. The TUI merges these with
/// its local built-in command registry to provide autocomplete for commands
/// registered by plugins or future extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonCommandInfo {
    /// Command name (without leading `/`).
    pub name: String,
    /// Human-readable description for autocomplete display.
    pub description: String,
}

/// Request payload for [`PatternProtocol::GetStatus`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStatusRequest;

/// Request payload for [`PatternProtocol::GetClientCount`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetClientCountRequest;

/// Request payload for [`PatternProtocol::Shutdown`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRequest;

/// Response to [`PatternProtocol::Shutdown`].
///
/// The daemon responds before exiting so the client's `.await` can resolve
/// cleanly. After sending, the daemon calls `std::process::exit(0)` after a
/// brief delay to let the response flush over the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownResponse;

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
    /// Agent that emitted this batch's response events. Phase 6 T8: the
    /// TUI is mount-scoped and uses this to label each historical batch
    /// with its responding agent (matching live batches tagged from
    /// `TaggedTurnEvent.agent_id`).
    pub agent_id: AgentId,
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
    /// Stable partner identity for this daemon session.
    ///
    /// Clients use this to construct `Author::Partner(Partner { user_id })`
    /// when building the `origin` field of [`AgentMessage`]. The daemon mints
    /// this once at spawn time so all clients that connected to the same daemon
    /// process share a consistent partner identity in the agents' message
    /// history.
    ///
    /// TUI clients should store this and pass it as `user_id` in every
    /// subsequent `SendMessage`. Phase 6 Task 8 will wire this into the
    /// multi-fronting TUI path.
    pub partner_id: SmolStr,
    /// Optional human-readable display name for the partner.
    ///
    /// Sourced from `.pattern.kdl` `partner { display_name "..." }` when
    /// present. `None` means no display name was configured — TUI should
    /// fall back to an anonymous label (e.g. "you").
    ///
    /// Phase 6 will complete `.pattern.kdl` partner-config parsing; until
    /// then the daemon always returns `None`.
    pub partner_display_name: Option<String>,
    /// Snapshot of the per-mount fronting state at InitSession time.
    ///
    /// Lets the TUI render the initial status bar + constellation panel
    /// without an extra `GetFronting` round-trip. `None` only in echo mode
    /// (no real mount).
    ///
    /// Phase 6 T8: TUI fronting integration.
    pub fronting_snapshot: Option<FrontingSnapshot>,
    /// Set when session initialization failed. The session is in a degraded
    /// state — the TUI should surface this error to the user.
    pub error: Option<String>,
}

/// Snapshot of a [`FrontingSet`](pattern_core::fronting::FrontingSet) for the wire.
///
/// Returned in [`SessionInfo::fronting_snapshot`] (initial state) and emitted
/// inside [`WireTurnEvent::FrontingChanged`] (live updates). Same shape both
/// ways so the TUI consumes either through one render path.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrontingSnapshot {
    pub active: Vec<String>,
    pub fallback: Option<String>,
    pub rules: Vec<WireRoutingRule>,
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

    /// Subscribe to ALL events for a project mount (Phase 6 T8).
    ///
    /// The default subscription mode for the mount-scoped TUI: receives
    /// every agent's events under one stream, plus daemon-level events
    /// (`FrontingChanged`, `ConstellationChanged`) routed via the `"daemon"`
    /// agent_id sentinel.
    #[rpc(tx = mpsc::Sender<TaggedTurnEvent>)]
    SubscribeAll(MountSubscription),

    /// List all agents currently registered with the daemon.
    #[rpc(tx = oneshot::Sender<Vec<AgentInfo>>)]
    ListAgents(ListAgentsRequest),

    /// List all slash commands registered with the daemon.
    ///
    /// The TUI calls this on session init to augment its local built-in command
    /// registry with any commands provided by plugins or runtime extensions.
    #[rpc(tx = oneshot::Sender<Vec<DaemonCommandInfo>>)]
    ListCommands(ListCommandsRequest),

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

    /// Return the number of currently connected clients.
    ///
    /// Used by `--stop-daemon-on-exit` (AC6.7): after the TUI exits, the
    /// client calls this and shuts down the daemon if the count is zero,
    /// ensuring no stale daemon state persists between development runs.
    #[rpc(tx = oneshot::Sender<usize>)]
    GetClientCount(GetClientCountRequest),

    /// Request the daemon to shut down cleanly.
    ///
    /// The daemon responds with [`ShutdownResponse`] before exiting so the
    /// client's `.await` resolves. A brief `tokio::time::sleep` delay follows
    /// the response to allow the reply to flush, then `std::process::exit(0)`
    /// terminates the process.
    #[rpc(tx = oneshot::Sender<ShutdownResponse>)]
    Shutdown(ShutdownRequest),

    /// Read the current fronting state for the active project mount.
    ///
    /// Returns the active personas, fallback, and routing rules as a
    /// [`FrontingGetResponse`]. If no project is mounted, returns an empty
    /// `WireFrontingSet`.
    ///
    /// Phase 5 (v3-multi-agent) introduces this variant.
    #[rpc(tx = oneshot::Sender<FrontingGetResponse>)]
    GetFronting(FrontingGetRequest),

    /// Set the active fronting personas and optional fallback for the current
    /// project mount.
    ///
    /// The mutation is persisted to the mount's DB via
    /// [`crate::server::ProjectMount::update_fronting`]. On success, fans out
    /// a [`WireTurnEvent::FrontingChanged`] to all subscribers.
    ///
    /// Phase 5 (v3-multi-agent) introduces this variant.
    #[rpc(tx = oneshot::Sender<FrontingSetResponse>)]
    SetFronting(FrontingSetRequest),

    /// Replace the routing rules for the current project mount.
    ///
    /// Rules are compiled before the write lock is acquired — invalid regex
    /// patterns are rejected and the existing rules are left unchanged.
    /// On success, fans out a [`WireTurnEvent::FrontingChanged`] to all
    /// subscribers.
    ///
    /// Phase 5 (v3-multi-agent) introduces this variant.
    #[rpc(tx = oneshot::Sender<UpdateRoutingResponse>)]
    UpdateRouting(UpdateRoutingRequest),

    /// Promote a `Draft` persona to `Active`.
    ///
    /// Loads the persona from its `config_path`, opens its session through
    /// the normal session-open path (which calls `AgentRegistry::register_active`
    /// and auto-drains any messages queued against the draft), and flips
    /// the persona registry status to `Active`.
    ///
    /// Phase 6 (v3-multi-agent) introduces this variant.
    #[rpc(tx = oneshot::Sender<PromoteDraftResponse>)]
    PromoteDraft(PromoteDraftRequest),

    /// List persona records, optionally filtered by project path.
    #[rpc(tx = oneshot::Sender<ListPersonasResponse>)]
    ListPersonas(ListPersonasRequest),

    /// Add a relationship edge between two personas.
    #[rpc(tx = oneshot::Sender<AddRelationshipResponse>)]
    AddRelationship(AddRelationshipRequest),

    /// List persona groups, optionally filtered by project path.
    #[rpc(tx = oneshot::Sender<ListGroupsResponse>)]
    ListGroups(ListGroupsRequest),

    /// Create a new persona group.
    #[rpc(tx = oneshot::Sender<CreateGroupResponse>)]
    CreateGroup(CreateGroupRequest),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::origin::{Partner, Sphere};
    use pattern_core::types::turn::StopReason;

    fn test_partner_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Partner(Partner {
                user_id: "test-user-id".into(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    #[test]
    fn shutdown_request_roundtrip() {
        // Unit struct carries no payload; the roundtrip exercises that the
        // `Serialize` + `Deserialize` derives exist and round-trip via both
        // backends. postcard is the wire format used by irpc at runtime, so
        // verifying it separately from serde_json catches cases where a type
        // encodes fine as JSON but can't be represented in postcard's subset
        // (e.g. `serde_json::Value`, untagged enums without a discriminant).
        let req = ShutdownRequest;
        let json = serde_json::to_string(&req).unwrap();
        let _decoded: ShutdownRequest = serde_json::from_str(&json).unwrap();

        let bytes = postcard::to_allocvec(&req).unwrap();
        let _decoded: ShutdownRequest = postcard::from_bytes(&bytes).unwrap();
    }

    #[test]
    fn shutdown_response_roundtrip() {
        let resp = ShutdownResponse;
        let json = serde_json::to_string(&resp).unwrap();
        let _decoded: ShutdownResponse = serde_json::from_str(&json).unwrap();

        let bytes = postcard::to_allocvec(&resp).unwrap();
        let _decoded: ShutdownResponse = postcard::from_bytes(&bytes).unwrap();
    }

    /// Verifies that `AgentMessage` with a Partner origin round-trips through
    /// both JSON (serde) and postcard (IRPC wire format).
    #[test]
    fn agent_message_direct_roundtrip() {
        let msg = AgentMessage {
            batch_id: "batch-001".into(),
            recipient: Recipient::Direct("agent-1".into()),
            parts: vec![ContentPart::Text("hello".into())],
            origin: test_partner_origin(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(&decoded.recipient, Recipient::Direct(id) if id == "agent-1"),
            "expected Direct recipient"
        );
        assert_eq!(decoded.batch_id, "batch-001");
        // Also verify postcard round-trip (IRPC wire format).
        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded2: AgentMessage = postcard::from_bytes(&bytes).unwrap();
        assert!(
            matches!(&decoded2.recipient, Recipient::Direct(id) if id == "agent-1"),
            "postcard: expected Direct recipient"
        );
    }

    #[test]
    fn agent_message_auto_roundtrip() {
        let msg = AgentMessage {
            batch_id: "batch-002".into(),
            recipient: Recipient::Auto,
            parts: vec![ContentPart::Text("hello fronting".into())],
            origin: test_partner_origin(),
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: AgentMessage = postcard::from_bytes(&bytes).unwrap();
        assert!(matches!(decoded.recipient, Recipient::Auto));
    }

    #[test]
    fn agent_message_address_roundtrip() {
        let msg = AgentMessage {
            batch_id: "batch-003".into(),
            recipient: Recipient::Address("alice".into()),
            parts: vec![ContentPart::Text("@alice hi".into())],
            origin: test_partner_origin(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(&decoded.recipient, Recipient::Address(id) if id == "alice"),
            "expected Address recipient"
        );
    }

    #[test]
    fn agent_message_roundtrip_preserves_parts() {
        let msg = AgentMessage {
            batch_id: "b".into(),
            recipient: Recipient::Direct("a".into()),
            parts: vec![
                ContentPart::Text("first".into()),
                ContentPart::Text("second".into()),
            ],
            origin: test_partner_origin(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.parts.len(), 2);
    }

    /// Verifies that `AgentMessage` with an `Agent` origin (agent-to-agent RPC)
    /// round-trips correctly — not just Partner origins.
    #[test]
    fn agent_message_agent_origin_roundtrip() {
        use pattern_core::types::origin::AgentAuthor;
        let msg = AgentMessage {
            batch_id: "batch-004".into(),
            recipient: Recipient::Auto,
            parts: vec![ContentPart::Text("cross-agent message".into())],
            origin: MessageOrigin::new(
                Author::Agent(AgentAuthor {
                    agent_id: "sender-agent".into(),
                }),
                Sphere::System,
            ),
        };
        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: AgentMessage = postcard::from_bytes(&bytes).unwrap();
        assert!(
            matches!(&decoded.origin.author, Author::Agent(a) if a.agent_id == "sender-agent"),
            "Agent origin must survive postcard round-trip"
        );
    }

    #[test]
    fn tagged_turn_event_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "batch-001".into(),
            agent_id: "agent-1".into(),
            event: WireTurnEvent::Text("hello world".into()),
            mount_path: None,
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
            mount_path: None,
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
            partner_id: "test-partner-abc123".into(),
            partner_display_name: Some("orual".into()),
            fronting_snapshot: None,
            error: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent_id, "pattern-default");
        assert_eq!(decoded.persona_name, "Pattern Default");
        assert_eq!(decoded.available_agents.len(), 2);
        assert_eq!(decoded.partner_id, "test-partner-abc123");
        assert_eq!(decoded.partner_display_name.as_deref(), Some("orual"));
    }

    #[test]
    fn wire_routing_rule_roundtrip() {
        // Postcard-safe: all fields are plain strings + u32.
        let rule = WireRoutingRule {
            id: "rule-1".into(),
            pattern_type: "Prefix".into(),
            pattern_value: "!cmd".into(),
            target: "entropy".into(),
            priority: 100,
        };
        let bytes = postcard::to_allocvec(&rule).unwrap();
        let decoded: WireRoutingRule = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.id, "rule-1");
        assert_eq!(decoded.pattern_type, "Prefix");
        assert_eq!(decoded.priority, 100);
    }

    #[test]
    fn wire_fronting_set_roundtrip() {
        let set = WireFrontingSet {
            active: vec!["alice".into(), "bob".into()],
            fallback: Some("charlie".into()),
            rules: vec![WireRoutingRule {
                id: "r1".into(),
                pattern_type: "Contains".into(),
                pattern_value: "#art".into(),
                target: "alice".into(),
                priority: 10,
            }],
        };
        let json = serde_json::to_string(&set).unwrap();
        let decoded: WireFrontingSet = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.active.len(), 2);
        assert_eq!(decoded.fallback.as_deref(), Some("charlie"));
        assert_eq!(decoded.rules.len(), 1);

        // Also verify postcard round-trip.
        let bytes = postcard::to_allocvec(&set).unwrap();
        let decoded2: WireFrontingSet = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded2.active, decoded.active);
    }

    #[test]
    fn fronting_changed_wire_event_roundtrip() {
        let event = TaggedTurnEvent {
            batch_id: "b3".into(),
            agent_id: "a3".into(),
            event: WireTurnEvent::FrontingChanged {
                active: vec!["alice".into()],
                fallback: None,
                rules: vec![],
            },
            mount_path: Some("/path/to/mount".into()),
        };
        let bytes = postcard::to_allocvec(&event).unwrap();
        let decoded: TaggedTurnEvent = postcard::from_bytes(&bytes).unwrap();
        assert!(matches!(
            decoded.event,
            WireTurnEvent::FrontingChanged {
                ref active,
                fallback: None,
                ..
            } if active == &["alice"]
        ));
    }

    #[test]
    fn fronting_set_request_roundtrip() {
        let req = FrontingSetRequest {
            active: vec!["alice".into()],
            fallback: Some("bob".into()),
        };
        let bytes = postcard::to_allocvec(&req).unwrap();
        let decoded: FrontingSetRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.active, ["alice"]);
        assert_eq!(decoded.fallback.as_deref(), Some("bob"));
    }

    #[test]
    fn update_routing_request_roundtrip() {
        let req = UpdateRoutingRequest {
            rules: vec![WireRoutingRule {
                id: "r2".into(),
                pattern_type: "Regex".into(),
                pattern_value: "^hello".into(),
                target: "orual".into(),
                priority: 50,
            }],
        };
        let bytes = postcard::to_allocvec(&req).unwrap();
        let decoded: UpdateRoutingRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.rules.len(), 1);
        assert_eq!(decoded.rules[0].pattern_type, "Regex");
    }
}
