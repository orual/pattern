// MOVING TO: pattern_runtime/src/agent/processing/mod.rs
// ORIGIN: crates/pattern_core/src/agent/processing/mod.rs
// PHASE: 3
// RESHAPE: See Phase 3 for decomposition
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Processing loop implementation for agents.
//!
//! This module contains the core processing logic extracted from DatabaseAgent,
//! organized into reusable components:
//!
//! - `content`: Content block iteration and processing
//! - `errors`: Processing error types and centralized error handling
//! - `retry`: Model completion with retry logic
//! - `loop_impl`: Main processing loop and helper functions

mod content;
mod errors;
mod loop_impl;
mod retry;

pub use content::{ContentItem, iter_content_items};
pub use errors::{ErrorContext, ProcessingError, handle_processing_error, run_error_recovery};
pub use loop_impl::{LoopOutcome, ProcessingContext, ProcessingState, run_processing_loop};
pub use retry::{PromptModification, RetryConfig, RetryDecision, complete_with_retry};
