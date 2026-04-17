// MOVING TO: pattern_runtime/src/queue/mod.rs
// ORIGIN: crates/pattern_core/src/queue/mod.rs
// PHASE: future
// RESHAPE: May be superseded by tidepool-runtime turn scheduler; reshape or retire during subagent/runtime plan
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Queue processing infrastructure
//!
//! Provides polling-based message queue and scheduled wakeup processing.

mod processor;

pub use processor::{QueueConfig, QueueProcessor};
