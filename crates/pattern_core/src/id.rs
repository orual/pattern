//! Type-safe ID generation and management
//!
//! This module provides a generic, type-safe ID system with consistent prefixes
//! and UUID-based uniqueness guarantees.

use jacquard::IntoStatic;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::str::FromStr;
use uuid::Uuid;

/// Trait for types that can be used as ID markers
pub trait IdType: Send + Sync + 'static {
    /// The table name for this ID type (e.g., "agent" for agents, "user" for users)
    const PREFIX: &'static str;

    /// Convert to a string key for RecordId
    fn to_key(&self) -> String;

    /// Convert from a string key
    fn from_key(key: &str) -> Result<Self, IdError>
    where
        Self: Sized;
}

/// Errors that can occur when working with IDs
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum IdError {
    #[error("Invalid ID format: expected prefix '{expected}', got '{actual}'")]
    #[diagnostic(help("Ensure the ID starts with the correct prefix followed by an underscore"))]
    InvalidPrefix { expected: String, actual: String },

    #[error("Invalid UUID: {0}")]
    #[diagnostic(help("The UUID portion of the ID must be a valid UUID v4 format"))]
    InvalidUuid(#[from] uuid::Error),

    #[error("Invalid ID format: {0}")]
    #[diagnostic(help(
        "IDs must be in the format 'prefix_uuid' where prefix matches the expected type"
    ))]
    InvalidFormat(String),
}

/// Macro to define new ID types with minimal boilerplate
#[macro_export]
macro_rules! define_id_type {
    ($type_name:ident, $table:expr) => {
        #[derive(
            Debug,
            PartialEq,
            Eq,
            Hash,
            Clone,
            ::serde::Serialize,
            ::serde::Deserialize,
            ::schemars::JsonSchema,
        )]
        pub struct $type_name(pub String);

        impl $crate::id::IdType for $type_name {
            const PREFIX: &'static str = $table;

            fn to_key(&self) -> String {
                self.0.clone()
            }

            fn from_key(key: &str) -> Result<Self, $crate::id::IdError> {
                Ok($type_name(key.to_string()))
            }
        }

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "{}:{}",
                    <$type_name as $crate::id::IdType>::PREFIX,
                    self.0,
                )
            }
        }

        impl $type_name {
            pub fn generate() -> Self {
                $type_name(::uuid::Uuid::new_v4().simple().to_string())
            }

            pub fn nil() -> Self {
                $type_name(::uuid::Uuid::nil().simple().to_string())
            }

            pub fn to_record_id(&self) -> String {
                self.0.clone()
            }

            pub fn from_uuid(uuid: ::uuid::Uuid) -> Self {
                $type_name(uuid.simple().to_string())
            }

            pub fn is_nil(&self) -> bool {
                self.0 == ::uuid::Uuid::nil().simple().to_string()
            }
        }

        impl ::std::str::FromStr for $type_name {
            type Err = $crate::id::IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok($type_name(s.to_string()))
            }
        }
    };
}

define_id_type!(RelationId, "rel");

/// AgentId is a simple string wrapper for agent identification.
/// Unlike other ID types, it accepts any string (not just UUIDs) for flexibility.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
#[repr(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    /// Create a new AgentId from any string
    pub fn new(id: impl Into<String>) -> Self {
        AgentId(id.into())
    }

    /// Generate a new random AgentId (UUID-based)
    pub fn generate() -> Self {
        AgentId(Uuid::new_v4().simple().to_string())
    }

    /// Create a nil/empty AgentId
    pub fn nil() -> Self {
        AgentId(Uuid::nil().simple().to_string())
    }

    /// Create from a UUID (for Entity macro compatibility)
    pub fn from_uuid(uuid: Uuid) -> Self {
        AgentId(uuid.simple().to_string())
    }

    /// Check if this is a nil ID
    pub fn is_nil(&self) -> bool {
        self.0 == Uuid::nil().simple().to_string()
    }

    /// Get the inner string value
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert to record ID string (for database)
    pub fn to_record_id(&self) -> String {
        self.0.clone()
    }
}

impl Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        AgentId(s)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        AgentId(s.to_string())
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for AgentId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(AgentId(s.to_string()))
    }
}

impl IdType for AgentId {
    const PREFIX: &'static str = "agent";

    fn to_key(&self) -> String {
        self.0.clone()
    }

    fn from_key(key: &str) -> Result<Self, IdError> {
        Ok(AgentId(key.to_string()))
    }
}

// Other ID types using the macro
define_id_type!(UserId, "user");
define_id_type!(ConversationId, "convo");
define_id_type!(TaskId, "task");
define_id_type!(ToolCallId, "toolcall");
define_id_type!(WakeupId, "wakeup");
define_id_type!(QueuedMessageId, "queue_msg");

impl Default for UserId {
    fn default() -> Self {
        UserId::generate()
    }
}

/// Unlike other IDs in the system, MessageId doesn't follow the `prefix_uuid`
/// format because it needs to be compatible with Anthropic/OpenAI APIs which
/// expect arbitrary string UUIDs.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
#[repr(transparent)]
pub struct MessageId(pub String);

impl Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// MessageId cannot implement Copy because String doesn't implement Copy
// This is intentional as MessageId needs to own its string data

impl MessageId {
    pub fn generate() -> Self {
        let uuid = uuid::Uuid::new_v4().simple();
        MessageId(format!("msg_{}", uuid))
    }

    pub fn to_record_id(&self) -> String {
        // Return the full string as the record key
        // MessageId can be arbitrary strings for API compatibility
        self.0.clone()
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        MessageId(format!("msg_{}", uuid))
    }

    pub fn nil() -> Self {
        MessageId("msg_nil".to_string())
    }
}

impl IdType for MessageId {
    const PREFIX: &'static str = "msg";

    fn to_key(&self) -> String {
        self.0.clone()
    }

    fn from_key(key: &str) -> Result<Self, IdError> {
        Ok(MessageId(key.to_string()))
    }
}

impl FromStr for MessageId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(MessageId(s.to_string()))
    }
}

impl JsonSchema for Did {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "did".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        generator.root_schema_for::<String>()
    }
}

/// Unlike other IDs in the system, Did doesn't follow the `prefix_uuid`
/// format because it follows the DID standard (did:plc, did:web)
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Did(#[serde(borrow)] pub jacquard::types::string::Did<'static>);

impl std::fmt::Display for Did {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_string())
    }
}

impl FromStr for Did {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Did(jacquard::types::string::Did::new(s)
            .map_err(|_| IdError::InvalidFormat(format!("Invalid DID format: {}", s)))?
            .into_static()))
    }
}

impl IdType for Did {
    const PREFIX: &'static str = "";

    fn to_key(&self) -> String {
        self.0.to_string()
    }

    fn from_key(key: &str) -> Result<Self, IdError> {
        Ok(Did(jacquard::types::string::Did::new(key)
            .map_err(|_| IdError::InvalidFormat(format!("Invalid DID format: {}", key)))?
            .into_static()))
    }
}

// More ID types using the macro
define_id_type!(MemoryId, "mem");
define_id_type!(EventId, "event");
define_id_type!(SessionId, "session");

// Define new ID types using the macro
define_id_type!(ModelId, "model");
define_id_type!(RequestId, "request");
define_id_type!(GroupId, "group");
define_id_type!(ConstellationId, "constellation");
define_id_type!(OAuthTokenId, "oauth");
define_id_type!(DiscordIdentityId, "discord_identity");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_generation() {
        let id1 = AgentId::generate();
        let id2 = AgentId::generate();

        // IDs should be unique
        assert_ne!(id1, id2);

        // IDs should have correct table name
        assert_eq!(AgentId::PREFIX, "agent");
    }

    #[test]
    fn test_id_serialization() {
        let id = AgentId::generate();

        // JSON serialization
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_different_id_types() {
        let agent_id = AgentId::generate();
        let user_id = UserId::generate();
        let task_id = TaskId::generate();

        // All should be different UUIDs
        assert_ne!(agent_id.0, user_id.0);
        assert_ne!(user_id.0, task_id.0);
    }
}
