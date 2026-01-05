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
