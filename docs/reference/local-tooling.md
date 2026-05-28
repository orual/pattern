# Local Tooling Reference: orual-plugins & rust-genai

Integration guide for rebuilding Pattern's plugin and authentication infrastructure. Documents architecture, contracts, and failure modes discovered in /home/orual/Projects/orual-plugins and /home/orual/Projects/PatternProject/rust-genai.

## orual-plugins Architecture

### Overview

`orual-plugins` is a Claude Code plugin system adapted from ed3d-plugins (CC BY-SA 4.0). Located at `/home/orual/Projects/orual-plugins`, it provides seven specialized plugin packages that coordinate Pattern's agents, planning, code review, and development workflows.

**Structure:** Each plugin resides in `plugins/{plugin-name}/` with:
- `.claude-plugin/plugin.json` — metadata manifest
- `agents/` — declarative agent definitions (YAML front matter)
- `skills/` — knowledge modules invoked via Skill tool
- `commands/` — CLI commands that delegate to skills
- `hooks/` — lifecycle handlers (SessionStart, PostToolUse)

### Plugin Contracts

#### 1. Plugin Manifest (`.claude-plugin/plugin.json`)

```json
{
  "name": "plugin-namespace",
  "description": "...",
  "version": "X.Y.Z",
  "author": { "name": "author" },
  "license": "CC-BY-SA-4.0"
}
```

The manifest is declarative; plugins are discovered by this file, not by code introspection.

**Key insight:** Plugin contracts are purely *discoverable* via these manifests. There is no "plugin interface" — each plugin defines its own surface (agents, skills, commands, hooks).

#### 2. Agents

Agents are declarative YAML with metadata headers:

```yaml
---
name: code-reviewer
description: Adversarial code reviewer. Actively hunts for problems...
model: opus
color: cyan
---
[Markdown prompt body]
```

**Location:** `plugins/{name}/agents/{agent-id}.md`

**Properties:**
- `name` — identifier for tool invocation (e.g., `code-reviewer`)
- `model` — Claude model: `haiku`, `sonnet`, `opus`
- `color` — UI hint (optional)
- Body is markdown prompt; no special encoding

**Invocation:** Agents are called via the `Agent` tool (pseudo-MCP interface). Pattern wraps this in `Task` for delegation.

#### 3. Skills

Skills are invoked via the `Skill` tool and are the primary way plugins extend Claude's behavior.

**Location:** `plugins/{name}/skills/{skill-id}/`

**Structure per skill:**
- `config.json` (optional) — metadata
- `intro.md` or primary content file — skill definition
- Supporting files (examples, templates)

**Skill definition (intro.md):**
```markdown
---
name: skill-identifier
description: Brief description
user-invocable: [true|false]
---
[Markdown content: workflow, rules, examples]
```

**Properties:**
- `name` — identifier used in Skill tool: `Skill(skill="plugin-name:skill-identifier")`
- `user-invocable` — whether exposed to user or called internally
- Body defines the skill's workflow, rules, and guidance

**Examples in orual-plugins:**
- `orual-plan-and-execute:using-plan-and-execute` — mandatory skill that runs at session start
- `orual-plan-and-execute:test-driven-development` — strict TDD workflow for implementation
- `orual-research-agents:investigating-a-codebase` — systematic codebase exploration
- `orual-extending-claude:writing-claude-md-files` — CLAUDE.md authoring guidance

#### 4. Commands

Commands are CLI entry points that delegate to skills. Located in `plugins/{name}/commands/{command-id}.md`.

**Format:**
```markdown
---
description: User-facing description
argument-hint: [arg1] [arg2]
---
[Implementation details, often delegating to a skill]
```

**Example:** `/execute-implementation-plan [plan-dir] [working-dir]` delegates to the `executing-an-implementation-plan` skill with validation.

#### 5. Hooks

Plugins can register lifecycle hooks. Located in `plugins/{name}/hooks/hooks.json`.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/session-start.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Agent",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/hooks/sanity-check-prepare.sh",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

**Hook types:**
- `SessionStart` — triggered when Claude Code session begins (startup, resume, clear, compact)
- `PostToolUse` — triggered after a tool completes; matcher specifies which tool (e.g., `Agent`)

**Example use in orual-plugins:**
- `orual-plan-and-execute/hooks/session-start.sh` — initializes planning state
- `orual-plan-and-execute/hooks/sanity-check-prepare.sh` — post-Agent sanity check (30s timeout)

### Key Differences from ed3d-plugins

1. **Model tiers:** Sonnet is the default implementation floor (not Haiku). Opus reserved for review and complex reasoning.
2. **Adversarial code review:** `code-reviewer` agent actively hunts for implementation failures, not just style issues. See `/home/orual/Projects/orual-plugins/plugins/orual-plan-and-execute/agents/code-reviewer.md` for full review checklist.
3. **Three execution modes:** Autonomous (full delegation), Collaborative (human-in-loop), Light (small tasks).
4. **VCS:** jj (jujutsu) workspaces instead of git worktrees.
5. **Sanity-check hook:** Post-Agent Haiku scan for self-deception patterns (runs every 30s after agent completion).
6. **NixOS-aware Playwright:** MCP wrapper using nix-patched browser binaries; no FHS.

### Claude Code Plugin Format Compatibility

**Status:** orual-plugins uses Claude Code's native plugin format (`.claude-plugin/plugin.json` + agents + skills).

**Compatibility notes:**
- Plugins are discovered via `.claude-plugin/plugin.json` in the plugin root
- Agents and skills are discovered by file naming convention (agents/, skills/)
- Commands are discovered from commands/ directory
- Hooks use Claude Code's hook system (SessionStart, PostToolUse, etc.)

**Can Pattern embed orual-plugins?** 

Only as a *reference implementation* for plugin structure. Pattern cannot directly embed Claude Code's plugin format because:

1. Pattern is a standalone agent system (not Claude Code-specific)
2. The contracts are tied to Claude Code's Skill/Agent/Task tools
3. Plugins must run within Claude Code's environment (hooks, tool registry)

**Recommendation for Pattern:**
- Adopt orual-plugins' *conceptual* structure (agents, skills, commands, hooks)
- Implement Pattern's own plugin format (TOML or JSON declarative, Pattern-specific tool invocation)
- Use orual-plugins as a pattern library, not a runtime dependency

### Core Plugins Reference

**orual-basic-agents** — Generic subagents (Sonnet default). Other plugins depend on this.

**orual-plan-and-execute** — Planning and execution workflows with TDD, code review, and jj integration. Contains:
- Skills: `start-design-plan`, `starting-an-implementation-plan`, `executing-an-implementation-plan`, `test-driven-development`, `verification-before-completion`, `requesting-code-review`
- Agents: `code-reviewer` (adversarial), `planner`, `implementer`
- Commands: `/start-design-plan`, `/execute-implementation-plan`, `/flesh-it-out`

**orual-research-agents** — Codebase investigation and internet research. Used to answer "does X exist?" and "how does X work?" questions.

**orual-extending-claude** — Knowledge for creating plugins, skills, agents, hooks, MCP servers.

**orual-coding-style** — Language-specific guidance (Rust, NixOS, testing patterns).

**orual-playwright** — Browser automation via MCP wrapper. NixOS-native (no FHS).

---

## rust-genai Fork: Anthropic OAuth Implementation

### Overview

`rust-genai` is a Rust client library for LLM APIs. Orual maintains a fork at `/home/orual/Projects/PatternProject/rust-genai` with OAuth 2025-04-20 support for Anthropic's subscription endpoint.

**Upstream:** https://github.com/jeremychone/rust-genai  
**Fork:** https://github.com/orual/rust-genai  
**Pattern dependency:** `/home/orual/Projects/PatternProject/pattern/Cargo.toml` uses `git = "https://github.com/orual/rust-genai"` (not upstream)

### Fork Changes Relative to Upstream

**Commit chain (HEAD..upstream/main):**

1. **`18225ac` — Anthropic OAuth workaround** (primary)
   - Detects OAuth by `Bearer ` prefix in api_key
   - Switches auth headers: `Authorization: Bearer {token}` vs `x-api-key: {key}`
   - Adds `anthropic-beta: oauth-2025-04-20` header for OAuth
   - Forces system prompt array format for OAuth (non-OAuth can use string)
   - Injects "You are Claude Code" identification (OAuth requirement)

2. **`7cc71e3` — Anthropic extended thinking via reasoning budget parameter**
   - Adds thinking support: `thinking: { type: "enabled", budget_tokens: N }`
   - Enforces constraints: budget >= 1024, budget < max_tokens - 100
   - Prevents `temperature` when thinking is enabled
   - Restricts `top_p` to [0.95, 1.0] when thinking is enabled

3. **`9e5c1d7` — Bug fix for extended thinking API limitations**
   - Handles edge case where thinking + temperature causes API errors
   - Handles top_p restrictions

4. **`e0e16db` — Proper support for anthropic-style thinking**
   - Preserves thinking blocks in response content (not just text concatenation)
   - Introduces `ContentBlock::Thinking { text, signature }` and `ContentBlock::RedactedThinking`
   - Handles mixed content (thinking + text + tool calls)

5. **`db3dd51` — Made Gemini failures a bit less fatal**
   - Unrelated to OAuth; Gemini error handling

### OAuth Authentication Flow

#### Token Detection (`adapter_impl.rs:86-90`)

```rust
let api_key = get_api_key(auth, &model)?;
// ...
let is_oauth = api_key.starts_with("Bearer ");
```

**How it works:**
- `get_api_key()` retrieves credential from `AuthData` (passed by Pattern)
- Simple string prefix check: if it starts with "Bearer ", assume OAuth
- Pattern is responsible for providing OAuth tokens in this format

**Failure mode:** If token doesn't have "Bearer " prefix, treated as API key. Will fail with 401 auth error when Anthropic validates `x-api-key` header against Bearer token.

#### Headers (`adapter_impl.rs:92-104`)

```rust
let headers = if is_oauth {
    Headers::from(vec![
        ("Authorization".to_string(), api_key),  // Full: "Bearer {token}"
        ("anthropic-version".to_string(), ANTHROPIC_VERSION.to_string()),
        ("anthropic-beta".to_string(), "oauth-2025-04-20".to_string()),
    ])
} else {
    Headers::from(vec![
        ("x-api-key".to_string(), api_key),
        ("anthropic-version".to_string(), ANTHROPIC_VERSION.to_string()),
    ])
};
```

**OAuth differences:**
- `Authorization: Bearer {token}` instead of `x-api-key`
- Requires `anthropic-beta: oauth-2025-04-20` header (current version as of 2025-04-20)
- Standard Anthropic version header still included

#### System Prompt Formatting (`adapter_impl.rs:620-650`)

**OAuth requires array format; non-OAuth can use string.**

```rust
if is_oauth {
    // Array format with mandatory Claude Code identification
    let mut parts = vec![
        json!({"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."})
    ];
    
    for (idx, (content, is_cache_control)) in systems.iter().enumerate() {
        let text = if idx == 0 {
            format!("You are NOT Claude Code. {}", content)
        } else {
            content.clone()
        };
        
        let mut part = json!({"type": "text", "text": text});
        if *is_cache_control {
            part["cache_control"] = json!({"type": "ephemeral"});
        }
        parts.push(part);
    }
    Some(json!(parts))
} else {
    // Non-OAuth: string format or array if cache control needed
    // [existing logic]
}
```

**Key constraint:** OAuth system prompts must be arrays. The fork injects a required "You are Claude Code" block, then immediately clarifies the user's prompt overrides this identity.

**Fragility:** The identity injection-then-override is a workaround. If the user's first system prompt already contains "You are NOT Claude Code", it will be duplicated.

#### Request Payload (`adapter_impl.rs:166-186`)

Thinking configuration is added *before* other options because thinking constraints affect other parameters:

```rust
if thinking_enabled {
    let budget_tokens = match options_set.reasoning_effort() {
        Some(ReasoningEffort::Low) => 4096,
        Some(ReasoningEffort::Medium) => 16384,
        Some(ReasoningEffort::High) => 32768,
        Some(ReasoningEffort::Budget(b)) => *b as u32,
        None => 16384,
    };
    
    let budget_tokens = budget_tokens.max(1024).min(max_tokens.saturating_sub(100));
    
    let thinking = json!({
        "type": "enabled",
        "budget_tokens": budget_tokens
    });
    payload.x_insert("thinking", thinking)?;
}

// Temperature cannot be set when thinking is enabled
if !thinking_enabled {
    if let Some(temperature) = options_set.temperature() {
        payload.x_insert("temperature", temperature)?;
    }
}

// top_p must be in [0.95, 1.0] when thinking is enabled
if let Some(top_p) = options_set.top_p() {
    if thinking_enabled {
        if top_p >= 0.95 && top_p <= 1.0 {
            payload.x_insert("top_p", top_p)?;
        }
        // Otherwise skip setting top_p
    } else {
        payload.x_insert("top_p", top_p)?;
    }
}
```

**Constraints enforced:**
- `budget_tokens` minimum: 1024 (Anthropic hard limit)
- `budget_tokens` maximum: `max_tokens - 100` (safety margin)
- Temperature: incompatible with thinking (silently skipped)
- top_p: when thinking enabled, must be >= 0.95; otherwise skipped (doesn't error)

### OAuth Failure Modes

#### 1. Missing Bearer Prefix

**Symptom:** 401 Unauthorized with "invalid x-api-key" error

**Root cause:** Pattern passes token without "Bearer " prefix. Fork tries to use it as an API key:
```
Authorization: x-api-key: {actual_token}  (wrong header)
```

**Fix:** Pattern must format token as `Bearer {token}` before passing to rust-genai.

#### 2. Expired Access Token

**Current:** No refresh logic in rust-genai. No `expires_in` tracking.

**Symptom:** 401 Unauthorized after token expiration (10-12 hours typically)

**Root cause:** OAuth access tokens expire. Anthropic expects clients to call `/v1/oauth/token` with refresh token to get new access token.

**Comparison with claude-code:** 

In `/home/orual/Git_Repos/claude-code/services/oauth/client.ts:145-180`, token refresh is explicitly handled:

```typescript
export async function refreshOAuthToken(
  refreshToken: string,
  { scopes: requestedScopes }: { scopes?: string[] } = {},
): Promise<OAuthTokens> {
  const requestBody = {
    grant_type: 'refresh_token',
    refresh_token: refreshToken,
    client_id: getOauthConfig().CLIENT_ID,
    scope: (...).join(' '),
  }
  
  const response = await axios.post(getOauthConfig().TOKEN_URL, requestBody, {
    headers: { 'Content-Type': 'application/json' },
    timeout: 15000,
  })
  
  // ...returns new access_token and refresh_token
}
```

**Pattern must implement this at the application layer** (not in rust-genai):
1. Store `refresh_token` securely (separate from `access_token`)
2. Before calling rust-genai, check if `access_token` is within 5 minutes of expiry
3. If expired, call OAuth refresh endpoint to get new token
4. Pass new token to rust-genai

#### 3. Missing `anthropic-beta` Header

**Symptom:** Older Anthropic API versions reject Bearer token as if using API key endpoint

**Root cause:** OAuth is a beta feature. Header `anthropic-beta: oauth-2025-04-20` is required. Fork includes this only if `is_oauth` is true.

**Current version:** `oauth-2025-04-20` (hardcoded in `/home/orual/Projects/PatternProject/rust-genai/src/adapter/adapters/anthropic/adapter_impl.rs:97`)

**Risk:** If Anthropic updates the beta header, fork will break. Currently no mechanism to detect or update the version.

#### 4. System Prompt Array Format Mismatch

**Symptom:** 400 Bad Request: "system must be either a string or an array of content blocks"

**Root cause:** Non-OAuth code path returns string; OAuth requires array. If OAuth detection fails (missing Bearer prefix), system is formatted as string.

**Fix:** Ensure Bearer prefix is present in token.

#### 5. Thinking Budget Validation

**Symptom:** 400 Bad Request: "thinking budget_tokens must be >= 1024"

**Root cause:** User requests thinking with budget < 1024, or calculated budget becomes too small.

**Current handling:** Fork enforces `budget_tokens.max(1024)` silently, even if user requested lower. No warning.

**Risk:** Users may request Low effort (4096 tokens) but get the minimum (1024) without knowing.

#### 6. Temperature + Thinking Conflict

**Symptom:** 400 Bad Request: "temperature cannot be set when thinking is enabled"

**Current handling:** Fork silently skips temperature if thinking is enabled (lines 184-189).

**Risk:** User requests both; temperature is silently dropped. No warning logged.

#### 7. top_p Constraint Violation

**Symptom:** 400 Bad Request: "top_p must be in [0.95, 1.0] when thinking is enabled"

**Current handling:** Fork silently skips top_p if outside range (lines 196-205). No warning.

**Risk:** Same as temperature — user doesn't know top_p was ignored.

#### 8. Authentication Header Consistency

**Inconsistency discovered:** claude-code uses `Authorization: Bearer {token}` format across all OAuth contexts (files API, sessions, team memory sync). rust-genai adapter just passes token as-is.

**In claude-code (`services/api/client.ts:326`):**
```typescript
headers['Authorization'] = `Bearer ${token}`
```

**In rust-genai (adapter_impl.rs:95):**
```rust
("Authorization".to_string(), api_key)  // assumes api_key is "Bearer {token}"
```

**Risk:** If Pattern's credential storage already prepends "Bearer ", then rust-genai would be creating `Authorization: Bearer Bearer {token}` (double prefix). Verify Pattern's auth layer.

### No Refresh Token Handling

**Critical gap:** rust-genai does NOT implement token refresh. It assumes the token passed is always valid.

**What's missing:**
- No tracking of `expires_at` or `expires_in`
- No automatic refresh on 401 (token expired)
- No refresh token storage
- No token rotation

**Consequences:**
1. Long-running agents (> 10 hours) will fail after token expiration
2. Multi-agent sessions across day boundaries will fail mid-session
3. Pattern must implement refresh externally and notify rust-genai of new token

### Pattern's genai Integration

**File:** `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/model.rs`

**Usage:**
```rust
use genai::{adapter::AdapterKind, chat::ChatOptions};

pub struct ResponseOptions {
    pub response_format: Option<genai::chat::ChatResponseFormat>,
    pub reasoning_effort: Option<genai::chat::ReasoningEffort>,
    // ...
}

pub fn to_chat_options(self) -> (ModelInfo, ChatOptions) {
    // converts Pattern ResponseOptions to genai ChatOptions
}
```

**How tokens are passed:**
1. Pattern retrieves OAuth token (from secure storage)
2. Formats as `Bearer {access_token}` 
3. Passes to genai via `AuthData` (enum with API key variants)
4. rust-genai detects OAuth prefix and switches headers

**Where to add refresh logic:**
1. Pattern's authentication module (`pattern_auth` crate)
2. Check token expiry before each agent invocation
3. Call Anthropic refresh endpoint if needed
4. Update stored token and pass fresh token to genai

---

## Integration Recommendations

### For Pattern Plugin System

1. **Adopt orual-plugins' structure** but implement Pattern-native format:
   - Agents: TOML or JSON declarative (not Claude Code YAML)
   - Skills: Pattern task definitions (not Claude Code Skill tool)
   - Commands: Pattern CLI commands (not Claude Code commands)
   - Hooks: Pattern agent lifecycle hooks

2. **Reference implementation:** `/home/orual/Projects/orual-plugins/plugins/orual-plan-and-execute/` shows how to structure complex, multi-phase workflows. Pattern agents (memory supervisors, task planners, etc.) should follow this pattern.

3. **Copy patterns, not code:** Don't embed orual-plugins as a dependency. Use it as a reference for:
   - Skill checklists (markdown structures for workflow documentation)
   - Agent prompts (code-reviewer's checklist is reusable for Pattern)
   - Hook lifecycle (session init, post-execution verification)

### For OAuth and Authentication

1. **Separate concerns:**
   - rust-genai handles *request formatting* only (headers, payload)
   - Pattern handles *credential lifecycle* (obtain, refresh, store, rotate)

2. **Implement token refresh in `pattern_auth`:**
   ```rust
   async fn ensure_valid_oauth_token(
       account: &OAuthAccount,
       client: &HttpClient,
   ) -> Result<String> {
       if is_expired(&account.access_token, &account.expires_at) {
           let new_tokens = refresh_oauth_token(
               &account.refresh_token,
               &oauth_config,
               client,
           ).await?;
           save_tokens(&new_tokens)?;
           return Ok(format!("Bearer {}", new_tokens.access_token));
       }
       Ok(format!("Bearer {}", account.access_token))
   }
   ```

3. **Pre-check before agent invocation:**
   - In agent loop, call `ensure_valid_oauth_token()` before creating genai client
   - Log warnings if token is near expiry (< 5 minutes)
   - Fail fast with clear error if refresh fails

4. **Monitor for API version changes:**
   - Anthropic's `anthropic-beta` header is version-pinned (`oauth-2025-04-20`)
   - Set up a lint or CI check to alert if API docs show newer version
   - Consider adding configurable beta header version to rust-genai

5. **Add diagnostics:**
   - Log full request headers (redacting token) in debug mode
   - Log token expiry times and refresh events
   - Detect and report header mismatches (double Bearer prefix, etc.)

### Testing Recommendations

1. **Test OAuth token detection:**
   - Test with `Bearer {token}` (should use OAuth headers)
   - Test with API key format (should use x-api-key header)
   - Test with invalid Bearer (no space) — should fail clearly

2. **Test thinking configuration constraints:**
   - Test thinking + temperature (should silently skip temperature)
   - Test thinking + top_p in/out of range (should silently skip out-of-range)
   - Test budget_tokens bounds (should clamp to [1024, max_tokens-100])

3. **Test system prompt formatting:**
   - OAuth with system prompt (should be array)
   - OAuth without system prompt (should be None)
   - Non-OAuth with cache control (should be array)
   - Non-OAuth without cache control (should be string)

4. **Test token refresh in agent loop:**
   - Mock token expiry; verify refresh is called
   - Mock refresh failure; verify clear error to user
   - Long-running conversation across token boundary

---

## References

- orual-plugins: `/home/orual/Projects/orual-plugins/`
- rust-genai fork: `/home/orual/Projects/PatternProject/rust-genai/`
- Pattern genai integration: `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/model.rs`
- claude-code OAuth implementation: `/home/orual/Git_Repos/claude-code/services/oauth/`
- Anthropic API docs: https://docs.anthropic.com (OAuth 2025-04-20 beta)
