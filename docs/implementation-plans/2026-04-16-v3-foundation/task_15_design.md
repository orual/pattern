# Task 15 design — memory-edit cache preservation via pattern-test-cli

**Phase 5 Task 15.** Replaces the wiremock-only plan with a
pattern-test-cli subcommand that exercises the real
session → provider → cache round-trip. Operator runs it with their
real Anthropic credentials; the tool prints per-turn cache metrics
and flags the expected pattern.

## Why this over wiremock

- **Wiremock** proves the composer/cache plumbing is *structurally*
  correct — our request carries the right segment markers, the
  response-handling code interprets `cache_read_input_tokens` the way
  we think. But wiremock has to synthesise the cache numbers, and the
  synthesis is just our own prediction of what Anthropic would
  report. If our prediction is wrong, the test still passes.
- **Live** cuts through that: Anthropic actually runs the cache, and
  the numbers we get back are ground truth. A test that passes live
  is evidence the whole pipeline (compose → wire → server cache →
  response → metric extraction) works.
- **CLI delivery** matches AC9's "manual checklist" pattern (the
  phase's deliberate preference over env-gated live tests in CI —
  credentials rotate, rate-limit noise etc.).

## The scenario (from plan task spec, 3 wire turns with one memory edit)

### Turn 1 — baseline

- Fresh session using a realistic persona (see "Test persona"
  section below — we use **Anchor**, Pattern's maintenance facet,
  because its persona + memory blocks are focused enough that
  responses are predictable but rich enough that cache-read/write
  differences between turns are measurable).
- 3 memory blocks pre-seeded from `bsky_agent/*-block.md` content:
  - `persona` — `anchor-persona-block.md` (~400 words, reserved
    label per `pattern_core::PERSONA_LABEL`)
  - `current_human` — `pattern-current-human-block.md` (~200
    words; tracks who Anchor is currently observing)
  - `partner` — `pattern-partner-block.md` (~400 words; longer
    relational context)
- User message: `"check on me. how am i doing today?"` — tuned
  for Anchor's voice; produces a response that references the
  current_human + partner blocks so segment 3's content is
  non-trivially in play.
- Expected:
  - `cache_read_input_tokens` ≈ 0 (nothing was cached yet)
  - `cache_creation_input_tokens` ≈ total prompt size (all segments
    get written to cache on first turn — seg1 is ~persona + base +
    CODE_TOOL, seg2 is empty-history + memory pseudo-messages, seg3
    is `[memory:current_state]` with the three blocks)
  - `fresh_input_tokens` ≈ 0 (small — the appended "user message"
    is the only truly fresh content)

### Turn 2 — no memory change, just another user message

- User message: `"did i eat anything yet?"` — same persona, still
  a maintenance check. Model likely references the prior turn's
  response; prior-turn messages land in segment 2, still cached.
- Expected:
  - `cache_read_input_tokens` ≈ turn-1's prompt size (seg1 + seg2 +
    seg3 all hit — nothing invalidated)
  - `cache_creation_input_tokens` ≈ small (only the incremental
    new segment-2 content: the turn-1 assistant response +
    the user message turn-2 input position, which slot into
    segment 2's tail)
  - `fresh_input_tokens` ≈ only the new user message + turn-1
    assistant response (appended past the segment-3 boundary)

### Memory edit between turn 2 and turn 3

- Modify `current_human` block — e.g. simulate the operator
  reporting a new status: `"user just drank a full glass of water,
  ate lunch, meds taken at 12:30"`. This is realistic Anchor
  material and the block change should meaningfully alter how
  Anchor responds.

### Turn 3 — after memory edit

- User message: `"how am i doing now?"` — Anchor should reference
  the updated state. If the memory edit was cached-through properly
  the response reflects the new content.
- Expected (this is what the test asserts):
  - **AC8.1 — segment 1 cache preserved:** `cache_read_input_tokens`
    is STILL high, covering at least segment 1 (persona + base +
    CODE_TOOL) and probably segment 2 (prior history is stable).
    The only thing that should bust is segment 3.
  - **AC8.2 — segment 3 dropped:** `cache_read_input_tokens` for
    turn 3 is MEASURABLY LESS than turn 2's. The difference
    approximates the segment-3 content size (current_state
    pseudo-turn + the three blocks). This is the memory-edit
    invalidation.
  - **AC8.3 — `[memory:updated]` pseudo-message in segment 2:**
    the composed request for turn 3 contains a pseudo-message
    rendered from the `BlockWrite` that captured the `notes` edit.
    Visible by inspecting the request the composer built
    pre-wire — the CLI can print this too.

## `pattern-test-cli` surface

New subcommand: `pattern-test-cli cache-test`. Leverages the existing
credential chain + gateway infrastructure the `ask` command already
uses; adds runtime wiring (TidepoolSession, memory store, multi-turn
loop).

```
pattern-test-cli cache-test [OPTIONS]

OPTIONS:
    --model <MODEL>            [default: claude-opus-4-7]
    --shaper <SHAPER>          [default: subscription] (like `ask`)
    --block-size <BYTES>       [default: 1024] — size per seeded block
    --blocks <N>               [default: 3] — number of seeded blocks
    --edit-block <LABEL>       [default: notes] — which block to edit
    --verbose                  print full turn outputs, composed
                               requests, and sink events
```

## Command flow

1. **Credential + gateway setup** — identical to `ask` subcommand:
   - `AnthropicAuthChain::resolve()` → `ResolvedCredential`
   - Build `PatternGatewayClient` with shaper + rate limiter + token
     counter.
2. **Runtime setup** (new):
   - `InMemoryMemoryStore` pre-seeded with N blocks × `block_size`
     bytes of lorem-ipsum-esque content. Label the edit target
     something recognisable (`notes` by default).
   - `PersonaConfig` with a minimal persona program: doesn't matter
     what the agent does — the test is about cache behaviour, not
     agent logic. Something like "You are a test agent. Respond
     briefly."
   - Build a `VecSink` + optional stdout-forwarding wrapper so
     `--verbose` dumps the event stream.
   - `TidepoolSession::open_with_agent_loop(persona, sdk,
     memory_store, provider, turn_sink, prelude_dir)`.
3. **Turn 1** — `session.step_with_agent_loop(turn_input("What's in
   my memory?"))`. Capture `StepReply`. Extract
   `reply.turns[0].cache_metrics`. Print.
4. **Turn 2** — same, different prompt. Capture + print.
5. **Memory edit** — `memory_store.update_block_content(agent_id,
   "notes", "CHANGED: different content")`. (If `MemoryStore`
   doesn't have `update_block_content`, use the lower-level
   `persist_block` after mutating the `StructuredDocument`.)
6. **Turn 3** — same, different prompt. Capture + print.
7. **Observations print-out** — computed deltas + pass/fail
   commentary:

   ```
   turn 1 (baseline):        fresh=...    read=...    create=...
   turn 2 (no edits):        fresh=...    read=...    create=...
   turn 3 (after edit):      fresh=...    read=...    create=...

   OBSERVATIONS
   ---
   [AC8.1] seg1 preserved:
     turn 3 cache_read >= turn 2 cache_read * 0.6 ?  PASS / FAIL
     (reasoning: seg1 is ~persona+base+CODE_TOOL, typically 3-6K tokens.
      If turn 3 read drops below 60% of turn 2 read, segment 1 also
      busted — investigate with break-detection snapshot diff.)

   [AC8.2] seg3 invalidated:
     turn 3 cache_read < turn 2 cache_read ?         PASS / FAIL
     delta = turn 2 read - turn 3 read = X tokens
     (reasoning: segment 3 contains the 3 blocks; its size is
      roughly block_size * 3 plus pseudo-turn framing ~200 tokens.
      Delta should be in that ballpark.)

   [AC8.3] pseudo-message present:
     turn 3 composed request included "[memory:updated]" ?  PASS / FAIL
     (reasoning: the block edit produced a BlockWrite that the
      pseudo-message renderer converts to a segment-2 user-role
      message wrapped in <system-reminder>.)
   ```

## Hooking AC8.3 — composed-request inspection

For the third assertion we need to see what went into the prompt,
not just what came back. The composer builds the request inside
`drive_step`, and by the time we get a `StepReply` the request is
gone.

### Decision: emit the full `CompletionRequest` via `TurnSink`

Add a new variant `TurnEvent::ComposedRequest(CompletionRequest)`
— the whole struct, not a projection. Sinks choose how much to
render / log / discard.

**Why full-struct over projection:**

- **Debug-level tracing was the previous approach** and it had
  problems — massive logs (KBs per turn, hard to grep, noisy in
  CI). A `tracing::debug` dump is the wrong abstraction: every
  subscriber pays the serialisation cost whether they care or not.
- **Request tap via sink is opt-in.** `NoOpSink` (the default)
  drops the event immediately — free. Serious subscribers
  (CLI cache-test, future replay debugger, regression-snapshot
  capture) want the full structure and will pay the clone cost
  knowingly.
- **Letting the sink filter** keeps the event producer simple
  and the consumer flexible. A condensed-projection approach
  locks in a specific "what matters" decision at the producer
  layer; different consumers want different slices.

**Event shape:**

```rust
// In pattern_core::traits::turn_sink
pub enum TurnEvent {
    // ... existing variants ...
    /// The composer produced a complete CompletionRequest; the
    /// orchestrator is about to hand it to the provider. Emitted
    /// once per wire turn, immediately before the provider call.
    /// Consumers typically use this for debugging, request
    /// replay / snapshot testing, or cache-behaviour inspection.
    ComposedRequest(Box<CompletionRequest>),
}
```

Boxed to keep the enum stable-sized (CompletionRequest is large
and variable).

**Where the orchestrator emits:**

In `agent_loop::orchestrate`, immediately before
`ctx.provider().complete(req).await`:

```rust
let sink = ctx.turn_sink().clone();
sink.emit(TurnEvent::ComposedRequest(Box::new(req.clone())));
let mut stream = ctx.provider().complete(req).await?;
```

**Sink rendering recipes:**

- `NoOpSink::emit` — matches `TurnEvent::ComposedRequest(_)` and
  does nothing. One extra branch in a hot path; negligible cost.
- `VecSink::emit` — stores it. Memory grows linearly with wire
  turn count; test writers should `drain()` periodically if they
  run long sessions.
- CLI cache-test sink — in `--verbose` mode prints a condensed
  summary (system block count + hash, messages count + last few
  roles/previews, breakpoint placements); otherwise drops it.
- Future snapshot/replay — captures the raw struct, serialises to
  disk, plays back via a scripted provider.

**Landing order:** lands in a prep commit BEFORE Task 15
implementation — it's a general `TurnSink` enhancement, not
test-specific. Task 15's CLI subcommand then becomes a consumer.

## Dependencies

### Must land before Task 15

- **Task 12** (cache metrics from response usage) — the CLI reads
  `turn.cache_metrics.{fresh,read,creation}_input_tokens` directly.
  Without Task 12 these fields don't exist.
- **Task 20 part 5f** (composer integration in drive_step) —
  otherwise the request isn't composed with segments and cache
  behaviour is meaningless. DONE.
- ~~**Prelude dir env var**~~ (resolved Phase 6 Task C) — tidepool-extract
  now bundles the prelude internally. No env var is needed; the eval
  worker is always available when `tidepool-extract` is on `$PATH`.

### Nice to have but not blocking

- **`TurnEvent::ComposedRequest`** (see AC8.3 hook) — if we want the
  "[memory:updated] appears" check automated. Otherwise the operator
  eyeballs `--verbose` output.

## Test persona — Anchor from `bsky_agent/`

The test uses Anchor, Pattern's maintenance facet, as the persona
fixture. Reasons:

- **Focused voice**: Anchor has a distinct, constrained character
  (physical-maintenance observations, steady cadence, no
  meta-fanfare) so cache-hit behaviour is empirically assessable
  — if Turn 3 after a memory edit produces Anchor-voiced
  responses, we can eyeball that the persona block cached
  correctly.
- **Memory-reading by design**: Anchor's persona specifically
  describes observing user state (water, medication, sleep). With
  `current_human` pre-seeded, the model WILL pull from the block
  in its response, giving segment 3 real weight in token counts
  (the segment-3 drop after a memory edit becomes measurable).
- **Realistic size**: the `bsky_agent/*.md` block files are
  ~200-500 words each — comfortable 1 KB range per block,
  matching the plan's "3+ blocks ~1 KB each" recipe.

The CLI loads from `bsky_agent/anchor-persona-block.md`,
`bsky_agent/pattern-current-human-block.md`, and
`bsky_agent/pattern-partner-block.md`. If `bsky_agent/` isn't
present (non-dev checkouts), fall back to embedded minimal
content so the test still runs.

## Memory-store surface — what's needed vs what exists

### `InMemoryMemoryStore` (pattern_runtime::testing)

Today supports: `create_block`, `get_block`, `get_rendered_content`,
`mark_dirty`, `persist_block`, `set_block_type`.

For the CLI we need to:
- Create 3 blocks with pre-set content → `create_block` then
  `persist_block` with content via `StructuredDocument::set_text`.
  Existing surface sufficient.
- Edit a block → `get_block` to fetch the doc, mutate via
  `set_text` or similar, `persist_block`. Existing surface
  sufficient.

So no trait extension needed. If the real `pattern_db`-backed
`MemoryStore` has different ergonomics we might want a shared
helper; not blocking for Task 15.

## Output shape — human-readable + machine-grep-able

```
pattern-test-cli cache-test

[auth] tier: session-pickup (expires in 4h 17m)
[session] opened agent=test-agent model=claude-opus-4-7 shaper=subscription
[memory] seeded 3 blocks (notes, context, goals) @ 1024 bytes each

[turn 1] "What's in my memory?"
  stop=end_turn  usage: prompt=4213 completion=87 total=4300
  cache: fresh=4213 read=0 create=4213 (hit_ratio=0.000)
  duration: 1.82s

[turn 2] "Still there?"
  stop=end_turn  usage: prompt=4301 completion=52 total=4353
  cache: fresh=88 read=4213 create=88 (hit_ratio=0.980)
  duration: 0.91s

[memory] edited block 'notes' (256 bytes new content)

[turn 3] "Anything change?"
  stop=end_turn  usage: prompt=4450 completion=65 total=4515
  cache: fresh=237 read=3125 create=1088 (hit_ratio=0.702)
  duration: 1.15s

OBSERVATIONS
[AC8.1] seg1 preserved:     PASS (turn-3 read 3125 / turn-2 read 4213 = 74.2%)
[AC8.2] seg3 invalidated:   PASS (turn-2 read 4213 - turn-3 read 3125 = 1088 tokens)
[AC8.3] pseudo-message:     PASS (found "[memory:updated]" in turn-3 composed request)

SUMMARY: 3/3 expectations met — cache invalidation matches segment layout.
```

## Implementation sketch — where files land

- Extend `crates/pattern_runtime/src/bin/pattern-test-cli.rs` —
  add `Cmd::CacheTest { ... }` variant + `cmd_cache_test(...)`
  function. The existing `cmd_ask` is the closest template (build
  credential chain, build gateway). The runtime setup is new.
- Possibly `crates/pattern_runtime/src/bin/cache_test.rs` if the
  cache-test command grows big enough to warrant its own file.
  Decide at implementation time.
- New `TurnEvent::ComposedRequest(ComposedRequestSnapshot)` variant
  in `pattern_core::traits::turn_sink` if we go route A for AC8.3.
  Small projection struct:

  ```rust
  pub struct ComposedRequestSnapshot {
      pub system_block_count: usize,
      pub tool_count: usize,
      pub message_previews: Vec<(ChatRole, String)>,
      pub breakpoint_count: usize,
  }
  ```

## Out-of-scope for Task 15

- **Multi-session replay** — one session per invocation; testing
  cross-session cache behaviour is future work.
- **Automatic wiremock fallback** — CLI requires live credentials.
  If ops-folks want an offline sanity check later, we can add a
  `--mock` flag that uses a static response set.
- **Cross-provider cache comparison** — Anthropic-only. Gemini /
  OpenAI cache semantics differ materially.
- **Latency regressions** — we print durations but don't enforce
  thresholds. Cache-hit latency is an emergent property.

## Commit plan

Assuming Task 12 lands (populates `TurnCacheMetrics`):

1. (optional) `[pattern-core] TurnEvent::ComposedRequest for
   composer-output observability` — if we want automatic AC8.3
   checking.
2. `[pattern-runtime] Task 15: pattern-test-cli cache-test
   subcommand — memory-edit cache preservation (AC8.1, AC8.2, AC8.3)`

## Human operator checklist (phase 6 integration)

Once implemented, add to `crates/pattern_runtime/CLAUDE.md`'s
smoke-test section (per Phase 6 Task 4):

```
5. Verify cache preservation semantics:

   $ pattern-test-cli cache-test

   Expected: 3/3 PASS on the observations block. Anthropic's
   subscription tier reports `cache_read_input_tokens` per-request;
   the turn-3 drop relative to turn-2 approximates segment 3's
   size. Look for:
   - Turn 1: hit_ratio ≈ 0 (everything fresh)
   - Turn 2: hit_ratio > 0.9 (everything cached)
   - Turn 3: hit_ratio < turn-2's but > 0.5 (seg1+seg2 preserved,
     seg3 busted)

   If AC8.1 fails (seg1 busted): segment-1 bust-detection
   warning should have fired in stderr — cross-reference the
   break-detection snapshot diff to identify what changed.
   Typical suspects: shaper output drift, tool-list ordering,
   persona content changing.
```
