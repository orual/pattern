//! `TaskItemId` newtype for task item identifiers.
//!
//! Each task item gets a time-ordered Snowflake id minted by the workspace's
//! ferroid-backed generator. Ids are base32-encoded Mastodon-style Snowflakes:
//! lexicographically sortable, collision-resistant across concurrent calls.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use smol_str::SmolStr;

/// A unique identifier for a task item within a task list block.
///
/// Ids are base32-encoded Mastodon-style Snowflakes generated via
/// [`crate::types::ids::new_snowflake_id`]. Any non-empty string is accepted
/// by [`parse`][TaskItemId::parse] — Snowflake shape is NOT validated at
/// parse time, which allows short synthetic ids in fixtures and tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskItemId(SmolStr);

impl TaskItemId {
    /// Mint a fresh `TaskItemId` using the workspace Snowflake generator.
    ///
    /// The underlying generator is an `AtomicSnowflakeGenerator` with a
    /// monotonic clock; collision resistance across concurrent calls is
    /// guaranteed by atomic counter increments within the same millisecond.
    pub fn new() -> Self {
        Self(crate::types::ids::new_snowflake_id())
    }

    /// Parse a `TaskItemId` from a string slice.
    ///
    /// Rejects empty strings with [`TaskItemIdError::Empty`].
    /// Does NOT validate Snowflake encoding — any non-empty string is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`TaskItemIdError::Empty`] if `s` is empty.
    pub fn parse(s: &str) -> Result<Self, TaskItemIdError> {
        if s.is_empty() {
            return Err(TaskItemIdError::Empty);
        }
        Ok(Self(SmolStr::from(s)))
    }

    /// Return the inner string value.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Default for TaskItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TaskItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl FromStr for TaskItemId {
    type Err = TaskItemIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TaskItemId::parse(s)
    }
}

// --- Serde: transparent string ---

impl Serialize for TaskItemId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for TaskItemId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <&str as Deserialize>::deserialize(deserializer)?;
        TaskItemId::parse(s).map_err(de::Error::custom)
    }
}

// --- Error type ---

/// Errors that can occur when parsing a [`TaskItemId`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskItemIdError {
    /// The input string was empty.
    #[error("task item id must not be empty")]
    Empty,
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    // AC1.8 — parse("") → TaskItemIdError::Empty
    #[test]
    fn parse_empty_string_returns_empty_error() {
        let result = TaskItemId::parse("");
        assert!(
            matches!(result, Err(TaskItemIdError::Empty)),
            "expected TaskItemIdError::Empty, got {result:?}"
        );
    }

    // AC1.8 — new() produces a non-empty string that round-trips through parse
    #[test]
    fn new_produces_non_empty_id() {
        let id = TaskItemId::new();
        assert!(!id.as_str().is_empty(), "new() must produce a non-empty id");
    }

    #[test]
    fn new_round_trips_through_parse() {
        let id = TaskItemId::new();
        let s = id.to_string();
        let reparsed = TaskItemId::parse(&s).expect("parse of a fresh id must succeed");
        assert_eq!(
            id, reparsed,
            "round-trip through parse must preserve the value"
        );
    }

    #[test]
    fn from_str_is_equivalent_to_parse() {
        let id: TaskItemId = "fixture-id"
            .parse()
            .expect("parse must accept non-empty strings");
        assert_eq!(id.as_str(), "fixture-id");
    }

    // AC1.8 — FromStr propagates the Empty error
    #[test]
    fn from_str_empty_returns_error() {
        let result: Result<TaskItemId, _> = "".parse();
        assert!(matches!(result, Err(TaskItemIdError::Empty)));
    }

    // AC1.8 — Display round-trips
    #[test]
    fn display_matches_as_str() {
        let id = TaskItemId::parse("some-id").unwrap();
        assert_eq!(id.to_string(), "some-id");
        assert_eq!(id.as_str(), "some-id");
    }

    // AC1.9 — 32 concurrent threads produce 32 distinct ids
    #[test]
    fn concurrent_new_produces_distinct_ids() {
        use std::thread;

        let handles: Vec<_> = (0..32).map(|_| thread::spawn(TaskItemId::new)).collect();
        let ids: HashSet<String> = handles
            .into_iter()
            .map(|h| h.join().expect("thread must not panic").to_string())
            .collect();

        assert_eq!(
            ids.len(),
            32,
            "32 concurrent TaskItemId::new() calls must produce 32 distinct ids"
        );
    }

    // Serde: serialises to a quoted string
    #[test]
    fn serde_serialises_as_string() {
        let id = TaskItemId::parse("test-id-123").unwrap();
        let json = serde_json::to_string(&id).expect("serialization must succeed");
        assert_eq!(json, r#""test-id-123""#);
    }

    // Serde: deserialises from a string
    #[test]
    fn serde_deserialises_from_string() {
        let id: TaskItemId = serde_json::from_str(r#""test-id-123""#)
            .expect("deserialization of a valid id must succeed");
        assert_eq!(id.as_str(), "test-id-123");
    }

    // Serde: rejects empty string on deserialise
    #[test]
    fn serde_rejects_empty_string() {
        let result: Result<TaskItemId, _> = serde_json::from_str(r#""""#);
        assert!(result.is_err(), "deserializing empty string must fail");
    }

    // Serde round-trip: to_string → from_str
    #[test]
    fn serde_round_trip() {
        let original = TaskItemId::new();
        let json = serde_json::to_string(&original).unwrap();
        let recovered: TaskItemId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, recovered);
    }

    // Clone and equality
    #[test]
    fn clone_and_equality() {
        let a = TaskItemId::parse("abc").unwrap();
        let b = a.clone();
        assert_eq!(a, b);
    }

    // Hash: equal ids hash equal (required by std::hash contract)
    #[test]
    fn hash_consistent_with_equality() {
        use std::collections::HashSet;
        let a = TaskItemId::parse("abc").unwrap();
        let b = TaskItemId::parse("abc").unwrap();
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b), "equal ids must hash the same");
    }
}
