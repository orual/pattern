# Claude Code Ecosystem: Architecture Reference for Pattern

This document analyzes the claude-code ecosystem (upstream claude-code, rommie-code fork, claude-code-modes prompting, popup-mcp UI toolkit) to identify patterns, OAuth flows, and architectural decisions relevant to Pattern's design as a persistent multi-agent system.

**Key finding:** Pattern should adopt claude-code's OAuth flow and CLI architecture, but rebuild in Rust with simplified prompting (no mode system needed initially) and persistent agent personas stored in SQLite/CRDT.

---

## 1. Claude Code (Upstream)

**Repository:** `/home/orual/Git_Repos/claude-code`  
**License:** Proprietary (Anthropic, internal exposure 2026-03-31)  
**Language:** TypeScript (Bun runtime)  
**Architecture:** React/Ink terminal UI + CLI with feature-gated modules  

### Structure

```
src/
├── entrypoints/        # CLI entry (cli.tsx, init.ts, mcp.ts)
├── commands/           # 90+ slash commands (login, mcp, memory, commit, etc.)
├── tools/              # 47 tools (Bash, File I/O, Web, MCP, Agents, etc.)
├── services/           # OAuth, MCP, analytics, API client, memory
├── components/         # ~160 Ink UI components
├── bridge/             # IDE/remote-control bridge (replBridge.ts, bridgeMain.ts)
├── coordinator/        # Multi-agent orchestration
├── hooks/              # Permission system, state management
├── ink/                # Custom Ink renderer
├── context.ts          # System prompt construction
├── QueryEngine.ts      # Core agentic loop (~46K lines)
├── query.ts            # Query pipeline (~70K lines)
├── commands.ts         # Command registry with feature gates
├── tools.ts            # Tool registry with feature gates
└── constants/          # OAuth config, prompt constants
```

### Core Loop: QueryEngine

Each interaction:
1. **Build system prompt** — environment-specific, contextual
2. **Normalize messages** — strip signatures, format for API
3. **Stream call to Claude API** (Anthropic SDK)
4. **Execute tool calls** — with permission checks (3-level hierarchy: auto-allow rules → deny rules → interactive)
5. **Check token budget** — compact history if needed
6. **Return to REPL** — render response to terminal

**Key files:**
- `/home/orual/Git_Repos/claude-code/QueryEngine.ts` — main loop orchestration
- `/home/orual/Git_Repos/claude-code/query.ts` — query construction and streaming
- `/home/orual/Git_Repos/claude-code/context.ts` — system prompt assembly

### Tool System

Every tool (`/home/orual/Git_Repos/claude-code/tools/`) is self-contained:
- **Input schema** (Zod validation)
- **Permission model** (read-only, needs approval, etc.)
- **Execution logic** with error handling
- **Metadata** (name, description, tags)

**Distinctive tool types:**
- **Bash/PowerShell** — shell execution with TTY capture
- **File I/O** — Read, Write (complete), Edit (string replacement), Glob, Grep
- **Web** — WebFetch, WebSearch (Anthropic provider native)
- **MCP** — MCPTool for server invocation, ListMcpResources discovery
- **Agents** — AgentTool spawns sub-agents, SkillTool runs custom skills
- **Tasks** — TaskCreate/Update/Get/List/Stop for background jobs
- **Collaboration** — SendMessage (inter-agent), AskUserQuestion, Team management

Each tool receives `ToolUseContext` with:
- `AppState` (current session state)
- Permission context (for per-invocation checks)
- LRU file cache (avoid re-reading same files)
- MCP client registry
- Agent definitions
- UI callbacks (progress bars, dialogs)

### Permission System

Three-level hierarchy (all in `/home/orual/Git_Repos/claude-code/hooks/toolPermission/`):

1. **Auto-allow rules** — whitelist of safe operations (e.g., `cat` on known safe paths)
2. **Deny rules** — blacklist of dangerous patterns (e.g., `rm -rf /`, credentials access)
3. **Interactive prompt** — if rules don't match, ask user (Shift+Tab cycles: `default` → `ask` → `acceptEdits` → `plan` → `slipstream` → `bypassPermissions` → `auto`)

**Slipstream mode** intercepts 15+ destructive patterns (filesystem wipes, package publishing, git history rewrites) even when auto-approve is set.

---

## 2. OAuth Flow (Critical for Pattern)

**Source files:**
- `/home/orual/Git_Repos/claude-code/constants/oauth.ts` — endpoints and config
- `/home/orual/Git_Repos/claude-code/services/oauth/` — OAuth client
- `/home/orual/Git_Repos/claude-code/services/oauth/auth-code-listener.ts` — localhost server
- `/home/orual/Git_Repos/claude-code/utils/auth.ts` — token storage and refresh
- `/home/orual/Git_Repos/claude-code/commands/login/login.tsx` — login UI

### OAuth Endpoints (Production)

```typescript
// /home/orual/Git_Repos/claude-code/constants/oauth.ts
CLIENT_ID: "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
CONSOLE_AUTHORIZE_URL: "https://platform.claude.com/oauth/authorize"
CLAUDE_AI_AUTHORIZE_URL: "https://claude.com/cai/oauth/authorize"  // 307 redirect
CLAUDE_AI_ORIGIN: "https://claude.ai"
TOKEN_URL: "https://platform.claude.com/v1/oauth/token"
API_KEY_URL: "https://api.anthropic.com/api/oauth/claude_cli/create_api_key"
ROLES_URL: "https://api.anthropic.com/api/oauth/claude_cli/roles"
```

### OAuth Scopes

```typescript
// Two sets depending on user type:

// Console OAuth (API key creation)
CONSOLE_OAUTH_SCOPES = [
  "org:create_api_key",
  "user:profile"
]

// Claude.ai OAuth (subscription/inference)
CLAUDE_AI_OAUTH_SCOPES = [
  "user:profile",
  "user:inference",              // ← Critical for Opus access
  "user:sessions:claude_code",
  "user:mcp_servers",
  "user:file_upload"
]

// Login requests ALL scopes to handle both paths
ALL_OAUTH_SCOPES = [
  "org:create_api_key",
  "user:profile",
  "user:inference",
  "user:sessions:claude_code",
  "user:mcp_servers",
  "user:file_upload"
]
```

### Flow: Authorization Code (PKCE)

1. **Generate state + code challenge** (PKCE)
   ```typescript
   state = randomString(32)
   codeChallenge = base64url(sha256(codeVerifier))
   ```

2. **Start localhost listener** on OS-assigned port
   ```typescript
   // /home/orual/Git_Repos/claude-code/services/oauth/auth-code-listener.ts
   const listener = new AuthCodeListener("/callback")
   const port = await listener.start()  // Random port, e.g. 59382
   ```

3. **Open browser to authorize**
   ```
   https://claude.com/cai/oauth/authorize?
     client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e
     &redirect_uri=http://localhost:{port}/callback
     &response_type=code
     &scope=user:profile+user:inference+...
     &state={state}
     &code_challenge={codeChallenge}
     &code_challenge_method=S256
   ```

4. **Capture authorization code** at `http://localhost:{port}/callback?code=AUTH_CODE&state=STATE`
   - Validate state parameter (CSRF protection)
   - Extract auth code from query string

5. **Exchange for tokens**
   ```http
   POST https://platform.claude.com/v1/oauth/token
   Content-Type: application/json

   {
     "grant_type": "authorization_code",
     "code": "{AUTH_CODE}",
     "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
     "code_verifier": "{codeVerifier}",
     "redirect_uri": "http://localhost:{port}/callback"
   }
   ```

6. **Response contains:**
   ```json
   {
     "access_token": "...",
     "refresh_token": "...",
     "expires_in": 3600,
     "scope": "user:profile user:inference ..."
   }
   ```

### Token Storage

Tokens stored in **secure storage per OS** (not `~/.claude/` plaintext):
- **macOS:** Keychain via `security` command
- **Linux:** Secret Service or plaintext with restricted permissions
- **Windows:** Credential Manager

Access method:
```typescript
// /home/orual/Git_Repos/claude-code/utils/auth.ts
getClaudeAIOAuthTokens(): OAuthTokens | null
  → getSecureStorage().getOAuthToken()
  → OS-specific secure storage lookup
```

Backup location: `~/.claude/settings.json` stores minimal metadata (profile, billing type, subscription created date) but NOT tokens.

### Token Refresh

Automatic refresh on every request:
```typescript
// /home/orual/Git_Repos/claude-code/services/oauth/client.ts
checkAndRefreshOAuthTokenIfNeeded()
  → isOAuthTokenExpired(expiresAt)
    → bufferTime = 5 minutes
    → return (now + bufferTime >= expiresAt)
  → if expired: POST to TOKEN_URL with refresh_token
  → save new tokens to secure storage
```

### Key Decision Points for Pattern

1. **Use same OAuth endpoints** — Anthropic handles subscription validation, no need to reinvent
2. **Store tokens in OS keychain** — never plaintext config files
3. **5-minute refresh buffer** — prevents edge-case auth failures mid-conversation
4. **Validate state parameter** — CSRF protection in localhost listener
5. **Support both Console and Claude.ai flows** — both grant inference access but through different paths
6. **Token scope determines capabilities** — presence of `user:inference` scope gates Opus access

---

## 3. Rommie Code (Fork with AT Protocol + Copilot)

**Repository:** `/home/orual/Git_Repos/rommie-code`  
**Language:** TypeScript (Bun, same as claude-code upstream)  
**Status:** Active fork, 2026-03-31 snapshot baseline  

### Distinctive Additions

1. **AT Protocol Integration** — 8 specialized agents for ATProto operations
   - Orchestrator, Bot, Feed Generator, OAuth, Labeler, Lexicon, PDS, App View
   - All agents include MCP tool awareness
   - Full protocol documentation sourced from ATProto MCP server

2. **GitHub Copilot Integration** (`src/services/copilot/`, `CopilotTool`)
   - Quota management per workspace/user
   - Model listing via Copilot SDK
   - Delegation routing (sub-agents can use Copilot models)

3. **Multi-Provider Delegation** (`src/services/delegation/`)
   - Route sub-agents to different providers (Anthropic, Copilot, Ollama, etc.)
   - Provider resolution chain: env var → agent override → settings → parent
   - SessionFS auto-checkpoint during cross-provider handoffs

4. **Persistent State** (missing in upstream)
   - App state stored in SQLite (not shown in code tree, but referenced in commands)
   - Team memory sync across instances
   - Session recovery via `/resume` command

### Architecture (from `/home/orual/Git_Repos/rommie-code/CLAUDE.md`)

```
cli.tsx → main.tsx → init() → setUpCommands() → getTools() → createQueryEngine() → renderAndRun()
```

Entry bootstrap:
1. Set `MACRO` globals (version, package URL)
2. Bootstrap auth, settings, migrations, MCP
3. Load policy limits, team memory, session state
4. Initialize query engine
5. Launch React/Ink REPL or headless mode

### Notable Architectural Patterns

**Feature gates** — Bun's `bun:bundle` dead code elimination:
```typescript
import { feature } from 'bun:bundle'

const voiceCommand = feature('VOICE_MODE')
  ? require('./commands/voice/index.js').default
  : null
```

Supports: PROACTIVE, KAIROS, BRIDGE_MODE, DAEMON, VOICE_MODE, CHICAGO_MCP, TRANSCRIPT_CLASSIFIER, AGENT_TRIGGERS, etc.

**Permission modes** — Shift+Tab cycles through:
```
default → ask → acceptEdits → plan → slipstream → bypassPermissions → auto
```

Each mode defines what operations auto-approve vs. require interactive prompt.

**Command registry** — Dynamic loading from 4 sources:
1. Built-in (`commands.ts`)
2. Skills (`~/.rommie/skills/`)
3. Plugins (dynamic `init()` hooks)
4. MCP servers (slash commands from MCP tools)

**State management** — Zustand-like store (`AppState.tsx`, `AppStateStore.ts`):
- Messages, in-progress tools, tasks, permissions, MCP servers
- Team members, settings, user preferences
- Components and tools subscribe via `useAppState(selector)`

### What's Unsuitable for Rust Port

1. **React/Ink UI** — Pattern uses different UI (Tauri/web + TUI fallback), so reimplement in Rust UI frameworks
2. **Bun feature gates** — Rust has feature flags (Cargo.toml), similar concept but different mechanism
3. **Keyboard input buffering** — Rommie has complex stdin handling for raw mode; Rust libraries (crossterm, termwiz) handle this better
4. **Plugin system via dynamic imports** — Rust would use wasm plugins or hardcoded trait implementations

---

## 4. Claude Code Modes (Prompting Framework)

**Repository:** `/home/orual/Git_Repos/claude-code-modes`  
**Language:** TypeScript (Bun)  
**License:** MIT  
**Author:** Nick Klisch  

### Core Concept

Replace claude-code's default system prompt (cautious, minimal, terse) with behavioral tuning:

```bash
claude-mode create      # Build from scratch with proper architecture
claude-mode extend      # Extend a fast-built project, improve incrementally
claude-mode safe        # Surgical precision, minimal risk
claude-mode refactor    # Restructure freely across the codebase
claude-mode explore     # Read-only — understand code without changing it
claude-mode debug       # Investigation-first debugging (chill base)
```

### Axis Model

Three independent behavioral axes:

1. **Agency** — How much initiative?
   - `autonomous` — makes decisions, restructures without asking
   - `collaborative` — explains reasoning, checks in at decision points
   - `surgical` — executes exactly what was asked, nothing more

2. **Quality** — What code standard?
   - `architect` — proper abstractions, error handling, forward-thinking
   - `pragmatic` — match existing patterns, improve incrementally
   - `minimal` — smallest correct change, no speculative improvements

3. **Scope** — How far beyond the request?
   - `unrestricted` — free to create, reorganize, restructure
   - `adjacent` — fix related issues in the neighborhood
   - `narrow` — only what was explicitly asked

Presets combine these (e.g., `create` = autonomous/architect/unrestricted).

### Implementation

System prompt assembled from markdown fragments:
```
prompts/
  base/           Standard base (derived from upstream Claude Code)
  chill/          Alternative base (emotion-research-informed, leaner)
  axis/           Behavioral axes (agency, quality, scope)
  modifiers/      Layers (bold, debug, methodical, director, readonly)
```

Each base has `base.json` manifest declaring fragment order:
```json
["core.md", "axes", "actions.md", "tools.md", "modifiers", "env.md"]
```

When run: resolve preset → read base + axis fragments → detect environment → write temp file → spawn `claude --system-prompt-file /tmp/...md`

### Chill Base

Alternative base informed by Anthropic's emotion research — shorter (~65% original size), calmer framing, no ALL-CAPS emphasis:

```bash
claude-mode create --base chill
```

Key insight: Claude's confidence state directly affects output quality. Chill base uses positive, confident framing instead of hedging and over-engineering.

### Limitations for Pattern

1. **Pattern doesn't need mode tuning initially** — single system prompt is fine
2. **Environment info is static** — git status, branch captured once; doesn't refresh during session
3. **Named specialists ignore the prompt** — Explore, Plan agents have hardcoded prompts on Haiku
4. **For Pattern:** Create agents with explicit personality in CLAUDE.md, don't rely on prompting tricks

---

## 5. Popup MCP (Native UI Toolkit)

**Repository:** `/home/orual/Git_Repos/popup-mcp`  
**Language:** Rust  
**Architecture:** MCP server with egui GUI + TUI fallback  
**License:** MIT  

### Purpose

Display interactive popup windows from AI assistants through MCP protocol. Rich dialogue trees with conditional branches.

### Structure

```rust
crates/
  popup-common/       // Shared types (PopupState, Element, etc.)
    element_deser.rs
  popup-gui/          // egui GUI implementation
    gui/mod.rs
    mcp_server.rs
    json_parser.rs
    schema.rs
    theme.rs
  popup-tui/          // TUI fallback (crossterm, ratatui)
```

### Element Types (JSON-based)

```json
{
  "title": "Configure your project",
  "elements": [
    {
      "select": "Project Type",
      "options": ["Web", "CLI", "Library"],
      "Web": [
        {
          "select": "Framework",
          "options": ["React", "Vue"],
          "React": [
            {
              "check": "TypeScript",
              "reveals": [{"check": "Strict mode"}]
            }
          ]
        }
      ]
    }
  ]
}
```

**Element types:**
- `text` — display text
- `input` / `input` with `rows` — text entry
- `select` — dropdown
- `multi` — multiselect
- `check` — checkbox with optional `reveals` (conditional children)
- `slider` — range input
- `group` — section grouping

**Conditional visibility:** `"when": "selected(field_id, 'value') && count(other) > 2"`

### MCP Server Interface

```rust
// crates/popup-gui/src/mcp_server.rs
impl MCPServer for PopupMcpServer {
    async fn handle_tool_call(
        name: &str,
        params: serde_json::Value,
    ) -> Result<ToolResult> {
        match name {
            "popup" => {
                // Parse JSON, render GUI, return user selections
                let config: PopupConfig = serde_json::from_value(params)?;
                let result = self.render_popup(config).await?;
                Ok(ToolResult::from_value(result))
            }
        }
    }
}
```

### Integration with Pattern

**Distinctive value:** popup-mcp demonstrates how to surface native UI from an MCP server:
1. MCP tool call triggers popup render
2. User interacts with GUI (not terminal)
3. Selections returned as structured JSON
4. Agent processes response, updates conversation

**For Pattern:**
- Can adopt similar approach for configuration dialogs
- Or embed popup-style UIs in Tauri frontend (more direct)
- MCP server pattern useful for headless deployments

---

## 6. Cross-Repo Patterns Worth Stealing

### 1. OAuth Flow + Subscription Model

**What to steal:**
- Local listener pattern (port 0 for auto-assignment)
- State + PKCE for CSRF/spoofing protection
- 5-minute refresh buffer prevents edge cases
- OS keychain storage (secure by default)
- Scope-based capability gating

**For Pattern:**
```rust
// Pattern OAuth flow (Rust implementation)
pub struct OAuthFlow {
    client_id: String,
    client_secret: Option<String>,  // Optional for public clients
    authorize_url: String,
    token_url: String,
    redirect_host: String,           // "localhost"
}

impl OAuthFlow {
    pub async fn login(&self) -> Result<OAuthTokens> {
        let state = random_string(32);
        let (verifier, challenge) = pkce::generate();
        
        let listener = LocalListener::bind("127.0.0.1:0").await?;
        let port = listener.port();
        
        let auth_url = format!(
            "{}?client_id={}&redirect_uri=http://localhost:{}/callback&...",
            self.authorize_url, self.client_id, port
        );
        
        open_browser(&auth_url)?;
        let (code, returned_state) = listener.wait_for_code().await?;
        
        if returned_state != state { return Err("CSRF failed"); }
        
        let tokens = self.exchange_code(&code, &verifier).await?;
        keychain::save_tokens(&tokens)?;  // Secure storage
        
        Ok(tokens)
    }
}
```

### 2. Tool System Architecture

**What to steal:**
- Self-contained tool modules (schema + execution + metadata)
- ToolContext with minimal dependencies (avoids circular references)
- Permission checks at invocation time (not compile time)
- Read-only flag + defer mechanism for tool scheduling

**For Pattern (Rust):**
```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> &serde_json::Schema;
    fn is_read_only(&self) -> bool;
    fn should_defer(&self) -> bool;
    
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput>;
}

pub struct ToolContext {
    pub app_state: Arc<AppState>,
    pub permission_context: PermissionContext,
    pub file_cache: Arc<Mutex<LruCache<PathBuf, String>>>,
    pub mcp_clients: Arc<MCP_ClientRegistry>,
}
```

### 3. Permission System (Hierarchy, Not Flat)

**What to steal:**
- Three-tier model (auto-allow → deny → interactive)
- Slipstream safety net (catches destructive patterns even when auto-approve)
- Permission mode cycling (Shift+Tab)
- Per-invocation checks (not session-wide)

**For Pattern:**
```rust
pub enum PermissionResult {
    Allowed,
    Denied { reason: String },
    RequiresInteractive,
}

pub struct PermissionChecker {
    allow_patterns: Vec<Regex>,
    deny_patterns: Vec<Regex>,
    slipstream_guards: Vec<Box<dyn Guard>>,
}

impl PermissionChecker {
    pub async fn check(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        mode: PermissionMode,
    ) -> PermissionResult {
        // 1. Check allow patterns
        if self.matches_allow(tool_name, input) {
            return PermissionResult::Allowed;
        }
        
        // 2. Check deny patterns
        if self.matches_deny(tool_name, input) {
            return PermissionResult::Denied { /* ... */ };
        }
        
        // 3. Check slipstream guards
        if mode == PermissionMode::Slipstream {
            if let Some(guard) = self.slipstream_guards.iter().find(|g| g.matches(input)) {
                return PermissionResult::Denied { /* ... */ };
            }
        }
        
        // 4. Fall back to interactive
        PermissionResult::RequiresInteractive
    }
}
```

### 4. Message Normalization + Token Budget

**What to steal:**
- Strip signatures from messages before switching API keys (prevents rejection of stale signatures)
- Track token budget across turns
- Auto-compact when approaching context limit
- Preserve semantic meaning during compaction

**For Pattern:**
```rust
pub async fn normalize_messages(
    messages: &[Message],
    api_key_changed: bool,
) -> Vec<Message> {
    messages
        .iter()
        .map(|msg| {
            if api_key_changed {
                msg.strip_signature_blocks()
            } else {
                msg.clone()
            }
        })
        .collect()
}

pub struct TokenBudget {
    limit: usize,
    current_usage: usize,
    margin: usize,  // e.g., 10% reserved
}

impl TokenBudget {
    pub fn should_compact(&self) -> bool {
        self.current_usage + self.margin >= self.limit
    }
}
```

### 5. MCP Server Integration

**What to steal:**
- Connection pooling for multiple MCP servers
- Tool discovery endpoint (ListMcpResources)
- Per-server auth (OAuth, API keys, etc.)
- Fallback to TUI when GUI unavailable (popup-mcp pattern)

---

## 7. System Prompt Structure

Claude Code's system prompt consists of markdown sections assembled dynamically:

```markdown
# Claude Code - System Prompt

## Core Instructions
- You are Claude Code, an agentic CLI tool...
- You operate in a terminal environment...
- Your primary mode is the React/Ink REPL...

## Tool Instructions
[Tool schemas, invocation details, examples]

## Environment Detection
[Current user, shell, platform, git state, etc.]

## Contextual Guidance
[Project type inference, language detection, etc.]

## Permission Model
[Tool permission categories, what requires approval, etc.]

## Output Format
[How to format messages, code blocks, etc.]
```

**For Pattern:** Create persistent agent personas as CLAUDE.md files, not system prompt variants.

---

## 8. Unsuitable for Rust Rewrite

### 1. React/Ink Terminal UI
- **Reason:** Rust doesn't have a mature drop-in replacement
- **Pattern solution:** Use Tauri for GUI, crossterm/ratatui for TUI, separate implementations
- **Cost:** UI layer is ~160 components in claude-code; rewrite is 1-2 weeks

### 2. Keyboard Event Buffering
- **Reason:** Raw mode stdin handling is complex in Node.js; Rust libraries abstract better
- **Pattern solution:** Use crossterm's event loop (handles this natively)
- **Advantage:** Rust approach is actually simpler

### 3. Dynamic Plugin System (Eval)
- **Reason:** Bun supports `require()` from CLI; Rust doesn't
- **Pattern solution:** Use WASM plugins or hardcoded trait implementations
- **Cost:** Plugins need compilation, can't be user scripts

### 4. Feature Gate Dead Code Elimination
- **Reason:** Bun's `bun:bundle` does this at build time
- **Pattern solution:** Use Cargo feature flags (similar concept, different mechanism)
- **Tradeoff:** Requires separate feature combinations to build, not single binary

### 5. Prompt Composition from Fragments
- **Reason:** Easy in Node.js (filesystem + string concat), doable in Rust but more boilerplate
- **Pattern solution:** Embed prompts as constants, assemble at runtime
- **Advantage:** Faster (no filesystem read), no security issues

---

## 9. Critical Architectural Differences for Pattern

### Token Billing Model

**Claude Code assumption:** Opus access via OAuth subscription, per-token billing via Anthropic API.

**Pattern difference:** Wants to avoid per-token billing — use:
1. Cached system prompts (reduces input tokens)
2. Session compression on boundaries (reduces history tokens)
3. Batch API if available (Anthropic Batch API)
4. Or: user pre-purchases API credit with Pattern-owned key

**OAuth still needed** for: user identification, feature flags (GrowthBook), team membership validation.

### Persistent Agent Personas

**Claude Code:** Fresh system prompt per session, no cross-session memory except in `~/.claude/memories/`.

**Pattern:** Each agent has persistent persona:
```rust
pub struct AgentPersona {
    pub id: AgentId,
    pub name: String,
    pub description: String,
    pub system_prompt: String,           // Merged with global + task
    pub role: AgentRole,                 // Coach, Debugger, Architect, etc.
    pub memory: Vector<MemoryBlock>,     // CRDT-backed (Loro)
    pub traits: HashMap<String, String>, // Personality traits
    pub preferences: AgentPreferences,   // Communication style, verbosity, etc.
    pub created_at: Timestamp,
    pub last_active: Timestamp,
}
```

Stored in SQLite with CRDT synchronization across sessions.

### Multi-Agent Coordination

**Claude Code:** Sub-agents inherit parent's system prompt via fork mechanism.

**Pattern:** Coordinator spawns agents with:
1. Parent context checkpoint (SessionFS integration)
2. Task-specific personality tuning
3. Provider routing (Copilot for some, Opus for others)
4. Task output aggregation

---

## 10. Concrete File References

### OAuth Implementation
- `/home/orual/Git_Repos/claude-code/constants/oauth.ts` — endpoints, scopes, client ID
- `/home/orual/Git_Repos/claude-code/services/oauth/client.ts` — token refresh, expiry
- `/home/orual/Git_Repos/claude-code/services/oauth/auth-code-listener.ts` — localhost handler
- `/home/orual/Git_Repos/claude-code/utils/auth.ts` — token storage, getClaudeAIOAuthTokens()
- `/home/orual/Git_Repos/claude-code/commands/login/login.tsx` — login UI

### Tool System
- `/home/orual/Git_Repos/claude-code/Tool.ts` — tool interface definition
- `/home/orual/Git_Repos/claude-code/tools.ts` — tool registry with feature gates
- `/home/orual/Git_Repos/claude-code/tools/*/index.ts` — individual tool implementations
- `/home/orual/Git_Repos/claude-code/hooks/toolPermission/` — permission checks

### Permission System
- `/home/orual/Git_Repos/claude-code/hooks/toolPermission/` — full permission logic
- `/home/orual/Git_Repos/claude-code/utils/permissions/slipstreamGuardrails.ts` — destructive pattern detection

### Query Engine
- `/home/orual/Git_Repos/claude-code/QueryEngine.ts` — main loop
- `/home/orual/Git_Repos/claude-code/query.ts` — query construction
- `/home/orual/Git_Repos/claude-code/context.ts` — system prompt assembly

### Rommie-Specific
- `/home/orual/Git_Repos/rommie-code/CLAUDE.md` — architecture documentation
- `/home/orual/Git_Repos/rommie-code/src/services/delegation/` — multi-provider routing
- `/home/orual/Git_Repos/rommie-code/src/services/copilot/` — GitHub Copilot integration

### Claude Code Modes
- `/home/orual/Git_Repos/claude-code-modes/prompts/` — axis and modifier fragments
- `/home/orual/Git_Repos/claude-code-modes/src/assemble.ts` — prompt assembly logic
- `/home/orual/Git_Repos/claude-code-modes/src/cli.ts` — CLI interface

### Popup MCP
- `/home/orual/Git_Repos/popup-mcp/crates/popup-gui/src/mcp_server.rs` — MCP interface
- `/home/orual/Git_Repos/popup-mcp/crates/popup-gui/src/json_parser.rs` — element parsing
- `/home/orual/Git_Repos/popup-mcp/crates/popup-common/src/` — shared types

---

## 11. Recommendations for Pattern

### Priority 1: Adopt
1. **OAuth flow** (exact same endpoints, scopes, PKCE)
2. **Tool system architecture** (self-contained, schema-driven)
3. **Permission hierarchy** (3-tier model, slipstream guards)
4. **Token budget + compaction** (automatic history management)
5. **OS keychain storage** (never plaintext)

### Priority 2: Adapt
1. **UI framework** — Tauri + web frontend (not Ink)
2. **Feature flags** — Cargo features (not Bun bundles)
3. **Plugin system** — WASM or hardcoded traits (not dynamic require)
4. **Prompting** — CLAUDE.md per agent (not mode system, initially)

### Priority 3: Skip
1. **React/Ink code** — rewrite for native UI
2. **Keyboard input buffering** — use crossterm instead
3. **Bun-specific patterns** — use Rust equivalents
4. **Prompt composition fragments** — embed as constants

### New for Pattern
1. **CRDT-backed memory** (Loro) for agent personas
2. **Coordinator for multi-agent tasks** (pattern routing + orchestration)
3. **SessionFS integration** (cross-provider checkpoints)
4. **Team/org context** (beyond single-user like claude-code)

---

## Summary

Claude Code's architecture is mature and battle-tested. Pattern should borrow liberally from:
- OAuth flow (don't reinvent authentication)
- Tool system (proven design for agent extensibility)
- Permission checks (slipstream guards work)
- Token management (budget + compaction prevents context overflow)

But Pattern's key differentiators are:
- **Persistent agent personas** (Loro CRDT for durability)
- **Multi-user/team support** (vs. single user CLI)
- **Native GUI** (Tauri, not terminal-only)
- **Simplified prompting initially** (agent personality in code, not prompt tricks)

The OAuth flow and tool system are load-bearing. Everything else can be adapted to Rust idioms and Pattern's specific needs.
