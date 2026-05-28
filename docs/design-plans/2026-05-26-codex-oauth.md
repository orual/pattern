# Codex OAuth Design

## Summary

Pattern already has a working provider gateway for Anthropic that handles OAuth token management, credential chaining, request shaping, and URL dispatch. This design extends that same infrastructure to support OpenAI — specifically the OAuth-authenticated path used by Codex CLI, which targets a different backend endpoint (`chatgpt.com/backend-api/codex/responses`) than the standard OpenAI API and requires additional headers derived from an OpenID Connect token.

The approach is to build three new modules in `pattern_provider/src/auth/`: an OAuth state machine that implements the same PKCE and device-code flows as Codex CLI, a storage layer that can read and write Codex's `~/.codex/.auth.json` format while keeping keyring as the primary store, and a refresh manager that serializes token renewal both in-process (Tokio mutex) and cross-process (advisory file lock) to handle the realistic case of Pattern and Codex CLI running on the same machine simultaneously. These modules are then wired into the existing gateway via a new `OpenAiAuthChain` that mirrors `AnthropicAuthChain`'s tier-walking shape, with targeted extensions to `chat_url_for` and `auth_headers_for_tier` to cover the two OpenAI adapter kinds and the OAuth-vs-api-key URL split. A pre-flight gate prevents the easy misconfiguration of using a Chat Completions model under the OAuth tier, which only supports the Responses API. The design adds no new types to the underlying `genai` library and introduces no model catalog of its own, delegating that routing logic entirely to genai's existing `AdapterKind::from_model`.

## Definition of Done

**Primary deliverables:**
- An `OpenAiAuthChain` is added to `pattern_provider`, mirroring `AnthropicAuthChain`. Tier order: stored OAuth (keyring primary, `~/.codex/.auth.json` interop) → `OPENAI_API_KEY` env → `OPENAI_API_KEY` field embedded in `.auth.json` (codex's ApiKey mode). OAuth tiers gated under the existing `subscription-oauth` feature.
- Hybrid Codex login flow driven by a new `pattern auth login openai` CLI subcommand: PKCE loopback (port 1455, fallback 1457) when a browser is available, device-code fallback for headless / `--headless`. PKCE constants and endpoints mirror codex CLI byte-for-byte.
- Storage is **keyring-primary** (service `"Codex Auth"`, account key byte-identical to codex's derivation). `~/.codex/.auth.json` is read on load when present and written back atomically on every save **only if it already existed at load time** — Pattern never creates the file. Cross-process safety via advisory flock on a sidecar lock file.
- Refresh is reactive (on 401) + proactive (8s buffer before expiry, matching codex). Serialized by an in-process `tokio::sync::Mutex` AND a cross-process flock; refresh-token rotation supported (persist new refresh token atomically when server returns one).
- The OpenAI provider is wired into the gateway end-to-end. `chat_url_for` gains arms for `AdapterKind::OpenAI` → `https://api.openai.com/v1/chat/completions` and `AdapterKind::OpenAIResp` → `https://api.openai.com/v1/responses` (api-key tier) or `https://chatgpt.com/backend-api/codex/responses` (OAuth tier). `auth_headers_for_tier`'s OAuth-tier branch grows OpenAI-aware logic that adds `ChatGPT-Account-Id` (from `resolved.token.session_id`, sourced from id_token JWT claim) and `originator: pattern` headers when the adapter is OpenAI/OpenAIResp.
- Tier-vs-protocol pre-flight gate: when OAuth tier is active and `AdapterKind::from_model(model)` resolved to `AdapterKind::OpenAI` (Chat Completions), the gateway returns `ProviderError::TierMismatch { model, hint }` before any network call, with the hint pointing the user at codex-family model names or the `openai_resp::` namespace prefix. genai's existing `from_model` codex/gpt-5/chatgpt routing is the single source of truth for which protocol each model speaks — Pattern does not maintain a model catalog.

**Success criteria:**
- `pattern auth login openai` completes the OAuth dance end-to-end (loopback OR device-code) and stores credentials in the keyring (and `.auth.json` if it exists).
- `openai/*` requests through the gateway work for both auth tiers: api-key tier hits `api.openai.com` with the right path-per-adapter; OAuth tier hits `chatgpt.com/backend-api/codex/responses` with the required Bearer + ChatGPT-Account-Id + originator headers.
- Refresh happens automatically (proactive + reactive), is serialized in-process and cross-process, and persists rotated refresh tokens atomically.
- A codex-CLI-logged-in user can run Pattern with no extra setup (Pattern reads codex's existing storage on load).
- Picking a Chat-Completions-only model (e.g. `gpt-4o`) under OAuth tier returns `ProviderError::TierMismatch` with a usable hint before reaching the network.
- Test pyramid: unit (resolution, file IO, JWT decode, refresh state machine, header composition) + wiremock integration (token endpoints, request shaping, tier-vs-protocol gate) + snapshot (`.auth.json` schema, codex request body) + manual live-model validation mode for first capture.

**Out of scope:**
- TUI modal login (CLI subcommand only for this design).
- Cross-machine token sync.
- Fixing codex CLI's own concurrent-refresh race (we can't; Pattern is correct on its side via flock).
- Upstreaming a `RequestOverrideMerge` variant to genai — investigation showed pattern's gateway already uses full-replace `RequestOverride` and constructs the entire header set itself, so a merge variant adds nothing to this design. May be done independently as a courtesy upstream PR.

## Acceptance Criteria

### codex-oauth.AC1: OAuth login flow (PKCE loopback + device-code)
- **codex-oauth.AC1.1 Success:** PKCE loopback flow on a desktop with browser auto-open succeeds and yields `TokenData { access_token, refresh_token, id_token, account_id }`.
- **codex-oauth.AC1.2 Success:** Device-code flow with `--headless` prints user_code + verification URL, polls, succeeds when user completes verification.
- **codex-oauth.AC1.3 Success:** Loopback port 1455 in use → fallback to 1457 transparently.
- **codex-oauth.AC1.4 Success:** Both ports in use → auto-falls back to device-code flow without user re-prompt.
- **codex-oauth.AC1.5 Failure:** User denies authorization in browser → `OAuthDenied { reason }` returned with no token written.
- **codex-oauth.AC1.6 Failure:** PKCE state mismatch on callback → `StateInvalid` (cannot proceed; potential CSRF).
- **codex-oauth.AC1.7 Failure:** Token-exchange POST returns non-200 → `TokenExchangeFailed { status, body }`.
- **codex-oauth.AC1.8 Failure:** id_token signature invalid (JWKS verification fails) → `IdTokenInvalid { reason }`.
- **codex-oauth.AC1.9 Failure:** Device-code expires before user verifies (15 min) → `DeviceCodeExpired`.
- **codex-oauth.AC1.10 Edge:** id_token missing `chatgpt_account_id` claim → stored as `account_id: None`; runtime requests proceed without the header (server may reject; surface clear error).
- **codex-oauth.AC1.11 Edge:** Device-code server returns `slow_down` → polling interval increases per server hint.

### codex-oauth.AC2: Storage + codex interop
- **codex-oauth.AC2.1 Success:** Save then load returns identical `AuthDotJson` (round-trip).
- **codex-oauth.AC2.2 Success:** Property-test round-trip across arbitrary `AuthDotJson` instances.
- **codex-oauth.AC2.3 Success:** Snapshot test against captured-from-real-codex JSON pins schema byte-shape.
- **codex-oauth.AC2.4 Success:** Save with `file_existed == true` updates both keyring AND `.auth.json` atomically.
- **codex-oauth.AC2.5 Success:** Save with `file_existed == false` updates keyring only; `.auth.json` does not appear on disk.
- **codex-oauth.AC2.6 Success:** Keyring-only Pattern user and codex-CLI user with `.auth.json` both load successfully via the same `CodexAuthStore`.
- **codex-oauth.AC2.7 Failure:** Atomic-rename target read-only → `StorageError::WriteFailed` with original file untouched (no corruption).
- **codex-oauth.AC2.8 Failure:** Keyring backend unavailable → fall back to file backend if file exists; clean error if neither works.
- **codex-oauth.AC2.9 Edge:** Two concurrent `save()` calls in-process → flock serializes; final on-disk state matches the last writer.
- **codex-oauth.AC2.10 Edge:** codex CLI writes to `.auth.json` while Pattern holds the flock → codex's write blocks; no torn file.

### codex-oauth.AC3: Refresh manager + `OpenAiAuthChain`
- **codex-oauth.AC3.1 Success:** Stored OAuth tier resolves with valid non-expiring token → returns immediately, no refresh.
- **codex-oauth.AC3.2 Success:** Token expires within 8s buffer → proactive refresh triggers, fresh token persisted, returned.
- **codex-oauth.AC3.3 Success:** Server rotates refresh_token on refresh → new refresh_token persisted atomically.
- **codex-oauth.AC3.4 Success:** Server keeps refresh_token (no rotation) → existing refresh_token preserved.
- **codex-oauth.AC3.5 Success:** Two concurrent `resolve()` calls hitting refresh path → only one network call; second observes fresh token.
- **codex-oauth.AC3.6 Success:** Tier order respected: stored OAuth wins over `OPENAI_API_KEY` env, env wins over file-embedded `OPENAI_API_KEY`.
- **codex-oauth.AC3.7 Failure:** Refresh returns `refresh_token_expired` → `RefreshFailedReason::Expired` → `ProviderError::NoAuthAvailable`; user prompted to re-login.
- **codex-oauth.AC3.8 Failure:** Refresh returns `refresh_token_reused` → `RefreshFailedReason::Exhausted` with same surfacing.
- **codex-oauth.AC3.9 Failure:** Refresh returns `refresh_token_invalidated` → `RefreshFailedReason::Revoked`.
- **codex-oauth.AC3.10 Failure:** Refresh transient 5xx → retry-eligible classification.
- **codex-oauth.AC3.11 Edge:** Pattern + codex CLI race on refresh; Pattern holds flock first → codex's refresh is queued behind flock; only one network call wins.

### codex-oauth.AC4: Gateway integration (URLs, headers, tier gate)
- **codex-oauth.AC4.1 Success:** api-key tier + `gpt-4o` model → routed to `api.openai.com/v1/chat/completions` (OpenAI adapter).
- **codex-oauth.AC4.2 Success:** api-key tier + `gpt-5-codex` model → routed to `api.openai.com/v1/responses` (OpenAIResp adapter).
- **codex-oauth.AC4.3 Success:** OAuth tier + `gpt-5-codex` model → routed to `chatgpt.com/backend-api/codex/responses` with `Authorization: Bearer`, `chatgpt-account-id`, `originator: pattern` headers.
- **codex-oauth.AC4.4 Success:** OAuth tier + `openai_resp::gpt-4o` (namespace prefix forces OpenAIResp) → routed to chatgpt backend; works regardless of model name fallthrough.
- **codex-oauth.AC4.5 Failure:** OAuth tier + `gpt-4o` (no namespace, falls to OpenAI adapter) → `ProviderError::TierMismatch` BEFORE any network call, with hint mentioning `openai_resp::` prefix.
- **codex-oauth.AC4.6 Success:** 401 from chatgpt backend on a streaming pre-flight → force_refresh + single retry; if second attempt also 401, surface error.
- **codex-oauth.AC4.7 Success:** `pattern_server` builds with both providers registered; `NoOpShaper` registered for openai (verifiable via gateway introspection).
- **codex-oauth.AC4.8 Failure:** Anthropic shaper invoked on an openai request → structurally impossible; regression test asserts shaper dispatch by provider name.
- **codex-oauth.AC4.9 Edge:** `base_url_override` for `"openai"` provider points at wiremock → both api-key and OAuth-tier requests route through it correctly.

### codex-oauth.AC5: CLI subcommand
- **codex-oauth.AC5.1 Success:** `pattern auth login openai` interactive run completes loopback flow end-to-end (against wiremock).
- **codex-oauth.AC5.2 Success:** `pattern auth login openai --headless` runs device-code flow end-to-end.
- **codex-oauth.AC5.3 Success:** `pattern auth login openai --codex-home <path>` honours custom storage location.
- **codex-oauth.AC5.4 Success:** `pattern auth logout openai` clears keyring + `.auth.json` (if present) AND POSTs to revoke endpoint.
- **codex-oauth.AC5.5 Success:** `pattern-test-cli auth --provider openai` prints tier, token prefix, expiry, account_id-last-4.
- **codex-oauth.AC5.6 Failure:** Login command exits non-zero with clear miette diagnostic when any AC1 failure variant occurs.

### codex-oauth.AC6: Live-fixture validation + observability
- **codex-oauth.AC6.1 Success:** `pattern-test-cli openai-codex-smoke --capture` against live OpenAI emits a captured request/response fixture.
- **codex-oauth.AC6.2 Success:** Deterministic snapshot test built from the fixture runs in CI (no network) and passes.
- **codex-oauth.AC6.3 Success:** Gateway logs include `tier=stored_oauth model=openai/gpt-5-codex account_id_suffix=...` at `info` on resolve.
- **codex-oauth.AC6.4 Documentation:** `crates/pattern_provider/CLAUDE.md` gains a Codex OAuth section covering tier order, refresh semantics, file vs keyring storage, flock behaviour, and `originator` value.

## Glossary

- **Codex CLI**: OpenAI's official command-line coding assistant (`codex-rs`), which uses its own OAuth flow and stores credentials in `~/.codex/.auth.json`. Pattern aims for interop so a user already logged into Codex CLI needs no additional setup.
- **Responses API**: OpenAI's newer inference endpoint (`/v1/responses`), distinct from Chat Completions; required for Codex-family models under both api-key and OAuth tiers.
- **Chat Completions API**: OpenAI's original inference endpoint (`/v1/chat/completions`), used by models like `gpt-4o`; incompatible with OAuth tier in this design.
- **PKCE (Proof Key for Code Exchange)**: An OAuth 2.0 extension that prevents authorization code interception by binding a one-time verifier to the auth request; used here for the loopback browser-based login flow.
- **Device-code flow**: An OAuth 2.0 flow for headless environments where the user is shown a code and a URL to visit on another device; used as fallback when no browser is available.
- **JWKS (JSON Web Key Set)**: A published set of public keys used to verify JWT signatures; Pattern fetches this from `https://auth.openai.com/.well-known/jwks.json` to validate the `id_token` returned during login.
- **id_token**: A JWT returned alongside the access token during OAuth login; contains OpenID Connect claims, including the `chatgpt_account_id` claim under the `https://api.openai.com/auth` namespace.
- **`chatgpt-account-id` header**: A required HTTP header for requests to `chatgpt.com/backend-api/codex/responses`, sourced from the `chatgpt_account_id` claim in the id_token JWT.
- **`AdapterKind`**: A genai type (`AdapterKind::OpenAI`, `AdapterKind::OpenAIResp`, etc.) that classifies which wire protocol a model uses; genai's `from_model` is the single source of truth for this mapping.
- **`RequestOverride`**: A genai `AuthData` variant that instructs the genai HTTP client to use a fully caller-supplied URL and header set, bypassing genai's own auth logic; this is how Pattern's gateway exercises complete control over every request.
- **`NoOpShaper`**: A `pattern_provider` request shaper that passes messages through untouched, adding only a user-agent header; used for OpenAI to ensure the Anthropic-specific shaper (which injects subscription routing headers) cannot fire on OpenAI traffic.
- **`OpenAiAuthChain`**: The new credential chain struct (mirroring `AnthropicAuthChain`) that walks tiers — stored OAuth, `OPENAI_API_KEY` env, file-embedded api key — and returns a resolved credential with optional proactive refresh.
- **`CodexAuthStore`**: The new storage abstraction managing keyring + `~/.codex/.auth.json` with atomic-rename writes, `file_existed` provenance tracking, and advisory flock.
- **`flock` (advisory file lock)**: A POSIX mechanism (`fs2::FileExt`) for coordinating access across processes; used here on a sidecar `.lock` file to serialize concurrent `save()` calls between Pattern instances and Codex CLI.
- **`subscription-oauth`**: An existing Cargo feature flag in `pattern_provider` that gates OAuth credential tiers; the Codex OAuth tiers are gated behind this same flag.
- **`pattern_provider`**: The Pattern crate responsible for LLM provider integration — auth, request shaping, rate limiting, and the gateway that dispatches to genai.
- **`pattern_server`**: The Pattern daemon process that builds and owns the gateway, registering providers at startup.
- **`ProviderError::TierMismatch`**: A new error variant returned when the active credential tier is incompatible with the model's required protocol (e.g. OAuth tier with a Chat Completions model).
- **`originator` header**: A request header that identifies the calling tool to OpenAI's backend; Codex CLI sends `originator: codex_cli_rs`, Pattern will send `originator: pattern`.

## Architecture

Codex authentication targets `https://chatgpt.com/backend-api/codex/responses` — the same Responses-API wire shape as `api.openai.com/v1/responses`, but at a different host with a bearer token sourced from an OAuth flow and an extra required header (`ChatGPT-Account-Id`) sourced from an OpenID Connect id_token claim. Pattern integrates this with **zero new genai adapters and zero changes to genai**: the existing `openai_resp` adapter speaks the right protocol; `pattern_provider`'s gateway already drives URL + full header set per-request via `AuthData::RequestOverride`, so all Codex-specific awareness lives in `pattern_provider`.

**No model catalog.** genai's `AdapterKind::from_model` is the source of truth for which models speak Chat Completions (`OpenAI`) vs the Responses API (`OpenAIResp`). Codex-family models (gpt-5*, codex*, chatgpt*, gpt-*-codex*, gpt-*-pro*) already route to `OpenAIResp` upstream; the `openai_resp::` namespace prefix lets users force the protocol when needed. Pattern does not duplicate this mapping.

**Component layout (new modules in `pattern_provider/src/auth/`):**

- `codex_oauth.rs` — OAuth state machine. PKCE constants, scopes, endpoints, `LoginFlow::{Loopback, DeviceCode, Auto}`, `begin_login()`, `complete_login()`, JWT decoding for id_token claims (including `chatgpt_account_id`). Pure flow logic; no I/O beyond HTTP and the loopback listener.
- `codex_storage.rs` — typed `~/.codex/.auth.json` schema (`AuthDotJson`, `TokenData`, `AuthMode`), keyring backend at service `"Codex Auth"`, atomic-rename writes, `fs2::FileExt` advisory flock. `CodexAuthStore::load()` records whether the file existed; `save()` writes the file only if it did.
- `codex_refresh.rs` — refresh state machine. Mutex-serialized `refresh_with_lock()` that takes both an in-process `tokio::sync::Mutex` and a cross-process flock before issuing the token-exchange POST, then atomically persists rotated tokens. Classifies server errors into `RefreshFailedReason::{Expired, Exhausted, Revoked, Transient}`.
- Updates to `resolver.rs` — new `pub struct OpenAiAuthChain` mirroring `AnthropicAuthChain`'s shape, plus tier-forcing constructors (`api_key_only`, `oauth_only`) for tests.

**Gateway wiring (`pattern_provider/src/gateway.rs`):**

The gateway today only plumbs Anthropic and Gemini through `chat_url_for` and the per-adapter arms in `auth_headers_for_tier`. OpenAI's URL slot is a deliberate "fail-loud invalid URL" fallthrough. This design wires OpenAI properly:

- `chat_url_for` grows arms for `AdapterKind::OpenAI` → `{base}/v1/chat/completions` and `AdapterKind::OpenAIResp` → either `{base}/v1/responses` (api-key tier, base `api.openai.com`) or `{chatgpt_base}/backend-api/codex/responses` (OAuth tier, base `chatgpt.com`). The function signature gains a `tier: AuthTier` parameter (threaded from `resolved.source` in `complete()`); the existing Anthropic/Gemini arms ignore it.
- `auth_headers_for_tier` gets an OpenAI-specific branch in its OAuth-tier arm that, in addition to the existing `authorization: Bearer ...`, inserts `chatgpt-account-id: {token.session_id}` and `originator: pattern` when `adapter` is `OpenAI` or `OpenAIResp`. (`session_id` is the canonical home for the JWT-derived account_id on `ProviderCredential`.)
- Pre-flight tier-vs-protocol gate in `complete()` after `resolve()`: if `resolved.source.is_oauth() && adapter == AdapterKind::OpenAI`, return `ProviderError::TierMismatch { model, hint }` before constructing the `ServiceTarget`. The hint points the user at codex-family model names or the `openai_resp::` namespace prefix.
- Reactive refresh: on 401 from chatgpt backend, the gateway invokes `chain.resolve(force_refresh: true)` and retries the request once.

**Gateway construction (`pattern_server/src/main.rs`):**

Registers the OpenAI provider alongside Anthropic, **with `NoOpShaper` explicitly** (not the Anthropic shaper):

```rust
.with_provider("anthropic", anthropic_chain, anthropic_shaper, anthropic_limiter)
.with_provider("openai",    openai_chain,    Arc::new(NoOpShaper), openai_limiter)
```

Per-provider shaper dispatch is keyed on provider name (`gateway.rs:180`), so the Anthropic shaper is structurally unable to fire for OpenAI traffic. `NoOpShaper` (already in `pattern_provider/src/shaper/noop.rs`) leaves `system_blocks` untouched and emits only the user-agent identification header. New: `ProviderRateLimiter::openai_default()` constructor (mirrors `anthropic_default`).

**CLI:**

`pattern auth login openai` lives in `pattern_cli`. It constructs `OpenAiAuthChain::with_oauth(...)` against the user's `$CODEX_HOME` (default `~/.codex`), calls `begin_login(LoginFlow::Auto)`, opens the browser via the `open` crate (also prints the URL for fallback), awaits the loopback callback or polls the device-code endpoint, and persists via `CodexAuthStore::save`. A `--headless` flag forces `LoginFlow::DeviceCode`. The subcommand mirrors any existing Anthropic auth-login shape in pattern_cli.

**Data flow at request time:**

```
caller (runtime / pattern_cli / test harness)
  → PatternGatewayClient.complete(CompletionRequest { model: "gpt-5-codex", ... })
    → provider_for_model: AdapterKind::from_model("gpt-5-codex") = OpenAIResp, provider = "openai"
    → OpenAiAuthChain.resolve()
       ├─ proactive: check expiry vs 8s buffer, maybe refresh (mutex + flock)
       └─ returns ResolvedCredential { source: StoredOauth, token: ProviderCredential }
    → tier-vs-protocol gate: OAuth tier + OpenAIResp adapter → OK (pass)
                              OAuth tier + OpenAI adapter   → TierMismatch error here
    → shaper.shape(...): NoOpShaper returns identification headers only
    → auth_headers_for_tier(resolved, OpenAIResp) → authorization + chatgpt-account-id + originator
    → chat_url_for(OpenAIResp, model, base_override, StoredOauth) → chatgpt.com/backend-api/codex/responses
    → service_target → AuthData::RequestOverride { url, headers (full set) }
    → genai openai_resp adapter prepares request; genai webclient sees RequestOverride and uses our url+headers wholesale
    → POST chatgpt.com/backend-api/codex/responses
       └─ on 401: open_stream_with_retry classifies as 4xx-not-429 (current behaviour: not auto-retried). New helper in this design retries once after force_refresh.
```

## Existing Patterns

The design follows `AnthropicAuthChain` (`pattern_provider/src/auth/resolver.rs`) and the gateway's existing per-provider dispatch closely. Specifically:

- **Tier chain shape** — `OpenAiAuthChain` is a struct holding an `ApiKeyTier` and an optional `OAuthChainState` (feature-gated on `subscription-oauth`). `resolve()` walks tiers and returns `ResolvedCredential { source: AuthTier, token: ProviderCredential }`. Tier ordering is explicit-over-ambient (stored OAuth > env api-key > file-embedded api-key), matching the rationale documented in `crates/pattern_provider/CLAUDE.md` for the Anthropic chain.
- **Refresh mutex pattern** — single `tokio::sync::Mutex<()>` on the chain serializing the refresh path. First caller through the mutex does the network round trip and stores the new token; subsequent callers re-read the store and observe the fresh token. Cross-process flock is the new layer on top, specific to codex's multi-tool reality.
- **Feature gating** — `#[cfg(feature = "subscription-oauth")]` on OAuth tiers; chain collapses to api-key-only without the feature. Same flag Anthropic uses.
- **Storage abstraction** — keyring primary, file fallback, atomic-rename writes. Pattern's Anthropic flow uses `keyring` crate; we reuse it.
- **Tier-forcing helpers** — `api_key_only()`, `oauth_only()` constructors for tests, paralleling Anthropic's `pkce_only()` / `session_pickup_only()`.
- **Gateway full-replace `RequestOverride`** — `pattern_provider::gateway` already constructs the complete URL + complete header set for every request and hands them to genai via `AuthData::RequestOverride` (`gateway.rs:839`). Per-provider behaviour is keyed on `AdapterKind` in `chat_url_for` (URL choice) and on `(AuthTier, AdapterKind)` in `auth_headers_for_tier` (header composition). The Codex work **extends both functions** with OpenAI arms; it does not introduce a new auth mechanism in genai. This is why no `RequestOverrideMerge` patch is needed.
- **Per-provider shaper dispatch** — gateway looks up shaper by provider name at `gateway.rs:180`. Registering `"openai"` with `NoOpShaper` (already in `shaper/noop.rs`) at gateway-build time means the Anthropic shaper structurally cannot fire for OpenAI traffic. No conditional logic in the shapers themselves; the gating is in the builder configuration.
- **Observability** — `pattern-test-cli auth --provider openai` prints tier + token prefix + expiry, mirroring Anthropic's `auth` subcommand. Gateway logs tier at `info` level on resolve.

**Divergences from Anthropic, intentional:**

- No `SessionPickup` tier as a separate concept. For Anthropic, session-pickup reads `~/.claude/.credentials.json` read-only. For codex, the file is Pattern's own storage when present — there's no separate "borrow another tool's creds" semantic, just a storage location that Pattern co-owns with codex CLI.
- Cross-process flock — new requirement for codex because codex CLI may run concurrently with Pattern on the same machine. Anthropic doesn't have this concern (Claude Desktop / claude-code use OS-level secret storage with their own concurrency model).
- `ChatGPT-Account-Id` header sourced from a decoded JWT claim, populated into `ProviderCredential.session_id`. Anthropic doesn't need this — its bearer token is self-contained at the gateway.
- `chat_url_for` signature change — the function gains a `tier: AuthTier` parameter so the OpenAIResp arm can pick between `api.openai.com/v1/responses` (api-key) and `chatgpt.com/backend-api/codex/responses` (OAuth). Existing Anthropic and Gemini arms ignore the parameter; the change is additive but touches the call site in `service_target` (also covered by this design).

## Implementation Phases

<!-- START_PHASE_1 -->
### Phase 1: Codex OAuth state machine
**Goal:** Implement the PKCE-loopback and device-code flows + id_token JWT decoding. No storage yet; this phase produces a `TokenData` value from a completed login.

**Components:**
- `pattern_provider/src/auth/codex_oauth.rs` — constants (`CODEX_CLIENT_ID`, endpoint URLs, scopes), `LoginFlow` enum, `PkceMaterial` struct, `CodexLoginHandle` (URL + state for loopback; user_code + poll plan for device-code), `begin_login()`, `complete_login()`.
- PKCE generation matching codex: 64 random bytes → URL-safe base64 no-padding for verifier; `S256` SHA-256 challenge.
- Loopback listener on port 1455 (fallback 1457) serving `/auth/callback`, `/success`, `/cancel`. 5-minute callback timeout.
- Device-code: POST to `/api/accounts/deviceauth/usercode`, poll `/api/accounts/deviceauth/token` honoring server-provided `interval` and `slow_down` semantics. 15-minute hard timeout.
- JWT decoding via `jsonwebtoken` crate: fetch JWKS from `https://auth.openai.com/.well-known/jwks.json`, verify signature, extract claims under the `https://api.openai.com/auth` namespace (`chatgpt_account_id`, `chatgpt_plan_type`, `chatgpt_user_id`).
- `CodexOAuthError` enum (thiserror + miette diagnostics): `BrowserOpenFailed`, `LoopbackBindFailed`, `OAuthDenied`, `StateInvalid`, `TokenExchangeFailed`, `IdTokenInvalid`, `DeviceCodeExpired`, `DeviceCodeDenied`.

**Dependencies:** None.

**ACs covered:** `codex-oauth.AC1.*` (login flow success/failure/edge cases).

**Done when:** `cargo nextest run -p pattern_provider auth::codex_oauth` passes; tests cover PKCE generation determinism, JWT claim extraction, loopback URL construction, device-code polling loop (wiremock-scripted), error mapping for each error variant. JWKS fetch is mockable (configurable URL).
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Codex storage layer
**Goal:** Implement keyring + `.auth.json` storage with byte-for-byte codex compatibility, atomic writes, and cross-process locking.

**Components:**
- `pattern_provider/src/auth/codex_storage.rs` — `AuthDotJson`, `TokenData`, `AuthMode` types matching codex's schema (verified against fixtures from `~/Git_Repos/codex/codex-rs/login/src/auth/storage.rs` + `token_data.rs`). serde with `#[serde(rename_all = "snake_case")]`.
- `CodexAuthStore` with `load()` / `save()` / `forget()`. `load()` returns `(Option<AuthDotJson>, LoadProvenance { keyring_hit, file_existed })`. `save()` writes keyring always; writes `.auth.json` only if `file_existed == true`.
- Atomic write: `~/.codex/.auth.json.tmp.{pid}` → fsync → rename. 0o600 mode on Unix; `MOVEFILE_REPLACE_EXISTING` on Windows.
- Advisory flock via `fs2::FileExt` on `~/.codex/.auth.json.lock` (sidecar file, separate from data file). Lock spans the entire read-modify-write in `save()`.
- Keyring service `"Codex Auth"`, account key `cli|{sha256(canonical($CODEX_HOME))[0:16]}` matching codex's derivation in `login/src/auth/storage.rs:163-174`.

**Dependencies:** Phase 1 (uses `TokenData` shape).

**ACs covered:** `codex-oauth.AC2.*` (storage round-trip, atomic write, `file_existed` semantics, keyring parity with codex).

**Done when:** `cargo nextest run -p pattern_provider auth::codex_storage` passes. Property tests round-trip arbitrary `AuthDotJson` through serde. Snapshot test pins the JSON shape against a captured-from-real-codex fixture. Unit test verifies `save()` no-ops the file path when `file_existed == false`. Concurrent-write test verifies flock serializes two `save()` calls.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Refresh manager + `OpenAiAuthChain`
**Goal:** Wire OAuth tokens into Pattern's credential chain abstraction with serialized refresh and rotation support.

**Components:**
- `pattern_provider/src/auth/codex_refresh.rs` — `refresh_with_lock()` taking `&CodexAuthStore` and an `Arc<tokio::sync::Mutex<()>>`. Acquires file flock, then mutex, re-reads store under both locks (so the second concurrent caller observes the fresh token without redundant network), performs token-exchange POST if still expired, persists rotated tokens atomically, returns the fresh `TokenData`.
- `RefreshFailedReason::{Expired, Exhausted, Revoked, Transient}` classification from server error body (`refresh_token_expired` / `refresh_token_reused` / `refresh_token_invalidated`).
- Updates to `pattern_provider/src/auth/resolver.rs`:
  - `OpenAiAuthChain` struct mirroring `AnthropicAuthChain`. Holds `ApiKeyTier`, `Option<CodexOAuthState> { store, refresh_mutex }`. Constructors: `api_key_only()`, `with_oauth(store)`, `oauth_only(store)`.
  - `impl CredentialChain for OpenAiAuthChain` — `resolve()` walks tiers in order, on stored-OAuth tier checks expiry with 8s buffer and triggers `refresh_with_lock` if needed. Returns `ResolvedCredential` with `account_id` in `session_id`.
- Reactive refresh: gateway-side helper that, on a 401 from chatgpt backend, calls `chain.resolve_with(force_refresh: true)` and retries the request once. (Wired in Phase 4.)

**Dependencies:** Phase 1 (uses `TokenData`), Phase 2 (uses `CodexAuthStore`).

**ACs covered:** `codex-oauth.AC3.*` (tier order, proactive refresh, mutex/flock serialization, rotation, error classification).

**Done when:** `cargo nextest run -p pattern_provider auth::resolver::openai` passes. Tests cover: tier walking order (stored OAuth > env api-key > file api-key); proactive refresh on near-expiry; mutex serializes concurrent refresh in-process (only one network call); refresh-token rotation persists atomically; each `RefreshFailedReason` surfaces as the expected `ProviderError`. Cross-process flock test via spawning two processes through `pattern-test-cli`.
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Gateway integration — plumb OpenAI end-to-end
**Goal:** Wire the OpenAI provider into `PatternGatewayClient` end-to-end for both api-key and OAuth tiers, with the tier-vs-protocol gate, and ensure the daemon's gateway-builder registers OpenAI with `NoOpShaper`.

**Components:**
- `pattern_provider/src/gateway.rs`:
  - Extend `chat_url_for` signature with `tier: AuthTier`. Add arms for `AdapterKind::OpenAI` (→ `{base}/v1/chat/completions`) and `AdapterKind::OpenAIResp` (→ `{base}/v1/responses` for api-key, `{chatgpt_base}/backend-api/codex/responses` for OAuth tier). Default base for api-key tier: `https://api.openai.com`. Default base for OAuth tier: `https://chatgpt.com`. Both respect the existing `base_url_override` parameter for wiremock testing.
  - Extend `auth_headers_for_tier`'s OAuth-tier arm: when adapter is `OpenAI | OpenAIResp`, additionally insert `chatgpt-account-id: {resolved.token.session_id}` and `originator: pattern`. Document why these live here (header is auth-derived, sourced from id_token JWT claim) and not in the shaper.
  - In `complete()`, after `chain.resolve()` and before `service_target`, add the tier-vs-protocol pre-flight gate: if `resolved.source.is_oauth() && adapter == AdapterKind::OpenAI`, return `ProviderError::TierMismatch { model, hint }`.
  - Update `service_target` signature to take `tier: AuthTier` (or thread it through more narrowly via a new helper) so it can call the extended `chat_url_for`.
  - New `ProviderError::TierMismatch { model: String, hint: &'static str }` error variant in `pattern_core::error::ProviderError`.
  - Reactive-refresh helper: on 401 from the chatgpt backend during the streaming pre-flight (event-1 phase in `open_stream_with_retry`), call `chain.resolve_with(force_refresh: true)` and retry once before propagating the error.
- `pattern_provider/src/ratelimit.rs` — new `ProviderRateLimiter::openai_default()` constructor (mirrors `anthropic_default`).
- `pattern_server/src/main.rs:171-175` — register OpenAI provider in the gateway builder:
  ```rust
  .with_provider("openai", openai_chain, Arc::new(NoOpShaper), openai_limiter)
  ```
  Construct `openai_chain` from `OpenAiAuthChain::with_oauth` (Phase 3) or `api_key_only()` based on whether `subscription-oauth` is active and `$CODEX_HOME` resolves.
- Unit tests in `gateway.rs::tests` for: `auth_headers_for_tier` OpenAI api-key (Authorization only), OpenAI OAuth (Authorization + chatgpt-account-id + originator), and the tier-vs-protocol gate.
- Integration test in `crates/pattern_provider/tests/gateway_integration.rs` (or sibling file) exercising full pipeline: chain resolves OAuth → gateway routes to mocked chatgpt backend at the test's `base_url_override` → response decodes via `openai_resp`. Snapshot test pins request URL + headers + body shape against a captured-from-live fixture.

**Dependencies:** Phase 3 (`OpenAiAuthChain`, `CodexAuthStore`).

**ACs covered:** `codex-oauth.AC4.*` (URL routing per adapter/tier, header composition, tier-vs-protocol gate, reactive refresh, daemon registration).

**Done when:** `cargo nextest run -p pattern_provider` passes including new gateway tests. wiremock integration test passes for both api-key and OAuth tiers. `pattern_server` builds with OpenAI registered alongside Anthropic and the `NoOpShaper` slot.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: `pattern auth login openai` CLI subcommand
**Goal:** User-facing login command end-to-end.

**Components:**
- `pattern_cli/src/auth.rs` (or wherever existing Anthropic auth-login lives, located at implementation-plan time) — `pattern auth login openai [--headless] [--codex-home <path>]` subcommand. Constructs `OpenAiAuthChain::with_oauth(CodexAuthStore::from_codex_home(...))`, drives `begin_login` + `complete_login`, opens browser via `open` crate (printing the URL as fallback), awaits result, calls `store.save`.
- Terminal UX for device-code: highlight the user_code (terminal colour via Pattern's existing UI helpers), print verification URL + expiry, animate dots while polling.
- Logout subcommand: `pattern auth logout openai` calls `CodexAuthStore::forget` (clear keyring + file) and POSTs revoke to `https://auth.openai.com/oauth/revoke`.
- `pattern-test-cli auth --provider openai` parity with the Anthropic-side observability command: print tier, token prefix, expiry, `account_id` (last 4 chars).

**Dependencies:** Phase 3 (chain), Phase 2 (storage).

**ACs covered:** `codex-oauth.AC5.*` (CLI login UX, logout, observability subcommand).

**Done when:** Integration test in `pattern_cli` (or `pattern_runtime`'s `tests/`) drives the login subcommand end-to-end against a wiremock-scripted OAuth endpoint, asserts persistence in a temp `$CODEX_HOME`. Manual smoke test: real `pattern auth login openai` against live OpenAI yields a working token (verified via `pattern auth --provider openai`).
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Live-fixture validation + observability polish
**Goal:** Capture a real chatgpt-backend round trip into snapshot fixtures, gate the live-model harness behind an explicit flag, and finalize observability.

**Components:**
- Live-model harness mode in `pattern-test-cli` (per the temp-validation-mode pattern from `pattern-test-cli` cache tests): `pattern-test-cli openai-codex-smoke` sends a single-turn `ask` against the live chatgpt backend using the user's OAuth tier, captures the on-wire request + response, and writes them to `tests/fixtures/codex_smoke_{date}.json` if `--capture`. Default-off, requires `CODEX_LIVE_SMOKE=1` env.
- Convert the captured fixture into a deterministic snapshot test in Phase 5's wiremock test (pins request body + verifies response decoding round-trips).
- Gateway log: at resolve, log `tier=stored_oauth model=openai/gpt-5-codex account_id_suffix=…` at `info`.
- Documentation: append a `## Codex OAuth` section to `crates/pattern_provider/CLAUDE.md` documenting tier order, refresh semantics, file vs keyring storage, and the cross-process flock behaviour. Mirror the level of detail of the existing Anthropic section.

**Dependencies:** Phase 4 (gateway routing), Phase 5 (CLI for the smoke harness).

**ACs covered:** `codex-oauth.AC6.*` (live-smoke harness, captured-fixture snapshot, documentation).

**Done when:** `pattern-test-cli openai-codex-smoke --capture` succeeds against live OpenAI (user runs this manually with their own subscription) and emits a snapshot. The snapshot test (deterministic, in-CI) passes. Documentation merged.
<!-- END_PHASE_6 -->

## Execution Mode Recommendation

**Recommend: Collaborative.**

Six phases, mostly self-contained but several with judgment-call density that benefits from your eyes:

- Phase 1 (OAuth state machine) is mostly mechanical *if* the codex source is treated as ground truth, but the JWKS fetch + JWT decode has security implications worth pairing on.
- Phase 2 (storage interop) is where the highest-stakes bug-class lives: a corrupted `.auth.json` clobbers codex CLI auth. Atomic-rename + flock + `file_existed` flag interaction is exactly the kind of thing where an autonomous implementor could land something "passes tests but races in the wild." Wants a careful human at each commit.
- Phase 3 (refresh + chain) has the same flavour — concurrency code that's easy to write wrong.
- Phase 4 (gateway integration) touches a production-critical file (`gateway.rs`) that's the single chokepoint for every provider request. The `chat_url_for` signature change cascades; the tier-vs-protocol gate is logic that has to be exactly right or users get cryptic errors. Manual review of every diff.
- Phases 5–6 (CLI + live fixtures) are more mechanical and could be sped up with subagent delegation under your supervision, but the live-fixture capture in Phase 6 needs your hands on a keyboard with a real ChatGPT subscription.

If you want to override, **Light** would be reasonable only for an isolated cleanup pass after the main work lands. Autonomous is not recommended for this plan — too many concurrency invariants and external-system integration points.

## Additional Considerations

**Codex CLI concurrent-refresh race.** Codex uses no cross-process lock for refresh. If both Pattern and codex CLI try to refresh the same token simultaneously, the OpenAI server returns `refresh_token_reused` to the loser (refresh-token rotation invalidates the previous token on first use). Pattern's flock prevents this on Pattern's side and across Pattern instances; it doesn't fix codex's bug, but Pattern is correct on its own. Document this in the operator docs.

**JWKS caching.** id_token signature verification requires fetching JWKS from `https://auth.openai.com/.well-known/jwks.json`. Cache the keys per `kid` for the OpenID-recommended TTL (1 hour minimum); fetch on `kid` miss. Reuse Pattern's `reqwest` client.

**`OPENAI_API_KEY` precedence subtlety.** Codex's `.auth.json` can hold an api key in its `OPENAI_API_KEY` field (codex's "ApiKey mode" — distinct from Pattern's `OPENAI_API_KEY` env var). Pattern's chain tries the env var BEFORE the file-embedded one (mirror of Anthropic's explicit-over-ambient logic). Document this precedence in the CLAUDE.md update.

**`originator` header.** Codex uses `originator: codex_cli_rs`. Pattern uses `originator: pattern` (or a more specific UA — decide at implementation-plan time). OpenAI does not currently differentiate behaviour based on this header, but using a Pattern-specific value is the honest pattern-identification practice (consistent with Pattern's Anthropic shaper philosophy).

**Shaper routing safety.** The Anthropic shaper injects subscription-routing-specific content (slot[0] claude-code literal, `oauth-2025-04-20` beta header) that would be actively wrong for any non-Anthropic call. The gateway already keys shaper dispatch on provider name, so the operative safety net is the daemon's `with_provider("openai", ..., Arc::new(NoOpShaper), ...)` registration in `pattern_server/src/main.rs`. Phase 4 covers this; future provider additions should follow the same pattern and a regression test in the gateway-builder layer would catch accidental misregistration.

**genai `RequestOverrideMerge` as a courtesy upstream PR.** The merge-headers variant of `AuthData::RequestOverride` would be a quality-of-life improvement for genai's own consumers but is unnecessary for Pattern (the gateway builds full headers itself). If anyone wants to do this work, it's a clean 25-line standalone PR — but it should not gate this design plan.

**Future TUI login flow.** Out of scope for this design but architecturally trivial: `CodexLoginHandle` is consumable from a ratatui modal in `pattern_cli`'s TUI just as easily as from a CLI subcommand. The state machine is UI-agnostic.

**Live-model test scope.** The Phase 6 live smoke is **manually triggered**, not in CI. CI gets the deterministic snapshot derived from the captured fixture. This matches Pattern's overall posture: live-model is a last resort, used to capture ground truth and then frozen.
