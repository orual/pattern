//! Type-safe ID generation and management.
//!
//! This module provides a generic, type-safe ID system with consistent prefixes
//! and UUID-based uniqueness guarantees.
//!
//! # Design
//!
//! Most IDs are thin wrappers around `String` containing a UUID in simple
//! (non-hyphenated) format. The `Display` format for macro-generated IDs is
//! `prefix:uuid`, e.g. `"user:4bf5122f..."`. [`AgentId`] and [`MessageId`] are
//! exceptions: they display as their inner string without a prefix because they
//! interoperate with external APIs that expect arbitrary strings.
//!
//! # Examples
//!
//! ```
//! use pattern_core::types::ids::{AgentId, UserId};
//!
//! let agent = AgentId::generate();
//! let user = UserId::generate();
//! assert_ne!(agent.to_string(), user.to_string());
//! ```

use jacquard::IntoStatic;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::str::FromStr;
use uuid::Uuid;

/// Trait for types that can be used as ID markers.
pub trait IdType: Send + Sync + 'static {
    /// The type prefix used in display and routing (e.g. `"agent"`, `"user"`).
    const PREFIX: &'static str;

    /// Convert to a string key for storage.
    fn to_key(&self) -> String;

    /// Convert from a string key.
    fn from_key(key: &str) -> Result<Self, IdError>
    where
        Self: Sized;
}

/// Errors that can occur when working with IDs.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum IdError {
    /// The ID string had a prefix that did not match the expected type.
    #[error("invalid ID format: expected prefix '{expected}', got '{actual}'")]
    #[diagnostic(help("ensure the ID starts with the correct prefix followed by an underscore"))]
    InvalidPrefix { expected: String, actual: String },

    /// The UUID portion of the ID was not a valid UUID.
    #[error("invalid UUID: {0}")]
    #[diagnostic(help("the UUID portion of the ID must be a valid UUID v4 format"))]
    InvalidUuid(#[from] uuid::Error),

    /// The ID string did not match the expected format.
    #[error("invalid ID format: {0}")]
    #[diagnostic(help(
        "IDs must be in the format 'prefix:uuid' where prefix matches the expected type"
    ))]
    InvalidFormat(String),
}

/// Macro to define new ID types with minimal boilerplate.
///
/// Generated types support `Display`, `FromStr`, `Serialize`, `Deserialize`,
/// `JsonSchema`, `Hash`, and equality.
///
/// The display format is `prefix:uuid`, e.g. `"user:4bf5122f..."`.
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

        impl $crate::types::ids::IdType for $type_name {
            const PREFIX: &'static str = $table;

            fn to_key(&self) -> String {
                self.0.clone()
            }

            fn from_key(key: &str) -> Result<Self, $crate::types::ids::IdError> {
                Ok($type_name(key.to_string()))
            }
        }

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "{}:{}",
                    <$type_name as $crate::types::ids::IdType>::PREFIX,
                    self.0,
                )
            }
        }

        impl $type_name {
            /// Generate a new random ID backed by UUIDv4.
            pub fn generate() -> Self {
                $type_name(::uuid::Uuid::new_v4().simple().to_string())
            }

            /// Return the nil (all-zero) ID.
            pub fn nil() -> Self {
                $type_name(::uuid::Uuid::nil().simple().to_string())
            }

            /// Return the inner string as used in database storage.
            pub fn to_record_id(&self) -> String {
                self.0.clone()
            }

            /// Construct from an existing [`uuid::Uuid`].
            ///
            /// # Examples
            ///
            /// ```
            /// # use uuid::Uuid;
            /// use pattern_core::types::ids::UserId;
            /// let id = UserId::from_uuid(Uuid::nil());
            /// assert!(id.is_nil());
            /// ```
            pub fn from_uuid(uuid: ::uuid::Uuid) -> Self {
                $type_name(uuid.simple().to_string())
            }

            /// Check if this is the nil/zero ID.
            pub fn is_nil(&self) -> bool {
                self.0 == ::uuid::Uuid::nil().simple().to_string()
            }
        }

        impl ::std::str::FromStr for $type_name {
            type Err = $crate::types::ids::IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok($type_name(s.to_string()))
            }
        }
    };
}

define_id_type!(RelationId, "rel");
define_id_type!(UserId, "user");
define_id_type!(ConversationId, "convo");
define_id_type!(TaskId, "task");
define_id_type!(ToolCallId, "toolcall");
define_id_type!(WakeupId, "wakeup");
define_id_type!(QueuedMessageId, "queue_msg");
define_id_type!(BatchId, "batch");
define_id_type!(MemoryId, "mem");
define_id_type!(EventId, "event");
define_id_type!(SessionId, "session");
define_id_type!(ModelId, "model");
define_id_type!(RequestId, "request");
define_id_type!(GroupId, "group");
define_id_type!(ConstellationId, "constellation");
define_id_type!(OAuthTokenId, "oauth");
define_id_type!(DiscordIdentityId, "discord_identity");

// New v3 IDs
define_id_type!(WorkspaceId, "workspace");
define_id_type!(ProjectId, "project");

impl Default for UserId {
    fn default() -> Self {
        UserId::generate()
    }
}

/// Identifier for an agent in the system.
///
/// Unlike most IDs, `AgentId` accepts arbitrary strings (not just UUIDs) so
/// that human-readable names like `"orual-companion"` and external identifiers
/// interoperate without conversion. The inner string is displayed directly
/// without any prefix.
///
/// # Examples
///
/// ```
/// use pattern_core::types::ids::AgentId;
///
/// let by_name = AgentId::new("orual-companion");
/// let by_uuid = AgentId::generate();
/// assert_eq!(by_name.to_string(), "orual-companion");
/// assert_ne!(by_uuid.to_string(), "orual-companion");
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
#[repr(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    /// Create a new `AgentId` from any string.
    pub fn new(id: impl Into<String>) -> Self {
        AgentId(id.into())
    }

    /// Generate a new random `AgentId` backed by UUIDv4.
    pub fn generate() -> Self {
        AgentId(Uuid::new_v4().simple().to_string())
    }

    /// Return the nil `AgentId` (all-zero UUID).
    pub fn nil() -> Self {
        AgentId(Uuid::nil().simple().to_string())
    }

    /// Construct from an existing [`uuid::Uuid`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        AgentId(uuid.simple().to_string())
    }

    /// Check if this is the nil/zero ID.
    pub fn is_nil(&self) -> bool {
        self.0 == Uuid::nil().simple().to_string()
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the inner string as used in database storage.
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

/// Identifier for a message.
///
/// Displays as its inner string without a prefix, because message IDs
/// interoperate with Anthropic/OpenAI APIs that expect arbitrary strings like
/// `"msg_<uuid>"`.
///
/// # Examples
///
/// ```
/// use pattern_core::types::ids::MessageId;
///
/// let id = MessageId::generate();
/// assert!(id.to_string().starts_with("msg_"));
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
#[repr(transparent)]
pub struct MessageId(pub String);

impl Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl MessageId {
    /// Generate a new random `MessageId` with an `"msg_"` prefix.
    pub fn generate() -> Self {
        let uuid = uuid::Uuid::new_v4().simple();
        MessageId(format!("msg_{}", uuid))
    }

    /// Return the inner string as used in database storage.
    pub fn to_record_id(&self) -> String {
        self.0.clone()
    }

    /// Construct from an existing [`uuid::Uuid`], prefixed with `"msg_"`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use uuid::Uuid;
    /// use pattern_core::types::ids::MessageId;
    /// let id = MessageId::from_uuid(Uuid::nil());
    /// assert!(id.to_string().starts_with("msg_"));
    /// ```
    pub fn from_uuid(uuid: Uuid) -> Self {
        MessageId(format!("msg_{}", uuid.simple()))
    }

    /// Return the canonical nil `MessageId`.
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

/// A Decentralised Identifier (DID) following the `did:plc` or `did:web`
/// standards.
///
/// Unlike most IDs, `Did` does not follow the `prefix:uuid` format. It wraps
/// the validated `jacquard::types::string::Did` type.
///
/// # Examples
///
/// ```
/// use std::str::FromStr;
/// use pattern_core::types::ids::Did;
///
/// let did = Did::from_str("did:plc:abc123").unwrap();
/// assert!(did.to_string().starts_with("did:"));
/// ```
#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Did(#[serde(borrow)] pub jacquard::types::string::Did<'static>);

impl std::fmt::Display for Did {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Did {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Did(jacquard::types::string::Did::new(s)
            .map_err(|_| IdError::InvalidFormat(format!("invalid DID format: {}", s)))?
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
            .map_err(|_| IdError::InvalidFormat(format!("invalid DID format: {}", key)))?
            .into_static()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn agent_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = AgentId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: AgentId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn message_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = MessageId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: MessageId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn user_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = UserId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: UserId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn batch_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = BatchId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: BatchId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn conversation_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = ConversationId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: ConversationId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn task_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = TaskId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: TaskId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn session_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = SessionId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: SessionId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn workspace_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = WorkspaceId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: WorkspaceId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn project_id_roundtrip(uuid_bytes in any::<[u8; 16]>()) {
            let id = ProjectId::from_uuid(Uuid::from_bytes(uuid_bytes));
            let as_str = id.to_string();
            let parsed: ProjectId = as_str.parse().expect("roundtrip");
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn agent_id_display_is_inner_string(uuid_bytes in any::<[u8; 16]>()) {
            let uuid = Uuid::from_bytes(uuid_bytes);
            let id = AgentId::from_uuid(uuid);
            // AgentId displays without a prefix — just the inner string.
            prop_assert_eq!(id.to_string(), uuid.simple().to_string());
        }

        #[test]
        fn message_id_display_has_msg_prefix(uuid_bytes in any::<[u8; 16]>()) {
            let uuid = Uuid::from_bytes(uuid_bytes);
            let id = MessageId::from_uuid(uuid);
            prop_assert!(id.to_string().starts_with("msg_"));
        }

        #[test]
        fn workspace_id_display_has_workspace_prefix(uuid_bytes in any::<[u8; 16]>()) {
            let uuid = Uuid::from_bytes(uuid_bytes);
            let id = WorkspaceId::from_uuid(uuid);
            prop_assert!(id.to_string().starts_with("workspace:"));
        }

        #[test]
        fn project_id_display_has_project_prefix(uuid_bytes in any::<[u8; 16]>()) {
            let uuid = Uuid::from_bytes(uuid_bytes);
            let id = ProjectId::from_uuid(uuid);
            prop_assert!(id.to_string().starts_with("project:"));
        }
    }
}
