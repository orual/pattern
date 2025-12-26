# Pattern Configuration Examples

This directory contains example configuration files for Pattern:

## Main Configuration

- `pattern.example.toml` - Complete example showing all configuration options including:
  - User settings
  - Single agent configuration with memory blocks
  - Model provider settings
  - Database configuration (embedded and remote)
  - Agent groups with various member configuration methods
  - Bluesky integration settings

## Agent Configurations

- `agents/task_manager.toml.example` - Example external agent configuration file showing:
  - Agent persona and instructions
  - Multiple memory blocks (inline and from files)
  - Different memory types (Core, Working, Archival)

## Getting Started

1. Copy `pattern.example.toml` to `pattern.toml` (or `~/.config/pattern/config.toml`)
2. Edit the configuration to match your setup
3. Set required environment variables for your chosen model provider:
   - `OPENAI_API_KEY` for OpenAI
   - `ANTHROPIC_API_KEY` for Anthropic
   - `GEMINI_API_KEY` for Google Gemini
   - `OPENROUTER_API_KEY` for OpenRouter
   - `GROQ_API_KEY` for Groq
   - `COHERE_API_KEY` for Cohere
   - etc.

## OpenRouter Setup

OpenRouter provides access to multiple AI providers through a single API. It's especially useful for:
- Accessing models from multiple providers without managing separate API keys
- Using models that may not be directly available to you
- Cost optimization by routing to the best model for your use case

### Configuration

1. Get your API key from [OpenRouter](https://openrouter.ai/keys)
2. Set the environment variable:
   ```bash
   export OPENROUTER_API_KEY=sk-or-v1-your-key-here
   ```
3. Configure in `pattern.toml`:
   ```toml
   [model]
   provider = "OpenRouter"
   model = "anthropic/claude-3-opus"  # Use provider/model format
   ```

### Model Naming Convention

OpenRouter uses `provider/model-name` format for model IDs:
- `anthropic/claude-3-opus` - Claude 3 Opus via OpenRouter
- `openai/gpt-4o` - GPT-4o via OpenRouter
- `google/gemini-pro` - Gemini Pro via OpenRouter
- `meta-llama/llama-3.1-70b-instruct` - Llama 3.1 70B via OpenRouter
- `mistralai/mistral-large` - Mistral Large via OpenRouter

See [OpenRouter Models](https://openrouter.ai/models) for the full list of available models.

## Group Member Configuration

Groups support three ways to configure members:

1. **Reference existing agent**: Use `agent_id` to reference an agent already in the database
2. **External config file**: Use `config_path` to load agent configuration from a separate file
3. **Inline configuration**: Define the agent configuration directly in the group member section

See `pattern.example.toml` for examples of all three methods.
