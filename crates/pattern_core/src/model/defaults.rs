//! Model-specific default configurations
//!
//! This module provides accurate default settings for different language models,
//! including context windows, max output tokens, and capabilities.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::{ModelCapability, ModelInfo};

/// Static registry of model defaults
static MODEL_DEFAULTS: OnceLock<HashMap<&'static str, ModelDefaults>> = OnceLock::new();

/// Default configuration for a specific model
#[derive(Debug, Clone)]
pub struct ModelDefaults {
    /// Maximum context window (input + output tokens)
    context_window: usize,
    /// Maximum output tokens (if different from context_window/4)
    max_output_tokens: Option<usize>,
    /// Model capabilities
    capabilities: Vec<ModelCapability>,
    /// Cost per 1k prompt tokens
    cost_per_1k_prompt: Option<f64>,
    /// Cost per 1k completion tokens
    cost_per_1k_completion: Option<f64>,
}

/// Initialize the model defaults registry
fn init_defaults() -> HashMap<&'static str, ModelDefaults> {
    let mut defaults = HashMap::new();

    // Anthropic Claude models

    defaults.insert(
        "claude-sonnet-4-5-20250929",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::CodeExecution,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::ComputerUse,
                ModelCapability::TextEdit,
                ModelCapability::WebSearch,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.03),
            cost_per_1k_completion: Some(0.015),
        },
    );

    defaults.insert(
        "claude-opus-4-1-20250805",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(32_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::CodeExecution,
                ModelCapability::ComputerUse,
                ModelCapability::VisionInput,
                ModelCapability::TextEdit,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.015),
            cost_per_1k_completion: Some(0.075),
        },
    );

    defaults.insert(
        "claude-opus-4-20250514",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(32_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::CodeExecution,
                ModelCapability::ComputerUse,
                ModelCapability::VisionInput,
                ModelCapability::TextEdit,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.015),
            cost_per_1k_completion: Some(0.075),
        },
    );

    defaults.insert(
        "claude-sonnet-4-20250514",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::CodeExecution,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::ComputerUse,
                ModelCapability::TextEdit,
                ModelCapability::WebSearch,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.03),
            cost_per_1k_completion: Some(0.015),
        },
    );
    defaults.insert(
        "claude-3-7-sonnet-20250219",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::CodeExecution,
                ModelCapability::SystemPrompt,
                ModelCapability::ComputerUse,
                ModelCapability::VisionInput,
                ModelCapability::TextEdit,
                ModelCapability::WebSearch,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.003),
            cost_per_1k_completion: Some(0.015),
        },
    );

    defaults.insert(
        "claude-3-opus-20240229",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.015),
            cost_per_1k_completion: Some(0.075),
        },
    );

    defaults.insert(
        "claude-3-sonnet-20240229",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.003),
            cost_per_1k_completion: Some(0.015),
        },
    );

    defaults.insert(
        "claude-3-haiku-20240307",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(4_096),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.00025),
            cost_per_1k_completion: Some(0.00125),
        },
    );

    defaults.insert(
        "claude-3-7-sonnet-latest",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::ComputerUse,
                ModelCapability::TextEdit,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.003),
            cost_per_1k_completion: Some(0.015),
        },
    );

    defaults.insert(
        "claude-haiku-4-5-20251001",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::CodeExecution,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::ComputerUse,
                ModelCapability::TextEdit,
                ModelCapability::WebSearch,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.001),
            cost_per_1k_completion: Some(0.005),
        },
    );

    defaults.insert(
        "claude-3-5-haiku-20241022",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::CodeExecution,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.001),
            cost_per_1k_completion: Some(0.005),
        },
    );

    // OpenAI GPT models
    defaults.insert(
        "gpt-4-turbo",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(4_096),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ],
            cost_per_1k_prompt: Some(0.01),
            cost_per_1k_completion: Some(0.03),
        },
    );

    defaults.insert(
        "gpt-4.1",
        ModelDefaults {
            context_window: 1_047_576, // 1M tokens
            max_output_tokens: Some(32_768),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ],
            cost_per_1k_prompt: Some(0.002),
            cost_per_1k_completion: Some(0.008),
        },
    );

    defaults.insert(
        "gpt-4o",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
                ModelCapability::ImageGeneration,
            ],
            cost_per_1k_prompt: Some(0.0025),
            cost_per_1k_completion: Some(0.01),
        },
    );

    defaults.insert(
        "gpt-4o-mini",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(16_384),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ],
            cost_per_1k_prompt: Some(0.00015),
            cost_per_1k_completion: Some(0.0006),
        },
    );

    defaults.insert(
        "o1",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(100_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::ExtendedThinking,
                ModelCapability::LongContext,
            ],
            cost_per_1k_prompt: Some(0.015),
            cost_per_1k_completion: Some(0.06),
        },
    );
    defaults.insert(
        "o4-mini",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.0011),
            cost_per_1k_completion: Some(0.0044),
        },
    );

    defaults.insert(
        "o3-mini",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.0011),
            cost_per_1k_completion: Some(0.0044),
        },
    );
    defaults.insert(
        "o3",
        ModelDefaults {
            context_window: 200_000,
            max_output_tokens: Some(100_000),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.002),
            cost_per_1k_completion: Some(0.008),
        },
    );

    // Google Gemini models
    defaults.insert(
        "gemini-1.5-pro",
        ModelDefaults {
            context_window: 2_097_152, // 2M context
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
                ModelCapability::CodeExecution,
            ],
            cost_per_1k_prompt: Some(0.0035),
            cost_per_1k_completion: Some(0.014),
        },
    );

    defaults.insert(
        "gemini-1.5-flash",
        ModelDefaults {
            context_window: 1_048_576, // 1M context
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ],
            cost_per_1k_prompt: Some(0.00035),
            cost_per_1k_completion: Some(0.0014),
        },
    );

    defaults.insert(
        "gemini-2.0-flash",
        ModelDefaults {
            context_window: 1_048_576, // 1M context
            max_output_tokens: Some(8_192),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ],
            cost_per_1k_prompt: Some(0.00035),
            cost_per_1k_completion: Some(0.0014),
        },
    );

    defaults.insert(
        "gemini-2.5-pro",
        ModelDefaults {
            context_window: 1_048_576, // 1M context
            max_output_tokens: Some(65_536),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
                ModelCapability::JsonMode,
                ModelCapability::CodeExecution,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.00125),
            cost_per_1k_completion: Some(0.005),
        },
    );

    defaults.insert(
        "gemini-2.5-flash",
        ModelDefaults {
            context_window: 1_048_576, // 1M context
            max_output_tokens: Some(65_536),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
                ModelCapability::ExtendedThinking,
            ],
            cost_per_1k_prompt: Some(0.00015),
            cost_per_1k_completion: Some(0.0006),
        },
    );

    // Groq models
    defaults.insert(
        "llama3-70b-8192",
        ModelDefaults {
            context_window: 8_192,
            max_output_tokens: None,
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
            ],
            cost_per_1k_prompt: Some(0.00059),
            cost_per_1k_completion: Some(0.00079),
        },
    );

    defaults.insert(
        "mixtral-8x7b-32768",
        ModelDefaults {
            context_window: 32_768,
            max_output_tokens: None,
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
            ],
            cost_per_1k_prompt: Some(0.00024),
            cost_per_1k_completion: Some(0.00024),
        },
    );

    // Cohere models
    defaults.insert(
        "command-r-plus",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(4_096),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
            ],
            cost_per_1k_prompt: Some(0.003),
            cost_per_1k_completion: Some(0.015),
        },
    );

    defaults.insert(
        "command-r",
        ModelDefaults {
            context_window: 128_000,
            max_output_tokens: Some(4_096),
            capabilities: vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
            ],
            cost_per_1k_prompt: Some(0.0005),
            cost_per_1k_completion: Some(0.0015),
        },
    );

    defaults
}

/// Enhance a ModelInfo with known defaults based on model ID
///
/// This function takes a ModelInfo (potentially from a provider with incomplete data)
/// and enriches it with accurate defaults from our registry.
pub fn enhance_model_info(mut model_info: ModelInfo) -> ModelInfo {
    let defaults = MODEL_DEFAULTS.get_or_init(init_defaults);

    // Try exact match first
    if let Some(model_defaults) = defaults.get(model_info.id.as_str()) {
        apply_defaults(&mut model_info, model_defaults);
        return model_info;
    }

    // Try partial matches for common patterns
    let model_id_lower = model_info.id.to_lowercase();

    // Find the best matching default by checking if the model ID contains the default key
    for (default_id, model_defaults) in defaults.iter() {
        if model_id_lower.contains(default_id) {
            apply_defaults(&mut model_info, model_defaults);
            return model_info;
        }
    }

    // Apply provider-specific defaults if no model match found
    apply_provider_defaults(&mut model_info);

    model_info
}

/// Apply defaults from ModelDefaults to ModelInfo
fn apply_defaults(model_info: &mut ModelInfo, defaults: &ModelDefaults) {
    model_info.context_window = defaults.context_window;

    if defaults.max_output_tokens.is_some() {
        model_info.max_output_tokens = defaults.max_output_tokens;
    } else if model_info.max_output_tokens.is_none() {
        // Default to 1/4 of context window if not specified
        model_info.max_output_tokens = Some(defaults.context_window / 4);
    }

    model_info.capabilities = defaults.capabilities.clone();

    if defaults.cost_per_1k_prompt.is_some() {
        model_info.cost_per_1k_prompt_tokens = defaults.cost_per_1k_prompt;
    }

    if defaults.cost_per_1k_completion.is_some() {
        model_info.cost_per_1k_completion_tokens = defaults.cost_per_1k_completion;
    }
}

/// Apply provider-specific defaults when no model-specific match is found
fn apply_provider_defaults(model_info: &mut ModelInfo) {
    let provider_lower = model_info.provider.to_lowercase();

    match provider_lower.as_str() {
        "openrouter" => {
            // OpenRouter models use provider/model format (e.g., "anthropic/claude-3-opus")
            // Try to extract the underlying provider and model for better defaults
            // Data sourced from OpenRouter API: https://openrouter.ai/api/v1/models
            if let Some(slash_idx) = model_info.id.find('/') {
                let underlying_provider = &model_info.id[..slash_idx];
                let underlying_model = &model_info.id[slash_idx + 1..];

                // Apply defaults based on underlying provider
                match underlying_provider.to_lowercase().as_str() {
                    "anthropic" => {
                        // Base Claude defaults (claude-3-opus, claude-3-haiku)
                        model_info.context_window = 200_000;
                        model_info.max_output_tokens = Some(4_096);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::VisionInput,
                            ModelCapability::LongContext,
                        ];

                        // Claude 4.x series - sonnet/opus variants have different contexts
                        if underlying_model.contains("sonnet-4.5")
                            || underlying_model.contains("sonnet-4")
                        {
                            // claude-sonnet-4.5 and claude-sonnet-4 have 1M context
                            model_info.context_window = 1_000_000;
                            model_info.max_output_tokens = Some(64_000);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                            model_info.capabilities.push(ModelCapability::ComputerUse);
                            model_info.capabilities.push(ModelCapability::TextEdit);
                            model_info.capabilities.push(ModelCapability::CodeExecution);
                        } else if underlying_model.contains("opus-4.5")
                            || underlying_model.contains("opus-4")
                        {
                            // claude-opus-4.5 and claude-opus-4 have 200k context, 32k output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(32_000);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                            model_info.capabilities.push(ModelCapability::ComputerUse);
                            model_info.capabilities.push(ModelCapability::TextEdit);
                            model_info.capabilities.push(ModelCapability::CodeExecution);
                        } else if underlying_model.contains("haiku-4.5") {
                            // claude-haiku-4.5 has 200k context, 64k output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(64_000);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                            model_info.capabilities.push(ModelCapability::ComputerUse);
                            model_info.capabilities.push(ModelCapability::TextEdit);
                            model_info.capabilities.push(ModelCapability::CodeExecution);
                        } else if underlying_model.contains("claude-3.7-sonnet")
                            || underlying_model.contains("3.7-sonnet")
                        {
                            // claude-3.7-sonnet has 200k context, 64k output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(64_000);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                            model_info.capabilities.push(ModelCapability::ComputerUse);
                            model_info.capabilities.push(ModelCapability::TextEdit);
                        } else if underlying_model.contains("claude-3.5-sonnet")
                            || underlying_model.contains("3.5-sonnet")
                        {
                            // claude-3.5-sonnet has 200k context, 8192 output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(8_192);
                        } else if underlying_model.contains("claude-3.5-haiku")
                            || underlying_model.contains("3.5-haiku")
                        {
                            // claude-3.5-haiku has 200k context, 8192 output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(8_192);
                        }
                        // claude-3-opus, claude-3-sonnet, claude-3-haiku keep base defaults (200k/4096)
                    }
                    "openai" => {
                        // Base OpenAI defaults
                        model_info.context_window = 128_000;
                        model_info.max_output_tokens = Some(4_096);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::VisionInput,
                            ModelCapability::LongContext,
                            ModelCapability::JsonMode,
                        ];

                        if underlying_model.starts_with("o1")
                            || underlying_model.starts_with("o3")
                            || underlying_model.starts_with("o4")
                        {
                            // o1/o3/o4 reasoning models: 200k context, 100k output
                            model_info.context_window = 200_000;
                            model_info.max_output_tokens = Some(100_000);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                        } else if underlying_model.contains("gpt-4o") {
                            // gpt-4o variants: 128k context, 16384 output
                            model_info.context_window = 128_000;
                            model_info.max_output_tokens = Some(16_384);
                            if underlying_model.contains(":extended") {
                                model_info.max_output_tokens = Some(64_000);
                            }
                        } else if underlying_model.contains("gpt-4-turbo") {
                            // gpt-4-turbo: 128k context, 4096 output
                            model_info.context_window = 128_000;
                            model_info.max_output_tokens = Some(4_096);
                        } else if underlying_model == "gpt-4" {
                            // gpt-4 base: 8191 context, 4096 output, no vision
                            model_info.context_window = 8_191;
                            model_info.max_output_tokens = Some(4_096);
                            model_info.capabilities = vec![
                                ModelCapability::TextGeneration,
                                ModelCapability::FunctionCalling,
                                ModelCapability::SystemPrompt,
                                ModelCapability::JsonMode,
                            ];
                        } else if underlying_model.contains("gpt-5") {
                            // gpt-5 variants: 400k context (chat variants 128k), 128k output
                            if underlying_model.contains("-chat") {
                                model_info.context_window = 128_000;
                                model_info.max_output_tokens = Some(16_384);
                            } else {
                                model_info.context_window = 400_000;
                                model_info.max_output_tokens = Some(128_000);
                            }
                        }
                    }
                    "google" => {
                        // Gemini models default: 1M context, 8192 output
                        model_info.context_window = 1_048_576;
                        model_info.max_output_tokens = Some(8_192);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::VisionInput,
                            ModelCapability::LongContext,
                            ModelCapability::JsonMode,
                        ];

                        // Gemini 2.5+ models have 65536 output
                        if underlying_model.contains("gemini-2.5")
                            || underlying_model.contains("gemini-3")
                        {
                            model_info.max_output_tokens = Some(65_536);
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                        }
                    }
                    "meta-llama" => {
                        // Llama 3.x defaults: 131072 context (from API)
                        model_info.context_window = 131_072;
                        model_info.max_output_tokens = Some(16_384);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                        ];

                        // Llama 3.1-405b has reduced context on OpenRouter
                        if underlying_model.contains("405b") && !underlying_model.contains(":free")
                        {
                            model_info.context_window = 10_000;
                            model_info.max_output_tokens = None; // varies
                        }
                        // Vision models
                        if underlying_model.contains("vision") {
                            model_info.capabilities.push(ModelCapability::VisionInput);
                        }
                    }
                    "mistralai" => {
                        // Mistral defaults: varies significantly by model
                        model_info.context_window = 131_072;
                        model_info.max_output_tokens = Some(16_384);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                        ];

                        if underlying_model.contains("mistral-large") {
                            // mistral-large: 128k-262k context
                            model_info.context_window = 128_000;
                            model_info.max_output_tokens = None; // varies
                        } else if underlying_model.contains("mixtral-8x22b") {
                            // mixtral-8x22b: 65536 context
                            model_info.context_window = 65_536;
                            model_info.max_output_tokens = None;
                        } else if underlying_model.contains("mixtral-8x7b") {
                            // mixtral-8x7b: 32768 context, 16384 output
                            model_info.context_window = 32_768;
                            model_info.max_output_tokens = Some(16_384);
                        } else if underlying_model.contains("devstral") {
                            // devstral models: up to 262k context
                            model_info.context_window = 262_144;
                            model_info.max_output_tokens = Some(65_536);
                        } else if underlying_model.contains("mistral-medium") {
                            // mistral-medium-3.x: 131k context
                            model_info.context_window = 131_072;
                            model_info.max_output_tokens = None;
                        }
                        // pixtral and ministral models support vision
                        if underlying_model.contains("pixtral")
                            || underlying_model.contains("ministral")
                        {
                            model_info.capabilities.push(ModelCapability::VisionInput);
                        }
                    }
                    "deepseek" => {
                        // DeepSeek defaults: 163840 context, 65536 output
                        model_info.context_window = 163_840;
                        model_info.max_output_tokens = Some(65_536);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                        ];

                        if underlying_model.contains("deepseek-r1") {
                            // R1 reasoning models
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                        }
                        if underlying_model.contains("deepseek-chat") {
                            // deepseek-chat can output up to full context
                            model_info.max_output_tokens = Some(163_840);
                        }
                    }
                    "moonshotai" => {
                        // Moonshot Kimi models: 262144 context
                        model_info.context_window = 262_144;
                        model_info.max_output_tokens = Some(65_535);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::LongContext,
                        ];

                        if underlying_model.contains("thinking") {
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                        }
                        if underlying_model.contains("kimi-k2-0905") {
                            // kimi-k2-0905 can output up to full context
                            model_info.max_output_tokens = Some(262_144);
                        }
                    }
                    "z-ai" => {
                        // GLM models: ~200k context, 65536 output
                        model_info.context_window = 202_752;
                        model_info.max_output_tokens = Some(65_536);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::LongContext,
                        ];

                        if underlying_model.contains("glm-4.5") {
                            // glm-4.5: 131k context
                            model_info.context_window = 131_072;
                        }
                        if underlying_model.contains("glm-4.6v")
                            || underlying_model.contains("glm-4.5v")
                        {
                            model_info.capabilities.push(ModelCapability::VisionInput);
                        }
                    }
                    "qwen" => {
                        // Qwen defaults: varies significantly
                        model_info.context_window = 32_768;
                        model_info.max_output_tokens = Some(16_384);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                        ];

                        if underlying_model.contains("qwen3")
                            || underlying_model.contains("qwen-plus")
                            || underlying_model.contains("qwen-turbo")
                        {
                            // Qwen3 and newer models have larger contexts
                            model_info.context_window = 262_144;
                            model_info.max_output_tokens = Some(32_768);
                        }
                        if underlying_model.contains("-vl-")
                            || underlying_model.contains("vl-max")
                            || underlying_model.contains("vl-plus")
                        {
                            model_info.capabilities.push(ModelCapability::VisionInput);
                        }
                        if underlying_model.contains("thinking") {
                            model_info
                                .capabilities
                                .push(ModelCapability::ExtendedThinking);
                        }
                    }
                    "cohere" => {
                        // Cohere Command models: 128k context, 4000 output
                        model_info.context_window = 128_000;
                        model_info.max_output_tokens = Some(4_000);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::FunctionCalling,
                            ModelCapability::SystemPrompt,
                            ModelCapability::LongContext,
                            ModelCapability::WebSearch,
                        ];

                        if underlying_model.contains("command-a") {
                            // command-a: 256k context, 8192 output
                            model_info.context_window = 256_000;
                            model_info.max_output_tokens = Some(8_192);
                        }
                    }
                    _ => {
                        // Generic OpenRouter defaults for unknown providers
                        model_info.context_window = 32_768;
                        model_info.max_output_tokens = Some(4_096);
                        model_info.capabilities = vec![
                            ModelCapability::TextGeneration,
                            ModelCapability::SystemPrompt,
                        ];
                    }
                }
            } else {
                // No slash in model ID, use generic defaults
                model_info.context_window = 32_768;
                model_info.max_output_tokens = Some(4_096);
                model_info.capabilities = vec![
                    ModelCapability::TextGeneration,
                    ModelCapability::SystemPrompt,
                ];
            }
        }
        "anthropic" => {
            model_info.context_window = 200_000;
            model_info.max_output_tokens = Some(4_096);
            model_info.capabilities = vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ];
        }
        "openai" => {
            model_info.context_window = 128_000;
            model_info.max_output_tokens = Some(4_096);
            model_info.capabilities = vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ];
        }
        "gemini" | "google" => {
            model_info.context_window = 1_048_576; // 1M default for Gemini
            model_info.max_output_tokens = Some(8_192);
            model_info.capabilities = vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::VisionInput,
                ModelCapability::LongContext,
                ModelCapability::JsonMode,
            ];
        }
        "groq" => {
            model_info.context_window = 32_768;
            model_info.max_output_tokens = Some(8_192);
            model_info.capabilities = vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
            ];
        }
        "cohere" => {
            model_info.context_window = 128_000;
            model_info.max_output_tokens = Some(4_096);
            model_info.capabilities = vec![
                ModelCapability::TextGeneration,
                ModelCapability::FunctionCalling,
                ModelCapability::SystemPrompt,
                ModelCapability::LongContext,
                ModelCapability::WebSearch,
            ];
        }
        _ => {
            // Conservative defaults for unknown providers
            if model_info.context_window == 0 {
                model_info.context_window = 8_192;
            }
            if model_info.max_output_tokens.is_none() {
                model_info.max_output_tokens = Some(model_info.context_window / 4);
            }
            if model_info.capabilities.is_empty() {
                model_info.capabilities = vec![
                    ModelCapability::TextGeneration,
                    ModelCapability::SystemPrompt,
                ];
            }
        }
    }
}

/// Get raw model defaults
pub fn get_model_defaults(model_id: &str) -> Option<ModelDefaults> {
    let defaults = MODEL_DEFAULTS.get_or_init(init_defaults);
    defaults.get(model_id).cloned()
}

/// Calculate appropriate max_tokens based on model info and user config
pub fn calculate_max_tokens(model_info: &ModelInfo, user_max_tokens: Option<u32>) -> u32 {
    // If user specified a value, use it (but cap at model's limit)
    if let Some(user_tokens) = user_max_tokens {
        let model_limit = model_info
            .max_output_tokens
            .unwrap_or(model_info.context_window / 4) as u32;
        return user_tokens.min(model_limit);
    }

    // Otherwise use model's max output tokens, or 1/4 of context window
    model_info
        .max_output_tokens
        .unwrap_or(model_info.context_window / 4) as u32
}

/// Default configuration for embedding models
#[derive(Debug, Clone)]
pub struct EmbeddingDefaults {
    /// Maximum input tokens
    pub max_input_tokens: usize,
    /// Output dimensions
    pub dimensions: usize,
    /// Cost per 1k tokens
    pub cost_per_1k_tokens: Option<f64>,
}

/// Static registry of embedding model defaults
static EMBEDDING_DEFAULTS: OnceLock<HashMap<&'static str, EmbeddingDefaults>> = OnceLock::new();

/// Initialize the embedding model defaults registry
fn init_embedding_defaults() -> HashMap<&'static str, EmbeddingDefaults> {
    let mut defaults = HashMap::new();

    // OpenAI embedding models
    defaults.insert(
        "text-embedding-3-small",
        EmbeddingDefaults {
            max_input_tokens: 8_191,
            dimensions: 1_536,
            cost_per_1k_tokens: Some(0.00002),
        },
    );

    defaults.insert(
        "text-embedding-3-large",
        EmbeddingDefaults {
            max_input_tokens: 8_191,
            dimensions: 3_072,
            cost_per_1k_tokens: Some(0.00013),
        },
    );

    defaults.insert(
        "text-embedding-ada-002",
        EmbeddingDefaults {
            max_input_tokens: 8_191,
            dimensions: 1_536,
            cost_per_1k_tokens: Some(0.0001),
        },
    );

    // Cohere embedding models
    defaults.insert(
        "embed-english-v3.0",
        EmbeddingDefaults {
            max_input_tokens: 512,
            dimensions: 1_024,
            cost_per_1k_tokens: Some(0.0001),
        },
    );

    defaults.insert(
        "embed-multilingual-v3.0",
        EmbeddingDefaults {
            max_input_tokens: 512,
            dimensions: 1_024,
            cost_per_1k_tokens: Some(0.0001),
        },
    );

    // Voyage embedding models
    defaults.insert(
        "voyage-large-2",
        EmbeddingDefaults {
            max_input_tokens: 16_000,
            dimensions: 1_536,
            cost_per_1k_tokens: Some(0.00012),
        },
    );

    defaults.insert(
        "voyage-code-2",
        EmbeddingDefaults {
            max_input_tokens: 16_000,
            dimensions: 1_536,
            cost_per_1k_tokens: Some(0.00012),
        },
    );

    // Google Gemini embedding models
    defaults.insert(
        "gemini-embedding-001",
        EmbeddingDefaults {
            max_input_tokens: 2_048,
            dimensions: 1_536, // Flexible 128-3072, but 1,536 is middle ground default
            cost_per_1k_tokens: Some(0.000025), // Estimated based on Gemini pricing tiers
        },
    );

    defaults.insert(
        "gemini-embedding-exp-03-07",
        EmbeddingDefaults {
            max_input_tokens: 8_000,
            dimensions: 3_072,
            cost_per_1k_tokens: Some(0.000025),
        },
    );

    defaults.insert(
        "text-embedding-004",
        EmbeddingDefaults {
            max_input_tokens: 3_000,
            dimensions: 768,
            cost_per_1k_tokens: Some(0.000025), // Legacy model
        },
    );

    defaults
}

/// Get embedding model defaults
pub fn get_embedding_defaults(model_id: &str) -> Option<EmbeddingDefaults> {
    let defaults = EMBEDDING_DEFAULTS.get_or_init(init_embedding_defaults);
    defaults.get(model_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enhance_anthropic_model() {
        let model_info = ModelInfo {
            id: "claude-3-opus-20240229".to_string(),
            name: "Claude 3 Opus".to_string(),
            provider: "Anthropic".to_string(),
            capabilities: vec![],
            context_window: 0, // Will be fixed
            max_output_tokens: None,
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        let enhanced = enhance_model_info(model_info);

        assert_eq!(enhanced.context_window, 200_000);
        assert_eq!(enhanced.max_output_tokens, Some(8_192));
        assert!(
            enhanced
                .capabilities
                .contains(&ModelCapability::FunctionCalling)
        );
        assert_eq!(enhanced.cost_per_1k_prompt_tokens, Some(0.015));
    }

    #[test]
    fn test_enhance_gemini_model() {
        let model_info = ModelInfo {
            id: "gemini-2.5-flash".to_string(),
            name: "Gemini 2.5 Flash".to_string(),
            provider: "Gemini".to_string(),
            capabilities: vec![],
            context_window: 0,
            max_output_tokens: None,
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        let enhanced = enhance_model_info(model_info);

        assert_eq!(enhanced.context_window, 1_048_576);
        assert_eq!(enhanced.max_output_tokens, Some(65_536));
        assert!(enhanced.capabilities.contains(&ModelCapability::JsonMode));
    }

    #[test]
    fn test_provider_fallback() {
        let model_info = ModelInfo {
            id: "unknown-anthropic-model".to_string(),
            name: "Unknown Model".to_string(),
            provider: "Anthropic".to_string(),
            capabilities: vec![],
            context_window: 0,
            max_output_tokens: None,
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        let enhanced = enhance_model_info(model_info);

        // Should get Anthropic defaults
        assert_eq!(enhanced.context_window, 200_000);
        assert_eq!(enhanced.max_output_tokens, Some(4_096));
    }

    #[test]
    fn test_calculate_max_tokens() {
        let model_info = ModelInfo {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            provider: "Test".to_string(),
            capabilities: vec![],
            context_window: 100_000,
            max_output_tokens: Some(10_000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };

        // User requests 5k, model supports 10k -> use 5k
        assert_eq!(calculate_max_tokens(&model_info, Some(5_000)), 5_000);

        // User requests 20k, model supports 10k -> cap at 10k
        assert_eq!(calculate_max_tokens(&model_info, Some(20_000)), 10_000);

        // No user preference -> use model's max
        assert_eq!(calculate_max_tokens(&model_info, None), 10_000);
    }
}
