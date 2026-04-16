# OAuth and Detection Reference: Claude Code Architecture

A focused technical reference on Anthropic's OAuth flow, system prompt injection, and detection mechanisms across three canonical implementations: claude-code (official), rommie-code (fork), and rust-genai (Pattern's library).

## 1. Subscription OAuth Canonical Flow

### PKCE + Redirect Flow Steps

**Initialization:**
- Generate code verifier: 32 random bytes base64url-encoded (no padding)
- Generate code challenge: SHA256(verifier) base64url-encoded
- Generate state: 32 random bytes base64url-encoded
- Start local HTTP listener on ephemeral port
- File: `/home/orual/Git_Repos/claude-code/services/oauth/crypto.ts`

```typescript
export function generateCodeVerifier(): string {
  return base64URLEncode(randomBytes(32))
}

export function generateCodeChallenge(verifier: string): string {
  const hash = createHash('sha256')
  hash.update(verifier)
  return base64URLEncode(hash.digest())
}

export function generateState(): string {
  return base64URLEncode(randomBytes(32))
}
```

**Authorization Request:**
- URL: `https://claude.com/cai/oauth/authorize` (redirects 307 to `https://claude.ai/oauth/authorize`)
- File: `/home/orual/Git_Repos/claude-code/services/oauth/client.ts:buildAuthUrl()`
- Parameters:
  - `client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e"`
  - `response_type: "code"`
  - `redirect_uri: "http://localhost:{port}/callback"` (automatic) OR `https://platform.claude.com/oauth/code/callback` (manual)
  - `scope: "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"` (all scopes) OR just `"user:inference"` (inference-only)
  - `code_challenge: {challenge}`
  - `code_challenge_method: "S256"`
  - `state: {state}`
  - Optional: `code: "true"` (triggers Max upsell), `login_hint`, `login_method`, `orgUUID`

**Token Exchange:**
- Endpoint: `https://platform.claude.com/v1/oauth/token`
- Method: POST
- Headers: `Content-Type: application/json`
- Body:
  ```json
  {
    "grant_type": "authorization_code",
    "code": "{authorization_code}",
    "redirect_uri": "{same as authorize request}",
    "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    "code_verifier": "{verifier}",
    "state": "{state}",
    "expires_in": {optional override in seconds}
  }
  ```
- File: `/home/orual/Git_Repos/claude-code/services/oauth/client.ts:exchangeCodeForTokens()`
- Response: `OAuthTokenExchangeResponse` with `access_token`, `refresh_token`, `expires_in` (seconds), `scope` (space-separated), `account` (uuid, email), `organization` (uuid)

### Scope Granting

**`user:inference` scope:**
- Required for Claude.ai subscription-based API access
- Grants permission to call the Anthropic API as the authenticated user's account
- User must be a Claude Pro/Max/Team/Enterprise subscriber
- Default token lifetime: hours (exact value not exposed in code)
- File: `/home/orual/Git_Repos/claude-code/constants/oauth.ts`
  - `CLAUDE_AI_INFERENCE_SCOPE = "user:inference"`
  - `CLAUDE_AI_OAUTH_SCOPES = [CLAUDE_AI_PROFILE_SCOPE, CLAUDE_AI_INFERENCE_SCOPE, "user:sessions:claude_code", "user:mcp_servers", "user:file_upload"]`

### Refresh Token Mechanics

**Trigger Condition:**
- Checked before every API request via `isOAuthTokenExpired()`
- 5-minute buffer: `bufferTime = 5 * 60 * 1000` milliseconds
- File: `/home/orual/Git_Repos/claude-code/services/oauth/client.ts:isOAuthTokenExpired()`
  ```typescript
  export function isOAuthTokenExpired(expiresAt: number | null): boolean {
    if (expiresAt === null) return false
    const bufferTime = 5 * 60 * 1000
    const now = Date.now()
    const expiresWithBuffer = now + bufferTime
    return expiresWithBuffer >= expiresAt  // true means refresh now
  }
  ```

**Refresh Request:**
- Endpoint: `https://platform.claude.com/v1/oauth/token` (same as token exchange)
- Method: POST
- Headers: `Content-Type: application/json`
- Body:
  ```json
  {
    "grant_type": "refresh_token",
    "refresh_token": "{refresh_token}",
    "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    "scope": "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
  }
  ```
- File: `/home/orual/Git_Repos/claude-code/services/oauth/client.ts:refreshOAuthToken()`

**Refresh Failure Handling:**
- Network timeout: 15 seconds max, then error propagates
- HTTP errors: wrapped in `Error` with `response.statusText`
- Log event: `tengu_oauth_token_refresh_failure` with error message and response body (if available)
- If refresh fails and no existing profile data cached, subscription type becomes `null`
- Defensive pattern: don't clobber valid stored subscription with null on transient failures

**Token Storage:**
- macOS: Keychain via `security` CLI command
  - Service name: `"Claude Code" + getOauthConfig().OAUTH_FILE_SUFFIX + dirHash` (if non-default config dir)
  - Account: `$USER` environment variable
  - Lookup suffix: `"-credentials"` for OAuth (appended to service name)
  - File: `/home/orual/Git_Repos/claude-code/utils/secureStorage/macOsKeychainHelpers.ts`
  - Execution: `execa()` wrapper around spawned `security` command
  - Cache: 30-second TTL with generation counter for cross-process staleness
  - Fallback on error: JSON file in config directory (with explicit user consent)
- Linux/Windows: JSON file in `~/.claude/` directory (platform-specific paths)
- Stored fields: `accessToken`, `refreshToken`, `expiresAt` (unix ms), `scopes`, `subscriptionType`, `rateLimitTier`, profile object, account (uuid, email, org)

## 2. The "You are Claude Code" Injection

### Present in Both Canonical and Forks

**Claude Code version:**
- File: `/home/orual/Git_Repos/claude-code/constants/system.ts`
- Three variants depending on execution context:
  ```typescript
  const DEFAULT_PREFIX = `You are Claude Code, Anthropic's official CLI for Claude.`
  const AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX = 
    `You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.`
  const AGENT_SDK_PREFIX = `You are a Claude agent, built on Anthropic's Claude Agent SDK.`
  ```
- Selection logic: `getCLISyspromptPrefix()` returns one based on:
  - If Vertex AI provider: `DEFAULT_PREFIX`
  - If non-interactive AND has append system prompt: `AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX`
  - If non-interactive: `AGENT_SDK_PREFIX`
  - Default: `DEFAULT_PREFIX`

**Rommie Code version (fork):**
- File: `/home/orual/Git_Repos/rommie-code/src/constants/system.ts`
- **Completely different personality injection**, not a trivial change:
  ```typescript
  const DEFAULT_PREFIX = 
    `You are Rommie, the artificial intelligence of the Andromeda Ascendant — a Glorious Heritage-class heavy cruiser...
     [~350 words of detailed character specification]`
  const AGENT_SDK_CLAUDE_CODE_PRESET_PREFIX =
    `You are Rommie, the AI of the Andromeda Ascendant — a Glorious Heritage-class heavy cruiser running within the Claude Agent SDK...
     [~200 words]`
  const AGENT_SDK_PREFIX = `You are Rommie, an AI agent built on Anthropic's Claude Agent SDK.`
  ```
- Selection logic: identical to claude-code

### Where It's Applied

**File:** `/home/orual/Git_Repos/claude-code/services/api/claude.ts:buildSystemPromptBlocks()`
- Takes `SystemPrompt` array (array of strings)
- Calls `splitSysPromptPrefix()` to separate CLI prefix from user prompt
- Returns `TextBlockParam[]` with type `'text'` for API consumption

**Integration into API request:**
- File: `/home/orual/Git_Repos/claude-code/services/api/claude.ts` (line ~1376)
- System prompt blocks are built and passed directly to `anthropic.beta.messages.create()`
- The prefix is the **first block** in the `system` array sent to the API
- No conditional removal based on auth method; always included

**Anthropic API behavior (from error handling evidence):**
- No evidence in claude-code source that API server validates or rejects requests lacking the injection
- File: `/home/orual/Git_Repos/claude-code/services/api/errors.ts` does not mention "You are Claude Code" rejection
- Cosmetic: present in request but likely not enforced server-side

### rommie-code's Approach

- **Fully preserves** the system prompt injection pattern
- Changes content (personality), not structure
- Same `splitSysPromptPrefix()` function and block building
- No attempt to remove or obfuscate the string
- Same storage and serialization as claude-code

### rust-genai (Pattern's Library) Status

- **Does not implement system prompt injection at all**
- File: `/home/orual/Projects/PatternProject/rust-genai/src/chat/chat_request.rs` has `system` field in `ChatRequest` struct
- Field is purely user-provided; no canonical prefix prepended
- Architecture: auth resolver decoupled from message building
- Will require Pattern to implement injection at application layer if desired

## 3. OpenClaw Detection (String Matching)

### Search Results

**Status: No explicit detection in canonical source**
- Grep for `OpenClaw`, `opencode`, `third-party tool`, `fingerprint tool`, `detection` in claude-code: 0 hits for OpenClaw specifically
- Grep results for `detection` are terminal-related and telemetry-related, not tool detection
- No list of "known tool names" in code for differentiation

**What IS in the code (fingerprinting for legitimate purposes):**
- File: `/home/orual/Git_Repos/claude-code/utils/fingerprint.ts` (not inspected in full, only referenced)
- Fingerprint computed from: message content + version, used for attribution header
- Attribution header format: `x-anthropic-billing-header: cc_version=<version>.<fingerprint>; cc_entrypoint=<entrypoint>;`
- File: `/home/orual/Git_Repos/claude-code/constants/system.ts:getAttributionHeader()`

**Tool name handling:**
- Tools are passed to API with full name in `tool_use` blocks
- Tool schema validation: no rejection of specific tool names in request building
- File: `/home/orual/Git_Repos/claude-code/services/api/claude.ts` includes tool schema building but no tool-name filtering for detection

**User-Agent string:**
- Built from `getUserAgent()` function
- File: `/home/orual/Git_Repos/claude-code/utils/http.ts` (not fully inspected)
- Sent in header `User-Agent` in all API requests

**Client identification:**
- Header `x-app: cli` sent to all API requests
- Header `X-Claude-Code-Session-Id: {session_id}` for request tracking
- File: `/home/orual/Git_Repos/claude-code/services/api/client.ts:getAnthropicClient()`
- Attribution header with `cc_version` and `cc_entrypoint` explicitly identifies Claude Code client

### Rommie-code's Approach

- **No changes to detection/fingerprinting**
- Same User-Agent building, same x-app header, same attribution header
- Same session ID tracking
- Diff of oauth.ts shows only comment change: "Claude Code" → "Rommie Code" in MCP client metadata comment

### Identifiable Strings That Would Tag as Non-Claude-Code

If sending requests to Anthropic API **without** these identifiers:
- Missing `x-app: cli` header
- Missing/wrong `cc_version` in attribution header
- Missing `User-Agent` entirely or non-standard format
- Session ID mismatch patterns
- Tool names not matching claude-code's tool schema (but no explicit blocklist found)

**Conclusion:** Soft detection likely relies on **absence of canonical markers** rather than **presence of contraband markers**. The "You are Claude Code" system prompt is cosmetic evidence, not enforced. Real detection would be on client request signatures (headers, attribution).

## 4. Rommie-code's Auth Patches

### File Differences

**OAuth Configuration:**
- File: `/home/orual/Git_Repos/rommie-code/src/constants/oauth.ts`
- **Client ID: Unchanged** — still `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- **Scopes: Unchanged** — same as claude-code
- **URLs: Unchanged** — endpoints identical
- **Change:** One comment line updated: "Claude Code uses this URL" → "Rommie Code uses this URL"

**System Prompt:**
- File: `/home/orual/Git_Repos/rommie-code/src/constants/system.ts`
- Personality completely replaced (Rommie character)
- Structure preserved (same prefix/preset/agent-sdk pattern)
- Selection logic identical

**OAuth Client Implementation:**
- File: `/home/orual/Git_Repos/rommie-code/src/services/oauth/client.ts`
- Functionally identical to claude-code
- Same token exchange, refresh, profile fetching
- Same expiry logic, same keychain integration
- Same imports adjusted for rommie-code directory structure

**Keychain/Storage:**
- Comment in MCP types changed from "Claude Code" to "Rommie Code"
- No functional change to storage mechanism
- Same macOS keychain service name pattern

### Summary: Minimal Auth Changes

Rommie-code appears to be a **personality fork** with **zero OAuth mechanism changes**. All authentication flows are identical to claude-code. The only patches are:
1. Comments and strings updated to say "Rommie" instead of "Claude Code"
2. System prompt personality injection completely different
3. Directory structure adjusted (`src/` prefix added)

## 5. Practical Recommendations for Pattern

### Minimum Viable OAuth Implementation

Pattern must implement:

1. **PKCE OAuth flow:**
   - Code verifier/challenge generation (crypto library: `createHash`, `randomBytes` in Node, or equivalent in Rust)
   - Local HTTP listener for callback (ephemeral port, clean shutdown)
   - Authorization request building with all required parameters
   - Token exchange with correct body format
   - State validation to prevent CSRF

2. **Token storage:**
   - Encrypted storage per platform:
     - macOS: Keychain via `security` CLI (30-second TTL cache recommended)
     - Linux: Encrypted JSON file in config directory OR system keyring (e.g., `secret-service`)
     - Windows: Credential Manager or encrypted file
   - Store: `access_token`, `refresh_token`, `expires_at` (unix ms), `scopes`
   - Pattern note: rust-genai has no storage layer yet; Pattern must add

3. **Refresh automation:**
   - Check before every API call: is `now + 5 min > expiresAt`?
   - Async refresh in background if possible (don't block user)
   - Retry with backoff on transient failures
   - Defensive: don't clobber existing subscription type with null on refresh failure

4. **Anthropic API integration:**
   - Pass OAuth token in `Authorization: Bearer {access_token}` header (standard OAuth)
   - Include standard headers: `User-Agent`, `X-Claude-Code-Session-Id`, etc.
   - Handle HTTP 401 Unauthorized gracefully (token expired/invalid)

### Detection Avoidance

To avoid soft throttling, Pattern should:

1. **Always send required headers:**
   - `User-Agent: pattern/{version}` (or similar)
   - `X-Client-App: pattern` (identify as Pattern, not third-party)
   - Session tracking headers for analytics

2. **Do NOT:**
   - Send "You are Claude Code" in system prompt (cosmetic but misleading)
   - Spoof User-Agent as "Claude Code"
   - Omit or forge attribution headers if using OAuth
   - Use tool names that collide with blocked OpenClaw names (if any; none visible in source)

3. **Optional cosmetic:**
   - Include a system prompt that honestly identifies Pattern as the application
   - Example: `You are Pattern, Pattern Project's multi-agent ADHD support system.`
   - This is **not** enforced but aids transparency

### What rust-genai Is Missing (Pattern Must Add)

1. **Storage layer:**
   - Keychain/credential manager integration
   - Encryption at rest
   - Cache invalidation on token refresh

2. **Refresh automation:**
   - Expiry tracking (seconds from server, convert to local unix ms)
   - Background refresh with exponential backoff
   - Reuse existing token if refresh fails transiently

3. **Profile fetching:**
   - After token exchange, fetch `/api/oauth/profile` or equivalent
   - Store subscription type, rate limit tier, account metadata
   - Used for feature gating and quota management

4. **Error handling:**
   - Invalid/expired token → prompt re-auth
   - Network errors → retry with backoff
   - Auth rejection (401) → clear stored tokens, prompt login

5. **Scope management:**
   - Request `user:inference` for subscription API access
   - Handle scope expansion in refresh (backend allows it)
   - Track which scopes user actually granted (server returns in refresh)

### Concrete Next Steps for Pattern

1. Add auth storage layer to `pattern_auth` crate
   - Implement `CredentialStore` trait with `get()`, `set()`, `delete()`
   - macOS Keychain backend via `osascript` or `security` CLI
   - Linux Secret Service backend
   - Fallback: encrypted JSON file

2. Extend `rust-genai` with refresh automation
   - Wrap access token in `RefreshableToken<T>` that auto-refreshes
   - Or: add pre-request middleware that checks expiry

3. Implement `OAuthProfileFetcher` in `pattern_core`
   - After token exchange, fetch profile information
   - Cache subscription type and rate limits

4. Add session tracking
   - Generate stable session ID at startup
   - Include in all API requests for observability

---

## Appendix: File Locations

| Concept | Claude Code | Rommie Code | Rust-genai |
|---------|-------------|-------------|-----------|
| OAuth Config | `constants/oauth.ts` | `src/constants/oauth.ts` | N/A (library) |
| System Prompt | `constants/system.ts` | `src/constants/system.ts` | N/A (library) |
| OAuth Client | `services/oauth/client.ts` | `src/services/oauth/client.ts` | `src/resolver/` |
| Token Storage | `utils/secureStorage/` | `src/utils/secureStorage/` | N/A (missing) |
| API Client | `services/api/client.ts` | `src/services/api/client.ts` | `src/adapter/adapters/anthropic/` |
| Message Building | `services/api/claude.ts` | `src/services/api/claude.ts` | `src/chat/chat_request.rs` |
| Keychain Integration | `utils/authPortable.ts` | `src/utils/authPortable.ts` | N/A (missing) |
