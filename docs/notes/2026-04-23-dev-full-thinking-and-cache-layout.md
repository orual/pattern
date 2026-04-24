# `dev-full-thinking-2025-05-14` and the cache breakpoint layout — research findings

**Date:** 2026-04-23
**Context:** Evaluation of whether to flip `ShaperConfig::enable_dev_full_thinking` default on, plus a comparison of Pattern's three-segment cache layout against pi-mono's coding-agent implementation.

## Decision

1. **Keep `enable_dev_full_thinking: false` as the default.** Add a per-persona KDL config (`thinking_display: "full" | "summarized"`, default `summarized`) that plumbs through to the existing `ShaperConfig` flag. Flip it on one persona at a time and measure `ratio` via the existing `[cache: fresh=N read=N create=N ratio=NN%]` REPL summary before considering a broader default.
2. **No changes to the three-segment cache breakpoint layout.** It is already tighter than pi-mono's four-breakpoint approach on every axis that matters (TTL tiering, session latching, break-detection, deny-listed beta markers). The unused 4th budget slot stays fallow — there's no clearly-valuable use under Pattern's current one-persona-per-session + per-session UUID architecture.

## What the header actually does

`dev-full-thinking-2025-05-14` is listed at line 343 of the anthropic-sdk-typescript `AnthropicBeta` union, one line below `interleaved-thinking-2025-05-14` (same ship date). It is not in Anthropic's public docs, but scraped SDK references across ecosystem mirrors describe it as:

> Developer Thinking — Claude 4 models — Raw thinking mode for developers. Requires `thinking.type: "enabled"` + `thinking.budget_tokens`; returns regular `content[*].type: "thinking"` blocks instead of summaries.

Default behaviour on Claude 4 is **summarized thinking**: raw reasoning happens, a post-hoc compression pass generates a summary, and the summary is what is returned (with a signature covering the summary). The header opts out of the summarization pass — the raw reasoning tokens come back, signature-wrapped, and can be replayed verbatim next turn.

### Why it could help cache performance

Two mechanisms, neither proven empirically yet:

1. **Less variance between turns.** Summarization is a separate generation pass — effectively non-deterministic compression. Even slight summarizer noise means the "thinking" bytes you replay across sessions differ, breaking prefix match on the server's block cache. Raw thinking is the actual tokens the model emitted; once signed, it's a stable blob. Stable replay → stable prefix → cache hits.
2. **Downstream behavioural cache.** The Feb 2026 `redact-thinking-2026-02-12` rollout correlated with measurably different Claude Code tool-use patterns (research-first → edit-first; analysis over 17,871 blocks across 6,852 sessions). Same logic applies to summarized vs. raw: richer replay context means the model's next-turn tool choices converge on the same patterns it used originally, instead of reconstructing from a lossy summary. Consistent tool patterns → consistent messages → consistent cache prefixes.

### Why we still shouldn't default it on yet

Open empirical questions the research can't answer:

- **Token-cost delta.** Raw thinking is larger than summaries. After turn 1 it's a cache-read (free on subscription, $0.1×-base on API), but turn 1 pays a bigger cache-write (1.25×-base at 5m TTL, 2×-base at 1h). For short conversations compaction floors cut in before amortization, which could net negative.
- **Subscription quota behaviour.** Raw thinking tokens count against the 5-hour bucket like any other. Anthropic could throttle more aggressively under full-thinking on subscription tier — pure empirical question, needs a measured session pair.
- **Interaction with `context-management-2025-06-27` thinking-block clearing.** Both flags target thinking. Clearing under full-thinking has different semantics than clearing under summaries. Not decided if we'd want both on simultaneously.

## The byte-exact signature invariant

pi-mono shipped a coding agent with a `sanitizeSurrogates()` pass that strips unpaired UTF-16 surrogates from signed thinking text on the outbound path. When triggered (emoji in prior context, aborted streams, proxy re-encoding) the text mutates while the signature stays original, and Anthropic rejects the request with `"thinking or redacted_thinking blocks cannot be modified"`.

What this bug *reveals* about Anthropic's server is the genuinely useful finding:

1. **Thinking signatures are validated byte-exact** against the paired thinking text. Not semantically, not normalized — byte-for-byte. Any mutation (Unicode normalization, whitespace trim, surrogate strip, JSON escape-style drift) invalidates.
2. **Everything *around* the thinking block can be mutated freely.** Rewrite system, reorder tools, rewrite prose blocks — as long as `{thinking, signature}` is preserved byte-exact, the server caches the block independently of surrounding context. This is what makes Opus 4.5+'s "preserves thinking blocks across turns by default" behaviour a genuine cache win: the server uses the signature to confirm provenance, then caches the block independently of the prose around it.

### Implication for Pattern's provider layer

"Signed content is immutable" is a hard boundary. In Rust this means:

- Thinking-block text must travel from inbound SSE decode to outbound HTTP POST with zero intermediate transformations. No `String::trim`, no `unicode_normalization::normalize`, no stringify-and-reparse that could alter JSON escaping.
- Unicode normalization, if ever needed, runs on the *inbound* side — user text, tool results, system prompt — things Pattern owns. Never on replay of model-issued signed content.
- The genai fork's `ThinkingBlock` carries `text: Option<String>` + `signature: Option<String>`. `String` guarantees valid UTF-8 at the Rust layer (no invalid surrogates to strip in the first place), and `serde_json` round-trip preserves strings byte-exact.

An audit of Pattern's hot path confirmed no normalization libraries (`unicode_normalization`, `unicode_segmentation`) on the signed-content path. The `splice_text_onto_message` helper at `pattern_runtime/src/agent_loop.rs:1449` matches on `ChatRole::Tool` and falls through to User/System — never mutates assistant messages. Defensive hygiene opportunity: narrow the match to `ChatRole::User | ChatRole::System` and error on other roles, to prevent future callers from accidentally splicing onto assistant content.

One latent issue was found and fixed in rust-genai during this work — see commit `91c93a9` on the `fix/thinking-signature-roundtrip` branch. Details are in the commit message.

## Cache breakpoint layout — Pattern vs pi-mono

Confirmed by source read of `../pi-mono/packages/ai/src/providers/anthropic.ts` vs `pattern_provider/src/compose/passes/segment_{1,2,3}.rs`.

| Axis | pi-mono | Pattern |
|---|---|---|
| Breakpoints per request | 4 (fixed) | 3 active (seg 1/2 + post-splice), 4th budget slot unused |
| Marker positions | Two system blocks (OAuth identity + user system), last tool, last user message | Last system block, last prior-turn message, last spliced-attachment message |
| TTL policy | Single TTL per request (`cache_control.ttl` per marker) | Per-segment tiering — seg 1 at 1h (stable), seg 2/3 at 5m (volatile) |
| TTL mid-session stability | Not latched — TTL flips per-request | **Latched at session open** via `CacheProfile` — prevents mid-session TTL-flip cache busts |
| Cache-miss attribution | None | `BreakDetectionSnapshot` hashes system content, cache_control markers, tools, beta headers, model, per-message markers; diff between turns surfaces the specific subsystem responsible for an unexpected miss |
| Beta-header hygiene | Actively spoofs `claude-code-20250219` + `oauth-2025-04-20` + Claude Code's user-agent to piggyback on subscription routing | Deny list at `BANNED_BETA_MARKERS` prevents Pattern from ever emitting Anthropic's internal CLI markers; enforced at both `ShaperConfig::validate` and emit time |
| `dev-full-thinking-2025-05-14` | Not referenced anywhere in the repo | Plumbed as a capability-conditional beta (currently opt-in, defaulting to off) |
| Post-compaction cache handling | None | `ProviderClient::rotate_session_uuid()` on compaction to prevent the server's prefix cache from confusing post-compaction context with pre-compaction context |

### Choices pi-mono makes that Pattern deliberately doesn't

**Marker on last tool in `tools[]`.** Under Anthropic's prefix order (tools → system → messages), this isolates the tools cache from system-prompt content changes. Genuinely useful if: (a) a constellation shares tools across personas with different system prompts, AND (b) they share a session UUID. Under Pattern's one-persona-per-session architecture neither holds, so this isolation buys nothing. Revisit only if multi-persona-one-session becomes a real use case.

**Marker on the current turn's fresh user input** (pi-mono places its last marker on the freshest user message, not the last prior message). Shifts caching forward by one turn — the freshest user content enters the cache on the turn it's submitted rather than the turn after. Token accounting: one turn of user-message tokens shifts from "write on turn N+1" to "write on turn N". Net wash in long conversations, slight waste in 2-3-turn exchanges where content never gets re-read. Not worth changing the composition contract for.

**Two markers on system blocks.** Only meaningful if identity is stable while user-system varies mid-session. Pattern's `SubscriptionRoutingShape` has slot[0]/slot[1]/slot[2] all stable within a session (persona doesn't change), so a marker on slot[0] alone gives no additional cache isolation.

### What Pattern could still audit

1. **Continuation turns have only 2 active markers** (`agent_loop.rs:1431` skips the seg3 marker when `last_spliced_idx` is `None`). Probably fine — continuation turns share the prior request's prefix so existing 5m reads still hit — but worth pinning with an inline comment explaining *why* the skip is intentional.
2. **The Segment1 marker placement invariant** assumes stable composition order (last system block = persona slot). If the shaper ever inserts a 4th block (e.g., appending a `<system-reminder>` into system blocks) the marker shifts index and the invariant breaks silently. A `debug_assert_eq!` on the block's semantic type at the marked index, or a `stable_prefix: bool` tag on `SystemBlock`, would catch this at test time.
3. **The 4th budget slot** stays unused. Candidate: a marker on the last tool with `Ephemeral1h` as cheap insurance. Unlikely to change the cache ratio measurably under current architecture, but costs nothing to add and zero downside.

## Sequence for flipping the header on

Assuming the rust-genai fix has been merged into `rebase/pattern-v3-foundation`:

1. **Add persona-KDL config flag** — `thinking_display: "full" | "summarized"` in persona KDL, default `"summarized"`. Plumb to `ShaperConfig::enable_dev_full_thinking` at session open.
2. **Narrow `splice_text_onto_message` role match** (defense-in-depth) — match on `ChatRole::User | ChatRole::System` only, error on others. Prevents a future caller from accidentally splicing onto assistant content.
3. **Measure a matched pair of sessions** — same persona, same prompts, one with `thinking_display: "full"` and one with `"summarized"`. Compare `ratio` across a representative turn sequence (including tool-use and compaction if possible). `BreakDetectionSnapshot` will surface any unexpected invalidation.
4. **If measurement is positive**, consider defaulting to `"full"` for Anthropic-backed personas. If negative or wash, leave it as an opt-in for sessions that specifically want richer reasoning replay (e.g., long-horizon coding assistance).

## Sources

- [anthropic-sdk-typescript `AnthropicBeta` union (line 343)](https://github.com/anthropics/anthropic-sdk-typescript/blob/22cb810364debf9f9c1b18ecaf8d9364c0e535c5/src/resources/beta/beta.ts#L400)
- [Building with extended thinking — Claude API Docs](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
- [Prompt caching — Claude API Docs](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [wenerme/wener — Anthropic beta headers table](https://github.com/wenerme/wener/blob/master/notes/ai/maas/anthropic.md) (community mirror)
- [LiteLLM Bedrock docs — "Developer Thinking: Claude 4 models — Raw thinking mode for developers"](https://docs.litellm.ai/docs/providers/bedrock)
- [claude-code issue #42796 — redact-thinking rollout correlates with tool-use regression](https://github.com/anthropics/claude-code/issues/42796) — quantitative analysis of 17,871 thinking blocks across 6,852 sessions
- [openclaw issue #24612 — pi-ai `sanitizeSurrogates()` invalidates signed thinking](https://github.com/openclaw/openclaw/issues/24612)
- [opencode issue #16748 — `normalizeMessages()` strips empty parts between reasoning blocks, invalidating positional signatures](https://github.com/anomalyco/opencode/issues/16748)
- [Anthropic loses Claude Code trust in black-box fight — implicator.ai](https://www.implicator.ai/claude-probably-wasnt-secretly-nerfed-anthropic-made-the-black-box-too-dark/)
- [Gemini Thought Signatures — field on functionCall Part](https://ai.google.dev/gemini-api/docs/thought-signatures) (for the cross-provider shape comparison)
