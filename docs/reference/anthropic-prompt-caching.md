# Anthropic prompt caching reference

**Status:** reference / as-observed. Pattern v3 Phase 5 uses a subset of this.
**Sources:** `/home/orual/Git_Repos/claude-code/services/api/{claude.ts, promptCacheBreakDetection.ts}` (2026-04-16), Anthropic docs, cross-reference with `/home/orual/Projects/PatternProject/tidepool/` and `docs/reference/oauth-and-detection.md`.

This doc captures claude-code's cache_control implementation as a reference point. Pattern's three-segment cache layout (v3 foundation Phase 5) uses a simpler subset; this doc is for when we want to extend or tune later.

## The `cache_control` marker

Shape:
```typescript
{
  type: 'ephemeral',         // only valid value
  ttl?: '1h',                // omitted = 5-minute default
  scope?: 'global',          // omitted = implicit 'org'
}
```

Per-marker, not per-request. Attach to any `TextBlockParam` (system) or `MessageParam` (user/assistant) block.

## TTL variants

- **Default 5-minute**: omit the `ttl` field. Cheaper write (1.25× base input token cost). Standard for most caching.
- **1-hour extended**: `ttl: '1h'`. More expensive write (2× base input), but useful for content that's stable across many turns (system prompts, tool definitions, persona blocks).
- **24-hour** (`ttl: '24h'`): extended further. Even more expensive write. Pattern hasn't identified a use case requiring 24h yet.

**Reads** are 0.1× base input cost regardless of TTL.

**Beta header required for non-default TTLs**: `anthropic-beta: extended-cache-ttl-2025-04-11`. Pattern's Phase 4 shaper injects this conditionally when a 1h TTL is observed in any block's cache_control.

### Session-stable TTL latching

Claude-code observation: **mid-session TTL flips bust the server-side prompt cache** (~20K tokens per flip, per the `promptCacheBreakDetection.ts:31-32` comment about scope/TTL flips). Their implementation latches TTL eligibility per session in bootstrap state; they don't re-evaluate mid-request even when underlying conditions (GrowthBook config, overage status) change.

**Pattern implication**: once a session is opened with a 1h TTL policy, don't flip to 5m mid-session. Conversely, once a 5m TTL policy is in place, don't upgrade mid-session. Decide at session open, latch, ship the same TTL for the duration.

## Scope

- **omitted (implicit `org`)**: cache entry is scoped to the org of the authenticating account. Only requests from the same org can hit the cache.
- **`scope: 'global'`** (explicit): cache entry is shared across orgs, scoped only by content hash. Hit rates go up at the cost of cross-org visibility — any request with matching prefix content from any other caller can share the cache entry.

Claude-code sends scope='global' explicitly for blocks that benefit from wider cache-hit (standard system prompts, tool schemas); omits scope for blocks that might benefit from per-org isolation.

**Pattern implication (revised after checking rust-genai upstream):** `scope` is **not currently supported** by upstream rust-genai — the `CacheControl` enum has TTL variants but no scope field. Attempting to send `scope` would require a fork patch or upstream PR.

More importantly, `scope: 'global'` only matters for **cross-org** cache sharing. Pattern's agents within a single user's subscription are all in the same org, so `scope` omitted (implicit org) already covers the "constellation agents share cache" case. Cross-org sharing (different Anthropic orgs hitting the same cached prefix) isn't Pattern's use case.

**Pattern decision**: Phase 5 defaults to scope omitted (implicit org), which matches what upstream rust-genai emits. `CacheScope::Global` support is tracked as future work under "Not shipped in v3 foundation" — small fork patch if we ever discover a multi-org deployment use case that benefits from it.

## Breakpoint budget

Anthropic's API allows **max 4 `cache_control` markers per request**. Pattern's three-segment layout uses 3:

1. Last system block (segment 1 boundary) — typically 1h TTL, global scope
2. Last stable message in history (segment 2 boundary) — 5m TTL
3. Memory pseudo-turn (segment 3 boundary) — 5m TTL

Leaves one breakpoint available for future use (e.g., a second system-block boundary separating very-stable from moderately-stable content).

Claude-code's `buildSystemPromptBlocks` carries a comment: *"Do not add any more blocks for caching or you will get a 400"* — they're at or near the 4-breakpoint limit. Attempting a 5th returns a 400 error. Pattern's composer validates at composition time (per AC7.5).

## Claude-code's one-message-level-marker rule

Claude-code places **exactly one** message-level `cache_control` marker per request, on `messages[messages.length - 1]` (the last message). For fire-and-forget forks (`skipCacheWrite=true`), they shift to `messages.length - 2` instead. Rationale from `claude.ts:3078-3088`:

> *"Mycro's turn-to-turn eviction (`page_manager/index.rs: Index::insert`) frees local-attention KV pages at any cached prefix position NOT in `cache_store_int_token_boundaries`. With two markers the second-to-last position is protected and its locals survive an extra turn even though nothing will ever resume from there — with one marker they're freed immediately. For fire-and-forget forks we shift the marker to the second-to-last message: that's the last shared-prefix point, so the write is a no-op merge on mycro (entry already exists) and the fork doesn't leave its own tail in the KVCC."*

**This is an Anthropic-server-internal optimization, not an API constraint.** Pattern's three-segment layout uses two message-level markers (segment 2 boundary + segment 3 pseudo-turn boundary). The "local-attention KV page" survival cost applies but pattern isn't optimizing for claude-code's specific KV eviction pattern — we're optimizing for memory-edit cache preservation, a different trade-off. Two message markers is fine within Anthropic's API rules.

If cache costs become measurably higher than expected under Pattern's workload, revisit: could we collapse segments 2+3 into one boundary? Probably yes if the memory pseudo-turn is always the last non-user-input content (just place its content before the last stable history message's cache_control marker). Worth a measurement-driven iteration, not a design-time decision.

## `cache_reference`

Anthropic's `cache_reference` lets later requests stitch in previously-cached content blocks by referencing their cache entry without re-sending the content.

Claude-code usage (`claude.ts:3166-3207`):

- After placing the message-level `cache_control` marker, claude-code scans messages *before* the marker and adds `cache_reference: <tool_use_id>` to any `tool_result` block found there.
- Rule: `cache_reference` must appear "before or on" the last `cache_control` marker. Claude-code uses strict "before" to avoid edge cases with cache-edit splicing.
- Rationale: tool-result content is often large (shell outputs, file reads) and identical across turns. Referencing rather than re-sending saves tokens dramatically.

**Pattern implication**: `cache_reference` is a future-optimization. Phase 5 foundation does not implement it. Candidate future work if tool-result tokens become a measurable cost driver. Would require:
- Tracking which `tool_use_id`s have been cached in previous requests
- Pattern-side cache registry tracking live references
- Composer logic to emit `cache_reference` on tool_result blocks strictly before the last cache_control marker

Track in future compaction-enhancements plan; not v3 foundation scope.

## What breaks the cache (claude-code's break-detection list)

`promptCacheBreakDetection.ts:28-68` tracks these as cache-bust vectors. Pattern should avoid or latch each:

- **systemHash** change → system prompt content changed (segment 1 bust, expected when DEFAULT_BASE_INSTRUCTIONS or persona changes — rare)
- **toolsHash** change → tool schema changed (segment 1 bust)
- **cacheControlHash** change → TTL or scope flip (latched; pattern session-stable)
- **modelChanged** → `modelChanged: true` → segment 1 bust (expected on model switch)
- **betasChanged** → added/removed beta headers → segment 1 bust (pattern latches beta set at session open)
- **globalCacheStrategy change** → MCP tool discovery/removal → segment 1 bust
- **extraBodyChanged** → anthropic_internal config change → bust
- **effortChanged** → reasoning effort change → may or may not bust depending on how encoded
- **autoModeChanged, overageChanged, cachedMCChanged** → all latched to NOT bust in current claude-code; tracked to verify the fix holds

Pattern's equivalent latching surface (Phase 4 / 5):
- ShaperConfig fields (scope, TTL, beta set) latched at session open — never mid-session flip
- Tool registry snapshot locked per session (tools don't come and go mid-session)
- Persona block content can change between turns (that's the whole point of memory-edit invalidation), but segment 1 content stays stable

## Observability — cache-hit metrics

Anthropic's response `usage` field carries:

- `cache_creation_input_tokens` — tokens spent creating cache entries this turn (new content not previously cached)
- `cache_read_input_tokens` — tokens read from cache (cached content reused)
- `input_tokens` — fresh tokens this turn (not cached, not read from cache)

Pattern's Phase 5 verification (AC8) instruments per-segment cache-hit rates by:
- Capturing `cache_read_input_tokens` / `cache_creation_input_tokens` per turn
- Asserting that after a memory-block edit, segment 1 cache_read remains high (unchanged) while segment 3 cache_creation jumps (expected invalidation)
- If segment 1 cache_read drops unexpectedly, that's a segment-1 cache bust — metrics fire an alert

Pattern may want to export these metrics to tracing spans for telemetry. Future concern, not v3 foundation scope.

## Summary: what Pattern v3 Phase 5 ships

- `cache_control: { type: 'ephemeral', ttl: '1h' }` on last segment-1 block (system slot[2])
- `cache_control: { type: 'ephemeral' }` (5m default) on last stable segment-2 message
- `cache_control: { type: 'ephemeral' }` (5m default) on the `[memory:current_state]` pseudo-turn in segment 3
- TTL choice latched at session open, never flipped mid-session
- Scope omitted (implicit org) — matches upstream rust-genai's supported surface; single-user pattern constellations already share cache within the user's org
- 4th breakpoint reserved (leaves headroom for future extensions)
- No `cache_reference` yet; future optimization (requires upstream PR or fork patch)
- Per-segment cache-hit metrics captured from response usage for AC8 verification

## Not shipped in v3 foundation (architectural hooks exist, implementation deferred)

Phase 5's composer is a pipeline of passes with explicit extension points. The following features have attachment points in place but no implementation yet. Future plans can add them as additional passes or `CacheProfile` fields without refactoring the core composition logic.

- **`cache_reference` for tool_result stitching** — future compaction-enhancements plan. **Upstream rust-genai does not support `cache_reference` as of 2026-04-16** (verified); enabling it would require either upstream PR or a fork patch to add `cache_reference: Option<String>` to tool_result content blocks in `src/chat/tool.rs` and serialization in the Anthropic adapter. Hook from Pattern's side: a pass that runs after cache_control marker placement, scans messages strictly before the last marker, annotates `tool_result` blocks with `cache_reference: <tool_use_id>`.
- **Cache-deletions / microcompact `cache_edits`** (claude-code feature for deleting cached content mid-session) — future compaction-enhancements plan. Hook: another pass, sharing the breakpoint budget (may consume the reserved 4th breakpoint).
- **Per-tool cache hashing for cache-bust attribution** — future observability plan. Hook: `CacheProfile` gains a `per_tool_hashes: HashMap<String, u64>` field; break-detection gains a per-tool diff pass.
- **Session-level cache strategy flip logic** (claude-code's `globalCacheStrategy: 'tool_based' | 'system_prompt' | 'none'` based on MCP tool presence) — future MCP integration plan. Hook: `CacheProfile::strategy: CacheStrategy` enum; composer consults it when deciding which blocks are cache-marker candidates.
- **Overage-based TTL downgrade** (claude-code falls back to 5m when subscription overage kicks in) — future billing-awareness plan. Hook: `CacheProfile::allow_1h_ttl: bool` latched at session open from subscription status.
- **24-hour TTL** for very-stable content — future long-session-persistence plan. Hook: `CacheProfile::segment_1_ttl` can carry `TtlVariant::Ephemeral24h` once upstream rust-genai exposes that variant (it already does per investigation).
- **4th cache_control breakpoint usage** — reserved for whichever extension needs it first. Composer validates ≤4 total; Phase 5 uses exactly 3.

Design principle: **mimicking claude-code's patterns within reason is likely to produce good token-efficiency returns; Phase 5's architecture should not accidentally gate off the more sophisticated logic we'll almost certainly want later.**
