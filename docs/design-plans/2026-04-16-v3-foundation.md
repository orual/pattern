# Pattern v3 Foundation Design

## Summary

Pattern v3 Foundation rebuilds the Pattern agent runtime on a new architectural substrate while preserving existing storage code. The central structural shift is decomposing the current monolithic `pattern_core` crate — which today houses the agent loop, tool registry, memory, coordination logic, and provider integration all together — into a strict layering: a traits-only `pattern_core` defines contracts, a new `pattern_runtime` contains execution machinery, and a new `pattern_provider` owns LLM authentication and request shaping. Crates depend only on `pattern_core` interfaces, never on each other's concrete implementations.

The agent execution environment is built on Tidepool, a Haskell-in-Rust runtime, where agents are written in pure Haskell (IO disabled at the language level) and request side-effects through an algebraic-effects system (`freer-simple`). Rust-side handlers fulfill each effect under policy control; effects not yet in scope ship as stubs with stable names so the SDK surface doesn't need to change as future plans fill them in. Anthropic access is handled through a three-tier auth resolver (subscription session-pickup, PKCE OAuth, API key), with request shaping that honestly identifies Pattern as the client. The memory storage layer (loro CRDT + SQLite + FTS/vector search) is preserved unchanged, but where memory content appears in the model's context is redesigned: instead of blocks being rendered into the system prompt — where any edit invalidates the entire prefix cache — blocks are placed in a dedicated third cache segment just before the current turn, so edits invalidate only that segment and leave the stable system prompt and history segments untouched.

## Definition of Done

Pattern v3 Foundation — a minimal-but-usable Pattern runtime rebuilt on the new substrate. The plan is done when:

### Infrastructure

- `rewrite-v3` branch exists, cut from `main` at tagged `pre-rewrite-v3`
- New crate skeletons in place: `pattern_core` (traits-only), `pattern_runtime`, `pattern_provider` (absorbing pattern_auth's provider bits)
- Workspace `members` list narrowed to active crates; port-list doc tracking what's excluded
- `pattern_core` trait definitions landed (`AgentRuntime`, `MemoryStore`, `ProviderClient`, `MessageRouter`, `DataStream`)

### Runtime

- Tidepool (Haskell-in-Rust) embedded via FFI with external CPU/wall timeout wrapping at the FFI boundary
- Minimal agent loop working: program → LLM call → response → next turn
- `freer-simple` effect handlers for the core SDK hierarchy (`memory`, `message`, `shell`, `file`, `sources`, `mcp`, `time`, `ipc`, `log` — as stubs where backing services aren't in scope)
- Turn-level checkpoint + restore working

### Provider

- `pattern_provider` ships rebased `rust-genai` (thin auth-only patches) with three-tier auth resolution: stored-OAuth (pattern's own keyring/JSON-file token) → API key (`ANTHROPIC_API_KEY`) → session-pickup (`~/.claude/.credentials.json`). Rationale and full tier details in `crates/pattern_provider/CLAUDE.md §Anthropic auth chain — tier order`.
- Request shaping with honest pattern identification (client identifies itself as pattern rather than impersonating claude-code; appropriate `x-app`, User-Agent, and session-tracking headers populated)
- Per-provider token-bucket rate limiting
- Provider-session UUID rotates on configured boundaries
- Provider-reported token counting (dedicated `count_tokens` call pre-request and `usage`-field capture post-response) replaces the current heuristic approximation used across context-length calculations; async from provider through compaction

### Memory (existing code, repositioned)

- Existing `pattern_memory` storage (loro CRDT, block schema, sqlite+FTS+vector indexes) preserved unchanged at the storage layer
- Rendering layer revised per brainstorm-draft §3.4: blocks rendered as a pre-turn pseudo-turn with its own cache breakpoint; base system prompt no longer contains block content; block changes surface as pseudo-messages in message history
- Three-segment cache layout in place: system+instructions / history / block-state
- Cache TTL variants supported end-to-end: Anthropic's 5-minute default and 1-hour extended breakpoints surface as configurable options at the `pattern_provider` and composer layer; segments can request different TTLs based on stability expectations
- Base instructions (`DEFAULT_BASE_INSTRUCTIONS`) preserved

### Demonstration

Smoke test passes:
1. Create persona
2. Auth via subscription OAuth
3. Talk to Claude
4. Write memory block
5. Restart pattern
6. Memory retained
7. Edit block
8. Cache-hit metrics show system-prompt prefix unaffected by the edit

### Explicitly OUT OF SCOPE (deferred to future plans)

- fs-based memory redesign (Mode A/B/C), jj integration, Task/Skill block subtypes
- Subagent primitives (ephemeral/fork/sibling)
- Plugin system + MCP + CC compatibility
- Social integrations (pattern-atproto, pattern-discord, pattern-nd) migration
- v2 → v3 data migrator
- Compaction enhancements (existing strategies retained as-is with repositioning only)
- cosa runtime prep work
- iroh-rpc native IPC
- TUI/CLI polish beyond what's needed for the smoke test

### Context

This is the first of multiple design plans covering the Pattern v3 rewrite. Brainstorm draft covering full v3 scope lives at `docs/plans/2026-04-16-rewrite-v3-design-draft.md`. Future design plans will cover memory fs/jj redesign, subagent primitives, plugin system, MCP integration, social plugin migration, and the v2→v3 data migrator as separate iterations.

## Acceptance Criteria

### v3-foundation.AC1: pattern_core traits are defined, satisfiable, and documented

- **v3-foundation.AC1.1 Success:** `cargo check -p pattern_core` succeeds with zero warnings on the narrowed workspace
- **v3-foundation.AC1.2 Success:** `cargo doc -p pattern_core` produces complete documentation; every public trait and type has rustdoc
- **v3-foundation.AC1.3 Success:** Dummy struct impls of `AgentRuntime`, `Session`, `MemoryStore`, `ProviderClient`, `MessageRouter`, `DataStream`, `SourceManager` all compile, confirming trait shape is satisfiable
- **v3-foundation.AC1.4 Success:** Port-list doc at `docs/plans/rewrite-v3-portlist.md` exists and lists every currently-excluded crate with deferral-plan note
- **v3-foundation.AC1.5 Failure:** Removing a required method from a dummy trait impl causes `cargo check` to fail with a clear "missing implementation" error
- **v3-foundation.AC1.6 Edge:** Referencing a retired crate (e.g., `pattern_auth`) from an active crate's `Cargo.toml` causes explicit workspace error, not silent acceptance
- **v3-foundation.AC1.7 Success:** Every in-flight or pending-move code region has a `// MOVING TO:`, `// REPLACED BY:`, or `// MOVING WITHIN CRATE:` comment identifying its defined fate; port-list doc cross-references these markers
- **v3-foundation.AC1.8 Success:** No surface API contains `unimplemented!()` / `todo!()` without a comment identifying the filling phase and AC
- **v3-foundation.AC1.9 Failure:** A code region pending move that has no fate marker, OR a stubbed API with no phase/AC reference, causes the intermediate-state audit check to fail (grep-based scan during phase verification)
- **v3-foundation.AC1.10 Edge:** Commented-out code blocks in new-or-modified source files fail the audit check (git history is the record, not comments)

### v3-foundation.AC2: Tidepool runtime embedded with bounded execution

- **v3-foundation.AC2.1 Success:** A trivial Haskell program (`pure "hello"`) loads, runs, and returns its result to the Rust caller
- **v3-foundation.AC2.2 Success:** `ctx.time.now` effect dispatched from Haskell is handled in Rust and returns current time correctly to agent code
- **v3-foundation.AC2.3 Success:** `ctx.log` effect writes a structured entry observable from Rust-side instrumentation
- **v3-foundation.AC2.4 Success:** Turn-level checkpoint captures session environment; restore replays the captured state deterministically (round-trip equality)
- **v3-foundation.AC2.5 Failure:** Program exceeding wall-clock budget (e.g., `forever $ pure ()`) is killed by timeout harness before exceeding 1.5× the configured budget
- **v3-foundation.AC2.6 Failure:** Program exceeding CPU budget (non-yielding compute) is killed with `RuntimeError::Timeout { cpu_ms, .. }`
- **v3-foundation.AC2.7 Failure:** Effect hitting Tidepool's 10K-node response limit returns `RuntimeError::EffectOverflow`
- **v3-foundation.AC2.8 Failure:** GHC runtime crash mid-execution returns `RuntimeError::RuntimeCrashed`; session is marked unusable
- **v3-foundation.AC2.9 Edge:** Stubbed `spawn`/`mcp`/`ipc` effects return clear "not yet implemented" error, not silent hang
- **v3-foundation.AC2.10 Edge:** Concurrent turns on distinct sessions do not interfere with each other's state (thread-safety of FFI boundary)

### v3-foundation.AC3: Subscription session-pickup authentication

Note: the implemented tier order is stored-OAuth → API key → session-pickup (see
`crates/pattern_provider/CLAUDE.md §Anthropic auth chain — tier order` for rationale).
The credential file path is `~/.claude/.credentials.json` (claudeAiOauth wrapper, verified
2026-04-17), not `~/.claude/session.json`.

- **v3-foundation.AC3.1 Success:** With a valid unexpired session credential from `~/.claude/.credentials.json`, provider makes an authenticated request to Anthropic and returns a real response
- **v3-foundation.AC3.2 Success:** Session-pickup reads the file atomically; concurrent write from claude-code does not produce a torn read
- **v3-foundation.AC3.3 Failure:** Missing `~/.claude/.credentials.json` → resolver skips tier without error, falls through to next tier
- **v3-foundation.AC3.4 Failure:** Malformed JSON in credentials file → warning logged, tier skipped, falls through
- **v3-foundation.AC3.5 Failure:** Expired token in credentials file → tier skipped, falls through
- **v3-foundation.AC3.6 Edge:** Linux host with no pattern keyring entry but a valid claude-code session file → session-pickup succeeds (keyring absence never short-circuits session-pickup)

### v3-foundation.AC4: Stored OAuth, PKCE, and API-key authentication

Note: tier resolution order is stored-OAuth (pattern's own PKCE-minted token from
keyring/JSON) → API key (`ANTHROPIC_API_KEY` env var) → session-pickup (claude-code ambient
session). See `crates/pattern_provider/CLAUDE.md` for the rationale.

- **v3-foundation.AC4.1 Success:** No stored token present → PKCE opens localhost callback, user completes flow, token stored in keyring, subsequent request succeeds; `ResolvedCredential.source` is `AuthTier::Pkce`
- **v3-foundation.AC4.2 Success:** Pattern's stored OAuth token within 5-min of expiry → auto-refresh before request; new token stored; request succeeds with refreshed token; `source` is `AuthTier::StoredOauth`
- **v3-foundation.AC4.3 Success:** `ANTHROPIC_API_KEY` set → provider uses it, request succeeds; `source` is `AuthTier::ApiKey`
- **v3-foundation.AC4.4 Failure:** PKCE callback timeout → `ProviderError::AuthFlowTimeout` surfaced; no silent proceed
- **v3-foundation.AC4.5 Failure:** Refresh-token endpoint returns error → `ProviderError::RefreshFailed`; no silent degradation
- **v3-foundation.AC4.6 Failure:** Keyring unavailable AND JSON fallback file unreadable → explicit `ProviderError::CredentialStoreUnavailable`
- **v3-foundation.AC4.7 Edge:** Concurrent refresh attempts for the same persona are serialized by mutex; only one network call made

### v3-foundation.AC5: Request shaping and rate limiting

- **v3-foundation.AC5.1 Success:** Outbound request includes honest pattern identification headers (implementation chooses specific header names/values; must identify as pattern rather than impersonate claude-code) plus per-persona session-UUID header
- **v3-foundation.AC5.2 Success:** System-prompt prefix block contains pattern-specific persona content (not `You are Claude Code`), positioned in the same structural slot
- **v3-foundation.AC5.3 Success:** Session-UUID rotates when caller signals rotation boundary
- **v3-foundation.AC5.4 Success:** Rate-bucket exhaustion queues request with jitter; request eventually succeeds after bucket refills
- **v3-foundation.AC5.5 Failure:** Misconfigured shaper (missing required identification headers) → error at provider construction time, not at request time
- **v3-foundation.AC5.6 Edge:** Multiple providers maintain independent buckets — Anthropic exhaustion does not affect other (future) providers
- **v3-foundation.AC5.7 Edge:** Tokens-per-day bucket tracked independently from tokens-per-minute; day bucket stays depleted even while minute bucket refills

### v3-foundation.AC5b: Provider-reported token counting

- **v3-foundation.AC5b.1 Success:** `ProviderClient::count_tokens` returns Anthropic-reported counts for a composed request, matching (within provider precision) the count Anthropic would charge for the same request
- **v3-foundation.AC5b.2 Success:** Post-response `usage` field is captured and exposed to callers; subsequent compaction decisions can use these counts directly
- **v3-foundation.AC5b.3 Success:** Call sites that previously used heuristic token approximation (compaction thresholds, context-length checks) now consume provider-reported counts via the async path
- **v3-foundation.AC5b.4 Failure:** Provider token-counting endpoint failure surfaces as explicit `ProviderError::TokenCountFailed`; callers can fall back to heuristic if and only if they explicitly opt in (no silent fallback by default)
- **v3-foundation.AC5b.5 Edge:** Token-count requests are rate-limited independently from chat-completion buckets (Anthropic counts them separately); exhaustion of count-bucket does not block completion requests

### v3-foundation.AC6: Memory storage adapter preserves existing behavior

- **v3-foundation.AC6.1 Success:** `ctx.memory.write(handle, content)` persists to loro + sqlite, matching current pattern's storage semantics
- **v3-foundation.AC6.2 Success:** `ctx.memory.read(handle)` returns current content including recent writes
- **v3-foundation.AC6.3 Success:** `ctx.memory.search(query)` returns hybrid FTS + vector results (existing `pattern_db` behavior unchanged)
- **v3-foundation.AC6.4 Success:** Content survives process restart — write block, restart runtime, read block, content matches
- **v3-foundation.AC6.5 Failure:** Write to non-existent block handle → `MemoryError::BlockNotFound` with available-blocks context
- **v3-foundation.AC6.6 Edge:** Concurrent writes to the same block from different sources merge via loro CRDT without data loss

### v3-foundation.AC7: Three-segment cache layout structure

- **v3-foundation.AC7.1 Success:** Composed request has exactly three `cache_control` markers, at the segment-1/2, segment-2/3, and segment-3/fresh boundaries
- **v3-foundation.AC7.1b Success:** Composer exposes TTL selection per breakpoint; default configuration uses 1-hour TTL for segment 1 and 5-minute TTL for segments 2 and 3; caller can override
- **v3-foundation.AC7.2 Success:** Segment 1 contains identity + `DEFAULT_BASE_INSTRUCTIONS` + tool descriptions; contains no block content
- **v3-foundation.AC7.3 Success:** Segment 3 contains `[memory:current_state]` pseudo-turn rendering core + loaded-working blocks
- **v3-foundation.AC7.4 Success:** `DEFAULT_BASE_INSTRUCTIONS` text appears verbatim in segment 1 (byte-for-byte match against current `context/mod.rs` constant)
- **v3-foundation.AC7.5 Failure:** Attempt to emit a 5th `cache_control` marker (exceeds Anthropic's 4-breakpoint budget) → validation error at composition time, not at API boundary
- **v3-foundation.AC7.5b Failure:** Unsupported TTL value → configuration error at provider construction or composer setup, not at API request time
- **v3-foundation.AC7.6 Edge:** Persona with zero loaded blocks → segment 3 renders as empty `[memory:current_state]` pseudo-turn (present but empty), not omitted — preserves cache-boundary consistency

### v3-foundation.AC8: Cache-preservation across block edits

- **v3-foundation.AC8.1 Success:** After a turn establishes cached segments, editing a memory block and running the next turn shows segment 1 cache-hit metric unchanged (still hit)
- **v3-foundation.AC8.2 Success:** Same scenario: segment 3 cache-hit metric shows invalidation (expected, since segment 3 contains the edited block)
- **v3-foundation.AC8.3 Success:** `[memory:updated]` pseudo-message for the edited block appears in segment 2 of the next turn's message history
- **v3-foundation.AC8.4 Success:** Compression strategies (existing four) process pseudo-message-containing message streams without regression; archived batches include pseudo-messages correctly
- **v3-foundation.AC8.5 Failure:** If Anthropic response indicates segment 1 was invalidated unexpectedly, metrics detect it and the smoke test fails loudly rather than silently accepting the cache miss
- **v3-foundation.AC8.6 Edge:** Block written by a non-local-agent source (future subagent, future IPC) also surfaces `[memory:written]` pseudo-message with the correct author attribution

### v3-foundation.AC9: End-to-end foundation demonstration

- **v3-foundation.AC9.1 Success:** API-key smoke test in CI passes deterministically: create persona → auth → send message → receive response → write block → persist → restart → read-back matches → edit block → next turn shows expected cache behavior
- **v3-foundation.AC9.2 Success:** Manual subscription-OAuth smoke test passes on user's machine with a valid `~/.claude/session.json`; procedure documented in test file header
- **v3-foundation.AC9.3 Success:** Minimal CLI entry point drives the full flow from the command line (functional, not polished)
- **v3-foundation.AC9.4 Success:** Cache-hit metric assertions pass: segment 1 hit rate stays high across block edits; segment 3 invalidates as expected
- **v3-foundation.AC9.5 Failure:** Any step failing in the smoke flow (persona creation, auth, message send, memory write, restart, read-back, edit, cache metric) causes the smoke test to fail loudly with a specific error pointing at which step failed

## Glossary

- **Tidepool**: An embedded Haskell-in-Rust runtime. Agents write pure Haskell programs; Tidepool evaluates them with IO disabled. Side-effects are modeled as algebraic effects that Rust handlers fulfill.
- **freer-simple**: A Haskell algebraic-effects library that Tidepool uses internally. Agents declare effects (memory reads, shell calls, etc.) as data; a handler tree interprets them. Pattern's SDK is expressed as `freer-simple` effect algebras.
- **FFI boundary**: The Foreign Function Interface crossing point between Rust code and the embedded GHC (Haskell) runtime inside Tidepool. Errors and timeouts are trapped here.
- **GHC**: The Glasgow Haskell Compiler runtime that Tidepool embeds. A GHC-side crash surfaces as `RuntimeError::GhcPanic`.
- **loro CRDT**: A conflict-free replicated data type library (`loro` crate) used for memory block storage. Concurrent writes to the same block are merged automatically without data loss.
- **sqlite-vec**: A SQLite extension providing vector similarity search, used alongside FTS5 for hybrid semantic search over memory blocks.
- **FTS5**: SQLite's fifth-generation full-text search extension, used for keyword-based memory search alongside vector search.
- **session-pickup**: An authentication tier that reads an existing `~/.claude/session.json` file written by claude-code, reusing the active subscription session without the user re-authenticating.
- **PKCE**: Proof Key for Code Exchange — an OAuth 2.0 extension for secure public-client authorization flows. Pattern's second auth tier; opens a localhost callback and stores the resulting token in the OS keyring.
- **three-segment cache layout**: The request composition strategy that divides a model request into three stable regions, each marked with `cache_control: ephemeral`: (1) system prompt + base instructions + tools, (2) message history, (3) current memory block state. Edits to memory invalidate only segment 3.
- **`cache_control`**: An Anthropic API feature that marks a request prefix as cacheable. Anthropic supports up to four cache breakpoints per request (the three-segment layout uses three slots) and offers TTL variants (5-minute default and 1-hour extended). Per-segment TTL choice is configurable based on stability expectations.
- **pseudo-turn / pseudo-message**: A synthesized message injected into the request that was never sent by a real user or agent. Used to surface memory block state (`[memory:current_state]`) and block-change events (`[memory:updated]`, `[memory:written]`) in the conversation history.
- **persona**: Pattern's term for an agent identity — the combination of instructions, memory blocks, and provider credentials that define a specific agent instance.
- **port-list doc**: A tracking document (`docs/plans/rewrite-v3-portlist.md`) listing every crate excluded from the narrowed workspace `members` list during the rewrite, with notes on which future design plan will handle each.
- **fate markers**: Source-code comments (`// MOVING TO:`, `// REPLACED BY:`, `// MOVING WITHIN CRATE:`) that annotate code in transition during the rewrite, preventing orphaned or cruft code from accumulating across phase boundaries.
- **rust-genai**: A Rust client library for generative AI providers. Pattern maintains a fork with auth patches; v3 rebases that fork to auth-only patches on current upstream.
- **cosa**: A future agent runtime mentioned as a forward-compatibility target. Not implemented in this plan; `AgentRuntime` trait semantics are designed to accommodate it.
- **iroh-rpc**: A future native IPC transport. Explicitly out of scope for this plan.
- **jj**: Jujutsu, a version control system. Mentioned as part of a future fs-based memory redesign; out of scope for this plan.
- **`DEFAULT_BASE_INSTRUCTIONS`**: A constant in the current codebase containing Pattern's base philosophical instructions to agents (burst consciousness, memory-as-continuity, authenticity). Preserved verbatim in v3's segment 1.
- **token bucket**: A rate-limiting algorithm where tokens accumulate over time up to a cap; each request consumes tokens. Used per-provider for both tokens-per-minute and tokens-per-day limits.
- **`RequestShaper`**: A trait in `pattern_provider` responsible for adding honest identification headers (identifying as pattern rather than impersonating claude-code; specific header choices left to implementation) and a pattern-specific system-prompt prefix to outbound requests.
- **session UUID**: A per-persona identifier included in request headers that rotates on configured boundaries, used for provider-side session tracking.
- **turn-level checkpoint**: A snapshot of the agent's execution environment captured at the start or end of each agent turn, enabling deterministic restore if a turn fails mid-execution.
- **`EnvSnapshot`**: The concrete type representing a turn-level checkpoint — the serialized Haskell session environment.
- **`MessageBatch` integrity**: A compression invariant: the existing compression strategies only archive complete batches, never partial ones. Preserved in v3.

## Architecture

Pattern v3 Foundation introduces a **trait-only core** (`pattern_core`) that defines contracts every subsequent crate implements. Runtime, memory, provider, and plugin crates depend on `pattern_core` for types and interfaces; they never depend on each other's implementations. This inverts the current pattern where `pattern_core` contains most execution logic and concrete types.

**Substrate layer**: Tidepool, a Haskell-in-Rust runtime, provides the agent execution environment. Agents write Haskell that runs inside Tidepool; IO is disabled at the language level (Tidepool evaluates pure expressions only). Effects request IO through `freer-simple` — the algebraic-effects library Tidepool uses — and Rust-side handlers fulfill each effect request under permission and policy control.

The FFI boundary between Rust and Haskell is instrumented with an external CPU/wall-clock timeout wrapper (Tidepool itself lacks these). Every agent turn is bounded; runaway programs are killable at the boundary.

**SDK hierarchy** (exposed to agent Haskell via module prefixes — cosa will need module support added as prep for a future phase):

```
Ctx
  identity:         agentId, workspaceId, project, caller
  memory:           read, write, append, search, recall, archive
  message:          send, reply, notify
  shell:            execute, spawn, kill, status
  file:             read, write, list
  sources:          stream, subscribe, list
  spawn:            (future — this plan ships stubs/types only)
  mcp:              (future)
  ipc:              (future)
  time:             now, sleep, schedule
  log, parallel, checkpoint
  tool:             call, list     -- dynamic-dispatch escape hatch
```

Namespaces not in scope for this plan (`spawn`, `mcp`, `ipc`) ship as effect declarations with `unimplemented!`-style handlers so the SDK surface is stable. Future design plans fill in handlers without breaking the SDK shape.

**Provider layer**: `pattern_provider` resolves Anthropic authentication across three paths, tried in order (explicit-over-ambient, matching Unix convention):

1. Stored OAuth — pattern's own PKCE-minted token from OS keyring / JSON-file fallback (`$XDG_CONFIG_HOME/pattern/creds/anthropic.json`). Most explicit: user ran `pattern auth` deliberately. `ResolvedCredential.source = AuthTier::StoredOauth`.
2. API key — `ANTHROPIC_API_KEY` env var. Env-level explicit choice. `source = AuthTier::ApiKey`.
3. Session-pickup from `~/.claude/.credentials.json` (claudeAiOauth wrapper). Ambient fallback — uses whatever claude-code is authenticated as. `source = AuthTier::SessionPickup`.

Tokens for pattern-owned paths live in the OS keyring (`keyring` crate, JSON-file fallback if keyring unavailable). Session-pickup reads but never writes claude-code's credentials file. See `crates/pattern_provider/CLAUDE.md §Anthropic auth chain — tier order` for the full rationale.

Requests are shaped by a `RequestShaper` implementing honest identification: client identifies itself as pattern (specific header values and User-Agent format left to implementation), per-persona session-UUID (rotates on configured boundaries), and pattern-specific system-prompt prefix filling the same structural slot as claude-code's `You are Claude Code` string (per rommie-code proof-of-concept).

Rate limiting uses per-provider token buckets for tokens-per-minute and tokens-per-day. Bucket exhaustion queues with jitter rather than silent hang.

**Memory layer (existing storage, repositioned rendering)**: Storage keeps the current loro-CRDT + sqlite + FTS/vector-index implementation unchanged. What changes is **where memory content lands in the model's context**.

Instead of rendering blocks into the system prompt (where an edit busts the entire prefix cache), the provider's request composer builds three cached segments:

```
[segment 1] cache_control: {1h TTL}  — system + base instructions + tool descriptions    ← very stable
[segment 2] cache_control: {5m TTL}  — historical message stream                         ← stable-ish
[segment 3] cache_control: {5m TTL}  — current block state as pseudo-turn                ← block-edit boundary
[fresh]                                latest user turn + in-progress tool results
```

A block edit invalidates segment 3 forward only; segment 1 (typically the largest static content) stays cached across edits. Default TTL per segment reflects stability expectations — segment 1 earns the 1-hour variant because base instructions rarely change across sessions; segments 2 and 3 use the 5-minute default because history rolls forward and block state is mutable. TTL choices are configurable, not hardcoded.

**Block changes between turns** surface as pseudo-messages embedded in segment 2 — `[memory:updated] block X modified: …` or `[memory:written] agent Y wrote block Z: …`. These bake into history as it rolls forward. **Current block state** renders in segment 3 as a synthesized `[memory:current_state]` pseudo-turn just before the latest user message, placing blocks near the recent-context attention window.

`DEFAULT_BASE_INSTRUCTIONS` (pattern's philosophy about burst consciousness, memory-as-continuity, authenticity) is preserved verbatim in segment 1.

## Existing Patterns

Codebase investigation surfaced the following. v3 Foundation preserves where the current implementation is sound; diverges where the v3 architecture demands.

**Preserved patterns**:

- **loro CRDT for memory blocks**: `crates/pattern_core/src/memory/cache.rs` manages `Arc<LoroDoc>` snapshots backed by sqlite. Storage layer intact.
- **pattern_db FTS5 + sqlite-vec hybrid search**: `crates/pattern_db/src/{fts.rs, vector.rs, search.rs}`. Used as-is.
- **Block schema with types**: `crates/pattern_core/src/memory/schema.rs` — Text / Map / List / Log / Composite with viewport/display rules. Task and Skill subtypes deferred to future plan.
- **MessageBatch integrity in compression**: `crates/pattern_core/src/context/compression.rs` operates on complete batches, never archives incomplete ones. All four strategies (Truncate / RecursiveSummarization / ImportanceBased / TimeDecay) retained as-is.
- **DEFAULT_BASE_INSTRUCTIONS**: `crates/pattern_core/src/context/mod.rs:27-78`. Copied verbatim into v3's system-prompt composer.
- **Coordination infrastructure**: `crates/pattern_core/src/coordination/` (supervisor, round-robin, pipeline, voting, sleeptime, dynamic). Not exercised in this plan's scope but left intact for future subagent work.
- **Anthropic OAuth keychain storage** from `pattern_auth`: absorbed into `pattern_provider`; session-pickup tier added as new path.

**Divergences from current code**:

- **pattern_core becomes trait-only**. Current `pattern_core` houses the agent loop (`agent/processing/loop_impl.rs:67`), tool registry, runtime orchestration, and concrete coordination logic. All execution machinery moves to `pattern_runtime`; only traits and types remain.
- **Memory rendering split from storage**. Current `crates/pattern_core/src/context/builder.rs:226-316` renders blocks inline in the system prompt. v3 splits: storage stays in `pattern_memory` (existing code); rendering moves to the provider's request composer, positioned per the three-segment cache layout.
- **pattern_auth dissolves**. Provider-related auth → `pattern_provider`. ATProto + Discord bits are out of scope for this plan (handled by future plugin-migration plan).
- **rust-genai fork rebased**. Current fork (`~/Projects/PatternProject/rust-genai`) carries auth patches plus other modifications that have diverged from upstream. v3 rebase strips to auth-only and lands on current upstream (needed for adaptive thinking, 1M-context Opus/Sonnet 4.6/4.7, newer beta headers).

**Patterns not applicable (no existing precedent)**:

- **Tidepool FFI integration**. Novel. Uses tidepool-runtime's `compile_haskell` / `compile_and_run_with_nursery_size` API per `docs/reference/tidepool.md`. No prior Pattern work to reference.
- **Three-segment cache layout**. Tentative pending claude-code verification (see Additional Considerations).

## Implementation Phases

<!-- START_PHASE_1 -->
### Phase 1: Branch + scaffold

**Goal:** Establish the `rewrite-v3` branch, new crate skeletons, and port-list tracking document. Infrastructure-only; no functional code yet.

**Components:**
- Tag `pre-rewrite-v3` applied to current `main` tip
- Branch `rewrite-v3` cut from the tag
- Workspace `Cargo.toml` `members` narrowed to `["crates/pattern_core", "crates/pattern_runtime", "crates/pattern_provider", "crates/pattern_db"]`
- `crates/pattern_core/` skeleton — `Cargo.toml`, `src/lib.rs` with module declarations
- `crates/pattern_runtime/` skeleton — `Cargo.toml`, `src/lib.rs`
- `crates/pattern_provider/` skeleton — `Cargo.toml`, `src/lib.rs`
- `docs/plans/rewrite-v3-portlist.md` — living doc listing excluded crates (pattern_cli, pattern_server, pattern_mcp, pattern_discord, pattern_nd, pattern_api, pattern_surreal_compat, pattern_auth) with "deferred to plan: X" notes

**Dependencies:** None (first phase)

**Done when:** `cargo check` succeeds on the narrowed workspace; port-list doc exists listing every currently-excluded crate with deferral notes; git history on `rewrite-v3` is clean since the tag. Infrastructure phase — verified operationally.
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: pattern_core trait definitions

**Goal:** Land the trait-only API that every other crate will implement or consume. No concrete implementations.

**Components:**
- `crates/pattern_core/src/traits/` — `AgentRuntime`, `Session`, `MemoryStore`, `ProviderClient` (including async `count_tokens` for exact pre-request counts + `usage`-field capture on responses), `MessageRouter`, `DataStream`, `SourceManager`
- `crates/pattern_core/src/types/` — `PersonaSnapshot`, `SessionSnapshot`, `Block`, `BlockHandle`, `Message`, `TurnInput`, `TurnOutput`, `Caller` (discriminates `Agent(persona_id)` vs `Human(user_id)`), `WorkspaceId`, `AgentId`, `ProjectId`
- `crates/pattern_core/src/error.rs` — `CoreError`, `RuntimeError`, `ProviderError`, `MemoryError` hierarchy with `#[non_exhaustive]` and thiserror/miette integration
- Trait-satisfaction doc-examples (dummy structs implementing traits) that compile without concrete impls, exercising trait shape
- Rustdoc on public surface with inline examples

**Dependencies:** Phase 1

**Done when:** `cargo check -p pattern_core` succeeds with zero warnings; `cargo doc -p pattern_core` produces complete documentation; doc-examples compile; trait-satisfaction dummy impls compile. Covers: `v3-foundation.AC1.*`.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Tidepool FFI + minimal runtime

**Goal:** Tidepool embedded in Rust with external CPU/wall-clock timeout wrapping; minimal agent loop runs a trivial Haskell program and dispatches SDK effects through Rust handlers.

**Components:**
- `crates/pattern_runtime/src/tidepool/` — FFI wrappers around `compile_haskell`, `compile_and_run_with_nursery_size`; handles GHC-side panics as `RuntimeError` variants
- `crates/pattern_runtime/src/timeout.rs` — wall-clock + CPU timeout harness wrapping FFI calls (Tidepool provides neither). Configurable per-turn budget, default 30s wall / 10s CPU.
- `crates/pattern_runtime/src/sdk/` — freer-simple effect algebra declaration for `memory`, `message`, `shell`, `file`, `sources`, `mcp`, `time`, `ipc`, `log`. `spawn` is declared but all constructors are `unimplemented!` stubs.
- `crates/pattern_runtime/src/loop_impl.rs` — agent turn loop: `instantiate → step → handle YieldForHost → step → Completed`
- `crates/pattern_runtime/src/checkpoint.rs` — turn-level `EnvSnapshot` capture and restore
- `crates/pattern_runtime/src/agent_runtime_impl.rs` — implements `pattern_core::AgentRuntime` / `Session` traits using Tidepool as the executor
- Handlers: `time` (fully implemented), `log` (fully implemented), all others stub-or-error for this plan

**Dependencies:** Phase 2

**Done when:**
- A "hello-world" Haskell program (`pure "hello"` or effect-requesting stub) loads, runs, and completes via the runtime
- Timeout harness kills a provably-infinite program before exceeding the configured budget
- Turn-level checkpoint/restore round-trips without data loss on a non-trivial session state
- `time` and `log` effects flow through their handlers and return results to agent code
- Covers: `v3-foundation.AC2.*`.
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: pattern_provider (rebased rust-genai + three-tier auth)

**Goal:** Anthropic LLM access via subscription session-pickup, PKCE fallback, and API-key path. Rate-limited. Request-shaped. Ready for runtime integration.

**Components:**
- `crates/pattern_provider/Cargo.toml` — depends on rebased `rust-genai` fork (pattern's fork, auth-only patches, on current upstream)
- `crates/pattern_provider/src/auth/session_pickup.rs` — reads `~/.claude/session.json` (path per claude-code source), parses, validates expiry; never writes claude-code's file
- `crates/pattern_provider/src/auth/pkce.rs` — full PKCE flow per `docs/reference/oauth-and-detection.md` (SHA256 challenge, localhost callback, refresh with 5-min buffer)
- `crates/pattern_provider/src/auth/api_key.rs` — env + config file
- `crates/pattern_provider/src/auth/resolver.rs` — three-tier resolution with fallback chain; stale-session handling (skip without error)
- `crates/pattern_provider/src/creds_store.rs` — `keyring` crate primary + JSON fallback (0600/0700 permissions); used only for our own credentials, not claude-code's session
- `crates/pattern_provider/src/shaper.rs` — `RequestShaper` trait + `HonestPatternShaper` default impl
- `crates/pattern_provider/src/ratelimit.rs` — per-provider token buckets
- `crates/pattern_provider/src/token_count.rs` — async `count_tokens` implementation (Anthropic `/v1/messages/count_tokens` endpoint for pre-request sizing; `usage`-field capture from response for post-hoc accounting and subsequent estimation cache); exposed via the `ProviderClient::count_tokens` trait method
- `crates/pattern_provider/src/session_uuid.rs` — per-persona UUID with rotation on explicit caller signal
- `crates/pattern_provider/src/provider_impl.rs` — implements `pattern_core::ProviderClient`

**Dependencies:** Phase 2

**Done when:**
- Session-pickup path: with valid `~/.claude/session.json`, provider makes a real Anthropic request and returns a real response
- PKCE path: with no session/key, PKCE flow opens browser, completes, stores token, makes request, returns response
- API-key path: with only `ANTHROPIC_API_KEY` set, provider uses it, makes request, returns response
- Shaper: identification headers populated per implementation choice; system-prompt prefix contains pattern-specific content, not `You are Claude Code`
- Rate limiter: queues when bucket exhausted, releases on refill, visible delay surfaces to caller
- `count_tokens` returns real Anthropic-reported counts for a representative request; post-response `usage` is captured and available to callers
- Covers: `v3-foundation.AC3.*`, `v3-foundation.AC4.*`, `v3-foundation.AC5.*`, `v3-foundation.AC5b.*`.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: Memory integration with repositioned rendering

**Goal:** Existing pattern_memory storage wired into the new runtime; memory content positioned in a segment-3 pseudo-turn with its own cache breakpoint; block changes surface as pseudo-messages; system prompt no longer contains block content.

**Blocking pre-phase research** (see Additional Considerations): verify three-segment cache layout against `~/Git_Repos/claude-code/services/api/` and rommie-code patches. Stamp `§3.4` of brainstorm draft with "verified: YYYY-MM-DD" and resulting decision. If revised, adjust this phase's design before starting implementation.

**Components:**
- `crates/pattern_runtime/src/memory/adapter.rs` — wraps existing `pattern_core::memory` storage (preserved, re-exposed via appropriate module path) as an impl of `pattern_core::MemoryStore`
- `crates/pattern_runtime/src/memory/pseudo_messages.rs` — emission of `[memory:updated]` / `[memory:written]` pseudo-messages on block writes; queues for insertion into next turn's message history
- `crates/pattern_runtime/src/memory/current_state.rs` — synthesizes the pre-turn `[memory:current_state]` pseudo-turn rendering core + loaded-working blocks
- `crates/pattern_provider/src/compose.rs` — request composer assembling the three cached segments with `cache_control` markers at each boundary (system→history→block-state→fresh); exposes per-breakpoint TTL selection (5-min default / 1-hour extended) with defaults tuned to segment stability
- Metric instrumentation: per-segment cache-hit reporting (from Anthropic response headers) for observability
- Removal of block-rendering code from any surviving system-prompt builder path

**Dependencies:** Phase 3 (runtime), Phase 4 (provider)

**Done when:**
- A persona writes a block; next turn's composed request shows block content in segment 3 with `cache_control` marker, NOT in segment 1
- A persona edits a block; subsequent request invalidates segment 3 onward only; cache-hit metrics confirm segment 1 stays hit
- Block changes between turns appear as `[memory:updated]` / `[memory:written]` pseudo-messages in segment 2
- Existing compression strategies operate on the reshaped message stream without regression (batch integrity preserved); strategy shape unchanged, but call sites that previously consumed heuristic token estimates now consume provider-reported counts via async `count_tokens`
- Context-length / compaction-threshold decisions use provider-reported counts; any previously-sync token paths that become async are explicitly documented in this phase's commit messages
- Memory storage unchanged; existing reads/writes round-trip correctly
- Covers: `v3-foundation.AC6.*`, `v3-foundation.AC7.*`, `v3-foundation.AC8.*`.
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: End-to-end smoke test

**Goal:** Demonstration of the full DoD: create persona, authenticate, talk to Claude, retain memory across restart, verify cache behavior on block edit.

**Components:**
- `crates/pattern_runtime/tests/smoke_e2e.rs` — integration test exercising the full flow in API-key mode (CI-friendly)
- `crates/pattern_runtime/tests/smoke_e2e_oauth.rs` — manual-only test gated behind `PATTERN_V3_MANUAL_OAUTH=1` env flag; documented procedure for running against subscription session-pickup
- Cache-hit metric assertions: segment 1 cache-hit rate stays high across a block edit; segment 3 invalidates as expected
- Minimal CLI entry point (`pattern-v3 spawn` or similar) sufficient to drive the smoke flow from the command line — not polished UX

**Dependencies:** Phase 5

**Done when:**
- Smoke test passes deterministically in API-key mode on CI
- Manually-run subscription-OAuth smoke test passes on user's machine (documented procedure in test file header)
- Cache-hit metric assertions pass
- CLI entry point drives the full DoD demonstration (create → auth → talk → write → restart → read → edit → observe cache) end-to-end
- Covers: `v3-foundation.AC9.*`.
<!-- END_PHASE_6 -->

## Execution Mode Recommendation

**Recommendation: Collaborative.**

Reasoning:

- **Tidepool integration is novel**. N=1 external consumer (pattern is the second), alpha-stage, no resource/timeout infrastructure we can rely on. FFI surprises are likely; the timeout wrapper in Phase 3 may need iteration. Requires judgment, not mechanical execution.
- **Cache-breakpoint design is tentative**. Phase 5 is explicitly blocked on verifying §3.4 against claude-code source; results may revise the layout mid-plan. Autonomous assumes the design is stable enough for mechanical implementation; this plan's design isn't.
- **OAuth preservation is adversarial against unknowns**. Anthropic's server-side detection can change; request shaping may need iteration based on real response behavior. Benefits from human-in-loop review between phases 4 and 5.
- **Memory repositioning is a real architectural change**. Even with storage preserved, the rendering split has cache-impact consequences worth a human checkpoint.
- **6 phases at substantial scope**. Not small enough for Light (1-3 phases); not mechanical enough for Autonomous safely.

User can override to Autonomous if comfortable with the risk profile and willing to handle pivots manually. Light is not appropriate for this scope.

## Additional Considerations

**Required research before Phase 5** (blocking that phase only, not the entire plan):

- Examine `~/Git_Repos/claude-code/services/api/` for how claude-code positions `cache_control` markers, treats segments as stable vs. volatile, and handles tool-result caching edge cases
- Cross-check rommie-code patches for multi-provider cache variations
- Check upstream `rust-genai` for `cache_control` TTL-variant support (5-minute default, 1-hour extended breakpoints). If upstream has it, our fork's rebase just uses it. If upstream lacks it, add TTL support to our fork's patch set alongside the auth patches (keeps the fork's value-add tight and justified).
- Stamp brainstorm-draft §3.4 with "verified: YYYY-MM-DD" and resulting decision; adjust Phase 5 design if revised

**Error handling at FFI boundary:**

- GHC runtime panics surfaced as `RuntimeError::GhcPanic { reason }`
- Timeout violations return `RuntimeError::Timeout { wall_ms, cpu_ms }`
- EffectResponse-node-limit hits return `RuntimeError::EffectOverflow`
- Caller (agent loop) can retry, checkpoint-rollback, or propagate to the persona's error-handling program

**Edge cases:**

- Concurrent token refresh: auth resolver serializes refresh per-provider-per-persona via mutex to prevent racing refresh calls
- Keyring unavailable: automatic JSON fallback with file-permissions enforcement (0600 file, 0700 parent dir)
- Tidepool runtime crash: timeout wrapper treats crash as `RuntimeError::RuntimeCrashed`; session marked unusable; caller must instantiate a fresh session
- Session-pickup with stale token: resolver skips session-pickup tier without error, falls through to PKCE / API-key
- Session-pickup file missing or malformed: skipped without error, falls through

**Forward compatibility with later design plans:**

- `AgentRuntime` trait designed around cosa-like semantics (per-statement observability, cheap fork, reifiable env) so a future cosa-native runtime plan can slot in without changing the trait
- SDK hierarchy has stable name slots for `spawn`, `mcp`, `ipc` even though handlers are stubbed — future plans fill handlers without SDK surface changes
- Memory storage untouched so future fs-based redesign can swap storage without affecting runtime callers
- `pattern_provider` keychain abstraction reusable by future plugin crates needing their own credential storage

**Intermediate code-state policy (dead code vs. cruft):**

During this plan, pattern_core is being gutted from "contains everything" to "traits only." The existing implementation code (agent loop in `agent/processing/loop_impl.rs`, tool registry, coordination patterns, memory cache, context builder, router) has a defined fate per component. Intermediate states during the rewrite have explicit rules to prevent cruft accumulation.

**Acceptable intermediate states:**

- **Code pending move to a crate that exists in-tree**: the code may temporarily remain in its old location with a clearly-commented `// MOVING TO: crates/<target>` marker at the module / item level. Move must happen by end of the phase that introduces the target.
- **Code pending move to a crate not yet in `members`**: the crate directory is excluded from workspace `members` (per the workspace-manipulation pattern in the brainstorm draft §9.3); code sits in the excluded crate directory until its turn. Port-list doc records the deferral with target-plan reference.
- **Code pending deletion because it's being replaced**: the old code may remain through the phase where its replacement is being built, clearly marked `// REPLACED BY: <new-path> — delete after phase N lands`. Dedicated deletion commit in phase N+1 with rationale in the commit message.
- **Code preserved but moving namespaces**: if moving to a different module within the same crate, either move in the same commit as the rename OR add `// MOVING WITHIN CRATE: <new-path>` comment. Move by end of the phase.

**NOT acceptable (cruft):**

- Code with no defined fate sitting in the tree across phase boundaries.
- `unimplemented!()` or `todo!()` in surface API without a comment identifying which phase fills it and which AC covers it.
- Commented-out code left in source files. If code is being removed, remove it; git preserves history.
- Modules with empty `mod.rs` and no plan to populate them.
- References in `Cargo.toml` or `use` statements to items that have been removed and not replaced.

**Verification**: every phase's "done when" check includes a scan for these cruft markers. Phase 1's port-list doc is the authoritative tracker for what is in flight and where it's going.

**Relationship to workspace `members` list**: the `members` list is narrowed per the workspace-manipulation pattern. Crates currently being worked on: in `members`. Crates not yet touched that reference rewritten stuff: excluded from `members`. Already-rewritten-and-working: in `members`. Retired (pattern_auth, pattern_surreal_compat): directory deleted in dedicated commits once their responsibilities have migrated. `members` list grows as the rewrite progresses.

**Scope containment reminders:**

- Do not add plugin loading to any phase even if "easy"; plugin system is its own design plan
- Do not attempt v2→v3 data migration; migration is a separate plan after subagent + plugin plans land
- Do not redesign compaction strategies; current four retained as-is even if tempted by deferred-enhancements list
- Do not build iroh-rpc transport; plugin-transport work is a separate plan
