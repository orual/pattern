# pattern_provider

LLM provider integration for Pattern v3. Owns Anthropic authentication
(three-tier: session-pickup, PKCE, API key), request shaping (honest pattern
identification), per-provider rate limiting, provider-reported token counting,
and the request composer that emits the three-segment cache layout.

Last verified: 2026-04-19

Absorbs the Anthropic-facing bits of the retired `pattern_auth` crate. Depends
on `pattern_core` for trait definitions; carries its own rebased fork of
`rust-genai` (auth-only patches on current upstream, plus any Opus-4.7
migration patches not yet in upstream).

See `docs/design-plans/2026-04-16-v3-foundation.md` §Provider and §Architecture
for the auth flow diagram and shaping contract.

## Anthropic auth chain — tier order

`AnthropicAuthChain::resolve()` tries tiers in this order:

1. **Stored OAuth** (keyring primary, JSON fallback). Pattern's own
   PKCE-minted token. Most explicit user intent — they ran `pattern auth`
   and deliberately stored a token.
2. **API key** (`ANTHROPIC_API_KEY` env var, loaded via dotenvy in
   `pattern-test-cli` when a `.env` is present). Env-level user choice.
   Takes precedence over session-pickup so `ANTHROPIC_API_KEY=sk-…` in a
   `.env` actually works without requiring the user to shuffle claude-code
   state.
3. **Session-pickup** (reads `~/.claude/.credentials.json`, matching the
   `claudeAiOauth` wrapper verified on 2026-04-17). Ambient fallback —
   use whatever claude-code happens to be authed against when neither of
   the explicit tiers resolves.

**Rationale:** explicit-over-ambient matches Unix convention and the
mental model every other Anthropic SDK (python, TS) imposes — env vars
win, config is convenience. The only twist is that pattern's own
stored OAuth trumps even the env var, because that token was obtained
via a deliberate PKCE flow the user performed; silently overriding it
because an env var happens to be set would erase their deliberate action.

**Observability.** `pattern-test-cli auth` always prints which tier
resolved, so users can verify their environment without guessing. The
gateway also logs the tier at `info` level on each resolve for request
correlation.

**Footgun mitigation.** User with both a claude-code session AND an
`ANTHROPIC_API_KEY` env var gets charged API credit, not subscription
quota. This is the Unix-convention-correct behaviour but can surprise.
The `auth` command's tier printout is the documented way to check.

### AuthTier::StoredOauth (split from Pkce)

Previously both fresh PKCE resolutions and stored-OAuth lookups returned
`AuthTier::Pkce`. These are now distinct: `AuthTier::StoredOauth` is
returned when the token comes from keyring/JSON storage, while
`AuthTier::Pkce` is reserved for a fresh interactive PKCE flow. This
matters for observability (the `auth` command and gateway logs report
the actual resolution path) and for beta-header decisions (both tiers
emit `oauth-2025-04-20`).

### Tier-forcing entry points

`AnthropicAuthChain::session_pickup_only()` and
`AnthropicAuthChain::pkce_only()` construct chains where all other tiers
return `None`, forcing resolution to a specific path. Used by
`pattern-test-cli spawn --auth <tier>`. `ApiKeyTier::disabled()` and
`SessionPickupTier::noop()` are the building blocks. `MemOnlyCredsStore`
provides an in-memory-only credential store for test chains that should
not touch the real keyring.

## ShaperCompatMode — empirical decision (verified 2026-04-17)

Phase 4 Task 20 required an empirical test of `HonestPattern` vs.
`SubscriptionRoutingShape` against a real Anthropic subscription tier.
Procedure: send a single-turn `ask` request in each mode via
`pattern-test-cli` with a live subscription OAuth token (session-pickup
from `~/.claude/.credentials.json`).

**Outcome:**

- `SubscriptionRoutingShape` → **200 + content**. Works as the documented
  structural requirement suggests.
- `HonestPattern` → **429 Too Many Requests**, despite having substantial
  5-hour subscription budget remaining. Running the test twice in the
  same session with `SubscriptionRoutingShape` succeeding in between
  rules out raw quota exhaustion.

**Interpretation:** the `"You are Claude Code, …"` literal in slot[0]
is not cosmetic, and not just a structural filter — it's what Anthropic's
subscription router reads to decide which quota bucket to charge the
request against. Without it in slot[0], requests route to the
pay-as-you-go pricing tier which a Max subscription has zero credit on;
the router surfaces this as 429 rather than 403 or a quota-specific error.

**Decision:** `ShaperCompatMode::default()` stays at
`SubscriptionRoutingShape` under `subscription-oauth`. `HonestPattern`
is retained for API-key-auth builds (`--no-default-features`) where
subscription routing doesn't apply. `FullSurfaceImpersonation` remains
unimplemented and requires explicit sign-off before any future work.

**Honest framing preserved:** slot[0]'s claude-code literal is a structural
Anthropic-side requirement for subscription-tier routing, not an identity
claim. Pattern's real identity and behaviour live in slot[1]
(`"You are NOT Claude Code." + DEFAULT_BASE_INSTRUCTIONS`) and slot[2]
(the persona block). Agents reading slot[0] should understand it as a
routing token, not a self-description.

## Shape-vs-empty invariants

Anthropic rejects outbound requests when any system array entry has
empty `text` ("system: text content blocks must be non-empty"). The
shaper's `build_system_prompt` drops empty fragments before joining —
empty persona + empty extras in `SubscriptionRoutingShape` produces a
two-block system (slot[0] + slot[1]), not a three-block system with an
empty slot[2]. Tests pin this behaviour in
`shaper/anthropic/system_prompt.rs::tests::subscription_routing_skips_slot_2_*`.

## Beta-header allow / deny list

The `Anthropic-Beta` value is curated per-request by
`shaper::anthropic::headers::build_beta_header_value`. This function is the
**single source of truth** for the full header value — do NOT emit
`anthropic-beta` from `gateway::auth_headers_for_tier` or any other
path, as `BTreeMap::extend` is last-insert-wins per key and would
silently overwrite the shaper's capability markers.

**Auth-tier-conditional** (lives in `shaper::anthropic::headers::build_beta_header_value`
alongside the capability markers — NOT in `auth_headers_for_tier`):

- `oauth-2025-04-20` — emitted for the PKCE + session-pickup tiers so
  Anthropic routes the call via its OAuth path. Never emitted for
  API-key auth. Must appear in the same comma-joined value as any
  capability markers (e.g. `prompt-caching-scope-2026-01-05`) so they
  coexist in a single header rather than overwriting each other.

**Capability-conditional** (shaper, driven by `ShaperConfig` flags +
model inspection):

- `prompt-caching-scope-2026-01-05` — always on for first-party traffic.
- `interleaved-thinking-2025-05-14` — claude-4 opus/sonnet + config opt-in.
- `dev-full-thinking-2025-05-14` — claude-4 opus/sonnet + config opt-in.
- `context-management-2025-06-27` — any claude-4-* model + config opt-in.
- `extended-cache-ttl-2025-04-11` — config opt-in, model-agnostic.
- `context-1m-2025-08-07` — specific 1M-context models + config opt-in.

**Permanent deny list** (`BANNED_BETA_MARKERS`, enforced both at
`ShaperConfig::validate` and at emit time as defense-in-depth):

- `claude-code-20250219`
- `cli-internal-2026-02-09`
- `summarize-connector-text-2026-03-13`
- `token-efficient-tools-2026-03-28`

These are Anthropic's internal CLI markers. Pattern is a distinct
client and emits none of them regardless of config. The deny list is
enforced both at `ShaperConfig::validate` time and as a defense-in-depth
strip inside `build_beta_header_value` before joining.

## Refresh-mutex serialization (AC4.7)

`AnthropicAuthChain` holds a single `tokio::sync::Mutex<()>` guarding
the OAuth refresh path. When multiple persona requests arrive at the
same near-expiry token, the first to acquire the mutex performs the
network round trip + writes the new token to `CredsStore`; subsequent
tasks re-read the store post-lock and observe the fresh token without
duplicating the refresh. Unit tests cover the single-path,
concurrent-refresh-serialization, and refresh-failure cases in
`auth::resolver::tests::oauth_chain`.

## `<system-reminder>` tag helper

`shaper::wrap_system_reminder(content: &str) -> String` wraps arbitrary
content in `<system-reminder>...</system-reminder>` tags. Meant for
the Phase 5 composer to inject transient per-turn metadata (e.g. token
pressure warnings, tool-result framing) into a user-message position
where Anthropic treats the tag as system-side framing rather than
persona identity. Phase 4 ships the helper + a round-trip test; Phase 5
wires it into the composer.

## Composer pipeline (`compose/`)

The composer assembles a `CompletionRequest` from a sequence of
`ComposerPass` implementations applied to a `PartialRequest`. Each
pass appends content and places one cache-breakpoint marker. The
canonical three-pass layout:

1. **`Segment1Pass`** — system prompt (via shaper) + tool schemas.
2. **`Segment2Pass`** — summary-head messages + prior-turn history +
   memory-change pseudo-messages (block writes).
3. **`Segment3Pass`** — `[memory:current_state]` pseudo-turn (rendered
   block content).

After all passes, the caller appends fresh user input (uncached), then
`finalize` applies breakpoint markers and assembles the final
`CompletionRequest`.

**Important:** the agent loop in `pattern_runtime` no longer uses
`Segment3Pass` at compose time. Memory snapshots are instead attached
as `MessageAttachment::BatchOpeningSnapshot` on batch-opening user
messages and spliced onto the wire post-compose. `Segment3Pass` remains
in this crate for standalone compose-pipeline tests and as the
reference implementation. See `crates/pattern_runtime/CLAUDE.md` for
the batch-anchored snapshot architecture.

### Segment2Pass MessageId origin tagging

`Segment2Pass` accepts `prior_messages` as `Vec<(SmolStr, ChatMessage)>`
and tags each with its Pattern `MessageId` via
`PartialRequest::push_message(msg, Some(id))`. Summary-head and
pseudo-messages are tagged with `None`. The parallel
`PartialRequest.message_origins` vector is returned alongside the
finalized request in `ComposeOutput.message_origins`, which the runtime
uses for attachment splicing by MessageId lookup instead of index math.

### CacheProfile latching

`CacheProfile` is computed once at session open and used for all turns
in that session. The profile determines which cache-control markers
(`ephemeral`, `breakpoint`) are placed by each pass. Changing the
profile mid-session would shift breakpoint positions and bust the cache
(see break-detection below).

### Break-detection (`compose/break_detection.rs`)

`BreakDetectionSnapshot` is a cheap per-turn hash snapshot of
cache-bust-sensitive dimensions: system content, cache_control markers,
tools, beta headers, model, and message-level markers. Diffing two
consecutive snapshots attributes an unexpected `cache_read_input_tokens`
drop to the specific subsystem that changed, surfaced as a single
`tracing::warn!` line.

Phase 5 added `message_markers_hash` and `compute_from_chat()` to
capture post-compose message-level marker state (including any markers
the agent loop's splice logic adds). This covers the gap between
compose-time intent (from `BreakpointTracker`) and actualised wire
state (from `ChatRequest.messages`).

## What lives elsewhere

- `tidepool-extract` / GHC plugin binary — `pattern_runtime` concern, not
  this crate. See `crates/pattern_runtime/CLAUDE.md`.
- Turn-loop / checkpoint machinery — `pattern_runtime`.
- Compaction + memory-block composer — `pattern_core` (Phase 5 wires
  the composer against this crate's `ProviderClient::count_tokens`).

## Verifying live auth paths

No env-gated live-credential test suite exists in this crate — live
paths are exercised manually via `pattern-test-cli` in `pattern_runtime`:

```sh
# Show which tier resolves (session-pickup / stored-oauth / api-key),
# print the token prefix + expiry.
cargo run -p pattern-runtime --bin pattern-test-cli -- auth

# One-shot completion through the full stack.
cargo run -p pattern-runtime --bin pattern-test-cli -- \
    ask --shaper subscription "hello?"

# Clear pattern's stored PKCE token (keyring + JSON fallback).
cargo run -p pattern-runtime --bin pattern-test-cli -- clear
```

AC9.1/9.2 of the v3-foundation plan documents the checklist this CLI
satisfies.
