//! Utility functions and helpers for pattern-core

pub mod debug;
pub mod error_logging;

/// Serde helpers for serializing `Option<Duration>` as milliseconds
pub mod duration_millis {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    /// Serialize an optional Duration as milliseconds
    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => d.as_millis().serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize milliseconds into an optional Duration
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis: Option<u64> = Option::deserialize(deserializer)?;
        Ok(millis.map(Duration::from_millis))
    }
}

/// Serde helpers for serializing `Option<Duration>` as seconds
pub mod duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    /// Serialize an optional Duration as seconds
    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => d.as_secs().serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize seconds into an optional Duration
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs: Option<u64> = Option::deserialize(deserializer)?;
        Ok(secs.map(Duration::from_secs))
    }
}

/// Serde helpers for serializing Duration (not optional) as milliseconds
pub mod serde_duration {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    /// Serialize a Duration as milliseconds
    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        duration.as_millis().serialize(serializer)
    }

    /// Deserialize milliseconds into a Duration
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis: u64 = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

/// Format a duration in a human-readable way
pub fn format_duration(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    let millis = duration.subsec_millis();

    let mut parts = Vec::new();

    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if seconds > 0 || (parts.is_empty() && millis == 0) {
        parts.push(format!("{}s", seconds));
    }
    if millis > 0 && parts.is_empty() {
        parts.push(format!("{}ms", millis));
    }

    parts.join(" ")
}

use ferroid::{Base32SnowExt, SnowflakeGeneratorAsyncTokioExt, SnowflakeMastodonId};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

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
        // Use the efficient base32 encoding via Display
        write!(f, "{}", self.0)
    }
}

impl FromStr for SnowflakePosition {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        // Try parsing as base32 first
        if let Ok(id) = SnowflakeMastodonId::decode(s) {
            return Ok(Self(id));
        }

        // Fall back to parsing as raw u64
        s.parse::<u64>()
            .map(|raw| Self(SnowflakeMastodonId::from_raw(raw)))
            .map_err(|e| format!("Failed to parse snowflake as base32 or u64: {}", e))
    }
}

impl Serialize for SnowflakePosition {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as string using Display
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SnowflakePosition {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize from string and parse
        let s = String::deserialize(deserializer)?;
        s.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

/// Type alias for the Snowflake generator we're using
type SnowflakeGen = ferroid::AtomicSnowflakeGenerator<SnowflakeMastodonId, ferroid::MonotonicClock>;

/// Global ID generator for message positions using Snowflake IDs
/// This provides distributed, monotonic IDs that work across processes
static MESSAGE_POSITION_GENERATOR: OnceLock<SnowflakeGen> = OnceLock::new();

pub fn get_position_generator() -> &'static SnowflakeGen {
    MESSAGE_POSITION_GENERATOR.get_or_init(|| {
        // Use machine ID 0 for now - in production this would be configurable
        let clock = ferroid::MonotonicClock::with_epoch(ferroid::TWITTER_EPOCH);
        ferroid::AtomicSnowflakeGenerator::new(0, clock)
    })
}

/// Get the next message position synchronously
///
/// This is designed for use in synchronous contexts like Default impls.
/// In practice, we don't generate messages fast enough to hit the sequence
/// limit (65536/ms), so Pending should rarely happen in production.
///
/// When the sequence is exhausted (e.g., in parallel tests), this will block
/// briefly until the next millisecond boundary to get a fresh sequence.
pub fn get_next_message_position_sync() -> SnowflakePosition {
    use ferroid::IdGenStatus;

    let generator = get_position_generator();

    loop {
        match generator.next_id() {
            IdGenStatus::Ready { id } => return SnowflakePosition::new(id),
            IdGenStatus::Pending { yield_for } => {
                // If yield_for is 0, we're at the sequence limit but still in the same millisecond.
                // Wait at least 1ms to roll over to the next millisecond and reset the sequence.
                let wait_ms = yield_for.max(1) as u64;
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                // Loop will retry after the wait
            }
        }
    }
}

/// Get the next message position as a Snowflake ID (async version)
pub async fn get_next_message_position() -> SnowflakePosition {
    let id = get_position_generator()
        .try_next_id_async()
        .await
        .expect("for now we are assuming this succeeds");
    SnowflakePosition::new(id)
}

/// Get the next message position as a String (for database storage)
pub async fn get_next_message_position_string() -> String {
    get_next_message_position().await.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1h 1m 1s");
        assert_eq!(format_duration(Duration::from_secs(90061)), "1d 1h 1m 1s");
    }

    #[test]
    fn test_duration_millis_serde() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize)]
        struct TestStruct {
            #[serde(with = "duration_millis")]
            duration: Option<Duration>,
        }

        let test = TestStruct {
            duration: Some(Duration::from_millis(1500)),
        };

        let json = serde_json::to_string(&test).unwrap();
        assert_eq!(json, r#"{"duration":1500}"#);

        let deserialized: TestStruct = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.duration, Some(Duration::from_millis(1500)));

        let test_none = TestStruct { duration: None };
        let json_none = serde_json::to_string(&test_none).unwrap();
        assert_eq!(json_none, r#"{"duration":null}"#);
    }
}
