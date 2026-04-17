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
