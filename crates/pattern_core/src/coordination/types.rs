//! Type definitions for agent coordination patterns

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

use crate::{AgentId, AgentState, messages::Message};

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

/// Context for agent selection
#[derive(Debug, Clone)]
pub struct SelectionContext {
    /// The message being processed
    pub message: Message,
    /// Recent selections for load balancing
    pub recent_selections: Vec<(DateTime<Utc>, AgentId)>,
    /// Available agents and their states
    pub available_agents: Vec<(AgentId, AgentState)>,
    /// Agent capabilities
    pub agent_capabilities: HashMap<AgentId, Vec<String>>,
}

/// Configuration for an agent selector
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionConfig {
    /// Name of this selector
    pub name: String,
    /// Description of how it works
    pub description: String,
    /// Configuration parameters
    pub parameters: HashMap<String, String>,
}
