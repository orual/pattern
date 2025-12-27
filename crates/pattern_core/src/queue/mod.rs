//! Queue processing infrastructure
//!
//! Replaces SurrealDB live queries with polling for message queue and scheduled wakeups.

mod processor;

pub use processor::{QueueConfig, QueueProcessor};
