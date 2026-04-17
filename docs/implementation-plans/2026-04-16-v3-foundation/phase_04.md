# Pattern v3 Foundation — Phase 4: pattern_provider (rebased rust-genai + multi-provider gateway)

> ## Revision log (2026-04-17)
>
> **Scope change: multi-provider gateway instead of Anthropic-only client.**
>
> Original plan targeted a single `AnthropicProviderClient`. Revised plan
> generalises this to `PatternGatewayClient` — one `ProviderClient` impl that
> dispatches per-call to any provider `rust-genai` supports, with pattern-side
> state (credential tiers, request shaper, rate limiter) keyed by
> `genai::adapter::AdapterKind`. Per-call `model` override drives provider
> selection; one client instance can hit Anthropic + Gemini + OpenAI (+ others)
> based solely on the model string in the request.
>
> **What changes:**
> - Task 6 creds store → keyed by provider; Anthropic gets full three-tier
>   resolution, Gemini gets API-key-only (second provider for abstraction
>   validation), OpenAI stub entry point kept for future wiring.
> - Task 10 three-tier resolver → generalises to a `CredentialTier` trait with
>   per-provider tier chains. Anthropic chain: session-pickup → PKCE → API key.
>   Gemini chain: API key only.
> - Task 12 RequestShaper → `RequestShaper` trait object, dispatched per
>   `AdapterKind`. `HonestPatternShaper` (Anthropic), `NoOpShaper` (default for
>   Gemini/OpenAI). Per-provider shaper-mapping lives in `PatternGatewayClient`;
>   future rule-set (model-group, cost-tier) extensions slot in as a second
>   dispatch layer without reworking the trait.
> - Task 14 rate limiter → map keyed by `AdapterKind`; shared bucket per
>   adapter kind (i.e. all Anthropic models share one bucket, all Gemini
>   models share one, etc.), not per-model.
> - Task 18 `AnthropicProviderClient` → `PatternGatewayClient` (generic,
>   dispatches per-call based on model → adapter inference).
> - Task 19 wiremock integration → add at least one Gemini-path end-to-end
>   test alongside the Anthropic tests to lock the per-provider abstraction.
> - Second provider scope (this phase): **Gemini**. OpenAI wiring deferred
>   to a follow-up task once Gemini proves the abstraction.
>
> **What does NOT change:**
> - Anthropic auth/shaper/beta-header details (Tasks 8, 9, 11, 12 [Anthropic
>   parts], 20).
> - Phase 5 dependency boundary — gateway still stands alone; runtime wiring
>   is Phase 5.
> - Router-level rule engine (model-group-based routing, cost-aware selection,
>   fallback chains) remains out of scope; future phase.
>
> **Why the change:** the user flagged multi-provider dispatch as a near-term
> need rather than a future-phase concern. `rust-genai` already does the
> adapter-kind inference natively on model strings and its `AuthResolver`
> closure dispatches per `ModelIden`, so the generalisation cost is modest
> (~additional abstraction design + second provider wiring). Avoiding a
> later rewrite outweighs the cost.
>
> Acceptance-criteria coverage unchanged; the ACs were already phrased in
> terms of `ProviderClient`, not an Anthropic-specific name.

**Goal:** Stand up `pattern_provider` as the multi-provider LLM gateway. Rebase the `rust-genai` fork onto current upstream, shedding obsolete thinking patches (upstream subsumes them) and keeping only the minimum pattern-specific patches. Implement multi-provider credential storage (Anthropic three-tier: session-pickup → PKCE → API key; Gemini API-key-only as the abstraction-validation partner), a keyring-backed credential store with JSON fallback, a per-`AdapterKind` request shaper dispatch (honest-pattern shaper for Anthropic with a discrete escalation ladder for subscription-tier compatibility; no-op shaper default), per-provider rate limiting with separate buckets for chat completions vs token counting, per-persona session UUID rotation, and an external async `count_tokens` wrapper. Retire `pattern_auth` as its Anthropic responsibilities land here.

**Architecture:**
- `pattern_provider` depends on rebased `rust-genai` (v0.6.0-beta.17 base) via path dep, consuming its adaptive-thinking + CacheControl APIs directly rather than maintaining fork-side deviations.
- Fork-side patches are minimal: `SystemBlock` / `ChatRequest::system_blocks` for per-block `cache_control` (required for Phase 5's three-segment cache layout), `ANTHROPIC_VERSION` pin-check, and `claude-opus-4-7` additions to the reasoning-support arrays (upstream still lists only 4-6 + regex-dispatched XHigh).
- `PatternGatewayClient` holds one `genai::Client` plus per-`AdapterKind` pattern-side state: credential-tier chain, request shaper, rate limiter. Per-call model string drives adapter inference inside genai; pattern-side dispatch uses the same `AdapterKind` key.
- Anthropic credential chain: session-pickup reads `~/.claude/.credentials.json` (canonical claude-code path per research, not the stale `session.json`); PKCE uses pattern's verified-working OAuth config; API-key falls back to `ANTHROPIC_API_KEY` env.
- Gemini credential chain (abstraction-validation partner): API-key only from `GOOGLE_API_KEY` / `GEMINI_API_KEY` env or config.
- `RequestShaper` is a trait dispatched per-provider. `HonestPatternShaper` (Anthropic) injects honest pattern identification with a `ShaperCompatMode` enum for subscription-tier compatibility: default `SubscriptionRoutingShape` (system prompt array with claude-code-literal `system[0]` as structural requirement + honest pattern content in `system[1]`/`[2]`), aspirational `HonestPattern` (flip default to this if Phase 4 verification proves it works), future-gated `FullSurfaceImpersonation` (not implemented; requires explicit sign-off). `NoOpShaper` default for non-Anthropic providers.
- Beta headers curated per Anthropic's 2026-04-16 list, opt-in via `ShaperConfig`, excluding `claude-code-20250219` and other claude-code-specific markers.
- Rate limiting via `governor` with separate token buckets per endpoint (chat completions + count-tokens per AC5b.5). Buckets keyed by `AdapterKind`, shared across all models within a provider.
- `ProviderClient` trait impl (`PatternGatewayClient`) wires resolver + shaper + rate limiter + token-count wrapper + rebased genai into the shape Phase 2 defined, dispatching per-call based on the request's model string.

**Tech Stack:** Rust 2024, `rust-genai` path dep to the rebased fork at `~/Projects/PatternProject/rust-genai`, `keyring` (with Linux backend features + JSON fallback), `oauth2` crate considered but deferred — pattern extends the existing hand-rolled PKCE which works post-fix, `governor` GCRA rate limiting, `reqwest` for raw `count_tokens` calls, `wiremock` for integration tests, `secrecy` for token redaction in logs.

**Scope:** Phase 4 of 6. Covers v3-foundation.AC3.*, AC4.*, AC5.*, AC5b.*.

**Codebase verified:** 2026-04-16 (plus empirical auth-flow verification during planning — the OAuth scope + redirect + CLI-display fixes landed live on the `rewrite-v3` bookmark and round-trip a real subscription token successfully).

---

## Acceptance Criteria Coverage

This phase implements and tests the following ACs in full:

### v3-foundation.AC3: Subscription session-pickup authentication

- **v3-foundation.AC3.1 Success:** With a valid unexpired `~/.claude/session.json`, provider makes an authenticated request to Anthropic and returns a real response
- **v3-foundation.AC3.2 Success:** Session-pickup reads the file atomically; concurrent write from claude-code does not produce a torn read
- **v3-foundation.AC3.3 Failure:** Missing `~/.claude/session.json` → resolver skips tier without error, falls through to PKCE
- **v3-foundation.AC3.4 Failure:** Malformed JSON in session file → warning logged, tier skipped, falls through to PKCE
- **v3-foundation.AC3.5 Failure:** Expired token in session file → tier skipped, falls through to PKCE
- **v3-foundation.AC3.6 Edge:** Linux host with no pattern keyring entry but a valid claude-code session file → session-pickup succeeds (keyring absence never short-circuits session-pickup)

> **Implementation note:** Design text says `~/.claude/session.json`. External research + claude-code source confirm the canonical path is `~/.claude/.credentials.json`. Session-pickup reads the current path as primary; `session.json` is kept as a compat fallback for any legacy installs. Session-pickup **never writes** either file — pattern is a reader only.

### v3-foundation.AC4: PKCE and API-key fallback authentication

- **v3-foundation.AC4.1 Success:** Neither session nor API key present → PKCE opens localhost callback, user completes flow, token stored in keyring, subsequent request succeeds
- **v3-foundation.AC4.2 Success:** Token within 5-min of expiry → auto-refresh before request; new token stored; request succeeds with refreshed token
- **v3-foundation.AC4.3 Success:** `ANTHROPIC_API_KEY` set → provider uses it, request succeeds
- **v3-foundation.AC4.4 Failure:** PKCE callback timeout → `ProviderError::AuthFlowTimeout` surfaced; no silent proceed
- **v3-foundation.AC4.5 Failure:** Refresh-token endpoint returns error → `ProviderError::RefreshFailed`; no silent degradation
- **v3-foundation.AC4.6 Failure:** Keyring unavailable AND JSON fallback file unreadable → explicit `ProviderError::CredentialStoreUnavailable`
- **v3-foundation.AC4.7 Edge:** Concurrent refresh attempts for the same persona are serialized by mutex; only one network call made

> **Implementation note on AC4.1:** Design says "PKCE opens localhost callback." Pattern's current code uses the manual-paste variant (`platform.claude.com/oauth/code/callback`) and empirical testing during planning confirmed it works. Phase 4 keeps manual-paste as the default (no ephemeral port listener needed, simpler security model), and declares a future `auth/loopback.rs` module backed by `jacquard-oauth`'s `loopback` feature for the automatic-flow polish pass. AC4.1's "localhost callback" text is interpreted as "OAuth flow via the PKCE tier completes end-to-end" — manual-paste satisfies that.

### v3-foundation.AC5: Request shaping and rate limiting

- **v3-foundation.AC5.1 Success:** Outbound request includes honest pattern identification headers (implementation chooses specific header names/values; must identify as pattern rather than impersonate claude-code) plus per-persona session-UUID header
- **v3-foundation.AC5.2 Success:** System-prompt prefix block contains pattern-specific persona content (not `You are Claude Code`), positioned in the same structural slot
- **v3-foundation.AC5.3 Success:** Session-UUID rotates when caller signals rotation boundary
- **v3-foundation.AC5.4 Success:** Rate-bucket exhaustion queues request with jitter; request eventually succeeds after bucket refills
- **v3-foundation.AC5.5 Failure:** Misconfigured shaper (missing required identification headers) → error at provider construction time, not at request time
- **v3-foundation.AC5.6 Edge:** Multiple providers maintain independent buckets — Anthropic exhaustion does not affect other (future) providers
- **v3-foundation.AC5.7 Edge:** Tokens-per-day bucket tracked independently from tokens-per-minute; day bucket stays depleted even while minute bucket refills

> **Implementation note on AC5.2:** Design expects "pattern-specific persona content, not `You are Claude Code`, positioned in the same structural slot." Empirical testing during planning revealed the system-prompt-array **shape** is load-bearing for subscription-tier routing on Anthropic's side, and the literal `"You are Claude Code, Anthropic's official CLI for Claude."` string appears to be part of what Anthropic validates for subscription-OAuth traffic. Phase 4 therefore ships two shaper modes:
> - `HonestPattern`: system[0] = honest pattern identification only. **Verified by a task in Phase 4**: if Anthropic accepts it for subscription-tier requests, this becomes the default.
> - `SubscriptionRoutingShape` (provisional default until verification lands): system[0] = verbatim claude-code string (structural requirement, not identity claim), system[1] = `"You are NOT Claude Code. <DEFAULT_BASE_INSTRUCTIONS>"`, system[2] = persona + long-lived content.
>
> AC5.2's intent — pattern's persona content is what actually drives agent behaviour, not claude-code impersonation — is satisfied by both modes. The shaper's mode is documented in `pattern_provider/CLAUDE.md` with explicit framing: claude-code string in slot [0] is a structural Anthropic-side requirement, not an identity claim. Pattern's real identity lives in slots [1] and [2].

### v3-foundation.AC5b: Provider-reported token counting

- **v3-foundation.AC5b.1 Success:** `ProviderClient::count_tokens` returns Anthropic-reported counts for a composed request, matching (within provider precision) the count Anthropic would charge for the same request
- **v3-foundation.AC5b.2 Success:** Post-response `usage` field is captured and exposed to callers; subsequent compaction decisions can use these counts directly
- **v3-foundation.AC5b.3 Success:** Call sites that previously used heuristic token approximation (compaction thresholds, context-length checks) now consume provider-reported counts via the async path
- **v3-foundation.AC5b.4 Failure:** Provider token-counting endpoint failure surfaces as explicit `ProviderError::TokenCountFailed`; callers can fall back to heuristic if and only if they explicitly opt in (no silent fallback by default)
- **v3-foundation.AC5b.5 Edge:** Token-count requests are rate-limited independently from chat-completion buckets (Anthropic counts them separately); exhaustion of count-bucket does not block completion requests

> **Implementation note on AC5b.3:** The compaction call sites live in `rewrite-staging/context/compression.rs` after Phase 2. Migrating them to consume `ProviderClient::count_tokens` happens in **Phase 5** as part of the memory-layer reshape. Phase 4 lands the `count_tokens` API and the Phase 5 plan consumes it. AC5b.3 is "covered" by Phase 4 in the sense that the API exists for Phase 5 to consume; the call-site migration commits to trunk in Phase 5's Subcomponent.

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**Working bookmark:** `rewrite-v3`
**Pre-phase state after Phase 3:** `pattern_core` is traits + types + errors + preserved memory storage. `pattern_runtime` has Tidepool FFI + 11-handler bundle + session lifecycle (time/log/display fully implemented, memory/message/shell/file/sources/mcp/ipc/spawn stubbed). `pattern_provider` is an empty skeleton from Phase 1.

**Rust-genai fork:** `/home/orual/Projects/PatternProject/rust-genai`, currently at `0.4.0-alpha.8-WIP` with 5 divergent commits (gemini fix, three extended-thinking commits, one OAuth workaround). Phase 4 rebases onto upstream main (or v0.6.0-beta.17 tag — see Task 1 decision) and prunes.

**OAuth config currently landed on rewrite-v3 bookmark** (verified working by empirical test during planning):
- `client_id`: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
- `auth_endpoint`: `https://claude.ai/oauth/authorize`
- `token_endpoint`: `https://console.anthropic.com/v1/oauth/token` **← Phase 4 Task confirms or corrects**; empirical test succeeded with this value, but cliproxy uses `api.anthropic.com/v1/oauth/token`, and `platform.claude.com/v1/oauth/token` is also documented. Phase 4 locks the correct one.
- `redirect_uri`: `https://platform.claude.com/oauth/code/callback` (manual paste, verified)
- Scopes: `["user:profile", "user:inference", "user:sessions:claude_code", "user:mcp_servers", "user:file_upload"]`

**Design reference:** `docs/design-plans/2026-04-16-v3-foundation.md` Phase 4 section. Reference docs: `docs/reference/oauth-and-detection.md` (PKCE flow + detection analysis), `docs/reference/tidepool.md` (indirectly relevant for provider's ProviderClient trait satisfaction).

**Beta header registry (final, after research):**

| Header | Always? | Notes |
|---|---|---|
| `oauth-2025-04-20` | ✓ on OAuth tier | Required for subscription-OAuth routing |
| `prompt-caching-scope-2026-01-05` | ✓ on 1P | Claude-code always sends; no-op without scope field but Anthropic expects it |
| `interleaved-thinking-2025-05-14` | opt-in | Model-capability-gated (claude-4+); only sent when reasoning is enabled. Reasoning is OFF by default (see "Default reasoning posture" below). |
| `dev-full-thinking-2025-05-14` | opt-in | Exposes full unsummarized thinking content; off by default (separate from interleaved; for debug/introspection) |
| `context-management-2025-06-27` | opt-in | Default on for claude-4+; enables API-side context management features |
| `extended-cache-ttl-2025-04-11` | opt-in | Required to request 1h cache TTL; off by default, enabled by Phase 5 composer when requesting 1h on segment 1 |
| `context-1m-2025-08-07` | opt-in | Only when 1M-context model is selected + user opts in |
| `claude-code-20250219` | **NEVER** | Claude-code-specific identifier; pattern must not send |
| `cli-internal-2026-02-09` | **NEVER** | Anthropic-internal claude-code-only |
| `summarize-connector-text-2026-03-13` | **NEVER** | Ant-only experimental |
| `token-efficient-tools-2026-03-28` | **NEVER** | Ant-only experimental |

**Build tools:**
- `cargo check -p pattern_provider`
- `cargo nextest run -p pattern_provider`
- `cargo test --doc -p pattern_provider`
- `cargo clippy -p pattern_provider --all-features --all-targets -- -D warnings`
- `just pre-commit-all`

**Commit convention:** `[pattern-provider] …` for crate work. `[meta]` for workspace / retirement commits. `[rust-genai]` for fork rebase commits (commits land in the fork repo, not pattern's repo, but pattern's phase-close commit references the fork's resulting commit hash).

**Audit script (from Phase 2):** `scripts/audit-rewrite-state.sh` — run at phase close; must pass.

**Default reasoning posture:**

Pattern's `ShaperConfig` defaults to reasoning-off. Callers opt into reasoning per-request via `CompletionRequest::with_reasoning(ReasoningEffort::…)`. When enabled, `Low` or `Medium` are the expected common values; `High` for genuinely hard tasks; `Max` / `Budget(n)` only when the caller is deliberately choosing to pay the latency + cost of maximal thinking.

Rationale:
- Pattern runs many short turns; paying Max reasoning latency on every turn is the wrong default.
- Reasoning tokens cost against the subscription quota; reserving them for intentional invocation preserves budget.
- Thinking-off has no overhead and is the safest baseline.

Implementation: `ShaperConfig::default_reasoning_effort: Option<ReasoningEffort>` defaulting to `None`; per-request override via `CompletionRequest::reasoning_effort`. When `None`, the `thinking` field in the outbound request is omitted entirely (upstream genai supports this). When `Some(_)`, `interleaved-thinking-2025-05-14` beta header is added; if caller also sets `enable_dev_full_thinking`, that header is also added.

**Rust-coding-style reminders:**
- All errors in this phase live in `pattern_core::error::ProviderError` (Phase 2 defined the hierarchy). New variants added here must be `#[non_exhaustive]`-compliant.
- Newtype IDs for session tracking (`PatternSessionUuid`, `ProviderRequestId`).
- Secrets use `secrecy::Secret<String>` — never log or Debug a raw token.
- `module.rs + module/submodule.rs` layout; avoid `mod.rs`.

---

<!-- START_SUBCOMPONENT_A (tasks 1-4) -->
<!-- START_TASK_1 -->
### Task 1: Verify rust-genai rebase target + decide upstream rev

**Verifies:** contributes to AC5.1, AC5.2, AC5b.1 (all depend on upstream API surface).

**Files:** None in pattern repo. Work happens in `/home/orual/Projects/PatternProject/rust-genai`.

**Step 1: Confirm upstream reachability and tag state**

```bash
cd /home/orual/Projects/PatternProject/rust-genai
git fetch upstream
git log --oneline upstream/main -20
git tag -l 'v0.4*' 'v0.5*' 'v0.6*' | sort -V
```

Expected: `v0.4.0`, `v0.4.0-alpha.10`, possibly higher tags exist. Main tip advances beyond `v0.6.0-beta.17` (the rev our earlier research pinned).

**Step 2: Decide rebase target**

Rebase onto whichever is more current and stable:
- If `v0.6.0-beta.17` (or later stable tag) exists and our required features (adaptive thinking, CacheControl TTL variants, extra_headers, AuthData::RequestOverride, full usage capture, streaming chunk events) are all present → rebase onto that tag.
- If upstream main has significant unreleased improvements → rebase onto main tip and pin by commit hash in pattern's Cargo.toml.

Document the chosen target (tag or commit hash) in the fork's `CHANGELOG.md` entry for this rebase.

**Step 3: Verify required upstream APIs exist**

```bash
cd /home/orual/Projects/PatternProject/rust-genai
# adaptive thinking
git grep -nE 'SUPPORT_ADAPTTIVE_THINK|adaptive' -- src/adapter/adapters/anthropic/
# CacheControl TTL variants
git grep -nE 'Ephemeral(5m|1h|24h)' -- src/chat/
# extra_headers
git grep -n 'extra_headers' -- src/chat/
# AuthData::RequestOverride
git grep -n 'RequestOverride' -- src/resolver/
# ChatStreamEvent variants
git grep -n 'ReasoningChunk\|ToolCallChunk' -- src/chat/
# full usage fields
git grep -n 'cache_creation_tokens\|cached_tokens\|reasoning_tokens' -- src/chat/
```

Every query returns hits → upstream has what Phase 4 needs. Any miss → document which upstream PR would need landing before rebase is viable, and decide to (a) wait for upstream, (b) keep fork's current patch for that feature, or (c) patch it forward in the fork.

**Step 4: Survey upstream model registry for Opus/Sonnet 4.7**

```bash
git grep -n 'claude-opus-4\|claude-sonnet-4\|claude-haiku-4' -- src/adapter/adapters/anthropic/
```

If `claude-opus-4-7` or `claude-sonnet-4-7` appear → no patch needed. If upstream still lists only `-6` → pattern's fork adds a one-line patch adding the 4-7 variants to the model match list (documented in Task 4).

**Commit:** No commit yet — this task produces a decision documented in a notes file.

**Deliverable:** `/home/orual/Projects/PatternProject/rust-genai/REBASE_NOTES_v3_foundation.md` (in fork repo) capturing the chosen target, the upstream APIs verified present, and which patches are kept / dropped / added.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Execute rust-genai rebase

**Verifies:** contributes to AC5.1, AC5.2, AC5b.

**Files:** Fork repo only. No pattern repo changes.

**Step 1: Create rebase branch**

```bash
cd /home/orual/Projects/PatternProject/rust-genai
git checkout -b rebase/pattern-v3-foundation
```

**Step 2: Identify patches to keep vs drop**

Based on Task 1's notes:
- **Drop** (upstream subsumes): commits `e0e16db`, `9e5c1d7`, `7cc71e3` (Anthropic extended thinking — upstream has adaptive thinking)
- **Drop** (unrelated): `db3dd51` (gemini failure reduction) — re-check if still needed against upstream; if yes, can cherry-pick later; if no, drop
- **Drop** (obsolete): `18225ac` (Anthropic OAuth workaround with `You are Claude Code` / `You are NOT Claude Code` override hack) — upstream's `AuthData::RequestOverride` lets pattern_provider pass Bearer auth as headers without fork-side Bearer detection. Pattern-side shaper handles the system-prompt array shape per Phase 4 Task 12.

**Step 3: Reset fork to upstream target**

```bash
git reset --hard <upstream-tag-or-commit-from-task-1>
```

Fork history is now clean on the new base.

**Step 4: Apply minimal kept patches** (see Tasks 3 and 4 for specifics)

Task 3 lands the system-prompt-as-array-with-cache-control patch. Task 4 lands the `ANTHROPIC_VERSION` bump and 4.7 model-ID patch (if needed).

**Step 5: Update `Cargo.toml` version**

Bump fork's workspace version to a new pattern-v3-foundation designation, e.g., `0.6.0-beta.17+pattern.1`, so pattern's Cargo.lock captures the delta explicitly.

**Step 6: Run fork's own test suite**

```bash
cargo nextest run --all-features
```

Tests must pass. If any fail due to API changes we introduced, fix them in this task — don't let the fork ship with broken tests.

**Commit (in fork, not pattern):**

```bash
git commit -am "rebase: reset onto <upstream-target>; drop obsolete patches

Dropped (upstream subsumes):
- e0e16db Anthropic extended thinking support (→ upstream adaptive thinking)
- 7cc71e3 reasoning_budget parameter (→ upstream ReasoningEffort::Max)
- 9e5c1d7 extended-thinking bug fix (→ upstream handles natively)

Dropped (obsolete):
- 18225ac Bearer-detection OAuth workaround (→ upstream AuthData::RequestOverride;
  pattern-side shaper handles subscription system-prompt shape)
- db3dd51 gemini failure reduction (unrelated to pattern's Anthropic path)

Kept patches (landed in follow-up commits):
- System prompt as array of blocks with per-block cache_control (Phase 5 requirement)
- ANTHROPIC_VERSION bump to <current-value>
- [conditional] claude-opus-4-7 / claude-sonnet-4-7 model ID additions"
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: System-prompt-array fork patch

**Verifies:** AC5.2 structural requirement; enables Phase 5's three-segment cache layout.

**Files:** Fork repo only.
- Modify: `src/chat/chat_request.rs` — change `pub system: Option<String>` to support either a String or an array of CachedTextBlock
- Modify: `src/adapter/adapters/anthropic/adapter_impl.rs` — serialize the array form correctly when present, including per-block `cache_control`

**Step 1: Design the API**

Option A (backward compat): keep `system: Option<String>` and add `system_blocks: Option<Vec<SystemBlock>>`. When both present, blocks wins.

Option B (clean break): replace `system` with `SystemPrompt` enum:

```rust
pub enum SystemPrompt {
    Single(String),
    Blocks(Vec<SystemBlock>),
}

pub struct SystemBlock {
    pub text: String,
    pub cache_control: Option<CacheControl>,
}
```

Recommend **Option B** — pattern is the primary consumer, clean-break doesn't hurt existing users since the fork is pattern-specific. Upstream consumers can continue using the single-string form via `SystemPrompt::Single`.

**Step 2: Implement**

```rust
// src/chat/chat_request.rs
impl ChatRequest {
    pub fn with_system(mut self, system: impl Into<SystemPrompt>) -> Self {
        self.system = Some(system.into());
        self
    }
}

impl From<&str> for SystemPrompt {
    fn from(s: &str) -> Self { Self::Single(s.to_string()) }
}

impl From<String> for SystemPrompt {
    fn from(s: String) -> Self { Self::Single(s) }
}

impl From<Vec<SystemBlock>> for SystemPrompt {
    fn from(blocks: Vec<SystemBlock>) -> Self { Self::Blocks(blocks) }
}
```

Adapter changes: when serializing to Anthropic's API format, `Single(s)` emits the existing single-string shape; `Blocks(vec)` emits the array shape, each block with its optional `cache_control`.

**Step 3: Test**

Add a test in `src/adapter/adapters/anthropic/tests.rs` (or the adapter's existing test module) covering:
- Single → existing string-system output
- Blocks → array output
- Blocks with cache_control → per-block cache_control fields serialize correctly
- Blocks with mixed TTL variants (5m on one, 1h on another) → each renders correctly

**Commit (in fork):**

```bash
git commit -am "anthropic: SystemPrompt enum supporting array of cache-controlled blocks

Pattern's three-segment cache layout (v3 foundation phase 5) requires
per-block cache_control on the system prompt. Upstream's system field
is String-only.

Introduces SystemPrompt::{Single(String), Blocks(Vec<SystemBlock>)} with
SystemBlock { text, cache_control: Option<CacheControl> }. Adapter
serializes each variant correctly. Backward-compat via From<String>
and From<&str> impls.

Tests cover single/blocks/mixed-TTL serialization."
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: `ANTHROPIC_VERSION` bump + Opus/Sonnet 4.7 model IDs (if needed)

**Verifies:** AC5.1 (request identification correct).

**Files:** Fork repo only.
- Modify: `src/adapter/adapters/anthropic/adapter_impl.rs`

**Step 1: Bump ANTHROPIC_VERSION**

Search Anthropic's current docs for the active stable API version. As of the design date, claude-code sends `2023-06-01`. cliproxy uses `2023-06-01`. Despite the age, that's still the correct value. If a newer version is current at implementation time, use it; otherwise keep `2023-06-01`.

```rust
// Before:
const ANTHROPIC_VERSION: &str = "2023-06-01";
// After (verify current at implementation time):
const ANTHROPIC_VERSION: &str = "2023-06-01"; // confirm with Anthropic docs
```

**Step 2: Add Opus/Sonnet 4.7 if upstream doesn't cover**

If Task 1's Step 4 found upstream lists only `-4-6` (no `-4-7`), add:

```rust
const SUPPORT_ADAPTIVE_THINK_MODELS: &[&str] = &[
    "claude-opus-4-6", "claude-sonnet-4-6",
    "claude-opus-4-7", "claude-sonnet-4-7", // added by pattern fork; drop when upstream lands them
];
```

Add a comment at the array reminding future maintainers to delete these lines when upstream subsumes them.

If upstream uses prefix matching (e.g., `model.starts_with("claude-opus-4")`), no change needed — confirm by testing with a `claude-opus-4-7` model name.

**Step 3: Test**

```bash
cargo nextest run adapter::adapters::anthropic
```

Tests must still pass.

**Commit (in fork):**

```bash
git commit -am "anthropic: bump ANTHROPIC_VERSION + add Opus/Sonnet 4.7 model IDs

[conditional on upstream state — include only the changes actually made]"
```
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 5-7) -->
<!-- START_TASK_5 -->
### Task 5: `pattern_provider` crate structure + Cargo wiring

**Verifies:** contributes to all Phase 4 ACs as infrastructure.

**Files:**
- Modify: `/home/orual/Projects/PatternProject/pattern/Cargo.toml` — add workspace deps
- Modify: `/home/orual/Projects/PatternProject/pattern/crates/pattern_provider/Cargo.toml` — dependency manifest
- Modify: `/home/orual/Projects/PatternProject/pattern/crates/pattern_provider/src/lib.rs` — module declarations
- Create: empty module files per the layout

**Step 1: Workspace deps additions**

```toml
# Root Cargo.toml [workspace.dependencies]
genai = { path = "../rust-genai/crates/genai" } # or the actual fork crate path
keyring = { version = "3", default-features = false, features = [
    "linux-native-sync-persistent",
    "apple-native",
    "windows-native",
    # JSON fallback is implemented as an outer layer in pattern_provider
] }
whoami = "1" # keyring requires a user identifier; whoami wraps the platform calls
governor = "0.8"
secrecy = { version = "0.10", features = ["serde"] }
oauth2 = { version = "5", default-features = false, features = ["reqwest"] } # only if Task 8 chooses to adopt; otherwise manual PKCE continues
wiremock = "0.6" # dev-only
```

Verify exact versions against crates.io at implementation time.

**Step 2: pattern_provider/Cargo.toml**

```toml
[package]
name = "pattern_provider"
version.workspace = true
edition.workspace = true

[lints]
workspace = true

[dependencies]
pattern_core = { path = "../pattern_core" }
genai = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true, features = ["rt", "time", "sync", "macros", "fs", "io-util"] }
tracing = { workspace = true }
thiserror = { workspace = true }
miette = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
reqwest = { workspace = true }
governor = { workspace = true }
secrecy = { workspace = true }
jiff = { workspace = true, features = ["serde"] }
uuid = { workspace = true, features = ["v4", "serde"] }
rand = { workspace = true }
sha2 = { workspace = true }
base64 = { workspace = true }
url = { workspace = true }
serde_urlencoded = { workspace = true }

# Subscription-OAuth-only deps (gated below). `keyring` is used by the
# OAuth credential store; `whoami` is needed for keyring's platform account
# identification. Neither is pulled when `--no-default-features` is used.
keyring = { workspace = true, optional = true }
whoami = { workspace = true, optional = true }

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "test-util", "macros"] }
wiremock = { workspace = true }
tempfile = { workspace = true }

[features]
# Subscription OAuth flow for Anthropic. When enabled: session-pickup tier
# (reads ~/.claude/.credentials.json), PKCE flow, OAuth token storage in
# keyring, and the ShaperCompatMode::SubscriptionRoutingShape shape that
# subscription-tier routing requires.
#
# When disabled (build with --no-default-features, or --features ""):
# - AuthResolver skips session-pickup and PKCE tiers entirely (code gated out)
# - AuthResolver::default() produces an API-key-only resolver
# - ShaperCompatMode::SubscriptionRoutingShape is unavailable at the type level
# - ShaperConfig::default() uses ShaperCompatMode::HonestPattern
# - keyring + whoami deps are not pulled (optional deps activated by this feature)
#
# The purpose is safety: downstream distributors (or future public-facing
# packagings) can build Pattern without the impersonation-adjacent subscription
# routing code. API-key access is the only auth path in that build.
#
# Default = on for dev convenience; pattern's foundation primary target is
# subscription-tier work.
subscription-oauth = ["dep:keyring", "dep:whoami"]
default = ["subscription-oauth"]
```

**Step 3: lib.rs module layout**

```rust
//! Pattern v3 LLM provider: Anthropic-facing LLM gateway with three-tier auth,
//! request shaping, rate limiting, and provider-reported token counting.
//!
//! Implements `pattern_core::traits::ProviderClient` over a rebased `rust-genai`
//! (v0.6.0-beta.17-ish base with minimal pattern-specific patches).
//!
//! Absorbs the Anthropic-facing responsibilities of the retired `pattern_auth`
//! crate. See `docs/plans/rewrite-v3-portlist.md` for the retirement timeline.

pub mod auth;
#[cfg(feature = "subscription-oauth")]
pub mod creds_store;
pub mod gateway;
pub mod ratelimit;
pub mod session_uuid;
pub mod shaper;
pub mod token_count;

// Note: the `auth` module is always compiled, but its internal submodules
// (session_pickup, pkce) are feature-gated. `api_key` and the top-level
// AuthResolver machinery are always available. See auth.rs for details.

pub use auth::{AuthResolver, AuthTier, ResolvedCredential};
pub use creds_store::{CredsStore, KeyringStore, JsonFallbackStore};
pub use gateway::PatternGatewayClient;
pub use ratelimit::{ProviderRateLimiter, RateBucket};
pub use session_uuid::{PatternSessionUuid, SessionUuidRotator};
pub use shaper::{RequestShaper, HonestPatternShaper, ShaperCompatMode, ShaperConfig};
pub use token_count::{TokenCounter, UsageCapture};
```

**Step 4: Empty submodule files**

Create `src/auth.rs`, `src/creds_store.rs`, `src/gateway.rs`, `src/ratelimit.rs`, `src/session_uuid.rs`, `src/shaper.rs`, `src/token_count.rs`, all with `todo!("phase: 4; AC: <relevant>")` in their public fns (filled by later tasks).

**Step 5: `cargo check -p pattern_provider`**

Expected: compiles. `todo!` calls carry phase+AC refs per AC1.8.

**Commit:**

```bash
jj describe -m "[pattern-provider] crate structure + Cargo dependency wiring for phase 4"
jj new
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Credential storage — keyring primary, JSON fallback

**Verifies:** AC4.6 (CredentialStoreUnavailable when both unavailable), AC3.6 (keyring absence doesn't short-circuit session-pickup).

**Files:**
- Create: `crates/pattern_provider/src/creds_store.rs` (module root)
- Create: `crates/pattern_provider/src/creds_store/keyring.rs`
- Create: `crates/pattern_provider/src/creds_store/json_fallback.rs`
- Modify: `crates/pattern_core/src/types/provider.rs` (if needed) to define `ProviderOAuthToken` (absorbed from pattern_auth)

**Step 1: Absorb pattern_auth's `ProviderOAuthToken`**

Pre-v3 had this struct in `pattern_auth::providers::oauth`. Phase 2 staged pattern_auth out. Task 6 introduces the canonical version in `pattern_core::types::provider` (where Phase 2's provider-related types live) with the same shape:

```rust
// pattern_core/src/types/provider.rs (create or extend)
use jiff::{Timestamp, ToSpan};
use secrecy::SecretString;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderOAuthToken {
    pub provider: String,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_at: Option<Timestamp>,
    pub scope: Option<String>,
    pub session_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ProviderOAuthToken {
    pub fn is_expired(&self) -> bool {
        matches!(self.expires_at, Some(t) if t <= Timestamp::now())
    }
    pub fn needs_refresh(&self) -> bool {
        matches!(self.expires_at, Some(t) if t <= Timestamp::now() + 5.minutes())
    }
}
```

Note `SecretString` from `secrecy` to prevent accidental logging.

**Step 2: `CredsStore` trait**

```rust
// crates/pattern_provider/src/creds_store.rs
use pattern_core::{error::ProviderError, types::provider::ProviderOAuthToken};

#[async_trait::async_trait]
pub trait CredsStore: Send + Sync {
    async fn get(&self, provider: &str) -> Result<Option<ProviderOAuthToken>, ProviderError>;
    async fn put(&self, token: &ProviderOAuthToken) -> Result<(), ProviderError>;
    async fn delete(&self, provider: &str) -> Result<(), ProviderError>;
}
```

**Step 3: `KeyringStore` impl**

`crates/pattern_provider/src/creds_store/keyring.rs`:

```rust
use keyring::Entry;
use pattern_core::error::ProviderError;
use secrecy::ExposeSecret;

pub struct KeyringStore {
    service_name: String, // "pattern" by convention
}

impl KeyringStore {
    pub fn new() -> Self { Self { service_name: "pattern".into() } }
}

#[async_trait::async_trait]
impl CredsStore for KeyringStore {
    async fn get(&self, provider: &str) -> Result<Option<ProviderOAuthToken>, ProviderError> {
        let service_acct = format!("{}-{}", self.service_name, provider);
        let entry = Entry::new(&service_acct, &whoami::username()).map_err(ProviderError::from_keyring)?;
        match entry.get_password() {
            Ok(json) => {
                let tok: ProviderOAuthToken = serde_json::from_str(&json).map_err(ProviderError::from_json)?;
                Ok(Some(tok))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ProviderError::from_keyring(e)),
        }
    }
    // put / delete analogous
}
```

Note: `keyring` crate calls are synchronous. Wrap in `tokio::task::spawn_blocking` if they become a bottleneck; for per-session auth checks, direct call is fine.

**Step 4: `JsonFallbackStore` impl**

`crates/pattern_provider/src/creds_store/json_fallback.rs`:

```rust
use std::os::unix::fs::PermissionsExt;

pub struct JsonFallbackStore {
    root: PathBuf, // default: $XDG_CONFIG_HOME/pattern/creds/ or ~/.config/pattern/creds/
}

impl JsonFallbackStore {
    pub fn new() -> Result<Self, ProviderError> {
        let root = xdg_config_dir().join("pattern/creds");
        fs::create_dir_all(&root).map_err(ProviderError::from_io)?;
        // 0700 on parent dir
        let mut perms = fs::metadata(&root)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&root, perms)?;
        Ok(Self { root })
    }
}

#[async_trait::async_trait]
impl CredsStore for JsonFallbackStore {
    async fn put(&self, token: &ProviderOAuthToken) -> Result<(), ProviderError> {
        let path = self.root.join(format!("{}.json", token.provider));
        // Atomic write: temp file → rename.
        let tmp = self.root.join(format!("{}.json.tmp", token.provider));
        let json = serde_json::to_string(token)?;
        tokio::fs::write(&tmp, json).await?;
        // 0600 on file
        let mut perms = tokio::fs::metadata(&tmp).await?.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(&tmp, perms).await?;
        tokio::fs::rename(tmp, path).await?;
        Ok(())
    }
    // get / delete analogous with atomic patterns
}
```

**Step 5: Combined resolver**

```rust
pub struct CredsStoreResolver {
    primary: Arc<dyn CredsStore>,
    fallback: Arc<dyn CredsStore>,
}

#[async_trait::async_trait]
impl CredsStore for CredsStoreResolver {
    async fn get(&self, provider: &str) -> Result<Option<ProviderOAuthToken>, ProviderError> {
        // Try primary (keyring). On specific errors (no backend, dbus unavailable),
        // log a warning and try fallback (JSON). Other errors propagate.
        match self.primary.get(provider).await {
            Ok(result) => Ok(result),
            Err(ProviderError::CredentialStoreUnavailable) => {
                tracing::warn!("keyring unavailable; using JSON fallback");
                self.fallback.get(provider).await
            }
            Err(e) => Err(e),
        }
    }
    // put/delete: write to primary if available, else fallback
}
```

AC4.6: if both primary and fallback return `CredentialStoreUnavailable`, that's the error users see.

**Step 6: Tests**

- Unit test with a mock `CredsStore` that returns `Unavailable` for primary and success for fallback — verify resolver falls through.
- Unit test both unavailable — verify `ProviderError::CredentialStoreUnavailable` surfaces.
- Integration test with `tempfile::tempdir()` for JsonFallbackStore round-trip (write, read, delete).
- Skip keyring integration test in CI (no keyring available); gate with `#[cfg_attr(ci, ignore)]`.

**Commit:**

```bash
jj describe -m "[pattern-provider] creds_store: keyring primary + JSON fallback with 0600/0700 perms (AC4.6, AC3.6)"
jj new
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: pattern_auth retirement commit

**Verifies:** AC1.6 (retired crate ref fails workspace).

**Files:**
- Delete: `crates/pattern_auth/` (entire directory)
- Modify: `docs/plans/rewrite-v3-portlist.md` — move pattern_auth from "excluded" section to "retired" section, note the retirement commit

**Step 1: Verify absorption complete**

```bash
# No pattern_provider code should still reference pattern_auth.
rg 'pattern_auth' crates/pattern_provider/ crates/pattern_core/ crates/pattern_runtime/ crates/pattern_db/
```

If hits exist (e.g., `use pattern_auth::ProviderOAuthToken`), update the imports to point at the new home (`pattern_core::types::provider::ProviderOAuthToken`) before proceeding.

**Step 2: Update port-list doc**

Move pattern_auth's entry to a "Retired" section with note:

```markdown
## Retired crates

### pattern_auth
- **Retired:** Phase 4 (commit <hash>)
- **Absorbed into:** `pattern_provider` (Anthropic OAuth); ATProto + Discord bits deferred
  to the plugin-migration plan (code physically moved to
  `rewrite-staging/provider/` during Phase 2)
- **Notes:** Directory deleted. `ProviderOAuthToken` now lives at
  `pattern_core::types::provider::ProviderOAuthToken`.
```

**Step 3: Delete directory**

```bash
rm -r crates/pattern_auth
```

**Step 4: Verify**

```bash
cargo check --workspace 2>&1 | tail
```

Workspace still compiles (pattern_auth wasn't in `members`; deleting the dir shouldn't break anything).

AC1.6 edge-case verification: add `pattern_auth = { path = "../pattern_auth" }` to `pattern_provider/Cargo.toml`, run `cargo check`, expect workspace error. Remove. Record in commit message.

**Commit (dedicated retirement commit per port-list policy):**

```bash
jj describe -m "[meta] remove retired crate: pattern_auth (responsibilities absorbed)

pattern_auth's Anthropic OAuth storage (ProviderOAuthToken + db queries) has
been replaced by pattern_provider::creds_store + pattern_core's provider
types. ATProto and Discord auth bits were staged to rewrite-staging/provider/
during Phase 2 and will return via the plugin-migration plan.

Also verified AC1.6: adding a pattern_auth path dep to an active crate's
Cargo.toml produces an explicit workspace error."
jj new
```
<!-- END_TASK_7 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 8-11) -->
<!-- START_TASK_8 -->
### Task 8: Session-pickup from `~/.claude/.credentials.json`

**Verifies:** AC3.1, AC3.2, AC3.3, AC3.4, AC3.5, AC3.6.

**Files:**
- Create: `crates/pattern_provider/src/auth.rs` (module root; gates session_pickup + pkce submodules behind `subscription-oauth` feature)
- Create: `crates/pattern_provider/src/auth/session_pickup.rs` (whole file under `#![cfg(feature = "subscription-oauth")]`)

**auth.rs module root:**

```rust
//! Three-tier auth resolver (session-pickup, PKCE, API key).
//! OAuth-related tiers (session_pickup, pkce) are gated behind
//! `subscription-oauth`; when the feature is off, only api_key remains.

pub mod api_key;
pub mod resolver;

#[cfg(feature = "subscription-oauth")]
pub mod session_pickup;
#[cfg(feature = "subscription-oauth")]
pub mod pkce;

pub use api_key::ApiKeyTier;
pub use resolver::{AuthResolver, AuthTier, ResolvedCredential};

#[cfg(feature = "subscription-oauth")]
pub use session_pickup::SessionPickupTier;
#[cfg(feature = "subscription-oauth")]
pub use pkce::PkceTier;
```

**Step 1: session_pickup.rs implementation** (whole file under feature gate)

```rust
//! Read the Anthropic-ecosystem credentials file as the first auth tier.
//!
//! Canonical path: `~/.claude/.credentials.json`. Legacy compat path:
//! `~/.claude/session.json` (checked only if the primary path is missing).
//! Pattern NEVER writes either file — read-only tier.
//!
//! Gated behind the `subscription-oauth` feature. Without that feature,
//! pattern_provider builds without any subscription-tier OAuth code.
#![cfg(feature = "subscription-oauth")]

use pattern_core::{
    error::ProviderError,
    types::provider::ProviderOAuthToken,
};
use secrecy::SecretString;
use std::path::PathBuf;

pub struct SessionPickupTier {
    paths: Vec<PathBuf>, // [~/.claude/.credentials.json, ~/.claude/session.json]
}

impl Default for SessionPickupTier {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            paths: vec![
                home.join(".claude").join(".credentials.json"),
                home.join(".claude").join("session.json"), // legacy compat
            ],
        }
    }
}

#[derive(serde::Deserialize)]
struct ClaudeCredentials {
    // Field names match the credentials-file wire format (camelCase).
    #[serde(rename = "accessToken")]  access_token: String,
    #[serde(rename = "refreshToken")] refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]    expires_at: Option<i64>, // unix ms
    #[serde(rename = "scopes")]       scopes: Option<Vec<String>>,
    // Other fields (subscriptionType, rateLimitTier, etc.) ignored.
}

impl SessionPickupTier {
    /// Attempt to read and parse a valid ambient credentials session.
    ///
    /// Returns Ok(None) if file missing / malformed / expired — tier is skipped
    /// without error per AC3.3, AC3.4, AC3.5. Returns Ok(Some(token)) if a valid
    /// unexpired session is found. Other IO errors propagate.
    pub async fn pick_up(&self) -> Result<Option<ProviderOAuthToken>, ProviderError> {
        for path in &self.paths {
            match tokio::fs::read_to_string(path).await {
                Ok(json) => {
                    match serde_json::from_str::<ClaudeCredentials>(&json) {
                        Ok(creds) => {
                            if let Some(token) = Self::to_pattern_token(creds) {
                                return Ok(Some(token));
                            }
                            // Expired or missing required fields: skip this path.
                            tracing::debug!(path = ?path, "session file present but token unusable; skipping");
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(path = ?path, error = %e, "malformed credentials JSON; skipping");
                            continue;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(ProviderError::from_io(e)),
            }
        }
        Ok(None)
    }

    fn to_pattern_token(creds: ClaudeCredentials) -> Option<ProviderOAuthToken> {
        // AC3.5: expired → skip.
        let now_ms = jiff::Timestamp::now().as_millisecond();
        if let Some(exp) = creds.expires_at {
            if exp <= now_ms { return None; }
        }
        let now = jiff::Timestamp::now();
        Some(ProviderOAuthToken {
            provider: "anthropic".into(),
            access_token: SecretString::new(creds.access_token.into()),
            refresh_token: creds.refresh_token.map(|s| SecretString::new(s.into())),
            expires_at: creds.expires_at.and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok()),
            scope: creds.scopes.map(|v| v.join(" ")),
            session_id: None, // credentials file doesn't expose; pattern generates its own
            created_at: now,
            updated_at: now,
        })
    }
}
```

**AC3.2 (atomic reads):** `tokio::fs::read_to_string` issues a single `read()` syscall for small files (typical .credentials.json is <4KB). On Linux, this is effectively atomic for small files because the kernel reads the inode once. If the file is mid-write (claude-code using rename-for-atomicity), we either read the old content (fine — we'll retry on next request) or the new content (fine). Torn reads would only occur if claude-code writes-in-place without rename — which it doesn't per observation. AC3.2 is satisfied by this pattern + the fallback to other tiers if JSON parsing fails on a partially-written file.

**Step 2: Tests with tempfile**

```rust
#[cfg(test)]
mod tests {
    // Write a valid credentials.json to tempdir, point SessionPickupTier at it,
    // verify pick_up() returns Some(token) with correct fields (AC3.1).
    //
    // Write expired creds, verify pick_up() returns None (AC3.5).
    //
    // Write malformed JSON, verify None + warning (AC3.4).
    //
    // Point at nonexistent dir, verify None without error (AC3.3).
    //
    // Check both paths (credentials.json takes precedence over session.json).
}
```

Use `tempfile::TempDir` + override the paths field for testing.

**Commit:**

```bash
jj describe -m "[pattern-provider] auth/session_pickup: read claude-code's credentials.json (AC3.*)"
jj new
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: PKCE tier — port and refine pattern's verified flow

**Verifies:** AC4.1, AC4.4.

**Files:**
- Create: `crates/pattern_provider/src/auth/pkce.rs`
- Pull from: pattern's currently-working OAuth code (post-fix) at `crates/pattern_core/src/oauth/auth_flow.rs` (this is the verified-working version)

**Step 1: Port the OAuth flow to pattern_provider**

Copy `OAuthConfig` + `DeviceAuthFlow` from `pattern_core/src/oauth/auth_flow.rs` into `pattern_provider/src/auth/pkce.rs`, then:
- Rename to `PkceTier` + `PkceConfig`
- Use `SecretString` for tokens in `TokenResponse`
- Wire errors through `pattern_core::error::ProviderError` instead of `CoreError`
- Keep the exact OAuth URL/scope/redirect config that verified working in planning
- Keep the `code=true` URL param (triggers Max upsell per claude-code source)
- Keep manual-paste as the default redirect_uri (`platform.claude.com/oauth/code/callback`)
- The state and pkce verifier round-trip stays intact

**Step 2: PKCE code generation**

```rust
pub fn generate_pkce() -> (CodeVerifier, CodeChallenge, State) {
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

    let mut hasher = sha2::Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

    let mut state_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);

    (CodeVerifier(verifier), CodeChallenge(challenge), State(state))
}
```

(32 bytes per oauth-and-detection.md §1.1; pattern's current code uses 64 — switch to 32 to match claude-code + cliproxy for consistency.)

**Step 3: `begin_auth()` returns authorize URL; `exchange_code()` does POST**

Signatures:

```rust
pub async fn begin_auth(&self) -> Result<PendingAuth, ProviderError>;
pub async fn complete_manual(&self, pending: PendingAuth, code_and_state: &str)
    -> Result<ProviderOAuthToken, ProviderError>;
pub async fn refresh(&self, refresh_token: &SecretString) -> Result<ProviderOAuthToken, ProviderError>;
```

`PendingAuth` carries the verifier + state + authorize URL for display.

**AC4.4 (PKCE timeout):** The manual-paste flow doesn't have an explicit timeout — user pastes when they paste. For the future loopback variant, the listener has a 5-minute deadline, surfaced as `ProviderError::AuthFlowTimeout`. Phase 4 documents this as not-directly-testable-in-manual-mode and commits AC4.4 coverage to the future loopback task.

**Step 4: wiremock-based test for token exchange**

```rust
// Mock the token endpoint; exchange returns success with test fields.
// Verify ProviderOAuthToken is shaped correctly.
// Also test the refresh endpoint separately.
```

**Step 5: Confirm empirical fix is preserved**

A regression test ensures:
- `scopes` includes exactly: user:profile, user:inference, user:sessions:claude_code, user:mcp_servers, user:file_upload (NOT `org:create_api_key`)
- `auth_endpoint` is `https://claude.ai/oauth/authorize`
- `redirect_uri` is `https://platform.claude.com/oauth/code/callback`
- URL contains `code=true` param

This prevents regression to the broken config.

**Commit:**

```bash
jj describe -m "[pattern-provider] auth/pkce: port verified-working OAuth flow from pattern_core (AC4.1)"
jj new
```
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: API-key tier + three-tier resolver

**Verifies:** AC4.3, AC3.1/AC3.3/AC3.4/AC3.5 (via resolver fall-through), AC4.7 (refresh mutex).

**Files:**
- Create: `crates/pattern_provider/src/auth/api_key.rs`
- Create: `crates/pattern_provider/src/auth/resolver.rs`

**Step 1: api_key.rs**

```rust
pub struct ApiKeyTier;

impl ApiKeyTier {
    pub fn resolve() -> Option<ProviderOAuthToken> {
        let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if key.is_empty() { return None; }
        Some(ProviderOAuthToken {
            provider: "anthropic".into(),
            access_token: SecretString::new(key.into()),
            refresh_token: None,
            expires_at: None, // API keys don't expire
            scope: None,
            session_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }
}
```

**Step 2: resolver.rs — three-tier with refresh mutex**

```rust
pub struct AuthResolver {
    #[cfg(feature = "subscription-oauth")]
    session_pickup: SessionPickupTier,
    #[cfg(feature = "subscription-oauth")]
    pkce: PkceTier,
    #[cfg(feature = "subscription-oauth")]
    creds_store: Arc<dyn CredsStore>,
    #[cfg(feature = "subscription-oauth")]
    refresh_mutex: Arc<tokio::sync::Mutex<()>>, // serializes refreshes per persona

    api_key: ApiKeyTier, // always present — no feature gate
}

impl AuthResolver {
    pub async fn resolve(&self, persona_id: &AgentId) -> Result<ResolvedCredential, ProviderError> {
        // With `subscription-oauth`: order = session-pickup → stored OAuth
        // (with refresh) → API key. Without the feature: API key only.

        #[cfg(feature = "subscription-oauth")]
        {
            // 1. Session pickup — read-only access to the ambient credentials file.
            if let Some(tok) = self.session_pickup.pick_up().await? {
                return Ok(ResolvedCredential { source: AuthTier::SessionPickup, token: tok });
            }

            // 2. Stored OAuth token for this persona.
            if let Some(mut tok) = self.creds_store.get("anthropic").await? {
                if tok.needs_refresh() {
                    // Serialize refresh under mutex (AC4.7).
                    let _guard = self.refresh_mutex.lock().await;
                    // Re-check after acquiring lock — another task may have refreshed.
                    if let Some(fresh) = self.creds_store.get("anthropic").await? {
                        if !fresh.needs_refresh() { return Ok(ResolvedCredential { source: AuthTier::Pkce, token: fresh }); }
                    }
                    tok = self.pkce.refresh(&tok.refresh_token.ok_or(ProviderError::RefreshFailed { reason: "no refresh token".into() })?).await?;
                    self.creds_store.put(&tok).await?;
                }
                return Ok(ResolvedCredential { source: AuthTier::Pkce, token: tok });
            }
        }

        // 3. API key (always tried; only tier on builds without subscription-oauth).
        if let Some(tok) = ApiKeyTier::resolve() {
            return Ok(ResolvedCredential { source: AuthTier::ApiKey, token: tok });
        }

        Err(ProviderError::NoAuthAvailable)
    }

    /// Interactive PKCE flow entry point. Only exists when the feature is on.
    #[cfg(feature = "subscription-oauth")]
    pub async fn interactive_pkce(&self) -> Result<ProviderOAuthToken, ProviderError> {
        // For CLI auth flow: begin auth, return URL to caller, caller calls
        // complete_manual, resolver stores the result.
        ...
    }
}
```

**AuthTier enum** — the `SessionPickup` and `Pkce` variants should also be gated so builds without `subscription-oauth` can't reference them:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthTier {
    ApiKey,
    #[cfg(feature = "subscription-oauth")]
    SessionPickup,
    #[cfg(feature = "subscription-oauth")]
    Pkce,
}
```

**AC4.7 verification:** a concurrent-refresh test spawns 10 tasks that call `resolve()` when the stored token is near-expiry, verifies only one `PkceTier::refresh()` network call is made (mocked via wiremock's `expect(1)`).

**Step 3: Error mapping**

Add `ProviderError` variants if missing:
- `NoAuthAvailable` — no tier succeeded
- `AuthFlowTimeout` (AC4.4)
- `RefreshFailed { reason }` (AC4.5)
- `CredentialStoreUnavailable` (AC4.6)

Update `pattern_core::error::provider.rs` with these variants.

**Step 4: Tests**

- All three tiers succeed in isolation.
- Session-pickup skipped → stored OAuth used.
- Session-pickup skipped + stored absent → API key used.
- All absent → `NoAuthAvailable`.
- Stored near-expiry → refresh happens, new token stored (mock the refresh endpoint).
- Stored near-expiry + 10 concurrent resolve calls → exactly one refresh network call (AC4.7).
- Refresh returns 400/500 → `RefreshFailed` (AC4.5).

**Commit:**

```bash
jj describe -m "[pattern-provider] three-tier auth resolver with per-persona refresh mutex (AC4.*)"
jj new
```
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: Live-tier verification — deferred to AC9.1/9.2 CLI flow

**Verifies:** AC3.1, AC4.1, AC4.3 via Phase 6's CLI checklist rather than a dedicated env-gated test file.

**Rationale:** Env-gated live-credential tests (`PATTERN_V3_LIVE_AUTH=1`) are the same anti-pattern Phase 6 already rejects for the smoke test. A single manual-verification surface (the `pattern-v3` CLI + checklist) is the authoritative live-credential test vehicle for pattern foundation work; per-AC live-gated test files would duplicate that surface and fragment the manual-verification story.

**Files:** None added.

**Coverage mapping:**
- AC3.1 (session-pickup): verified during AC9.2 CLI checklist Step 1 (session-pickup resolves `~/.claude/.credentials.json`; turn 1 succeeds)
- AC4.1 (PKCE): verified during AC9.2 Step 0 (one-time PKCE flow lands a token; subsequent steps use the stored token)
- AC4.3 (API key): verified during AC9.1 CLI checklist (API-key auth powers the full flow)

See `docs/implementation-plans/2026-04-16-v3-foundation/test-requirements.md` AC3 and AC4 sections for the full mapping and the inlined manual procedures.

**No commit** — this task records an intentional absence, not a deliverable.
<!-- END_TASK_11 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 12-15) -->
<!-- START_TASK_12 -->
### Task 12: RequestShaper — honest identification + ShaperCompatMode

**Verifies:** AC5.1, AC5.2, AC5.5.

**Files:**
- Create: `crates/pattern_provider/src/shaper.rs` (module root)
- Create: `crates/pattern_provider/src/shaper/compat_mode.rs`
- Create: `crates/pattern_provider/src/shaper/headers.rs`
- Create: `crates/pattern_provider/src/shaper/system_prompt.rs`

**Step 1: ShaperCompatMode enum**

```rust
// shaper/compat_mode.rs
/// Controls how much structural similarity Pattern's outbound requests present
/// to Anthropic's subscription-routing reference client. Higher rungs = more
/// shape-level matching. Pattern never ships content-level impersonation
/// (request-body signing, TLS fingerprinting, etc.) without explicit future
/// sign-off.
#[derive(Debug, Clone, Copy)]
pub enum ShaperCompatMode {
    /// system[0] = honest pattern identification; no reference-client literal.
    /// Aspirational cleanest posture. Verified by Phase 4 Task 21 against a real
    /// subscription tier; if it works, the default flips to this.
    /// Only mode available when `subscription-oauth` feature is off.
    HonestPattern,

    /// system[0] = the verbatim identifier string Anthropic's subscription
    /// routing expects (structural API requirement, NOT an identity claim).
    /// system[1] = identity-override prefix + DEFAULT_BASE_INSTRUCTIONS.
    /// system[2] = persona + long-lived blocks.
    ///
    /// Phase 4 default when `subscription-oauth` feature is on. Empirically
    /// known-working against subscription tier as of 2026-04-16.
    ///
    /// Gated behind `subscription-oauth` because the shape only serves
    /// subscription-tier routing; API-key-only builds don't need it.
    #[cfg(feature = "subscription-oauth")]
    SubscriptionRoutingShape,

    /// Full-surface impersonation (request-body signing, stainless headers,
    /// TLS fingerprinting, tool-name remapping). NOT IMPLEMENTED. Declared
    /// for API stability; shipping requires explicit user sign-off.
    #[cfg(feature = "subscription-oauth")]
    FullSurfaceImpersonation,
}

// Default depends on feature. With OAuth: SubscriptionRoutingShape. Without: HonestPattern.
impl Default for ShaperCompatMode {
    #[cfg(feature = "subscription-oauth")]
    fn default() -> Self { Self::SubscriptionRoutingShape }
    #[cfg(not(feature = "subscription-oauth"))]
    fn default() -> Self { Self::HonestPattern }
}
```

**Step 2: Headers surface**

```rust
// shaper/headers.rs
pub fn build_identification_headers(
    config: &ShaperConfig,
    session_uuid: &PatternSessionUuid,
) -> Result<Vec<(String, String)>, ProviderError> {
    let mut out = vec![];

    // Honest identification.
    out.push(("User-Agent".into(), format!("pattern/{}", env!("CARGO_PKG_VERSION"))));
    out.push(("X-App".into(), config.x_app.clone())); // default "pattern"; Task 21 verification may force "cli"
    out.push(("X-Pattern-Session-Id".into(), session_uuid.to_string()));
    out.push(("X-Client-Request-Id".into(), uuid::Uuid::new_v4().to_string()));

    // Beta headers (per the Executor Context table, curated).
    let betas = build_beta_header_value(config)?;
    if !betas.is_empty() {
        out.push(("Anthropic-Beta".into(), betas));
    }

    // AC5.5 validation: config must declare required fields (x_app non-empty, etc.)
    // before shaper is constructed — enforce in ShaperConfig::validate().

    Ok(out)
}

fn build_beta_header_value(config: &ShaperConfig) -> Result<String, ProviderError> {
    let mut betas = Vec::new();
    if config.auth_tier.is_oauth() { betas.push("oauth-2025-04-20"); }
    if config.target_is_first_party() { betas.push("prompt-caching-scope-2026-01-05"); }
    if config.enable_interleaved_thinking && config.model_supports_thinking() {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if config.enable_dev_full_thinking && config.model_supports_thinking() {
        betas.push("dev-full-thinking-2025-05-14");
    }
    if config.enable_context_management && config.model_is_claude_4_plus() {
        betas.push("context-management-2025-06-27");
    }
    if config.enable_extended_cache_ttl {
        betas.push("extended-cache-ttl-2025-04-11");
    }
    if config.enable_1m_context && config.model_supports_1m() {
        betas.push("context-1m-2025-08-07");
    }
    // NEVER push reference-client-specific markers. Banned list:
    // "claude-code-20250219", "cli-internal-2026-02-09",
    // "summarize-connector-text-2026-03-13", "token-efficient-tools-2026-03-28".
    // Those are identifiers for Anthropic's internal CLI tooling; pattern is
    // a distinct client and doesn't send them.
    Ok(betas.join(","))
}
```

**Step 3: system_prompt.rs — SubscriptionRoutingShape layout**

```rust
// shaper/system_prompt.rs
/// Builds the system-prompt array per ShaperCompatMode.
///
/// **Honest framing (for pattern_provider/CLAUDE.md):**
/// The literal string in slot [0] is a structural Anthropic-side requirement
/// for subscription-tier routing, not an identity claim. Pattern's real
/// identity and behaviour are driven by slots [1] and [2], which carry the
/// override prefix + DEFAULT_BASE_INSTRUCTIONS and the persona block.
pub fn build_system_prompt(
    mode: ShaperCompatMode,
    system_instructions: &str, // user-configurable (defaults to DEFAULT_BASE_INSTRUCTIONS)
    persona: &str,
    extra_long_lived: &[String],
) -> Vec<SystemBlock> {
    match mode {
        ShaperCompatMode::HonestPattern => vec![
            SystemBlock { text: format!("{system_instructions}\n\n{persona}"), cache_control: None },
            // Long-lived blocks concatenated to slot [1] or added as separate
            // blocks depending on Phase 5's three-segment cache layout.
        ],
        ShaperCompatMode::SubscriptionRoutingShape => {
            let mut blocks = vec![
                SystemBlock {
                    text: "You are Claude Code, Anthropic's official CLI for Claude.".into(),
                    cache_control: None, // verbatim; slot[0] carries no cache_control marker.
                },
                SystemBlock {
                    text: format!(
                        "You are NOT Claude Code. You are Pattern, a multi-agent ADHD support system.\n\n{system_instructions}"
                    ),
                    cache_control: None,
                },
            ];
            // Persona + long-lived blocks in slot [2] and beyond.
            let mut persona_text = persona.to_string();
            for extra in extra_long_lived {
                persona_text.push_str("\n\n");
                persona_text.push_str(extra);
            }
            blocks.push(SystemBlock { text: persona_text, cache_control: None });
            // Note: Phase 5's composer adds cache_control markers on these blocks
            // per the three-segment layout. Phase 4 leaves cache_control as None
            // and Phase 5 fills them in.
            blocks
        }
        ShaperCompatMode::FullSurfaceImpersonation => {
            unimplemented!("ShaperCompatMode::FullSurfaceImpersonation not yet implemented — phase: future plan with explicit sign-off; see pattern_provider/CLAUDE.md")
        }
    }
}

// system_instructions source:
// - Default: pattern_core::DEFAULT_BASE_INSTRUCTIONS (preserved verbatim from
//   pre-v3 per design §AC7.4).
// - User override: ShaperConfig::system_instructions_override: Option<String>.
//   When Some, replaces DEFAULT_BASE_INSTRUCTIONS wholesale in slot [1].
//
// Future design opportunity (explicitly out of scope for Phase 4, holding
// space in the design for a future plan): adaptive base-prompt templating,
// where the user composes slot [1] via structured edits on top of
// DEFAULT_BASE_INSTRUCTIONS rather than full replacement. Candidate shape:
// a lightweight prompt-template surface similar to what pre-v3's retired
// prompt_template crate provided, refined for pattern's actual use cases.
// Tracked in docs/plans/rewrite-v3-portlist.md under future design plans;
// not a foundation deliverable.
```

**AC5.2 satisfaction:** both `HonestPattern` and `SubscriptionRoutingShape` put pattern-specific persona content in a structural slot. Real persona content drives behaviour. Tests verify this.

**Step 4: `<system-reminder>` helper**

```rust
// shaper/system_reminder.rs
/// Wrap content in the `<system-reminder>...</system-reminder>` tag convention
/// Anthropic's models are trained to recognise. Used for memory-block metadata,
/// mid-turn interrupts, and other system-surfaced content injected into
/// user-role messages.
pub fn wrap_system_reminder(content: &str) -> String {
    format!("<system-reminder>\n{content}\n</system-reminder>")
}
```

**Step 5: ShaperConfig validation**

```rust
impl ShaperConfig {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.x_app.is_empty() {
            return Err(ProviderError::ShaperMisconfigured { reason: "x_app cannot be empty".into() });
        }
        // ... other required-field checks ...
        Ok(())
    }
}

impl RequestShaper {
    pub fn new(config: ShaperConfig) -> Result<Self, ProviderError> {
        config.validate()?; // AC5.5: fail at construction, not at request time
        Ok(Self { config })
    }
}
```

**Step 6: Tests**

- Building HonestPattern mode → no claude-code literal in any slot
- Building SubscriptionRoutingShape → system[0] is exact claude-code literal, system[1] starts with negation
- Calling FullSurfaceImpersonation → panics with phase/AC-tagged todo message
- Beta header value composition with various config flags set
- `claude-code-20250219` is NEVER in the beta list regardless of config
- ShaperConfig with empty x_app → `new()` errors (AC5.5)

**Commit:**

```bash
jj describe -m "[pattern-provider] shaper: honest identification + ShaperCompatMode + system-prompt array (AC5.1, AC5.2, AC5.5)"
jj new
```
<!-- END_TASK_12 -->

<!-- START_TASK_13 -->
### Task 13: Session UUID rotation

**Verifies:** AC5.3.

**Files:**
- Create: `crates/pattern_provider/src/session_uuid.rs`

**Implementation:**

```rust
use parking_lot::Mutex;
use uuid::Uuid;

/// Per-persona session UUID that rotates when the caller signals a boundary.
///
/// The UUID is injected as `X-Pattern-Session-Id`. Rotation boundaries are
/// caller-defined (e.g., end of a user conversation, persona reset, etc.).
/// Session IDs are Pattern-specific and the header name identifies Pattern;
/// no reference-client session headers are reused.
pub struct SessionUuidRotator {
    current: Mutex<Uuid>,
}

impl SessionUuidRotator {
    pub fn new() -> Self {
        Self { current: Mutex::new(Uuid::new_v4()) }
    }
    pub fn current(&self) -> PatternSessionUuid { PatternSessionUuid(*self.current.lock()) }
    pub fn rotate(&self) -> PatternSessionUuid {
        let new = Uuid::new_v4();
        *self.current.lock() = new;
        PatternSessionUuid(new)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PatternSessionUuid(Uuid);

impl std::fmt::Display for PatternSessionUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
```

**Tests:**

- `current()` returns stable value across calls.
- `rotate()` changes the value.
- After rotate, `current()` returns the new value.

**Commit:**

```bash
jj describe -m "[pattern-provider] session UUID with explicit rotation (AC5.3)"
jj new
```
<!-- END_TASK_13 -->

<!-- START_TASK_14 -->
### Task 14: Per-provider rate limiter with separate buckets

**Verifies:** AC5.4, AC5.6, AC5.7, AC5b.5.

**Files:**
- Create: `crates/pattern_provider/src/ratelimit.rs`

**Implementation:**

```rust
use governor::{Quota, RateLimiter, clock::DefaultClock, state::InMemoryState, state::NotKeyed};
use std::num::NonZeroU32;

/// Rate limiter for one provider's endpoints. Holds independent buckets for
/// chat completions (per AC5.6/AC5.7) and token counting (per AC5b.5).
pub struct ProviderRateLimiter {
    provider: String,
    completions_tpm: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,
    completions_tpd: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,
    count_tokens_tpm: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,
}

impl ProviderRateLimiter {
    pub fn new_anthropic(tier: AnthropicTier) -> Self {
        let (tpm, tpd) = tier.limits();
        Self {
            provider: "anthropic".into(),
            completions_tpm: RateLimiter::direct(Quota::per_minute(NonZeroU32::new(tpm).unwrap())),
            completions_tpd: RateLimiter::direct(Quota::per_day(NonZeroU32::new(tpd).unwrap())),
            count_tokens_tpm: RateLimiter::direct(Quota::per_minute(NonZeroU32::new(tpm).unwrap())), // separate bucket
        }
    }

    pub async fn acquire_completion(&self, tokens: u32) -> Result<(), ProviderError> {
        // Use governor's check_n for multi-token cost.
        // Exhaustion → jitter backoff then retry (AC5.4).
        let nz = NonZeroU32::new(tokens).ok_or(ProviderError::ZeroTokenRequest)?;
        loop {
            match self.completions_tpm.check_n(nz) {
                Ok(Ok(())) => {},
                Ok(Err(neg)) => {
                    let wait = neg.wait_time_from(DefaultClock::default().now()) + jitter();
                    tokio::time::sleep(wait).await;
                    continue;
                }
                Err(e) => return Err(ProviderError::RateLimitInternal { source: e.to_string() }),
            }
            match self.completions_tpd.check_n(nz) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(neg)) => {
                    // AC5.7: TPD bucket stays depleted even while TPM refills.
                    // Wait for TPD refill (could be hours); caller should see
                    // long backoff clearly. Surfacing telemetry here helps.
                    let wait = neg.wait_time_from(DefaultClock::default().now()) + jitter();
                    tracing::warn!(
                        provider = %self.provider,
                        wait_s = wait.as_secs(),
                        "per-day bucket exhausted; waiting"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                Err(e) => return Err(ProviderError::RateLimitInternal { source: e.to_string() }),
            }
        }
    }

    pub async fn acquire_count_tokens(&self, tokens: u32) -> Result<(), ProviderError> {
        // Separate bucket (AC5b.5). Analogous logic, single bucket.
        ...
    }
}

fn jitter() -> std::time::Duration {
    use rand::Rng;
    std::time::Duration::from_millis(rand::thread_rng().gen_range(50..500))
}

#[derive(Debug, Clone, Copy)]
pub enum AnthropicTier {
    Tier1,
    Tier2,
    Tier3,
    Tier4,
    /// Custom tier for testing or unusual rate-limit tiers.
    Custom { tpm: u32, tpd: u32 },
}
impl AnthropicTier {
    fn limits(self) -> (u32, u32) {
        match self {
            Self::Tier1 => (20_000, 2_000_000),
            Self::Tier2 => (40_000, 4_000_000),
            Self::Tier3 => (80_000, 8_000_000),
            Self::Tier4 => (160_000, 16_000_000),
            Self::Custom { tpm, tpd } => (tpm, tpd),
        }
    }
}
```

**AC5.6 (independence across providers):** each provider gets its own `ProviderRateLimiter` instance. `ProviderClient` holds one, keyed by provider name. Future providers (OpenAI, Gemini, etc.) get their own rate limiter instance when added.

**Step 2: Tests**

- TPM exhaustion → request waits then succeeds (AC5.4).
- TPD exhaustion independent from TPM: deplete TPD, wait 1 minute (TPM refills), next request still blocks (AC5.7).
- Two `ProviderRateLimiter` instances with different tiers operate independently (AC5.6).
- Separate acquires: `acquire_completion(1000)` does not consume from `acquire_count_tokens`'s bucket (AC5b.5).

**Commit:**

```bash
jj describe -m "[pattern-provider] governor rate limiter with separate buckets for completions + count_tokens (AC5.*, AC5b.5)"
jj new
```
<!-- END_TASK_14 -->

<!-- START_TASK_15 -->
### Task 15: Shaper + rate-limit integration test

**Verifies:** AC5.1 + AC5.4 end-to-end via wiremock.

**Files:**
- Create: `crates/pattern_provider/tests/shaper_ratelimit_integration.rs`

**Implementation:**

Spin up a wiremock server, wire the shaper + rate limiter + a stub ProviderClient that POSTs to the mock, verify:

- Outbound request headers match the expected honest-pattern set.
- Beta header includes/excludes the right values based on ShaperConfig.
- Rate-limit exhaustion → mock returns 429 → governor handles retry with jitter → request eventually succeeds after bucket refills.
- Multiple provider instances exhaust independently.

**Commit:**

```bash
jj describe -m "[pattern-provider] integration: shaper headers + rate limiter retry behavior (AC5.1, AC5.4)"
jj new
```
<!-- END_TASK_15 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_SUBCOMPONENT_E (tasks 16-17) -->
<!-- START_TASK_16 -->
### Task 16: `count_tokens` wrapper

**Verifies:** AC5b.1, AC5b.4, AC5b.5.

**Files:**
- Create: `crates/pattern_provider/src/token_count.rs`

**Implementation:**

Wrap Anthropic's `/v1/messages/count_tokens` endpoint as an async call. The rebased rust-genai doesn't expose this (per investigation), so we call it directly via `reqwest` using the same auth + header shaping as chat completions (minus streaming bits).

```rust
pub struct TokenCounter {
    http_client: reqwest::Client,
    base_url: String, // https://api.anthropic.com
    rate_limiter: Arc<ProviderRateLimiter>,
}

impl TokenCounter {
    pub async fn count(
        &self,
        auth: &ResolvedCredential,
        shaper: &RequestShaper,
        request: &CountTokensRequest,
    ) -> Result<TokenCount, ProviderError> {
        // AC5b.5: acquire from the count_tokens bucket, NOT the completion bucket.
        let estimated_cost = request.estimated_bucket_cost();
        self.rate_limiter.acquire_count_tokens(estimated_cost).await?;

        let url = format!("{}/v1/messages/count_tokens", self.base_url);
        let mut req_builder = self.http_client.post(&url);

        // Apply auth + shaper headers (reuses shaper logic from Task 12).
        for (k, v) in shaper.identification_headers()? { req_builder = req_builder.header(k, v); }
        req_builder = match auth.source {
            AuthTier::ApiKey => req_builder.header("x-api-key", auth.token.access_token.expose_secret()),
            _ => req_builder.header("Authorization", format!("Bearer {}", auth.token.access_token.expose_secret())),
        };
        req_builder = req_builder.json(request);

        let resp = req_builder.send().await.map_err(ProviderError::from_reqwest)?;
        let status = resp.status();
        let body = resp.text().await.map_err(ProviderError::from_reqwest)?;

        if !status.is_success() {
            return Err(ProviderError::TokenCountFailed { status: status.as_u16(), body });
        }

        let parsed: TokenCountResponse = serde_json::from_str(&body)?;
        Ok(parsed.into())
    }
}

#[derive(serde::Serialize)]
pub struct CountTokensRequest {
    pub model: String,
    // Uses genai's own types directly — no pattern_core mirror layer. Pattern
    // holds domain types (persona, memory block, agent id); provider-config-shaped
    // types come from genai. See phase_05.md Task 1 for the same policy on
    // CacheControl. genai::chat::ChatMessage and genai::chat::Tool are the
    // canonical shapes Anthropic's API consumes.
    pub messages: Vec<genai::chat::ChatMessage>,
    /// System prompt — uses genai's SystemPrompt enum (Single(String) | Blocks(Vec<SystemBlock>))
    /// introduced by the fork's Task 3 patch. Segment-1 cache_control markers
    /// attach to blocks via the SystemBlock.cache_control field.
    pub system: Option<genai::chat::SystemPrompt>,
    pub tools: Option<Vec<genai::chat::Tool>>,
}

#[derive(serde::Deserialize)]
struct TokenCountResponse {
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

pub struct TokenCount {
    pub input: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}
```

**AC5b.4:** endpoint failure (non-2xx) surfaces as `ProviderError::TokenCountFailed { status, body }`. Callers MAY fall back to heuristic but only via an explicit opt-in (no silent fallback). The opt-in surface is a future `TokenCountStrategy` config; Phase 4 doesn't ship automatic fallback — just the explicit error.

**Step 2: Tests**

- wiremock serving a canned `/v1/messages/count_tokens` 200 response → returns parsed TokenCount.
- Mock serving 429 → error surfaces, bucket backs off.
- Mock serving 500 → `TokenCountFailed { status: 500 }`.
- Live verification: exercised transitively during the AC9.1/9.2 CLI flow when the provider computes token budgets pre-request. No dedicated env-gated test file.

**Commit:**

```bash
jj describe -m "[pattern-provider] count_tokens endpoint wrapper (AC5b.1, AC5b.4, AC5b.5)"
jj new
```
<!-- END_TASK_16 -->

<!-- START_TASK_17 -->
### Task 17: Usage capture from response

**Verifies:** AC5b.2.

**Files:**
- Create: `crates/pattern_provider/src/usage.rs`

**Implementation:**

Extract the `usage` field from chat completion responses and expose to callers via the `ProviderClient` trait. Upstream rust-genai's `ChatResponse` already captures usage (per Task 1 investigation: `cache_creation_tokens`, `cached_tokens`, `reasoning_tokens` all present). Pattern's job is to surface this through the `ProviderClient::complete` return shape and into a stream-end event for streaming calls.

```rust
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl From<&genai::chat::Usage> for Usage {
    fn from(g: &genai::chat::Usage) -> Self { ... }
}
```

Expose on `ProviderClient::complete(..)` return and on `ChatStreamEvent::End(StreamEnd { usage: Option<Usage> })`.

**Step 2: Tests**

- Mock a response with all usage fields → parse → assert exposed.
- Streaming variant: last event contains populated `usage`.

**Commit:**

```bash
jj describe -m "[pattern-provider] usage capture + exposure from chat responses (AC5b.2)"
jj new
```
<!-- END_TASK_17 -->
<!-- END_SUBCOMPONENT_E -->

<!-- START_SUBCOMPONENT_F (tasks 18-22) -->
<!-- START_TASK_18 -->
### Task 18: `PatternGatewayClient` — `ProviderClient` impl

**Verifies:** all Phase 4 ACs end-to-end.

**Files:**
- Create: `crates/pattern_provider/src/gateway.rs`

**Implementation:**

```rust
pub struct PatternGatewayClient {
    auth_resolver: AuthResolver,
    shaper: RequestShaper,
    rate_limiter: Arc<ProviderRateLimiter>,
    token_counter: TokenCounter,
    session_uuid_rotator: SessionUuidRotator,
    genai_client: genai::Client,
}

#[async_trait::async_trait]
impl ProviderClient for PatternGatewayClient {
    /// Streaming completion — default path for all callers.
    ///
    /// Internally uses genai::Client::exec_chat_stream. Chunks forward to
    /// the caller as they arrive. The terminal `End` chunk carries the
    /// assembled `MessageContent` and captured `Usage` via StreamEnd.
    async fn complete(&self, req: CompletionRequest) -> Result<BoxStream<Result<CompletionChunk, ProviderError>>, ProviderError> {
        let auth = self.auth_resolver.resolve(&req.persona_id).await?;
        let shape = self.shaper.shape_request(req, &auth, self.session_uuid_rotator.current())?;
        let estimated_tokens = shape.estimated_tokens;
        self.rate_limiter.acquire_completion(estimated_tokens).await?;
        // Call genai_client.exec_chat_stream(...) with shape applied as
        // AuthData::RequestOverride carrying the Bearer + all shaper headers.
        // Wrap the returned genai::chat::ChatStream in our own stream adapter
        // that maps ChatStreamEvent → CompletionChunk and genai errors →
        // ProviderError.
        ...
    }

    async fn count_tokens(&self, req: &CompletionRequest) -> Result<TokenCount, ProviderError> {
        let auth = self.auth_resolver.resolve(&req.persona_id).await?;
        let request = CountTokensRequest::from_completion(req, &self.shaper)?;
        self.token_counter.count(&auth, &self.shaper, &request).await
    }

    fn usage(&self, response: &CompletionResponse) -> Usage { ... }
}

impl PatternGatewayClient {
    /// Convenience helper for callers that want the assembled content post-stream
    /// rather than iterating chunks themselves. Internally drives `complete`'s
    /// stream and collects chunks into `(MessageContent, Usage)`.
    ///
    /// Used by pattern_runtime's MessageHandler: agent Haskell programs see
    /// `Message.Ask` as a one-shot; this helper is where the streaming-underneath
    /// gets hidden. An optional `on_chunk` callback forwards chunks to a
    /// Display-handler subscriber as they arrive, so human-visible streaming
    /// (CLI typewriter effect, UX layers) works even though the Haskell agent
    /// only sees the final value.
    pub async fn complete_collected(
        &self,
        req: CompletionRequest,
        mut on_chunk: impl FnMut(&CompletionChunk) + Send,
    ) -> Result<(MessageContent, Usage), ProviderError> {
        use futures::StreamExt;
        let mut stream = self.complete(req).await?;
        let mut final_content: Option<MessageContent> = None;
        let mut final_usage: Option<Usage> = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            on_chunk(&chunk);
            if let CompletionChunk::End { content, usage } = &chunk {
                final_content = Some(content.clone());
                final_usage = Some(usage.clone());
            }
        }
        Ok((
            final_content.ok_or(ProviderError::StreamEndedWithoutFinalContent)?,
            final_usage.unwrap_or_default(),
        ))
    }
}
```

**CompletionChunk shape** maps genai's `ChatStreamEvent` 1:1 with a terminal `End` variant that carries the captured content and usage:

```rust
// pattern_core::types::provider (exposed to trait consumers)
#[derive(Debug, Clone)]
pub enum CompletionChunk {
    Text(String),
    Reasoning(String),
    ThoughtSignature(String),
    /// Tool-call argument fragment; multiple chunks assemble into one ToolCall.
    ToolCallChunk { call_id: String, name: String, args_delta: String },
    /// Terminal event. Carries the fully assembled content and captured usage.
    End { content: MessageContent, usage: Usage },
}
```

**Tool-call re-submission for the next turn** uses genai's helper directly — `ChatRequest::append_tool_use_from_stream_end(end, tool_response)` handles the ordering (thought-signatures → text → tool-calls in captured order, then the user-role ToolResponse). Pattern's MessageHandler calls this when resubmitting after a tool-call turn — no pattern-side ordering logic needed.

**Step 2: Error translation**

Map every genai error variant to pattern_core's `ProviderError`:
- 401 → `AuthFailed` (if Bearer rejected) or re-resolve through refresh flow
- 429 → handled by rate-limiter already; genai-surfaced 429 means server-side enforcement, wait + retry
- 500+ → `ServerError { status, body }`
- Network → `NetworkError { source }`

**Step 3: Streaming event translation**

Map `genai::chat::ChatStreamEvent` to Pattern's `CompletionChunk`. Preserve: text (`Chunk`), reasoning (`ReasoningChunk`), thought-signature (`ThoughtSignatureChunk`), tool-call chunks (`ToolCallChunk`), terminal `End` with captured content and usage (from `StreamEnd.captured_content` + `StreamEnd.captured_usage`).

**Commit:**

```bash
jj describe -m "[pattern-provider] PatternGatewayClient: ProviderClient impl wiring resolver + shaper + ratelimit + count_tokens"
jj new
```
<!-- END_TASK_18 -->

<!-- START_TASK_19 -->
### Task 19: wiremock integration suite

**Verifies:** covers all Phase 4 ACs in mock — live tests in Task 21.

**Files:**
- Create: `crates/pattern_provider/tests/provider_client_integration.rs`

**Implementation:**

Build a comprehensive wiremock setup that covers:
- Session-pickup flow (credentials.json in a tempdir, resolver picks up, sends a request)
- Stored OAuth flow (creds_store returns a valid token, request uses it)
- PKCE refresh flow (near-expiry token triggers refresh, new token stored, subsequent request uses it, concurrent refresh serializes)
- API-key flow (env var set, tier picks up)
- Rate limit behaviour: TPM exhaustion → retry, TPD exhaustion → long wait, count-tokens and completion buckets independent
- Error paths: token_count 500 → TokenCountFailed; refresh 500 → RefreshFailed; no auth → NoAuthAvailable

Each test asserts:
- Which tier was used (auth.source)
- Outbound headers match expectations (honest-pattern identification)
- System prompt shape matches ShaperCompatMode

**Commit:**

```bash
jj describe -m "[pattern-provider] wiremock integration suite covering all AC3/4/5/5b paths"
jj new
```
<!-- END_TASK_19 -->

<!-- START_TASK_20 -->
### Task 20: Phase 4 verification — default shaper mode decision via manual CLI spike

**Verifies:** Phase 4 default `ShaperCompatMode` decision.

**Rationale:** No `live_subscription_verification.rs` test file. The decision "does `HonestPattern` mode work against subscription tier, or do we need `SubscriptionRoutingShape`?" is a human-driven empirical question answered by running the CLI manually in each mode and observing the outcome.

**Procedure** (manual):

1. Build pattern-v3 bin.
2. In one run: configure the session with `ShaperCompatMode::HonestPattern` (via a `PATTERN_SHAPER_MODE=honest` env var the bin reads, or via a `--shaper-mode honest` CLI flag if added during Phase 6). Run the AC9.2 checklist Step 1 — just a single turn.
   - If turn 1 returns 200 and the agent responds: `HonestPattern` works. Flip `ShaperCompatMode::default()` to `HonestPattern` in the source; commit.
   - If turn 1 fails with 401/403/429/4xx: record the status + response body in the commit message. Keep `SubscriptionRoutingShape` as default.
3. In a second run: configure with `SubscriptionRoutingShape` (the current default). Confirm it works.

**Decision documentation** lands in a commit with the empirical result. The `pattern_provider/CLAUDE.md` file's ShaperCompatMode section reflects the decision.

**Files modified (conditionally):**
- `crates/pattern_provider/src/shaper/compat_mode.rs` — `Default` impl flipped if HonestPattern works, unchanged otherwise.
- `crates/pattern_provider/CLAUDE.md` — decision + observed status codes documented.

**Commit:**

```bash
jj describe -m "[pattern-provider] shaper default decision — empirical result from manual CLI spike

Tested <HonestPattern | SubscriptionRoutingShape> against subscription tier.
Outcome: <200 / 401 / 403 / 429 / ...>
Default ShaperCompatMode: <HonestPattern | SubscriptionRoutingShape>"
jj new
```
<!-- END_TASK_20 -->

<!-- START_TASK_21 -->
### Task 21: Zero-warning close + audit + doc

**Verifies:** cleanliness gates.

**Step 1: Compile check**

```bash
cargo check -p pattern_provider 2>&1 | tee /tmp/phase4-check.log
cargo clippy -p pattern_provider --all-features --all-targets -- -D warnings 2>&1 | tee /tmp/phase4-clippy.log
cargo doc -p pattern_provider --no-deps 2>&1 | tee /tmp/phase4-doc.log
```

All three zero-warning.

**Step 2: Update `crates/pattern_provider/CLAUDE.md`**

Document:
- ShaperCompatMode semantics + why claude-code-literal in system[0] is structural not identity
- Beta header allowlist + denylist
- Three-tier auth order + mutex serialization
- `tidepool-extract` is NOT relevant here (that's pattern_runtime)
- How to verify live auth paths: documented pointer to AC9.1/9.2 CLI checklists (no env-gated live test suite)
- `<system-reminder>` tag convention + where it's used

**Step 3: Audit script**

```bash
bash scripts/audit-rewrite-state.sh
```

Must pass. All `todo!()` carry phase/AC refs. No fate markers on brand-new pattern_provider code.

**Step 4: Full test suite**

```bash
cargo nextest run -p pattern_provider 2>&1 | tail
cargo test --doc -p pattern_provider
```

All pass (no live-credential tests exist in pattern_provider; live paths are exercised via the Phase 6 CLI checklist).

**Commit:**

```bash
jj describe -m "[pattern-provider] phase 4 close: zero warnings on check/clippy/doc; audit clean

AC3.1-3.6 session-pickup: PASS
AC4.1-4.7 PKCE + API key + resolver + refresh mutex: PASS
AC5.1-5.7 shaper + rate limiter + session UUID: PASS
AC5b.1-5b.5 count_tokens + usage + separate buckets: PASS"
jj new
```
<!-- END_TASK_21 -->
<!-- END_SUBCOMPONENT_F -->

---

## Phase 4 "Done when" checklist

- [ ] rust-genai fork rebased onto current upstream; fork-side patches reduced to: system-prompt-array + version bump + (conditional) Opus/Sonnet 4.7 model IDs
- [ ] pattern_auth directory deleted in a dedicated retirement commit; `ProviderOAuthToken` now lives in `pattern_core::types::provider`
- [ ] Three-tier auth resolver with session-pickup (`.credentials.json` primary + `session.json` legacy compat), PKCE, API key
- [ ] Per-persona refresh mutex serialization (AC4.7 verified)
- [ ] Keyring + JSON fallback credential store (0600/0700 perms verified)
- [ ] `RequestShaper` with `ShaperCompatMode::{HonestPattern, SubscriptionRoutingShape, FullSurfaceImpersonation(todo)}`
- [ ] Beta header registry exclusion of `claude-code-20250219` and other claude-code-specific markers
- [ ] `<system-reminder>` tag helper in the shaper for user-message metadata injection
- [ ] Per-provider rate limiter with separate buckets for chat + count_tokens (AC5.*/5b.5)
- [ ] `count_tokens` async wrapper against `/v1/messages/count_tokens`
- [ ] `usage` field capture from chat responses (+ streaming end event)
- [ ] `PatternGatewayClient` implements `pattern_core::traits::ProviderClient`
- [ ] wiremock integration suite covers all AC paths
- [ ] Live subscription auth verification (Task 20) determines default `ShaperCompatMode`
- [ ] All tests pass (`cargo nextest run -p pattern_provider`)
- [ ] `cargo check -p pattern_provider`, `cargo clippy`, `cargo doc` all zero-warning
- [ ] `bash scripts/audit-rewrite-state.sh` passes
- [ ] `just pre-commit-all` passes

## What this phase deliberately does NOT do

- Does not implement `SdkLocation::Embedded` / `Auto` (Phase 3 scope; not changed here).
- Does not implement `ShaperCompatMode::FullSurfaceImpersonation` (cliproxy-level impersonation — cch signing, fingerprint salt, TLS spoofing, tool-name remapping). Declared for API stability only. Shipping requires future design conversation with explicit sign-off.
- Does not migrate compaction call sites to consume `ProviderClient::count_tokens` — that's Phase 5's job (AC5b.3 is covered by the API existing for Phase 5 to consume).
- Does not implement automatic localhost-callback OAuth flow. Manual paste stays default. Future polish plan may use `jacquard-oauth`'s `loopback` feature.
- Does not add a profile-fetch endpoint wrapper (e.g., `/v1/oauth/profile` for subscription tier / rate-limit-tier discovery). Out of scope for foundation; may return via a future tier-auto-detect plan.
- Does not implement OpenAI or Gemini providers. ProviderClient trait shape accommodates them; concrete impls are out of foundation scope.
- Does not ship rate-limit tier detection (which Anthropic tier the user is on). `AnthropicTier::Tier1` is the default; callers override via `ProviderRateLimiter::new_anthropic(tier)` if they know better.
- Does not implement refresh-token expiry tracking beyond "refresh happens near expiry." If refresh token itself expires, user re-auths via PKCE flow.
- Does not surface the dual billing mode (subscription-only vs subscription+extra-usage). Pattern's behaviour is the same regardless; the distinction is Anthropic-side billing.
- Does not build adaptive base-prompt templating. Slot [1] accepts a full string override via `ShaperConfig::system_instructions_override`, but structured editing on top of `DEFAULT_BASE_INSTRUCTIONS` (inspired by pre-v3's retired `prompt_template` crate, rethought for Pattern's needs) is deliberate future-work scope. Holding design space for it in `docs/plans/rewrite-v3-portlist.md` under "Future design plans" so it's not forgotten when we decide to invest in the UX.
