// MOVING TO: pattern_runtime/src/coordination/patterns/mod.rs
// ORIGIN: crates/pattern_core/src/coordination/patterns/mod.rs
// PHASE: future-subagent
// RESHAPE: Full reshape pending subagent-primitives plan
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Coordination pattern implementations

mod dynamic;
mod pipeline;
mod round_robin;
mod sleeptime;
mod supervisor;
mod voting;

pub use dynamic::DynamicManager;
pub use pipeline::PipelineManager;
pub use round_robin::RoundRobinManager;
pub use sleeptime::SleeptimeManager;
pub use supervisor::SupervisorManager;
pub use voting::VotingManager;
