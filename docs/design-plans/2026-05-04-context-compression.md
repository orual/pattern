# Context-window compression: post-incident design notes

Last updated: 2026-05-04
Status: investigation + small-fix landing; larger items deferred.

## Background

On 2026-05-04 the `pattern` persona (claude-opus-4-6, 1M context) was
rejected by the Anthropic API:

```
HTTP 400 Bad Request — prompt is too long: 1001384 tokens > 1000000 maximum
```

The persona is configured with `compress-token-threshold 500000` and
`compress-check-message-floor 200`, with `RecursiveSummarization`
strategy (chunk-size 20, summarization model claude-haiku-4-5).
Compaction was supposed to fire well before the 1M ceiling, and did not.

This document captures the root cause, the small fix that landed, and a
larger set of compression-pipeline improvements identified during the
investigation. The deferred items are NOT scoped for immediate
implementation — they are recorded so the next planning cycle can pull
them in coherently.

## Root cause (landed)

`pattern_provider::compose::compression::should_compress` was called
from `pattern_runtime::compaction::maybe_compact` with a
`CompletionRequest` constructed solely from the active turn-history
messages:

```rust
// (pre-fix shape, in should_compress)
let messages: Vec<ChatMessage> = turns.iter()
    .flat_map(|t| t.messages.iter().cloned())
    .collect();
let request = CompletionRequest::new(model).with_messages(messages);
let count = client.count_tokens(&request).await?;
```

The actual wire request that the agent loop sends includes substantially
more content:

- Three-slot system prompt (subscription routing literal + base
  instructions + persona block)
- Tool schemas for the 15-effect SDK surface (tens of thousands of
  tokens)
- `BatchOpeningSnapshot` attachments rendered inline by `FreshInputPass`
- `BlockWriteNotifications` pseudo-messages

Counting only the message bodies undercounts the wire request by an
amount that scales with persona richness, tool schema breadth, and
snapshot size. For pattern's setup the gap was large enough that the
gate read ~400-500k while the wire was >1M, so `should_compress`
returned `false` and compaction never fired.

### Fix that landed (2026-05-04)

`should_compress` and `maybe_compact` were changed to take the actual
composed `CompletionRequest` as input. The agent loop now composes
first, calls the gate against the composed request, and recomposes if
the gate fired:

```rust
// agent_loop.rs::drive_step
let (mut req, mut has_segment_1) =
    compose_request_for_turn(&ctx, &turn_history, &cur_input, &cache_profile).await?;
let outcome = maybe_compact(&ctx, &turn_history, ctx.context_policy(), &req).await?;
if matches!(outcome, CompactionOutcome::Fired { .. }) {
    let (recomposed, has_seg_1) =
        compose_request_for_turn(&ctx, &turn_history, &cur_input, &cache_profile).await?;
    req = recomposed;
    has_segment_1 = has_seg_1;
}
```

Cost: one extra compose call per turn that fires compaction (rare).
`compose_request_for_turn` does not mutate `turn_history`, so it is
safe to call twice.

Tests: 10/10 in `crates/pattern_runtime/tests/compaction.rs`, 21/21 in
`crates/pattern_provider/src/compose/compression.rs::tests`.

## Soft-fail on count_tokens / summarizer errors (landed)

Discovered immediately after the gate fix landed: a transient OAuth
token-refresh failure (`error sending request for url
https://console.anthropic.com/v1/oauth/token`) propagated up through
`maybe_compact` and killed the turn. The gate is a *safety check*, not
load-bearing — a transient failure should not take down the user's
turn.

Two new `CompactionOutcome` variants:

- `GateError { error, active_turns }` — `count_tokens` failed (auth,
  network, server). Logged at `warn!`; turn proceeds with the original
  composed request. The actual `complete()` call attempts its own auth
  refresh — if the failure is genuinely persistent, complete() surfaces
  a clearer error of its own.
- `StrategyError { strategy_name, error, active_turns }` — strategy
  dispatch failed (most likely the `RecursiveSummarization` summarizer
  call). Same soft-fail rationale; same logging shape.

Tests updated to add catch-all panic arms for the new variants
(mock provider doesn't produce them, so they're "unexpected" in the
test suite).

**Follow-up: OAuth refresh should retry.** The underlying issue is
that `pattern_provider::auth::pkce::PkceAuthorizer::exchange` has zero
retry logic — single HTTP failure → `AuthExchangeFailed`. Should
follow the same `open_stream_with_retry` pattern used for the
chat-completion path (`gateway.rs`). This fix would benefit every
auth-using path, not just compaction.

### Tradeoff: tight threshold + soft-fail can cascade to overflow

The soft-fail behaviour preserves history (nothing archived, nothing
summarized) when the gate or strategy errors. That's the right
default — no data loss. But it has a failure mode:

If `compress_token_threshold` is set close to the actual wire ceiling
(e.g., 950k on a 1M model), and compaction soft-fails repeatedly, each
turn adds content while history stays unchanged. After N consecutive
soft-fails the active context can cross the wire ceiling, after which
every turn hard-fails at the API with no path to recovery.

**Invariant for threshold sizing:** `compress_token_threshold` must
leave enough headroom that several consecutive soft-failed compactions
cannot cross the wire ceiling. For a 1M-context model with average
~50k tokens added per turn, leaving at least 250k headroom (threshold
≤ 750k) absorbs 5 consecutive soft-fails. Pattern's current
`500000` threshold on opus-4-6 (1M) leaves 500k — comfortably safe.

**Mitigations if (1) document-only is not enough:**

- **Fallback strategy on summarizer failure.** Add a per-persona
  `on_summarizer_failure: "truncate" | "skip"` knob to
  `CompressionStrategy::RecursiveSummarization`. When set to
  `truncate` and the summarizer call errors, drop the oldest
  `chunk_size` turns via `apply_truncate` instead of bailing.
  Sacrifices the summary text; bounds context size. Opt-in.

- **Consecutive soft-fail counter.** Track soft-fail streaks on
  `SessionContext` or `TurnHistory`. After N (say 3) consecutive
  failures, escalate: force a non-summarizing strategy, or surface
  a hard error to the agent so the user is told what's happening.
  Belt-and-suspenders for paranoid configs.

These are deferred unless someone actually hits the cascade.

## Landing now: prompt + persona injection

Two small follow-on changes pair with the fix:

### Default summarizer prompt rewrite

The current `DEFAULT_SUMMARIZATION_SYSTEM_PROMPT` is "You are a helpful
assistant that creates concise summaries of conversations." This is
inadequate for Pattern's relational, multi-faceted agent style. It
loses voice, omits memory references, and produces summaries that the
agent cannot pick back up from in-character.

New default prompt is structured (analysis-then-summary, sectioned
output) with explicit slots for: what we've been up to, decisions and
commitments, observations about the partner, memory writes touched,
unresolved threads, verbatim partner messages.

Modeled loosely on the structural rigor of Claude Code's conversation
summarizer prompt (see Piebald-AI/claude-code-system-prompts), but
with code-task framing replaced by Pattern's relational framing.

### Persona injection into summarizer system

`generate_summary` reads the persona block from `MemoryStore` and
prepends it to the summarization system prompt, so the summarizer
model writes the summary in the agent's voice rather than as an
external observer. Cheap (one DB read), gives the model a strong voice
anchor without architectural changes.

Personas can still override `summarization_prompt`; the persona-block
prepend happens regardless of which prompt is used.

## Deferred work

The remaining items are recorded for future planning. None are scoped
for immediate implementation.

### (1) Hysteresis loop for `RecursiveSummarization`

**Problem.** `RecursiveSummarization` archives exactly `chunk_size`
turns per fire (default 20). If the threshold trip is barely over, one
chunk archive may barely drop us back under threshold — and the next
turn re-trips, firing the (expensive, summarizer-calling) strategy
again. The other strategies (`Truncate`, `ImportanceBased`,
`TimeDecay`) are cheap enough that single-shot per fire is fine; only
`RecursiveSummarization` needs hysteresis.

**Proposal.** Add `compress_target_pct` to `ContextPolicy` (default
~0.7-0.8). When the gate fires and the strategy is
`RecursiveSummarization`, loop:

```
while count_tokens(composed) > threshold * compress_target_pct
    && active_len > min_keep_recent {
    apply_one_chunk();
    recompose();
}
```

Bounded by a hard iteration cap and by `min_keep_recent` so we cannot
accidentally archive everything or get stuck on a misreporting count.

Per-event cost: N summarizer calls instead of 1. Per-session frequency
drops correspondingly. Net cost should be similar or lower; agent
turn-loop latency on the compaction-fire turn goes up.

### (2) Summarizer-window mismatch handling

**Problem.** A persona may run a 1M-context main model with a
200k-context summarizer (e.g., haiku-4-5). If the chunk being
summarized exceeds the summarizer's input window, the summarizer call
fails with `RuntimeError::ProviderError`, aborting the whole turn.

**Three options, ordered by ambition:**

(a) **Cap the chunk to fit.** Count tokens on the chunk content first;
if over the summarizer's window, trim `chunk_size` down (binary
search or halve until it fits). The trimmed-out turns stay archived
without summary on this pass. Simplest; loses some summary detail.

(b) **Split-and-merge.** If chunk > summarizer window, split into
sub-chunks that fit, summarize each independently, then write
multiple depth-0 summaries OR recursively summarize them into one.
This maps onto the depth ≥ 1 rollup work item already noted in
`pattern_runtime/CLAUDE.md`.

(c) **Per-persona summarizer max.** Add
`summarization_max_input_tokens` to the `RecursiveSummarization`
config; if not set, infer from a model→window registry. On overflow,
fall back to (a) or hard error with a clear message.

Recommended start: (a) + (c) for diagnosability. (b) belongs in a
follow-up that ties into depth-≥1 summary rollups.

### (3) Self-summarization with main-model cache reuse

**Idea.** Use the main agent model (e.g., opus-4-6) to summarize its
own conversation, leveraging Anthropic's prompt cache so the bulk of
the input is read at cache-read pricing rather than full input
pricing.

**Architectural angle.** When the gate fires, we have *just composed*
a `CompletionRequest` whose prefix matches what is currently hot in
Anthropic's cache (the prior wire turn sent essentially the same
prefix). Instead of discarding it, build the summarization request by:

```rust
fn build_summarization_request(composed: &CompletionRequest)
    -> CompletionRequest
{
    let mut req = composed.clone();
    req = req.append_message(ChatMessage::user(SUMMARIZATION_DIRECTIVE));
    req
}
```

The cache breakpoint placed by `Segment2Pass` / `FreshInputPass`
covers the prefix; the appended summarization directive + summary
output are uncached.

**Cost math (opus-4-6, ~500k cached prefix):**

| path                       | input                              | output                     | total  |
|----------------------------|------------------------------------|----------------------------|--------|
| haiku-4-5, fresh           | 500k @ $0.80/Mtok = $0.40          | 2k @ $4/Mtok = $0.01       | $0.41  |
| opus-4-6, **cache_read**   | 500k @ $1.50/Mtok = $0.75          | 2k @ $75/Mtok = $0.15      | $0.90  |
| opus-4-6, **no cache**     | 500k @ $15/Mtok = $7.50            | 2k @ $75/Mtok = $0.15      | $7.65  |

Cached opus is ~2.2x haiku — quality argument plausibly worth it.
Uncached opus is ~19x haiku — almost certainly not. **Cache hit is
load-bearing.** If cache TTL has expired (5 min default; 1 hour with
`extended-cache-ttl-2025-04-11`, which Pattern's gateway can opt into),
we fall to the uncached path and cost explodes.

For users on Anthropic Max subscription routing, the cost dimension
becomes quota burn rather than dollars. Opus burns more subscription
quota per token regardless of cache state. This is a per-persona
choice the user should make consciously.

**Caveats / footguns.**

- **Cache observability.** The summarization response carries
  `cache_read_input_tokens`. We should log it and back off the
  strategy if the hit rate falls below some threshold.
- **Eviction.** Cache eviction can happen for capacity reasons even
  within TTL. Same observability story.
- **Session UUID rotation order.** `maybe_compact` currently rotates
  `session_uuid` *after* compaction fires. The self-summarize call
  must happen *before* the rotation (it depends on the pre-rotation
  cache state). The order is correct in the current code, but a
  comment pinning the dependency is needed when this lands.

**Proposed shape.** Extend `CompressionStrategy::RecursiveSummarization`:
the existing `summarization_model: String` becomes
`summarization_model: Option<String>`. `None` means "use main model
with cache reuse"; `Some(model)` retains the current external-summarizer
behavior. Dispatch in `generate_summary`:

```rust
if summarization_model.is_none()
    || summarization_model.as_deref() == Some(ctx.model_id())
{
    generate_summary_self(ctx, composed_request, persona, prompt).await
} else {
    generate_summary_external(ctx, turn_history, chunk_size, model, prompt).await
}
```

`generate_summary_self` reuses the gate-counted request; produces a
**whole-context-aware** summary (the model sees everything, summarizes
in context). `generate_summary_external` builds a separate request to
the summarizer model; produces a **chunk-only** summary (narrower view).
These are semantically different — both have value.

### (4) Cache-residency keepalive (companion to #3)

**Idea.** If the user is sporadic (>5 min between turns, default cache
TTL), the next request misses cache. For self-summarize this is
catastrophic (10x cost spike). For normal turns it is just an extra
$X cost.

**Mechanism.** A background timer per session that, if no real request
has been sent for ~T minutes (T < TTL), sends a tiny request that
hits the same prefix and asks for one or two output tokens. Discard
the response. Cache stays warm.

**Math.** With extended-cache-ttl (1 hour, 25% input cost surcharge on
cache write), worst case keepalive frequency is ~1 per hour of idle.
Each keepalive: 500k cached input @ $1.50/Mtok = $0.75 + 1 output
token at negligible cost. So keeping a session warm for 8 idle hours =
$6 of keepalive cost. Whether that's worth it depends on usage shape.

**Footguns.**

- **Persona behavior side effects.** Even a "give me one token"
  request runs through the agent loop and could cause real side
  effects (memory writes, tool calls, etc.) if the prompt is
  ambiguous. The keepalive request needs an explicit
  "do-not-act" framing — perhaps a system-reminder-tagged user message
  saying "this is a cache keepalive; respond with a single period."
  And ideally the response is dropped without going through any
  handler.
- **Quota burn on subscription.** Periodic phantom requests still
  consume subscription quota. This may be unacceptable on plans with
  tight limits.
- **Discoverability.** Phantom requests appearing in logs / Anthropic
  console are confusing without explanation. Tag them clearly.

This is most attractive when paired with self-summarize (#3); on its
own, the cost/benefit is weaker.

### (5) Stale-attachment compose-time render filter

**Problem.** `MessageAttachment`s on Pattern messages get rendered
inline at compose time. Some attachments age out of usefulness:
- `BatchOpeningSnapshot` from earlier batches when newer Full
  snapshots exist on the wire
- `FileEdit` for files that have since been closed
- `BlockWriteNotifications` for blocks rewritten many times since
- `ShellOutput` chunks for processes that have exited

Currently these all render every time their host message is composed,
inflating the wire request and (post-fix) the gate count.

**Proposal.** A "latest-wins" filter applied at compose time: walk
the composed message list, build a set of (kind, key) pairs from later
messages, drop earlier renders of the same (kind, key). Keys per kind:

| attachment kind            | key             |
|----------------------------|-----------------|
| BatchOpeningSnapshot       | block label     |
| FileEdit                   | file path       |
| BlockWriteNotifications    | block label     |
| ShellOutput                | task_id         |
| PortEvent                  | (port id, event id?) — needs design |

Single pass; no mutation of stored messages. Can ship independently of
compaction work.

**Tradeoff.** Risk that some agent reasoning specifically depends on
seeing the older snapshot/edit verbatim. Mitigation: this should be
opt-in initially (per-persona flag), and we observe whether agents'
behavior degrades.

### (6) Should main context also strip stale tool I/O?

**Idea.** Pattern keeps verbatim tool output (`tool_result` content) in
the main wire forever. Old tool results often have low ongoing
informational value but high token cost. Stripping them — replacing
with a marker like `[tool_result truncated: 28k tokens elided]` —
would shrink the running context dramatically.

**Why this is risky.**

- The agent often later refers back to specific tool output (e.g.
  "earlier I read file X — let me find that line again"). Stripping
  breaks that.
- Stripping mid-session would change the message-history hash and
  bust segment 2 cache, which is one of the load-bearing caches.

**The only safe place to do it: at compaction time.** Compaction
already eats a `session_uuid` rotation (and thus cache reset) when it
fires. If we strip during compaction, the cache hit was already lost.
And compaction is rare relative to per-turn requests, so the
information-loss cost is amortized.

**Status: deferred indefinitely.** This is a real and tempting lever
but the agent-behavior risk is high. Should not land without explicit
user testing showing the agent doesn't rely on old tool_result content.

## Next steps

After this doc + the prompt/persona changes land:

- Live-test the gate fix on the actual pattern persona that hit the
  1M cap — verify compaction now fires at the configured 500k.
- Decide whether to scope (1) hysteresis or (2) summarizer-window
  handling next based on observed behavior post-fix.
- (3) self-summarization is the highest-value follow-up but also the
  most architectural; needs its own brainstorm session before
  scoping.
