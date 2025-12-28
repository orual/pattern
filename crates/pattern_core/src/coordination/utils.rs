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
