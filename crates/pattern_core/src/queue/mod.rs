//! Queue processing infrastructure
//!
//! Provides polling-based message queue and scheduled wakeup processing.

mod processor;

pub use processor::{QueueConfig, QueueProcessor};
