//! Batch value type: a group of messages sharing a single agent activation.
//!
//! An activation spans from "agent woke up to process input" through the
//! entire tool-call/response cycle until the agent naturally stops. All
//! messages produced or received during that span share a batch.
//!
//! Batches also enable "shadow-clone jutsu": additional user messages that
//! arrive during an in-flight activation can be grouped into a temporary
//! forked batch that rejoins the primary conversation afterward.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::ids::BatchId;
use crate::types::message::Message;

/// Classification of an agent-activation batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BatchType {
    /// User-initiated interaction.
    UserRequest,
    /// Inter-agent communication.
    AgentToAgent,
    /// System-initiated (e.g., scheduled task, sleeptime).
    SystemTrigger,
    /// Continuation of a previous batch (for long responses).
    Continuation,
}

/// A batch of messages produced by a single agent activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBatch {
    pub id: BatchId,
    pub batch_type: BatchType,
    /// Messages in creation order. Ordering uses `Message.created_at`.
    pub messages: Vec<Message>,
    /// Timestamp of the first message in this batch; used for inter-batch
    /// ordering without walking `messages`.
    pub started_at: Timestamp,
}
