# Multi-Provider OAuth Support Design

**Date:** 2025-12-28
**Status:** Draft
**Scope:** Add OAuth support for OpenAI Codex and Gemini (Cloud Code Companion)

## Overview

Extend Pattern's OAuth infrastructure to support three providers:
- **Anthropic** (existing) - Standard OAuth with header/system prompt transformation
- **OpenAI Codex** (new) - OAuth via ChatGPT Plus/Pro backend
- **Gemini Cloud Code** (new) - OAuth via Google's Cloud Code Companion API

## Key Finding: These Are Different APIs

Both new providers use OAuth to access **internal APIs**, not the standard public APIs:

| Provider | Standard API | OAuth API |
|----------|-------------|-----------|
| OpenAI | `api.openai.com` | `chatgpt.com/backend-api/codex/responses` |
| Gemini | `generativelanguage.googleapis.com` | `cloudcode-pa.googleapis.com/v1internal:*` |

This means we need new adapters in rust-genai, not just middleware.

---

## Architecture

### rust-genai Fork

New adapters handle the API differences:

```
rust-genai/src/adapter/adapters/
├── anthropic/          # Existing
├── openai/             # Existing (API key)
├── openai_codex/       # NEW - OAuth via ChatGPT backend
├── gemini/             # Existing (API key)
└── cloud_code/         # NEW - Gemini OAuth via Cloud Code Companion
```

```rust
pub enum AdapterKind {
    Anthropic,
    OpenAI,
    OpenAICodex,    // NEW
    Gemini,
    CloudCode,      // NEW
    // ...
}
```

### pattern_core

OAuth flow and auth resolver:

```
pattern_core/src/oauth/
├── mod.rs              # OAuthProvider enum, OAuthClient
├── auth_flow.rs        # PKCE, token exchange
├── resolver.rs         # Auth resolver for genai
├── integration.rs      # OAuthModelProvider
└── loopback.rs         # NEW - Local callback server
```

---

## Phase 1: OpenAI Codex

### OAuth Configuration

```rust
impl OAuthConfig {
    pub fn openai() -> Self {
        Self {
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
            redirect_uri: "http://localhost:1455/auth/callback".to_string(),
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
                "offline_access".to_string(),
            ],
        }
    }
}
```

### Extra Authorization Parameters

```rust
// Added to auth URL
id_token_add_organizations = true
codex_cli_simplified_flow = true
originator = codex_cli_rs
```

### OpenAICodex Adapter (rust-genai)

**Endpoint:** `https://chatgpt.com/backend-api/codex/responses`

**Required Headers:**
```
Authorization: Bearer <access_token>
chatgpt-account-id: <extracted_from_jwt>
OpenAI-Beta: responses=experimental
originator: codex_cli_rs
accept: text/event-stream
```

**JWT Account ID Extraction:**
```rust
fn extract_account_id(access_token: &str) -> Result<String> {
    // Decode JWT, extract claim at path:
    // `https://api.openai.com/auth` -> `chatgpt_account_id`
}
```

**Request Body Transformations:**
```rust
body.store = Some(false);           // Stateless mode
body.stream = Some(true);           // Always stream
body.instructions = Some(prompts);  // System prompt injection
body.include = Some(vec!["reasoning.encrypted_content".to_string()]);

// Remove unsupported fields
body.max_output_tokens = None;
body.max_completion_tokens = None;
```

**Model Normalization:**
```rust
const MODEL_MAP: &[(&str, &str)] = &[
    ("gpt-5.2-codex", "gpt-5.2-codex"),
    ("gpt-5.1-codex", "gpt-5.1-codex"),
    ("codex", "gpt-5.1-codex"),
    ("gpt-5", "gpt-5.1"),
    // Strip suffixes like -low, -medium, -high (reasoning effort)
];
```

**Reasoning Configuration:**
```rust
struct ReasoningConfig {
    effort: String,  // none/minimal/low/medium/high/xhigh
    summary: String, // auto/concise/detailed/off/on
}

// Model-specific constraints
// - gpt-5.2-codex: supports xhigh, no "none"
// - codex-mini: minimum "medium"
// - gpt-5.1: no xhigh, supports "none"
```

**Response Handling:**

SSE stream to JSON conversion for non-streaming:
```rust
fn parse_sse_to_json(stream: &str) -> Option<Value> {
    for line in stream.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                if parsed["type"] == "response.done"
                    || parsed["type"] == "response.completed" {
                    return Some(parsed["response"].clone());
                }
            }
        }
    }
    None
}
```

**System Prompt Injection:**

Codex requires specific system prompts fetched from GitHub:
- Model-family specific prompts (gpt-5.2-codex, gpt-5.1-codex, etc.)
- `TOOL_REMAP_MESSAGE` - Maps Codex tools to Pattern tools
- `CODEX_OPENCODE_BRIDGE` - Environment bridging (~550 tokens)

```rust
// Tool remapping critical content
const TOOL_REMAP: &str = r#"
❌ APPLY_PATCH DOES NOT EXIST → ✅ USE "edit" INSTEAD
❌ UPDATE_PLAN DOES NOT EXIST → ✅ USE "todowrite" INSTEAD
"#;
```

### Phase 1 Tasks

**rust-genai:**
1. Fix `exec_chat()` to handle `AuthData::RequestOverride` (bug)
2. Add `AdapterKind::OpenAICodex`
3. Implement `openai_codex` adapter module
   - URL construction
   - Header building with JWT parsing
   - Request body transformation
   - Model normalization
   - SSE response parsing

**pattern_core:**
1. Add `OAuthProvider::OpenAI`
2. Add `OAuthConfig::openai()`
3. Add extra auth params support to `DeviceAuthFlow`
4. Implement loopback callback server
5. Extend auth resolver for `AdapterKind::OpenAICodex`
6. Add Codex system prompt fetching and caching

**pattern_cli:**
1. Accept `openai` in auth commands

---

## Phase 2: Gemini Cloud Code

### OAuth Configuration

```rust
impl OAuthConfig {
    pub fn gemini() -> Self {
        Self {
            client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com".to_string(),
            client_secret: Some("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".to_string()),
            auth_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_endpoint: "https://oauth2.googleapis.com/token".to_string(),
            redirect_uri: "http://localhost:8085/oauth2callback".to_string(),
            scopes: vec![
                "https://www.googleapis.com/auth/cloud-platform".to_string(),
                "https://www.googleapis.com/auth/userinfo.email".to_string(),
                "https://www.googleapis.com/auth/userinfo.profile".to_string(),
            ],
        }
    }
}
```

### Extra Authorization Parameters

```rust
access_type = offline    // Get refresh token
prompt = consent         // Force consent screen
```

### State Encoding (Different from Anthropic/OpenAI)

PKCE verifier embedded in OAuth state as base64url JSON:
```rust
fn encode_state(verifier: &str) -> String {
    let payload = json!({ "verifier": verifier });
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(payload.to_string().as_bytes())
}
```

### CloudCode Adapter (rust-genai)

**Endpoint:** `https://cloudcode-pa.googleapis.com`

**URL Transformation:**
```rust
// Standard Gemini:
// /v1beta/models/{model}:generateContent

// Cloud Code:
// /v1internal:generateContent
// /v1internal:streamGenerateContent?alt=sse
```

**Required Headers:**
```
Authorization: Bearer <access_token>
User-Agent: google-api-nodejs-client/9.15.1
X-Goog-Api-Client: gl-node/22.17.0
Client-Metadata: ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI
Content-Type: application/json
Accept: text/event-stream  // For streaming
```

**Request Body Wrapping:**
```rust
// Input
{
    "contents": [...],
    "system_instruction": {...},
    "generationConfig": {...}
}

// Transformed (wrapped)
{
    "project": "<project_id>",
    "model": "<model_name>",
    "request": {
        "contents": [...],
        "systemInstruction": {...},    // camelCase
        "generationConfig": {...}
    }
}
```

**Field Renaming:**
```rust
const FIELD_RENAMES: &[(&str, &str)] = &[
    ("system_instruction", "systemInstruction"),
    ("cached_content", "cachedContent"),
    ("thinking_budget", "thinkingBudget"),
    ("thinking_level", "thinkingLevel"),
    ("include_thoughts", "includeThoughts"),
];
```

**Response Unwrapping:**
```rust
// API returns
{ "response": { "candidates": [...], "usageMetadata": {...} } }

// Unwrap to
{ "candidates": [...], "usageMetadata": {...} }
```

**Streaming Response:**
Transform each SSE line to unwrap the `response` field.

### Project Management

First-time setup requires project onboarding:

```rust
// 1. Check existing project
POST /v1internal:loadCodeAssist
{
    "metadata": {
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    }
}

// 2. If no project, onboard to free tier
POST /v1internal:onboardUser
{
    "tierId": "FREE",
    "metadata": {...}
}

// Response contains cloudaicompanionProject ID to store
```

**Project ID Storage:**

Store alongside refresh token using pipe delimiter:
```
<refresh_token>|<project_id>
```

Or add dedicated field to `ProviderOAuthToken`:
```rust
pub struct ProviderOAuthToken {
    // ... existing fields
    pub project_id: Option<String>,  // For Gemini
}
```

### Phase 2 Tasks

**rust-genai:**
1. Add `AdapterKind::CloudCode`
2. Implement `cloud_code` adapter module
   - URL construction (`/v1internal:*`)
   - Header building
   - Request body wrapping
   - Field renaming (snake_case → camelCase)
   - Response unwrapping
   - Streaming SSE transformation

**pattern_core:**
1. Add `OAuthProvider::Gemini`
2. Add `OAuthConfig::gemini()` with client secret support
3. Implement state encoding with embedded verifier
4. Extend token exchange for client secret
5. Add project management flow
   - `loadCodeAssist` call
   - `onboardUser` call
   - Project ID storage
6. Extend auth resolver for `AdapterKind::CloudCode`

**pattern_auth:**
1. Potentially add `project_id` field to `ProviderOAuthToken`

**pattern_cli:**
1. Accept `gemini` in auth commands
2. Handle project onboarding UX (first login)

---

## Phase 3: Polish

1. **Loopback server refinements**
   - Headless detection (SSH)
   - Manual paste fallback
   - Configurable timeout
   - Success HTML page

2. **Error handling**
   - Rate limit header parsing (Codex)
   - Retry-After extraction (Gemini)
   - Token revocation handling
   - Gemini 3 preview access errors

3. **Token refresh edge cases**
   - 60-second expiry buffer
   - Refresh token rotation (Gemini may return new refresh token)
   - Invalid grant handling (re-auth required)

4. **Testing**
   - Mock OAuth flows
   - Adapter unit tests
   - Integration tests with real tokens (manual)

---

## Loopback Server Design

Shared infrastructure for OAuth callbacks:

```rust
pub struct LoopbackConfig {
    pub host: String,           // "127.0.0.1"
    pub port: u16,              // Provider-specific
    pub path: String,           // "/auth/callback" or "/oauth2callback"
    pub timeout: Duration,      // 5 minutes
    pub open_browser: bool,     // Auto-open auth URL
}

impl LoopbackConfig {
    pub fn openai() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 1455,
            path: "/auth/callback".to_string(),
            timeout: Duration::from_secs(300),
            open_browser: true,
        }
    }

    pub fn gemini() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8085,
            path: "/oauth2callback".to_string(),
            timeout: Duration::from_secs(300),
            open_browser: true,
        }
    }
}
```

**Implementation pattern from jacquard:**
- Use `rouille` for simple one-shot HTTP server
- Channel to receive callback params
- Stoppable server that shuts down after callback
- Timeout handling with `tokio::time::timeout`

---

## Dependencies

**rust-genai (new):**
- `jsonwebtoken` - JWT parsing for Codex account ID
- `base64` - State encoding for Gemini

**pattern_core (new):**
- `rouille` - Loopback server (or reuse existing `axum`)
- `webbrowser` - Optional browser auto-open

---

## Cleanup: Centralized Auth Configuration

Currently, API key availability is checked in multiple places:

**model.rs** (`GenAiClient::new()`):
```rust
if std::env::var("ANTHROPIC_API_KEY").is_ok() {
    available_endpoints.push(AdapterKind::Anthropic);
}
// Repeated for each provider
```

**resolver.rs** (`create_oauth_auth_resolver()`):
```rust
if std::env::var("ANTHROPIC_API_KEY").is_ok() {
    return Ok(None);  // Fall back to API key
}
```

### Problem

- Duplicated logic
- Easy to get out of sync
- Doesn't account for OAuth tokens in endpoint discovery
- Adding new providers requires changes in multiple places

### Proposed Solution

Centralize auth configuration into a single source of truth:

```rust
/// Tracks available authentication methods for each provider
pub struct AuthConfig {
    providers: HashMap<AdapterKind, AuthMethod>,
}

pub enum AuthMethod {
    ApiKey,           // Environment variable available
    OAuth,            // OAuth token stored
    Both,             // Both available (prefer OAuth)
    None,             // Not configured
}

impl AuthConfig {
    /// Build config by checking env vars and OAuth token storage
    pub async fn discover(auth_db: &AuthDb) -> Self {
        let mut providers = HashMap::new();

        for kind in [AdapterKind::Anthropic, AdapterKind::OpenAI, AdapterKind::Gemini, ...] {
            let has_api_key = Self::check_api_key(&kind);
            let has_oauth = auth_db.get_provider_oauth_token(kind.as_str()).await.ok().flatten().is_some();

            let method = match (has_api_key, has_oauth) {
                (true, true) => AuthMethod::Both,
                (true, false) => AuthMethod::ApiKey,
                (false, true) => AuthMethod::OAuth,
                (false, false) => AuthMethod::None,
            };

            providers.insert(kind, method);
        }

        Self { providers }
    }

    fn check_api_key(kind: &AdapterKind) -> bool {
        let var_name = match kind {
            AdapterKind::Anthropic => "ANTHROPIC_API_KEY",
            AdapterKind::OpenAI => "OPENAI_API_KEY",
            AdapterKind::Gemini => "GEMINI_API_KEY",
            // ... etc
        };
        std::env::var(var_name).is_ok()
    }

    /// Get available endpoints (any auth method configured)
    pub fn available_endpoints(&self) -> Vec<AdapterKind> {
        self.providers.iter()
            .filter(|(_, method)| !matches!(method, AuthMethod::None))
            .map(|(kind, _)| *kind)
            .collect()
    }

    /// Check if OAuth should be used for a provider
    pub fn should_use_oauth(&self, kind: &AdapterKind) -> bool {
        matches!(
            self.providers.get(kind),
            Some(AuthMethod::OAuth) | Some(AuthMethod::Both)
        )
    }
}
```

### Usage

**model.rs:**
```rust
impl GenAiClient {
    pub async fn new(auth_config: &AuthConfig) -> Result<Self> {
        let client = genai::Client::default();
        let available_endpoints = auth_config.available_endpoints();
        Ok(Self { client, available_endpoints })
    }
}
```

**resolver.rs:**
```rust
pub fn create_oauth_auth_resolver(auth_db: AuthDb, auth_config: AuthConfig) -> AuthResolver {
    // Use auth_config to determine whether to try OAuth or fall back to API key
}
```

### When to Implement

This cleanup can be done:
1. **Before** multi-provider OAuth (cleaner foundation)
2. **During** Phase 1 (OpenAI Codex) - refactor as we add
3. **After** Phase 2 (Gemini) - once all providers are in

Recommendation: Do it during Phase 1 to avoid accumulating more tech debt.

---

## Open Questions

1. **System prompt fetching for Codex** - Cache locally or fetch on each request?
   - Recommendation: Cache with ETag-based conditional requests, 15-minute TTL

2. **Gemini project ID storage** - Separate field or embed in refresh token?
   - Recommendation: Separate field in `ProviderOAuthToken` for clarity

3. **Adapter naming** - `CloudCode` vs `GeminiCloudCode` vs `GeminiOAuth`?
   - Recommendation: `CloudCode` - it's a distinct API, not just "Gemini with OAuth"

4. **Loopback server** - `rouille` (lightweight) vs `axum` (already a dep)?
   - Either works; `axum` avoids new dependency

---

## References

- [opencode-openai-codex-auth](https://github.com/numman-ali/opencode-openai-codex-auth) - TypeScript Codex implementation
- [opencode-gemini-auth](https://github.com/jenslys/opencode-gemini-auth) - TypeScript Gemini implementation
- [jacquard-oauth loopback.rs](file:///home/orual/Projects/jacquard/crates/jacquard-oauth/src/loopback.rs) - Rust loopback server pattern
- [pattern_core oauth](file:///home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/oauth/) - Existing Anthropic OAuth
