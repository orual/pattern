// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Identifier types used across pattern_core.
//!
//! All IDs are [`SmolStr`] — small-string-optimized (≤24 bytes inline
//! on 64-bit), cheap to clone. Type aliases preserve naming for signature
//! clarity without newtype ceremony; there is no compile-time distinction
//! between kinds.
//!
//! When a distinct type is genuinely useful (rare, e.g. validation-bearing
//! atproto identifiers), wrap explicitly at the site that needs it rather
//! than making every id a newtype.
//!
//! Fresh identifiers are generated via [`new_id`], which returns a UUID-v4
//! string in simple (unhyphenated) form.
//!
//! # Examples
//!
//! ```
//! use pattern_core::types::ids::{AgentId, MessageId, new_id};
//!
//! let agent: AgentId = new_id();
//! let message: MessageId = new_id();
//! // Type aliases collapse: agent and message share the same runtime type.
//! assert_eq!(agent.len(), 32);
//! ```

use smol_str::SmolStr;
use uuid::Uuid;

// region: identifier type aliases

/// An agent identifier. Accepts arbitrary strings (human-chosen or generated).
pub type AgentId = SmolStr;

/// A persona identifier.
///
/// Same underlying type as [`AgentId`]; used in multi-agent code where the
/// distinction matters semantically. A persona is the persistent identity
/// config (KDL file + registry entry); an agent is a running session. Most
/// spawn-related APIs accept a `PersonaId` to name which persona to open.
pub type PersonaId = SmolStr;

/// A user identifier.
pub type UserId = SmolStr;

/// A message identifier.
pub type MessageId = SmolStr;

/// A batch identifier — spans a single agent activation (user input
/// through tool-call/response cycles until natural stop).
pub type BatchId = SmolStr;

/// A turn identifier — a single model invocation within an activation.
pub type TurnId = SmolStr;

/// A session identifier — persists across turns for the life of a
/// running agent instance.
pub type SessionId = SmolStr;

/// A workspace identifier.
pub type WorkspaceId = SmolStr;

/// A project identifier.
pub type ProjectId = SmolStr;

/// A conversation identifier.
pub type ConversationId = SmolStr;

/// A constellation identifier — groups a partner's agents.
pub type ConstellationId = SmolStr;

/// A group identifier — agents coordinating on a shared task.
pub type GroupId = SmolStr;

/// A relation identifier.
pub type RelationId = SmolStr;

/// A task identifier.
pub type TaskId = SmolStr;

/// A task item identifier — unique within its parent TaskList block.
///
/// Minted via [`new_snowflake_id`] for lexicographic time-ordering; any
/// non-empty string is also acceptable (used in test fixtures and in
/// agent-supplied references via wire formats like `TaskEdgeRef`).
/// Empty-string validation lives at the wire boundaries that see external
/// data (see `TaskEdgeRef::from_str`), not on this alias.
pub type TaskItemId = SmolStr;

/// A tool-call identifier — ties a tool invocation to its response.
pub type ToolCallId = SmolStr;

/// A wakeup identifier — scheduled-task reference.
pub type WakeupId = SmolStr;

/// A queued-message identifier.
pub type QueuedMessageId = SmolStr;

/// A memory block identifier.
pub type MemoryId = SmolStr;

/// An event identifier.
pub type EventId = SmolStr;

/// A model identifier (e.g. a provider's model name).
pub type ModelId = SmolStr;

/// A request identifier — correlates provider request/response pairs.
pub type RequestId = SmolStr;

/// An OAuth token identifier.
pub type OAuthTokenId = SmolStr;

/// A Discord identity identifier.
pub type DiscordIdentityId = SmolStr;

// endregion

/// Generate a fresh UUID-v4-based identifier in simple (unhyphenated) form.
///
/// Use for unordered identifiers like agent IDs, tool-call IDs, session IDs.
/// For identifiers that need lexicographic time-ordering (batch IDs, message
/// position keys), use [`new_snowflake_id`] instead.
///
/// # Examples
///
/// ```
/// use pattern_core::types::ids::new_id;
///
/// let id = new_id();
/// assert_eq!(id.len(), 32);
/// assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
/// ```
pub fn new_id() -> SmolStr {
    let uuid = Uuid::new_v4();
    SmolStr::from(uuid.simple().to_string().as_str())
}

/// Generate a fresh Mastodon-style Snowflake identifier, base32-encoded.
///
/// Wraps [`crate::utils::get_next_message_position_sync`] and returns the
/// base32 string form. The encoding is strictly lexicographically sortable
/// — later-generated IDs string-compare greater than earlier ones — which
/// matches the monotonicity requirement for batch / position ordering.
///
/// Use for:
/// - `BatchId` — a batch's identifier is the first message's position.
/// - Per-message position keys stored on `pattern_db::Message.position`.
///
/// Thread-safe and non-blocking in practice; blocks briefly only if the
/// per-ms sequence counter is exhausted (65k/ms).
pub fn new_snowflake_id() -> SmolStr {
    use smol_str::ToSmolStr;
    crate::utils::get_next_message_position_sync().to_smolstr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_returns_32_char_hex_string() {
        let id = new_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn new_id_values_are_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
    }

    #[test]
    fn new_snowflake_id_is_non_empty() {
        let id = new_snowflake_id();
        assert!(!id.is_empty());
    }

    /// AC1.9 (v3-task-skill-blocks): 32 concurrent calls to `new_snowflake_id`
    /// produce 32 distinct ids — proves collision resistance under concurrent
    /// multi-agent creates via the ferroid `AtomicSnowflakeGenerator`.
    #[test]
    fn new_snowflake_id_is_collision_resistant_concurrently() {
        use std::{collections::HashSet, thread};

        let handles: Vec<_> = (0..32).map(|_| thread::spawn(new_snowflake_id)).collect();
        let ids: HashSet<SmolStr> = handles
            .into_iter()
            .map(|h| h.join().expect("thread must not panic"))
            .collect();
        assert_eq!(
            ids.len(),
            32,
            "32 concurrent new_snowflake_id() calls must produce 32 distinct ids"
        );
    }
}
