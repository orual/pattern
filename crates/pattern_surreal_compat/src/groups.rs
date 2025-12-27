//! Agent groups and constellation management types
//!
//! Database entity definitions for group coordination and constellation management.

use chrono::{DateTime, Utc};
use pattern_macros::Entity;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

use crate::agent_entity::AgentRecord;
use crate::id::{AgentId, ConstellationId, GroupId, RelationId, UserId};

use ferroid::{Base32SnowExt, SnowflakeGeneratorAsyncTokioExt, SnowflakeMastodonId};
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

/// Defines how agents in a group coordinate their actions
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinationPattern {
    /// One agent leads, others follow
    Supervisor {
        /// The agent that makes decisions for the group
        leader_id: AgentId,
        /// Rules for how the leader delegates tasks to other agents
        delegation_rules: DelegationRules,
    },

    /// Agents take turns in order
    RoundRobin {
        /// Index of the agent whose turn it is (0-based)
        current_index: usize,
        /// Whether to skip agents that are unavailable/suspended
        skip_unavailable: bool,
    },

    /// Agents vote on decisions
    Voting {
        /// Minimum number of votes needed for a decision
        quorum: usize,
        /// Rules governing how voting works
        voting_rules: VotingRules,
    },

    /// Sequential processing pipeline
    Pipeline {
        /// Ordered list of processing stages
        stages: Vec<PipelineStage>,
        /// Whether stages can be processed in parallel
        parallel_stages: bool,
    },

    /// Dynamic selection based on context
    Dynamic {
        /// Name of the selector strategy to use
        selector_name: String,
        /// Configuration for the selector
        selector_config: HashMap<String, String>,
    },

    /// Background monitoring with intervention triggers
    Sleeptime {
        /// How often to check triggers (e.g., every 20 minutes)
        check_interval: Duration,
        /// Conditions that trigger intervention
        triggers: Vec<SleeptimeTrigger>,
        /// Agent to activate when triggers fire (optional - uses least recently active if None)
        intervention_agent_id: Option<AgentId>,
    },
}

/// Rules for delegation in supervisor pattern
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DelegationRules {
    /// Maximum concurrent delegations per agent
    pub max_delegations_per_agent: Option<usize>,
    /// How to select agents for delegation
    pub delegation_strategy: DelegationStrategy,
    /// What to do if no agents are available
    pub fallback_behavior: FallbackBehavior,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStrategy {
    /// Delegate to agents in round-robin order
    RoundRobin,
    /// Delegate to the least busy agent
    LeastBusy,
    /// Delegate based on agent capabilities
    Capability,
    /// Random selection
    Random,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FallbackBehavior {
    /// Supervisor handles it themselves
    HandleSelf,
    /// Queue for later
    Queue,
    /// Fail the request
    Fail,
}

/// Rules governing how voting works
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VotingRules {
    /// How long to wait for all votes before proceeding
    #[serde(with = "crate::utils::serde_duration")]
    #[schemars(with = "u64")]
    pub voting_timeout: Duration,
    /// Strategy for breaking ties
    pub tie_breaker: TieBreaker,
    /// Whether to weight votes based on agent expertise/capabilities
    pub weight_by_expertise: bool,
}

/// Strategy for breaking voting ties
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TieBreaker {
    /// Randomly select from tied options
    Random,
    /// The option that received its first vote earliest wins
    FirstVote,
    /// A specific agent gets the deciding vote
    SpecificAgent(AgentId),
    /// No decision is made if there's a tie
    NoDecision,
}

/// A stage in a pipeline coordination pattern
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PipelineStage {
    /// Name of this stage
    pub name: String,
    /// Agents that can process this stage
    pub agent_ids: Vec<AgentId>,
    /// Maximum time allowed for this stage
    #[serde(with = "crate::utils::serde_duration")]
    #[schemars(with = "u64")]
    pub timeout: Duration,
    /// What to do if this stage fails
    pub on_failure: StageFailureAction,
}

/// Actions to take when a pipeline stage fails
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StageFailureAction {
    /// Skip this stage and continue
    Skip,
    /// Retry the stage up to max_attempts times
    Retry { max_attempts: usize },
    /// Abort the entire pipeline
    Abort,
    /// Use a fallback agent to handle the failure
    Fallback { agent_id: AgentId },
}

/// A trigger condition for sleeptime monitoring
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleeptimeTrigger {
    /// Name of this trigger
    pub name: String,
    /// Condition that activates this trigger
    pub condition: TriggerCondition,
    /// Priority level for this trigger
    pub priority: TriggerPriority,
}

/// Conditions that can trigger intervention in sleeptime monitoring
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerCondition {
    /// Trigger after a specific duration has passed
    TimeElapsed {
        #[serde(with = "crate::utils::serde_duration")]
        #[schemars(with = "u64")]
        duration: Duration,
    },
    /// Trigger when a named pattern is detected
    PatternDetected { pattern_name: String },
    /// Trigger when a metric exceeds a threshold
    ThresholdExceeded { metric: String, threshold: f64 },
    /// Trigger based on constellation activity
    ConstellationActivity {
        /// Number of messages or events since last sync
        message_threshold: usize,
        /// Alternative: time since last activity
        #[serde(with = "crate::utils::serde_duration")]
        #[schemars(with = "u64")]
        time_threshold: Duration,
    },
    /// Custom trigger evaluated by named evaluator
    Custom { evaluator: String },
}

/// Priority levels for sleeptime triggers
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum TriggerPriority {
    /// Low priority - can be batched or delayed
    Low,
    /// Medium priority - normal monitoring
    Medium,
    /// High priority - should be checked soon
    High,
    /// Critical priority - requires immediate intervention
    Critical,
}

/// Pattern-specific state
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "pattern", rename_all = "snake_case")]
pub enum GroupState {
    /// Supervisor pattern state
    Supervisor {
        /// Track current delegations per agent
        current_delegations: HashMap<AgentId, usize>,
    },
    /// Round-robin pattern state
    RoundRobin {
        /// Current position in the rotation
        current_index: usize,
        /// When the last rotation occurred
        last_rotation: DateTime<Utc>,
    },
    /// Voting pattern state
    Voting {
        /// Active voting session if any
        active_session: Option<VotingSession>,
    },
    /// Pipeline pattern state
    Pipeline {
        /// Currently executing pipelines
        active_executions: Vec<PipelineExecution>,
    },
    /// Dynamic pattern state
    Dynamic {
        /// Recent selection history for load balancing
        recent_selections: Vec<(DateTime<Utc>, AgentId)>,
    },
    /// Sleeptime pattern state
    Sleeptime {
        /// When we last checked triggers
        last_check: DateTime<Utc>,
        /// History of trigger events
        trigger_history: Vec<TriggerEvent>,
        /// Current index for round-robin through agents
        current_index: usize,
    },
}

/// An active voting session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VotingSession {
    /// Unique ID for this voting session
    pub id: Uuid,
    /// What's being voted on
    pub proposal: VotingProposal,
    /// Votes collected so far
    pub votes: HashMap<AgentId, Vote>,
    /// When voting started
    pub started_at: DateTime<Utc>,
    /// When voting must complete
    pub deadline: DateTime<Utc>,
}

/// A proposal being voted on
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VotingProposal {
    /// Description of what's being voted on
    pub content: String,
    /// Available options to vote for
    pub options: Vec<VoteOption>,
    /// Additional context
    pub metadata: HashMap<String, String>,
}

/// An option in a voting proposal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteOption {
    /// Unique ID for this option
    pub id: String,
    /// Description of the option
    pub description: String,
}

/// A vote cast by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    /// Which option was selected
    pub option_id: String,
    /// Weight of this vote (if expertise weighting is enabled)
    pub weight: f32,
    /// Optional reasoning provided by the agent
    pub reasoning: Option<String>,
    /// When the vote was cast
    pub timestamp: DateTime<Utc>,
}

/// State of a pipeline execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineExecution {
    /// Unique ID for this execution
    pub id: Uuid,
    /// Which stage we're currently on
    pub current_stage: usize,
    /// Results from completed stages
    pub stage_results: Vec<StageResult>,
    /// When execution started
    pub started_at: DateTime<Utc>,
}

/// Result from a pipeline stage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    /// Name of the stage
    pub stage_name: String,
    /// Which agent processed it
    pub agent_id: AgentId,
    /// Whether it succeeded
    pub success: bool,
    /// How long it took
    #[serde(with = "crate::utils::serde_duration")]
    pub duration: Duration,
    /// Output data
    pub output: serde_json::Value,
}

/// A trigger event that occurred
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerEvent {
    /// Which trigger fired
    pub trigger_name: String,
    /// When it fired
    pub timestamp: DateTime<Utc>,
    /// Whether intervention was activated
    pub intervention_activated: bool,
    /// Additional event data
    pub metadata: HashMap<String, String>,
}

/// Role of an agent in a group
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupMemberRole {
    /// Regular group member
    Regular,
    /// Group supervisor/leader
    Supervisor,
    /// Specialist in a particular domain
    Specialist { domain: String },
}

/// Wrapper type for Snowflake IDs with proper serde support
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnowflakePosition(pub SnowflakeMastodonId);

impl SnowflakePosition {
    /// Create a new snowflake position
    pub fn new(id: SnowflakeMastodonId) -> Self {
        Self(id)
    }
}

impl fmt::Display for SnowflakePosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for SnowflakePosition {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(id) = SnowflakeMastodonId::decode(s) {
            return Ok(Self(id));
        }
        s.parse::<u64>()
            .map(|raw| Self(SnowflakeMastodonId::from_raw(raw)))
            .map_err(|e| format!("Failed to parse snowflake as base32 or u64: {}", e))
    }
}

impl Serialize for SnowflakePosition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SnowflakePosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

/// Type alias for the Snowflake generator
type SnowflakeGen = ferroid::AtomicSnowflakeGenerator<SnowflakeMastodonId, ferroid::MonotonicClock>;

/// Global ID generator for message positions
static MESSAGE_POSITION_GENERATOR: OnceLock<SnowflakeGen> = OnceLock::new();

pub fn get_position_generator() -> &'static SnowflakeGen {
    MESSAGE_POSITION_GENERATOR.get_or_init(|| {
        let clock = ferroid::MonotonicClock::with_epoch(ferroid::TWITTER_EPOCH);
        ferroid::AtomicSnowflakeGenerator::new(0, clock)
    })
}

/// Get the next message position synchronously
pub fn get_next_message_position_sync() -> SnowflakePosition {
    use ferroid::IdGenStatus;
    let generator = get_position_generator();
    loop {
        match generator.next_id() {
            IdGenStatus::Ready { id } => return SnowflakePosition::new(id),
            IdGenStatus::Pending { yield_for } => {
                let wait_ms = yield_for.max(1) as u64;
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            }
        }
    }
}

/// Get the next message position (async version)
pub async fn get_next_message_position() -> SnowflakePosition {
    let id = get_position_generator()
        .try_next_id_async()
        .await
        .expect("for now we are assuming this succeeds");
    SnowflakePosition::new(id)
}

/// Get the next message position as a String
pub async fn get_next_message_position_string() -> String {
    get_next_message_position().await.to_string()
}

/// Types of agents in the system
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub enum AgentType {
    Generic,
    Pattern,
    Entropy,
    Flux,
    Archive,
    Momentum,
    Anchor,
    Custom(String),
}

impl Serialize for AgentType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Generic => serializer.serialize_str("generic"),
            Self::Pattern => serializer.serialize_str("pattern"),
            Self::Entropy => serializer.serialize_str("entropy"),
            Self::Flux => serializer.serialize_str("flux"),
            Self::Archive => serializer.serialize_str("archive"),
            Self::Momentum => serializer.serialize_str("momentum"),
            Self::Anchor => serializer.serialize_str("anchor"),
            Self::Custom(name) => serializer.serialize_str(&format!("custom_{}", name)),
        }
    }
}

impl<'de> Deserialize<'de> for AgentType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if let Some(name) = s.strip_prefix("custom_") {
            Ok(Self::Custom(name.to_string()))
        } else {
            Ok(Self::from_str(&s).unwrap_or_else(|_| Self::Custom(s)))
        }
    }
}

impl AgentType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Generic => "generic",
            Self::Pattern => "pattern",
            Self::Entropy => "entropy",
            Self::Flux => "flux",
            Self::Archive => "archive",
            Self::Momentum => "momentum",
            Self::Anchor => "anchor",
            Self::Custom(name) => name,
        }
    }
}

impl FromStr for AgentType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "generic" => Ok(Self::Generic),
            "pattern" => Ok(Self::Pattern),
            "entropy" => Ok(Self::Entropy),
            "flux" => Ok(Self::Flux),
            "archive" => Ok(Self::Archive),
            "momentum" => Ok(Self::Momentum),
            "anchor" => Ok(Self::Anchor),
            other if other.starts_with("custom:") => Ok(Self::Custom(
                other.strip_prefix("custom:").unwrap().to_string(),
            )),
            other => Ok(Self::Custom(other.to_string())),
        }
    }
}

/// Strategy for compressing messages when context is full
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CompressionStrategy {
    Truncate {
        keep_recent: usize,
    },
    RecursiveSummarization {
        chunk_size: usize,
        summarization_model: String,
        #[serde(default)]
        summarization_prompt: Option<String>,
    },
    ImportanceBased {
        keep_recent: usize,
        keep_important: usize,
    },
    TimeDecay {
        compress_after_hours: f64,
        min_keep_recent: usize,
    },
}

impl Default for CompressionStrategy {
    fn default() -> Self {
        Self::Truncate { keep_recent: 100 }
    }
}

/// A constellation represents a collection of agents working together for a specific user
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "constellation")]
pub struct Constellation {
    /// Unique identifier for this constellation
    pub id: ConstellationId,
    /// The user who owns this constellation of agents
    pub owner_id: UserId,
    /// Human-readable name
    pub name: String,
    /// Description of this constellation's purpose
    pub description: Option<String>,
    /// When this constellation was created
    pub created_at: DateTime<Utc>,
    /// Last update time
    pub updated_at: DateTime<Utc>,
    /// Whether this constellation is active
    pub is_active: bool,

    // Relations
    /// Agents in this constellation with membership metadata
    #[entity(edge_entity = "constellation_agents")]
    pub agents: Vec<(AgentRecord, ConstellationMembership)>,

    /// Groups within this constellation
    #[entity(relation = "composed_of")]
    pub groups: Vec<GroupId>,
}

/// Edge entity for constellation membership
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "constellation_agents", edge = true)]
pub struct ConstellationMembership {
    pub id: RelationId,
    pub in_id: ConstellationId,
    pub out_id: AgentId,
    /// When this agent joined the constellation
    pub joined_at: DateTime<Utc>,
    /// Is this the primary orchestrator agent?
    pub is_primary: bool,
}

/// A group of agents that coordinate together
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "group")]
pub struct AgentGroup {
    /// Unique identifier for this group
    pub id: GroupId,
    /// Human-readable name for this group
    pub name: String,
    /// Description of this group's purpose
    pub description: String,
    /// How agents in this group coordinate their actions
    #[entity(db_type = "object")]
    pub coordination_pattern: CoordinationPattern,
    /// When this group was created
    pub created_at: DateTime<Utc>,
    /// Last update time
    pub updated_at: DateTime<Utc>,
    /// Whether this group is active
    pub is_active: bool,

    /// Pattern-specific state stored here for now
    #[entity(db_type = "object")]
    pub state: GroupState,

    // Relations
    /// Members of this group with their roles
    #[entity(edge_entity = "group_members")]
    pub members: Vec<(AgentRecord, GroupMembership)>,
}

/// Edge entity for group membership
#[derive(Debug, Clone, Serialize, Deserialize, Entity)]
#[entity(entity_type = "group_members", edge = true)]
pub struct GroupMembership {
    pub id: RelationId,
    pub in_id: AgentId,
    pub out_id: GroupId,
    /// When this agent joined the group
    pub joined_at: DateTime<Utc>,
    /// Role of this agent in the group
    pub role: GroupMemberRole,
    /// Whether this member is active
    pub is_active: bool,
    /// Capabilities this agent brings to the group
    pub capabilities: Vec<String>,
}
