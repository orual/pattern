// MOVING TO: pattern_runtime/src/coordination/utils.rs
// ORIGIN: crates/pattern_core/src/coordination/utils.rs
// PHASE: future-subagent
// RESHAPE: Full reshape pending subagent-primitives plan
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Utility functions for coordination patterns

use crate::messages::{MessageContent, Response, ResponseMetadata};
use genai::{ModelIden, adapter::AdapterKind};

/// Create a simple text response
pub fn text_response(text: impl Into<String>) -> Response {
    Response {
        content: vec![MessageContent::Text(text.into())],
        reasoning: None,
        metadata: ResponseMetadata {
            processing_time: None,
            tokens_used: None,
            model_used: None,
            confidence: None,
            model_iden: ModelIden::new(AdapterKind::Anthropic, "coordination"),
            custom: Default::default(),
        },
    }
}
