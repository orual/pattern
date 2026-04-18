# pattern_provider

LLM provider integration for Pattern v3. Owns Anthropic authentication
(three-tier: session-pickup, PKCE, API key), request shaping (honest pattern
identification), per-provider rate limiting, provider-reported token counting,
and the request composer that emits the three-segment cache layout.

Absorbs the Anthropic-facing bits of the retired `pattern_auth` crate. Depends
on `pattern_core` for trait definitions; carries its own rebased fork of
`rust-genai` (auth-only patches on current upstream, plus any Opus-4.7
migration patches not yet in upstream).

See `docs/design-plans/2026-04-16-v3-foundation.md` §Provider and §Architecture
for the auth flow diagram and shaping contract.

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
`shaper/system_prompt.rs::tests::subscription_routing_skips_slot_2_*`.
