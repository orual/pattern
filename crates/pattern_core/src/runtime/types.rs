//! Runtime configuration types

use crate::context::ContextConfig;
use crate::model::ResponseOptions;
use std::collections::HashMap;
use std::time::Duration;

/// Configuration for AgentRuntime behavior
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Timeout for tool execution
    pub tool_timeout: Duration,

    /// Whether to require permission checks for tools
    pub require_permissions: bool,

    /// Cooldown period between agent-to-agent messages to prevent loops
    pub agent_message_cooldown: Duration,

    /// Configuration for context building
    pub context_config: ContextConfig,

    /// Model-specific response options (keyed by model ID)
    /// Each ResponseOptions contains ModelInfo for that model
    pub model_options: HashMap<String, ResponseOptions>,

    /// Default response options to use when no model_id is specified or when the model_id is not found
    pub default_response_options: Option<ResponseOptions>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            tool_timeout: Duration::from_secs(30),
            require_permissions: true,
            agent_message_cooldown: Duration::from_secs(30),
            context_config: Default::default(),
            model_options: HashMap::new(),
            default_response_options: None,
        }
    }
}

impl RuntimeConfig {
    /// Get ResponseOptions for a model, if configured
    pub fn get_model_options(&self, model_id: &str) -> Option<&ResponseOptions> {
        self.model_options.get(model_id)
    }

    /// Register ResponseOptions for a model
    pub fn set_model_options(&mut self, model_id: impl Into<String>, options: ResponseOptions) {
        self.model_options.insert(model_id.into(), options);
    }

    /// Get the default ResponseOptions, if configured
    pub fn get_default_options(&self) -> Option<&ResponseOptions> {
        self.default_response_options.as_ref()
    }

    /// Set the default ResponseOptions
    pub fn set_default_options(&mut self, options: ResponseOptions) {
        self.default_response_options = Some(options);
    }
}
