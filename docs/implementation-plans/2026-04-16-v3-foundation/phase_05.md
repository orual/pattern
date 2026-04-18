# Pattern v3 Foundation — Phase 5: Memory integration with repositioned rendering

**Goal:** Wire the preserved `pattern_core::memory` storage into Phase 2's `MemoryStore` trait surface via a thin adapter. Relocate memory content from the system prompt into a segment-3 `[memory:current_state]` pseudo-turn. Emit `[memory:updated]` / `[memory:written]` pseudo-messages in segment 2 when blocks change between turns. Build a three-segment cache-composer as a pipeline of passes with per-segment TTL selection, scope choice, and break-detection hashing. Extend `CacheControl` to carry TTL variants. Migrate compaction call-sites from heuristic token counting to `ProviderClient::count_tokens`.

**Architecture:**
- Storage layer (loro CRDT + sqlite + FTS/vector in `pattern_core::memory`) is preserved unchanged; investigation confirmed the trait API already matches Phase 2's `MemoryStore` shape.
- Composer in `pattern_provider::compose` is a **pipeline of passes** that transform a `PartialRequest` into the final `ChatRequest`. Phase 5 implements base-request → system-block assembly → history-inclusion → memory-pseudo-turn injection → cache_control marker placement → validation & finalization. Future passes (cache_reference stitching, cache_edits for microcompact, cache-strategy-flip) attach as additional passes without reshaping the pipeline.
- `CacheProfile` latched at session open holds TTL policy, scope, beta set, tool registry snapshot. Composer reads from it; no mid-session flips (matches claude-code's observed constraint — see `docs/reference/anthropic-prompt-caching.md` §"Session-stable TTL latching").
- Break-detection hashing captured from turn 2 onward; when `cache_read_input_tokens` drops unexpectedly, diagnostic data attributes the bust to a specific component. Cheap insurance that unlocks future observability work.
- Pseudo-messages (`[memory:current_state]`, `[memory:updated]`, `[memory:written]`) are user-role messages with `<system-reminder>`-wrapped content, matching the tag convention from Phase 4 and claude-code's conversational memory pattern.
- Compression strategies preserved as-is; call-sites swap from heuristic token counting to async `count_tokens`.

**Tech Stack:** Rust 2024, reuses Phase 4's `pattern_provider` composer infrastructure, `serde_json` for break-detection hashing, `tracing` for cache-hit metric spans. No new external deps.

**Scope:** Phase 5 of 6. Covers v3-foundation.AC6.*, AC7.*, AC8.*.

**Codebase verified:** 2026-04-16

---

## Acceptance Criteria Coverage

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

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**Working bookmark:** `rewrite-v3`
**Pre-phase state after Phase 4:** `pattern_core` (traits + types + errors + preserved memory storage + base_instructions), `pattern_runtime` (Tidepool FFI + 11-handler bundle + session lifecycle), `pattern_provider` (three-tier auth + honest-pattern shaper + rate limiting + count_tokens + provider impl). `rust-genai` fork rebased onto upstream with minimal auth + system-prompt-array patches.

**Key codebase references (from Phase 5 investigation):**

- Current memory storage: `crates/pattern_core/src/memory/{cache.rs(2261), document.rs(2006), schema.rs(608), store.rs(262), mod.rs(126)}` — preserved verbatim.
- Block rendering relocation target: `crates/pattern_core/src/context/builder.rs:226-316` (moves to composer as segment-3 render).
- `DEFAULT_BASE_INSTRUCTIONS`: `crates/pattern_core/src/base_instructions.rs` (Phase 2 extracted this from `context/mod.rs:27-78`). Referenced verbatim in segment 1 per AC7.4.
- Compression strategies: `crates/pattern_core/src/context/compression.rs` (preserved; call-sites migrate to async `count_tokens`).
- Message shape: `crates/pattern_core/src/messages/types.rs` + `batch.rs` — supports pseudo-message injection via existing `synthetic_id` precedent (`loop_impl.rs:1054+`, `errors.rs:310+`).
- Tool schema: `crates/pattern_core/src/tool/mod.rs:400` exposes `ToolRegistry::to_genai_tools() -> Vec<genai::chat::Tool>` — Anthropic-compatible format ready to include in segment 1 directly.
- **CacheControl collision**: current `pattern_core::messages::types::CacheControl::Ephemeral` is a unit variant. Phase 5 extends it to carry TTL variants (5m / 1h / 24h).
- Cache reference doc: `docs/reference/anthropic-prompt-caching.md` — comprehensive claude-code cache pattern notes; Phase 5 implements a subset.

**Phase 5 design principle:** *mimic claude-code's patterns within reason; don't gate off future sophistication.* Composer is a pipeline of passes with explicit extension points. See `docs/reference/anthropic-prompt-caching.md` §"Not shipped in v3 foundation" for deferred features whose architectural hooks exist in Phase 5's output.

**Build tools:**
- `cargo check -p pattern_provider -p pattern_runtime -p pattern_core`
- `cargo nextest run --workspace`
- `cargo test --doc --workspace`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `just pre-commit-all`

**Commit convention:** `[pattern-<crate>]` per crate. `[meta]` for cross-crate docs / manifest.

**Audit script:** `scripts/audit-rewrite-state.sh` (Phase 2). Must pass at phase close.

**Rust-coding-style reminders:**
- `CacheControl` becomes `#[non_exhaustive]` with explicit variants for TTL.
- Newtype wrappers where they add clarity (`CacheBreakpointCount`, `TurnId`, `BlockChangeId`).
- `module.rs + module/submodule.rs` layout.
- Composer passes are structs implementing a `ComposerPass` trait for pipeline extensibility.

**Design reference:** `docs/design-plans/2026-04-16-v3-foundation.md` Phase 5 (between `<!-- START_PHASE_5 -->` and `<!-- END_PHASE_5 -->`). Blocking pre-phase research per design is substantially complete — this plan bakes in the outcome.

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
<!-- START_TASK_1 -->
### Task 1: Extend `CacheControl` with TTL variants

**Verifies:** AC7.1b, AC7.5b.

**Files:**
- Modify: `crates/pattern_core/src/types/message.rs` (remove pattern_core's placeholder `CacheControl`; re-export genai's type instead)
- Verify: no stale call sites reference the retired pattern_core `CacheControl` placeholder

**Implementation:**

**Upstream rust-genai already provides all the TTL variants we need** (verified 2026-04-16 in `upstream/main:src/adapter/adapters/anthropic/adapter_impl.rs:5, 543-546`):

```rust
// genai::chat::CacheControl (upstream)
pub enum CacheControl {
    Memory,        // OpenAI-style prompt cache (not used by Pattern)
    Ephemeral,     // Anthropic default (5m)
    Ephemeral5m,
    Ephemeral1h,
    Ephemeral24h,
}
```

Upstream does NOT support a `scope` field, and Pattern's single-user-intra-org use case doesn't need one — Pattern's agent constellations all live under the user's subscription org, so org-scoped caching (the default when scope is omitted) already gives cross-agent hits. Multi-org scope sharing is out of foundation scope.

**Phase 5's approach: use `genai::chat::CacheControl` directly.** No pattern_core mirror. No `From` conversion layer. No `CacheMarker` wrapper. Pattern_core holds *domain* types (persona, memory block, agent id); provider-config-shaped types come from genai directly. Consistency with Phase 4's `SystemPrompt` / `ChatMessage` usage.

```rust
// crates/pattern_core/src/types/message.rs (post-Phase-2 location)
//
// Re-export genai's CacheControl as the canonical type. pattern_core itself
// doesn't depend on genai as a hard dep; this re-export is feature-gated so
// callers who don't pull in the provider stack don't take the genai weight.
// In practice, every active workspace consumer (pattern_provider,
// pattern_runtime) pulls genai transitively, so the re-export is always
// present at build time.

#[cfg(feature = "provider-types")]
pub use genai::chat::CacheControl;
```

Pattern_core adds a `provider-types` feature that enables the re-export. pattern_provider and pattern_runtime set the feature in their `[dependencies]` block for pattern_core. Callers that don't need cache-control semantics (e.g., hypothetical future crates doing pure memory-layer work) can skip the feature.

**Step 1:** Delete any placeholder `CacheControl` enum in `pattern_core::types::message`. Add a direct re-export from genai:

```rust
// crates/pattern_core/src/types/message.rs
pub use genai::chat::CacheControl;
```

**Step 2:** Add `genai` as a regular dep to `pattern_core/Cargo.toml`:

```toml
[dependencies]
genai = { workspace = true }
```

(No feature gate. The theoretical consumer that would want pattern_core without genai is hypothetical; real workspace consumers all pull genai transitively. Keeping the dep unconditional is simpler than carrying a feature and the modeling-scope argument that justifies it.)

**Step 3:** Verify `cargo check -p pattern_core` compiles with the new dep.

**Step 4:** AC7.5b enforcement lives at the `CacheProfile` construction site (Task 2) — if the profile requests `Ephemeral1h` but the session's auth tier / subscription status doesn't allow extended TTL, the marker resolver downgrades to `Ephemeral5m` with a `tracing::warn`. The enum-based typing means malformed input is impossible to construct; no deser-level validation needed.

**Step 5:** Verify.

```bash
cargo check -p pattern_core
cargo check -p pattern_provider
```

**Commit:**

```bash
jj describe -m "[pattern-core] re-export genai::chat::CacheControl directly; no pattern-side mirror (AC7.1b, AC7.5b)

Pattern_core holds domain types; provider-config-shaped types (CacheControl,
SystemPrompt, ChatMessage, Tool) come from genai directly. This drops the
unnecessary mirror + From-conversion layer the earlier draft proposed.
Upstream rust-genai already supports Ephemeral5m/1h/24h; no scope field
upstream and Pattern doesn't need one (single-user intra-org use case)."
jj new
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `CacheProfile` — session-latched cache policy

**Verifies:** contributes to AC7.* (profile is read by composer), prevents mid-session cache-bust per reference-doc observation.

**Files:**
- Create: `crates/pattern_provider/src/compose.rs` (module root — established in Phase 4, extended here)
- Create: `crates/pattern_provider/src/compose/profile.rs`

**Implementation:**

```rust
//! Session-stable cache policy. Latched at session open; never mutated
//! mid-session. Matches the empirically-observed constraint that mid-session
//! TTL flips bust the server-side prompt cache (~20K tokens per flip).
//!
//! Uses genai's `CacheControl` type directly per the Phase 5 Task 1
//! decision — no pattern-side mirror.

use genai::chat::CacheControl;

#[derive(Debug, Clone)]
pub struct CacheProfile {
    /// TTL for segment 1 (system + instructions + tools). Default Ephemeral1h
    /// for long-lived stable content. Can be downgraded to Ephemeral5m if the
    /// caller's subscription is in overage (future billing-aware plan) or if
    /// the extended-cache-ttl-2025-04-11 beta header is unavailable.
    pub segment_1_ttl: CacheControl,

    /// TTL for segment 2 (history boundary). Default Ephemeral (5m).
    pub segment_2_ttl: CacheControl,

    /// TTL for segment 3 (memory pseudo-turn). Default Ephemeral (5m).
    pub segment_3_ttl: CacheControl,

    /// Whether 1h-TTL-capable (segment 1 may use Ephemeral1h). Latched from
    /// subscription status at session open. When false, segment 1 falls back
    /// to Ephemeral5m regardless of segment_1_ttl field.
    pub allow_extended_ttl: bool,

    /// Future-hook: strategy enum for deciding which blocks carry markers.
    /// Phase 5 only supports Default; Mcp and Bedrock variants declared for
    /// future MCP-integration / Bedrock-provider plans.
    pub strategy: CacheStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStrategy {
    /// Three-segment layout: system+tools → history → memory-pseudo-turn.
    /// Phase 5 default.
    Default,

    /// TODO: future MCP integration plan; adapts cache boundaries when MCP
    /// tools are dynamically discovered/removed mid-session.
    /// Unimplemented — callers should not construct this variant yet.
    McpAware,

    /// TODO: future Bedrock provider plan; different cache boundary rules.
    BedrockExtraBody,
}

impl CacheProfile {
    pub fn default_anthropic_subscriber() -> Self {
        Self {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        }
    }

    pub fn default_api_key() -> Self {
        Self {
            segment_1_ttl: CacheControl::Ephemeral1h,
            segment_2_ttl: CacheControl::Ephemeral5m,
            segment_3_ttl: CacheControl::Ephemeral5m,
            allow_extended_ttl: true,
            strategy: CacheStrategy::Default,
        }
    }

    /// Resolve the effective segment-1 CacheControl, accounting for allow_extended_ttl.
    /// When extended TTL isn't permitted, downgrades Ephemeral1h/24h → Ephemeral5m
    /// with a tracing::warn so cache-break-detection can attribute any bust.
    pub fn segment_1_control(&self) -> CacheControl {
        match (self.allow_extended_ttl, self.segment_1_ttl) {
            (false, CacheControl::Ephemeral1h) | (false, CacheControl::Ephemeral24h) => {
                tracing::warn!(
                    requested = ?self.segment_1_ttl,
                    applied = "Ephemeral5m",
                    "segment_1 extended TTL disabled; downgrading",
                );
                CacheControl::Ephemeral5m
            }
            _ => self.segment_1_ttl,
        }
    }

    pub fn segment_2_control(&self) -> CacheControl { self.segment_2_ttl }
    pub fn segment_3_control(&self) -> CacheControl { self.segment_3_ttl }

    /// True if any segment requests an extended-TTL variant, indicating the
    /// shaper must ensure `anthropic-beta: extended-cache-ttl-2025-04-11`
    /// is present in request headers.
    pub fn requires_extended_ttl_beta(&self) -> bool {
        [self.segment_1_control(), self.segment_2_control(), self.segment_3_control()]
            .iter()
            .any(|cc| matches!(cc, CacheControl::Ephemeral1h | CacheControl::Ephemeral24h))
    }
}
```

**Step 1:** Implement profile.

**Step 2:** Wire into `AnthropicProviderClient` (Phase 4): session open path latches the profile based on auth-tier + subscription status. Store on `TidepoolSession` (Phase 3) so the composer can read from it per-turn.

**Step 3:** Unit tests:
- `default_anthropic_subscriber()` → 1h TTL on segment 1, 5m on segments 2 and 3, strategy = Default
- `default_api_key()` → identical defaults (scope is not modeled)
- `allow_extended_ttl: false` forces segment 1 to `Ephemeral5m` regardless of segment_1_ttl value, and emits a `tracing::warn` (use tracing-test to capture the warn in the assertion)
- `requires_extended_ttl_beta()` returns true when any segment uses 1h or 24h; false when all are 5m
- `CacheStrategy::McpAware` / `BedrockExtraBody` constructible but composer pass for Phase 5 panics with a `todo!` citing future plans if strategy != Default

**Commit:**

```bash
jj describe -m "[pattern-provider] CacheProfile latched at session open; per-segment TTL + strategy hook (AC7.*)"
jj new
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `ComposerPass` trait + `PartialRequest` skeleton

**Verifies:** contributes to composer extensibility; no AC directly, but enables AC7.1 (exactly 3 markers via tracked count).

**Files:**
- Create: `crates/pattern_provider/src/compose/pipeline.rs`
- Create: `crates/pattern_provider/src/compose/partial_request.rs`

**Implementation:**

```rust
//! Composer pipeline. Each pass transforms a PartialRequest. Passes execute
//! in registered order; final finalization assembles and validates.

use pattern_core::error::ProviderError;

pub trait ComposerPass: Send + Sync {
    /// Human-readable name for debug / break-detection logs.
    fn name(&self) -> &'static str;

    /// Apply this pass to the partial request. Passes can mutate headers,
    /// system blocks, messages, and the breakpoint-tracker. They cannot
    /// perform I/O — all data needed must be captured at pass construction.
    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError>;
}

/// Mutable request being assembled by the pipeline.
pub struct PartialRequest {
    pub model: String,
    pub system_blocks: Vec<SystemBlock>,
    pub messages: Vec<MessageBlock>,
    pub tools: Vec<ToolSchema>,
    pub extra_headers: Vec<(String, String)>,
    pub breakpoints: BreakpointTracker,
    // ... other fields as needed (max_tokens, thinking config, etc.)
}

/// Tracks cache_control marker placements across passes. Enforces budget.
pub struct BreakpointTracker {
    placed: Vec<BreakpointPlacement>,
    max: usize, // 4 per Anthropic
}

pub struct BreakpointPlacement {
    pub location: BreakpointLocation,
    /// Cache-control value to attach at `location`. Uses genai's type directly.
    pub control: genai::chat::CacheControl,
    pub placed_by_pass: &'static str,
}

pub enum BreakpointLocation {
    SystemBlock(usize),       // index into system_blocks
    MessageBlock(usize),      // index into messages
    ToolSchema(usize),        // index into tools — future; Phase 5 doesn't use
}

impl BreakpointTracker {
    pub fn new() -> Self { Self { placed: vec![], max: 4 } }

    pub fn place(&mut self, location: BreakpointLocation, control: genai::chat::CacheControl, pass: &'static str)
        -> Result<(), ProviderError>
    {
        if self.placed.len() >= self.max {
            return Err(ProviderError::CacheBreakpointBudgetExceeded {
                budget: self.max,
                placed_by: self.placed.iter().map(|p| p.placed_by_pass).collect(),
                attempted_by: pass,
            });
        }
        self.placed.push(BreakpointPlacement { location, control, placed_by_pass: pass });
        Ok(())
    }

    pub fn count(&self) -> usize { self.placed.len() }
    pub fn placements(&self) -> &[BreakpointPlacement] { &self.placed }
}

/// Compose a ChatRequest through a sequence of passes.
pub fn compose(passes: &[Box<dyn ComposerPass>], initial: PartialRequest)
    -> Result<ChatRequest, ProviderError>
{
    let mut partial = initial;
    for pass in passes {
        pass.apply(&mut partial)
            .map_err(|e| ProviderError::ComposerPassFailed { pass: pass.name(), source: Box::new(e) })?;
    }
    finalize(partial)
}

fn finalize(partial: PartialRequest) -> Result<ChatRequest, ProviderError> {
    // Apply all accumulated breakpoints to the corresponding blocks.
    // Validate breakpoint count (must be ≤4; Phase 5 expects exactly 3).
    // Build the final ChatRequest for rust-genai.
    ...
}
```

**Step 1:** Write the trait + skeleton types.

**Step 2:** Add `ProviderError::{ComposerPassFailed, CacheBreakpointBudgetExceeded}` variants to `pattern_core::error::provider`.

**Step 3:** No tests yet — Tasks 4–10 add concrete passes and their tests.

**Commit:**

```bash
jj describe -m "[pattern-provider] ComposerPass trait + PartialRequest + BreakpointTracker (infrastructure)"
jj new
```
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->
<!-- START_TASK_4 -->
### Task 4: `MemoryStoreAdapter` — wrap preserved storage as Phase 2's trait impl

**Verifies:** AC6.1, AC6.2, AC6.3, AC6.4, AC6.5, AC6.6.

**Files:**
- Create: `crates/pattern_runtime/src/memory/adapter.rs`
- Modify: `crates/pattern_runtime/src/memory.rs` (module root — expose adapter)

**Implementation:**

Investigator confirmed the existing `MemoryStore` trait in `pattern_core::memory::store` has the right shape. Phase 2's relocation puts the trait at `pattern_core::traits::memory_store` and a dummy impl in `pattern_core::memory::store` satisfies it. Phase 5's adapter is a thin wrapper that bridges any shape differences introduced in Phase 2's refinements (e.g., the `block_changes_since` and `subscribe_writes` methods Phase 2 adds).

```rust
//! Adapter wrapping pattern_core's preserved storage as the Phase 2
//! MemoryStore trait surface. Delegates read/write/search to the storage
//! layer; adds change-tracking for Phase 5's pseudo-message emission.

use pattern_core::{
    error::MemoryError,
    traits::MemoryStore,
    types::{Block, BlockHandle, BlockChange, ...},
};
use std::sync::Arc;

pub struct MemoryStoreAdapter {
    storage: Arc<pattern_core::memory::cache::MemoryCache>, // preserved type
    change_log: Arc<parking_lot::RwLock<ChangeLog>>,        // Phase 5 addition
}

#[async_trait::async_trait]
impl MemoryStore for MemoryStoreAdapter {
    async fn create_block(&self, block: NewBlock) -> Result<BlockHandle, MemoryError> {
        // Delegate to storage; record change in log.
        let handle = self.storage.create_block(block.clone()).await?;
        self.change_log.write().record(ChangeEvent::Written {
            handle: handle.clone(),
            author: block.author,
            turn_id: block.turn_id,
            timestamp: jiff::Timestamp::now(), // stored as UTC instant; rendered in local time (Task 6)
        });
        Ok(handle)
    }

    async fn update_block(&self, handle: &BlockHandle, content: BlockContent, author: Caller)
        -> Result<(), MemoryError>
    {
        // Fetch previous content for diff; update storage; record change.
        let previous = self.storage.get_block(handle).await?;
        self.storage.update_block(handle, content.clone()).await?;
        self.change_log.write().record(ChangeEvent::Updated {
            handle: handle.clone(),
            author,
            previous_content_hash: hash(&previous),
            new_content: content,
            timestamp: jiff::Timestamp::now(), // stored as UTC instant; rendered in local time (Task 6)
        });
        Ok(())
    }

    async fn get_block(&self, handle: &BlockHandle) -> Result<Option<Block>, MemoryError> {
        self.storage.get_block(handle).await.map_err(Into::into)
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, MemoryError> {
        // Delegates to pattern_db's FTS+vector hybrid search via storage layer.
        self.storage.search(query).await.map_err(Into::into)
    }

    async fn block_changes_since(&self, turn: TurnId) -> Result<Vec<BlockChange>, MemoryError> {
        Ok(self.change_log.read().since(turn))
    }

    // ... other trait methods ...
}
```

**Step 1:** Implement adapter.

**Step 2:** Integration tests that exercise each AC:

- AC6.1: write block → persists to storage → subsequent read returns content
- AC6.2: read returns current content including recent writes
- AC6.3: search returns FTS+vector results (assert via pattern_db test fixture)
- AC6.4: write block, tear down adapter, rebuild from storage, read returns same content (simulates process restart)
- AC6.5: write to nonexistent handle returns `MemoryError::BlockNotFound { handle, available }`; the `available` field is populated by querying the storage for known handles
- AC6.6: two concurrent writes to same block via tokio::spawn; assert loro CRDT merge produced both changes without data loss

**Commit:**

```bash
jj describe -m "[pattern-runtime] MemoryStoreAdapter wrapping preserved storage (AC6.*)"
jj new
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Change-log infrastructure for pseudo-messages

**Verifies:** contributes to AC8.3, AC8.6.

**Files:**
- Create: `crates/pattern_runtime/src/memory/change_log.rs`

**Implementation:**

```rust
//! Per-session change log: records block updates for pseudo-message emission
//! in the next turn's segment 2.

use pattern_core::types::{BlockHandle, Caller, TurnId};

pub struct ChangeLog {
    events: Vec<ChangeEvent>,
    current_turn: TurnId,
}

#[derive(Debug, Clone)]
pub enum ChangeEvent {
    /// Block created this turn.
    Written {
        handle: BlockHandle,
        author: Caller,
        turn_id: TurnId,
        timestamp: jiff::Timestamp,
        content_preview: String, // first N chars for the pseudo-message body
    },
    /// Existing block modified this turn.
    Updated {
        handle: BlockHandle,
        author: Caller,
        previous_content_hash: u64, // for diff computation
        new_content: BlockContent,
        turn_id: TurnId,
        timestamp: jiff::Timestamp,
    },
}

impl ChangeLog {
    pub fn record(&mut self, event: ChangeEvent) {
        self.events.push(event);
    }

    /// Events emitted since `since` (exclusive). Used by the composer to
    /// build segment-2 pseudo-messages.
    pub fn since(&self, since: TurnId) -> Vec<ChangeEvent> {
        self.events.iter()
            .filter(|e| e.turn_id() > since)
            .cloned()
            .collect()
    }

    /// Called at turn boundary: advance current_turn, clear consumed events
    /// to bound memory.
    pub fn advance_turn(&mut self, new_turn: TurnId) {
        self.current_turn = new_turn;
        // Keep events from last N turns for history; prune older.
        // Design: keep last 10 turns, discard older. Tunable.
        self.events.retain(|e| e.turn_id().turns_before(new_turn) < 10);
    }
}
```

**Step 1:** Implement change log.

**Step 2:** Unit tests:
- Record event, query since earlier turn, event appears
- Record event, query since same/later turn, event is absent
- Advance turn multiple times, events beyond retention window are pruned
- AC8.6: record Written event with `Caller::Agent(other_persona_id)` — pseudo-message renders with correct author attribution

**Commit:**

```bash
jj describe -m "[pattern-runtime] change_log for block-edit pseudo-message emission (AC8.3, AC8.6)"
jj new
```
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-9) -->
<!-- START_TASK_6 -->
### Task 6: Pseudo-message renderer

**Verifies:** AC8.3, AC8.6.

**Files:**
- Create: `crates/pattern_provider/src/compose/pseudo_messages.rs`

**Implementation:**

```rust
//! Renders ChangeEvent into segment-2 pseudo-messages using the
//! `<system-reminder>` tag convention Anthropic's models are trained on.

use pattern_runtime::memory::change_log::ChangeEvent;

pub fn render_change_event(event: &ChangeEvent) -> MessageBlock {
    let inner = match event {
        ChangeEvent::Written { handle, author, timestamp, content_preview, .. } => {
            format!(
                "[memory:written] block '{}' was created by {} at {}\n\ncontent preview:\n{}",
                handle.label(),
                render_author(author),
                render_local_timestamp(*timestamp),
                content_preview,
            )
        }
        ChangeEvent::Updated { handle, author, previous_content_hash, new_content, timestamp, .. } => {
            let diff = compute_diff(previous_content_hash, new_content);
            format!(
                "[memory:updated] block '{}' was modified by {} at {}\n\ndiff:\n{}",
                handle.label(),
                render_author(author),
                render_local_timestamp(*timestamp),
                diff,
            )
        }
    };
    let wrapped = format!("<system-reminder>\n{inner}\n</system-reminder>");
    MessageBlock::user_text(wrapped)
}

/// Render a UTC timestamp in the user's local timezone, in a friendly but useful format.
/// Timestamps are stored as UTC instants for portability/correctness; rendered
/// in local time for display so the agent and user see familiar-looking times.
///
/// Example: UTC `2026-04-16T19:30:00Z` rendered as `2026-04-16, 12:30:00 PDT (Thursday)` - weekday at end to remain sortable)
/// when pattern is running in a PDT locale.
fn render_local_timestamp(ts: jiff::Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    // Format via jiff::fmt::strtime — see https://docs.rs/jiff/latest/jiff/fmt/strtime/index.html for specifier reference.
    // Example format: "%Y-%m-%d, %H:%M:%S %Z (%A)"
    jiff::fmt::strtime::format("%Y-%m-%d, %H:%M:%S %Z (%A)", &zoned)
        .unwrap_or_else(|_| zoned.to_string())
}

fn render_author(caller: &Caller) -> String {
    match caller {
        Caller::Agent(id) => format!("agent {}", id),
        Caller::Human(uid) => format!("user {}", uid),
        // Non-exhaustive — future variants: Plugin(PluginId), Scheduler, IPC, etc.
        _ => "<unknown source>".into(),
    }
}

fn compute_diff(previous_hash: u64, new_content: &BlockContent) -> String {
    // Phase 5 cheap diff: show new content entirely with a marker.
    // Future: a proper diff algorithm against persisted previous content.
    format!("(content replaced; previous hash {:x})\n\n{}", previous_hash, new_content.render_preview())
}
```

**Step 1:** Implement renderer.

**Step 2:** Tests:
- Render Written event → output contains `[memory:written]`, handle label, author, timestamp, `<system-reminder>` tags
- Render Updated event → output contains `[memory:updated]`, diff-like body
- AC8.6: author attribution uses the correct Caller variant; unknown variants render as `<unknown source>` with a tracing::warn
- Local-time rendering: construct a known UTC timestamp, assert the rendered string matches the expected local-offset format for the test environment's timezone (or gate with a `TZ=America/Los_Angeles` env override in the test to get deterministic output)

**Timestamp convention** (applies throughout Phase 5 + future pattern work): timestamps are stored as UTC instants (`jiff::Timestamp`) for portability, serialization, and cross-timezone correctness. They are rendered in the **user's local timezone** (`jiff::tz::TimeZone::system()`) whenever displayed to the user or included in LLM-facing text. Helper `render_local_timestamp()` is the canonical conversion point.

**Commit:**

```bash
jj describe -m "[pattern-provider] pseudo-message renderer for [memory:written] and [memory:updated] (AC8.3, AC8.6)"
jj new
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `[memory:current_state]` segment-3 pseudo-turn renderer

**Verifies:** AC7.3, AC7.6.

**Files:**
- Create: `crates/pattern_provider/src/compose/current_state.rs`

**Implementation:**

```rust
//! Renders current memory block state as a segment-3 pseudo-turn.
//! Produces a user-role message with <system-reminder>-wrapped content
//! listing loaded blocks.

use pattern_core::types::{Block, BlockHandle};

pub fn render_current_state(blocks: &[Block]) -> MessageBlock {
    let body = if blocks.is_empty() {
        // AC7.6: empty state renders as PRESENT BUT EMPTY, not omitted.
        "[memory:current_state]\n(no blocks loaded)".into()
    } else {
        let mut s = "[memory:current_state]\n".to_string();
        for block in blocks {
            s.push_str(&format!("<block id=\"{}\" type=\"{}\">\n", block.handle.label(), block.block_type()));
            s.push_str(&block.render_for_context());
            s.push_str("\n</block>\n");
        }
        s
    };
    let wrapped = format!("<system-reminder>\n{body}\n</system-reminder>");
    MessageBlock::user_text(wrapped)
}
```

**Step 1:** Implement renderer. Respect each block type's rendering rules per `pattern_core::memory::schema` (Text blocks may have viewport, Log blocks have display_limit, etc.). Reuse the existing rendering helpers from the pre-v3 `context/builder.rs:226-316` — extract into a helper during Phase 5 relocation.

**Step 2:** Tests:
- Non-empty block set → output has correct structure, each block rendered per its type
- Empty block set → AC7.6: output is `[memory:current_state]\n(no blocks loaded)` wrapped in `<system-reminder>`; NOT omitted
- Viewport respected for Text blocks
- Log blocks respect display_limit
- Composite blocks render children recursively

**Commit:**

```bash
jj describe -m "[pattern-provider] [memory:current_state] segment-3 pseudo-turn renderer (AC7.3, AC7.6)"
jj new
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Composer pass — segment 1 assembly (system + tools)

**Verifies:** AC7.2, AC7.4.

**Files:**
- Create: `crates/pattern_provider/src/compose/passes/segment_1.rs`

**Implementation:**

```rust
pub struct Segment1Pass {
    system_blocks: Vec<SystemBlock>,       // from Phase 4 shaper
    tools: Vec<ToolSchema>,                 // from tool registry
    profile: CacheProfile,                  // for marker placement
}

impl ComposerPass for Segment1Pass {
    fn name(&self) -> &'static str { "segment_1" }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        // Populate system_blocks (Phase 4 shaper built the array with structure;
        // Segment1Pass attaches it to the partial).
        partial.system_blocks.extend_from_slice(&self.system_blocks);
        partial.tools.extend_from_slice(&self.tools);

        // Place the segment-1 cache_control marker on the LAST system block.
        // Claude-code places it on the last block that warrants caching; Phase 5
        // follows suit — last block is the cache boundary.
        let last_system_idx = partial.system_blocks.len().saturating_sub(1);
        let control = self.profile.segment_1_control();
        partial.breakpoints.place(
            BreakpointLocation::SystemBlock(last_system_idx),
            control,
            self.name(),
        )?;

        Ok(())
    }
}
```

**Step 1:** Implement pass.

**Layout invariant (explicit):** In `SubscriptionRoutingShape` mode (Phase 4 default), the shaper emits three system blocks — slot[0] = the literal identifier string subscription routing expects (structural API requirement), slot[1] = identity-override prefix + `DEFAULT_BASE_INSTRUCTIONS` (user can override `system_instructions_override`), slot[2] = persona + long-lived blocks. The segment-1 cache_control marker lands on the LAST system block (slot[2]) so the entire segment 1 (all three slots) is part of the cached prefix. `DEFAULT_BASE_INSTRUCTIONS` therefore sits at index **≤** the marker's `SystemBlock(idx)` placement — satisfying AC7.4's "in segment 1" requirement.

In `HonestPattern` mode (aspirational, Phase 4 flips default if verification passes), the shaper emits one or two system blocks with `DEFAULT_BASE_INSTRUCTIONS` in slot[0]; marker still goes on the last block. Same invariant.

**Step 2:** AC7.4 verification — segment 1 contains `DEFAULT_BASE_INSTRUCTIONS` byte-for-byte, and that content sits at a position the segment-1 cache marker covers. Add a snapshot test:

```rust
#[test]
fn ac7_4_default_base_instructions_in_segment_1_cache_region() {
    let profile = CacheProfile::default_anthropic_subscriber();
    let shaper_config = ShaperConfig::default();
    let persona = test_persona_minimal();

    let partial = compose_with(vec![
        Box::new(Segment1Pass::from(&shaper_config, &profile, &persona, &[])),
    ], empty_partial());

    // Find the marker placement for segment 1.
    let marker_placement = partial.breakpoints.placements().iter()
        .find(|p| p.placed_by_pass == "segment_1")
        .expect("segment 1 pass must place a marker");
    let marker_system_idx = match marker_placement.location {
        BreakpointLocation::SystemBlock(idx) => idx,
        other => panic!("segment 1 marker should be on a SystemBlock, got {:?}", other),
    };

    // Find DEFAULT_BASE_INSTRUCTIONS in the system blocks.
    let base = pattern_core::base_instructions::DEFAULT_BASE_INSTRUCTIONS;
    let (found_idx, _block) = partial.system_blocks.iter().enumerate()
        .find(|(_, b)| b.text.contains(base))
        .expect("AC7.4: DEFAULT_BASE_INSTRUCTIONS must appear in a system block");

    // AC7.4 strengthened: the instructions must appear at an index covered by
    // the segment-1 cache marker (i.e., at or before the marker's position).
    assert!(
        found_idx <= marker_system_idx,
        "AC7.4: DEFAULT_BASE_INSTRUCTIONS at system_blocks[{}] must be <= segment-1 marker at system_blocks[{}] to be in the cached region",
        found_idx, marker_system_idx,
    );

    // Byte-for-byte match (no paraphrasing / reformatting).
    let block_containing_base = &partial.system_blocks[found_idx].text;
    assert!(
        block_containing_base.contains(base),
        "AC7.4: exact substring match required",
    );
}
```

**Step 3:** AC7.2 — segment 1 contains NO block content. Test composes segment 1 with a CacheProfile and a non-empty block set, verifies no `[memory:…]` strings and no block labels appear in any `system_blocks[..marker_idx+1]` (the cached region). Block content should only appear in segment 3 via the `[memory:current_state]` pseudo-turn.

**Commit:**

```bash
jj describe -m "[pattern-provider] segment 1 composer pass: system + tools + cache_control (AC7.2, AC7.4)"
jj new
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Composer passes — segment 2 (history + pseudo-messages) and segment 3 (current state)

**Verifies:** AC7.1, AC7.3, AC7.6, AC8.3.

**Files:**
- Create: `crates/pattern_provider/src/compose/passes/segment_2.rs`
- Create: `crates/pattern_provider/src/compose/passes/segment_3.rs`

**Implementation:**

```rust
// passes/segment_2.rs
pub struct Segment2Pass {
    history: Vec<MessageBlock>,                  // prior-turn messages
    pseudo_messages: Vec<MessageBlock>,          // from ChangeLog via Task 6 renderer
    profile: CacheProfile,
}

impl ComposerPass for Segment2Pass {
    fn name(&self) -> &'static str { "segment_2" }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        partial.messages.extend_from_slice(&self.history);
        partial.messages.extend_from_slice(&self.pseudo_messages);

        // Place segment-2 cache_control marker on the last message.
        let last_msg_idx = partial.messages.len().saturating_sub(1);
        let control = self.profile.segment_2_control();
        partial.breakpoints.place(
            BreakpointLocation::MessageBlock(last_msg_idx),
            control,
            self.name(),
        )?;
        Ok(())
    }
}
```

```rust
// passes/segment_3.rs
pub struct Segment3Pass {
    current_state_message: MessageBlock,  // from Task 7 renderer
    profile: CacheProfile,
}

impl ComposerPass for Segment3Pass {
    fn name(&self) -> &'static str { "segment_3" }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        partial.messages.push(self.current_state_message.clone());

        // Place segment-3 cache_control marker on the pseudo-turn message itself.
        let pseudo_turn_idx = partial.messages.len() - 1;
        let control = self.profile.segment_3_control();
        partial.breakpoints.place(
            BreakpointLocation::MessageBlock(pseudo_turn_idx),
            control,
            self.name(),
        )?;
        Ok(())
    }
}
```

**Step 1:** Implement both passes.

**Tool message flow note.** When the previous turn ended with the LLM emitting tool_use blocks and the agent dispatched tool_results, those appear in the current turn's message history as:
- Assistant message with ordered content (thinking → text → tool_use, preserved via `StreamEnd.captured_content` per Phase 4 Task 18)
- User message with tool_result blocks (one per tool_use invocation, keyed by matching `call_id`)

These are **regular message-history content** from the composer's perspective. Segment 2 pass includes them in `partial.messages` unchanged; no special pseudo-message wrapping (that's only for `[memory:updated]` / `[memory:written]` which are system-visibility events, not actual conversation). The rust-genai helper `ChatRequest::append_tool_use_from_stream_end(end, tool_response)` (Phase 4) is what pattern_provider's MessageHandler uses to construct the pair before the composer sees them, so ordering (assistant content → user tool_result) is guaranteed correct by the time they hit Segment2Pass.

The distinction:
- **Tool-call / tool-result exchange** = real LLM-conversation content, lives in history, no wrapping.
- **Memory-change notification** = system-visibility event injected by pattern, lives in history, wrapped in `<system-reminder>`.

Segment 2 pass treats them identically for cache-control purposes (both are just messages); the pseudo-message renderer (Task 6) only produces the memory variants.

**Step 2:** Composer pipeline (in `compose::pipeline`) registers passes in order: Segment1 → Segment2 → Segment3 → any future passes → Finalize. Fresh user turn appended as the last message AFTER segment 3 marker is placed (so it's the only un-cache_control-marked message).

**Step 3:** Integration test — full compose with non-trivial inputs:
- System blocks from shaper (3 blocks)
- Tools from registry (5 tools)
- History with 4 prior messages (pseudo-message emitted per Task 6 for a prior block update)
- Current block state (2 Text blocks + 1 Log block)
- Fresh user message
- Assert: final `ChatRequest` has exactly 3 cache_control markers (AC7.1), in the right places; segment counts match expectations.

**Commit:**

```bash
jj describe -m "[pattern-provider] segment 2 + 3 composer passes (AC7.1, AC7.3)"
jj new
```
<!-- END_TASK_9 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 10-12) -->
<!-- START_TASK_10 -->
### Task 10: Composer finalization + breakpoint validation

**Verifies:** AC7.1, AC7.5.

**Files:**
- Modify: `crates/pattern_provider/src/compose/pipeline.rs` — expand `finalize()`

**Implementation:**

```rust
fn finalize(partial: PartialRequest) -> Result<ChatRequest, ProviderError> {
    // AC7.5: validate breakpoint count ≤ 4. Phase 5 uses exactly 3; a 5th
    // attempt would have been caught earlier by BreakpointTracker::place, but
    // validate count here too as belt-and-suspenders.
    if partial.breakpoints.count() > 4 {
        return Err(ProviderError::CacheBreakpointBudgetExceeded {
            budget: 4,
            placed_by: partial.breakpoints.placements()
                .iter().map(|p| p.placed_by_pass).collect(),
            attempted_by: "finalize",
        });
    }

    // Apply breakpoints: walk the placement list, attach cache_control to
    // the indicated block. Validate that each target index is in bounds.
    for placement in partial.breakpoints.placements() {
        match placement.location {
            BreakpointLocation::SystemBlock(idx) => {
                partial.system_blocks.get_mut(idx)
                    .ok_or(ProviderError::InvalidBreakpointLocation { location: "system", idx })?
                    .cache_control = Some(placement.control);
            }
            BreakpointLocation::MessageBlock(idx) => {
                partial.messages.get_mut(idx)
                    .ok_or(ProviderError::InvalidBreakpointLocation { location: "message", idx })?
                    .set_cache_control(placement.control);
            }
            BreakpointLocation::ToolSchema(idx) => {
                // Phase 5 doesn't use this location type, but the infrastructure
                // supports it for future passes.
                partial.tools.get_mut(idx)
                    .ok_or(ProviderError::InvalidBreakpointLocation { location: "tool", idx })?
                    .cache_control = Some(placement.control);
            }
        }
    }

    // If any placement requires extended-ttl beta, Phase 4 shaper should
    // already have included the header — verify here and fail if missing.
    let needs_extended = partial.breakpoints.placements()
        .iter()
        .any(|p| matches!(p.control, genai::chat::CacheControl::Ephemeral1h | genai::chat::CacheControl::Ephemeral24h));
    if needs_extended && !partial.has_header("anthropic-beta", "extended-cache-ttl-2025-04-11") {
        return Err(ProviderError::MissingExtendedCacheTtlBeta);
    }

    Ok(ChatRequest { /* ... populated from partial ... */ })
}
```

**Step 1:** Implement finalization.

**Step 2:** AC7.5 test — construct a pipeline with 5 passes each placing a marker, verify `BreakpointTracker::place` rejects the 5th at placement time with `CacheBreakpointBudgetExceeded`. Assert error carries the list of passes that already placed markers for debuggability.

**Step 3:** Missing-beta-header test — construct a request with Ephemeral1h marker but no `extended-cache-ttl-2025-04-11` header; verify `MissingExtendedCacheTtlBeta` error.

**Step 4:** In-bounds test — construct a pass that places a marker at an out-of-bounds index; verify `InvalidBreakpointLocation` error.

**Commit:**

```bash
jj describe -m "[pattern-provider] composer finalize + breakpoint validation + extended-TTL header check (AC7.1, AC7.5)"
jj new
```
<!-- END_TASK_10 -->

<!-- START_TASK_11 -->
### Task 11: Break-detection hashing

**Verifies:** contributes to AC8.5 — when cache_read drops unexpectedly, diagnostic data attributes the bust.

**Files:**
- Create: `crates/pattern_provider/src/compose/break_detection.rs`

**Implementation:**

```rust
//! Simple hashing of cache-bust-sensitive components per request.
//! Compare to previous request's hashes; when a cache miss is observed,
//! the diff identifies which component caused the bust.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct BreakDetectionSnapshot {
    /// Hash of system blocks with cache_control STRIPPED (catches content
    /// changes without cache-marker churn).
    pub system_hash: u64,

    /// Hash of system blocks WITH cache_control intact (catches TTL/scope flips).
    pub cache_control_hash: u64,

    /// Hash of tools schema.
    pub tools_hash: u64,

    /// Hash of beta header set (sorted, joined).
    pub betas_hash: u64,

    /// Model ID.
    pub model: String,
}

impl BreakDetectionSnapshot {
    pub fn compute(partial: &PartialRequest) -> Self { /* ... */ }

    /// Produce a human-readable diff between self and previous.
    pub fn diff(&self, previous: &BreakDetectionSnapshot) -> Vec<String> {
        let mut changes = vec![];
        if self.system_hash != previous.system_hash { changes.push("system content changed".into()); }
        if self.cache_control_hash != previous.cache_control_hash { changes.push("cache_control changed (scope or TTL)".into()); }
        if self.tools_hash != previous.tools_hash { changes.push("tools schema changed".into()); }
        if self.betas_hash != previous.betas_hash { changes.push("beta headers changed".into()); }
        if self.model != previous.model { changes.push(format!("model changed: {} → {}", previous.model, self.model)); }
        changes
    }
}
```

**Step 1:** Implement snapshot + diff.

**Step 2:** Wire into `AnthropicProviderClient`: hold `last_snapshot: Mutex<Option<BreakDetectionSnapshot>>` on the client. On each turn, compute a snapshot pre-send. After response arrives, if `cache_read_input_tokens` is unexpectedly low (heuristic: < 10% of segment 1 size), log the diff at `tracing::warn` level for diagnostic visibility.

**Step 3:** Unit tests verifying:
- No-change snapshots produce empty diff
- Single-component changes produce single-entry diff
- TTL flip (cache_control_hash diff) is distinguishable from content change (system_hash diff)

**Commit:**

```bash
jj describe -m "[pattern-provider] break-detection hashing for cache-bust diagnosis (AC8.5 support)"
jj new
```
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: Cache-hit metrics capture from response usage

**Verifies:** AC8.1, AC8.2, AC8.5.

**Files:**
- Modify: `crates/pattern_provider/src/usage.rs` (Phase 4 created)
- Create: `crates/pattern_provider/src/compose/cache_metrics.rs`

**Implementation:**

Anthropic's response `usage` field provides:
- `cache_creation_input_tokens` — tokens committed to new cache entries
- `cache_read_input_tokens` — tokens read from cache
- `input_tokens` — fresh tokens not cached/read

Phase 5 exposes these as metrics per turn, tagged by session + turn_id:

```rust
pub struct TurnCacheMetrics {
    pub session_id: String,
    pub turn_id: TurnId,
    pub segment_1_estimated_tokens: u64,
    pub segment_2_estimated_tokens: u64,
    pub segment_3_estimated_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub fresh_input_tokens: u64,
    pub hit_ratio: f64, // cache_read / (cache_read + fresh)
}
```

**Step 1:** After each completion, capture metrics and emit via tracing span:

```rust
tracing::info!(
    session = %session_id,
    turn = %turn_id,
    seg1_est = segment_1_estimated,
    seg2_est = segment_2_estimated,
    seg3_est = segment_3_estimated,
    cache_create = cache_creation_input_tokens,
    cache_read = cache_read_input_tokens,
    fresh = fresh_input_tokens,
    hit_ratio = %hit_ratio,
    "turn cache metrics"
);
```

**Step 2:** AC8.5 — if segment 1 invalidation is detected (cache_read < segment_1_estimated when we expected a hit on segment 1), fire `tracing::warn` with break-detection diff attached. Don't silently accept the cache miss; surface it for observability.

**Step 3:** Tests:
- Mock response with known usage fields, verify metrics are captured and emitted
- Simulate segment 1 cache miss (cache_read_input_tokens == 0 but segment_1_estimated > 0); verify warning fires

**Commit:**

```bash
jj describe -m "[pattern-provider] cache-hit metrics from response usage + segment-1 bust detection (AC8.1, AC8.2, AC8.5)"
jj new
```
<!-- END_TASK_12 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_SUBCOMPONENT_E (tasks 13-14) -->
<!-- START_TASK_13 -->
### Task 13: Compression strategies — migrate call-sites to async `count_tokens`

**Verifies:** AC5b.3 (from Phase 4; call-site migration lives here), AC8.4.

**Files:**
- Modify: `crates/pattern_core/src/context/compression.rs` (move into active location per Phase 2 staging; Phase 5 brings it back from `rewrite-staging/context/compression.rs` and reshapes the token-counting call path)

**Implementation:**

Per Phase 2 staging, compression.rs is in `rewrite-staging/context/compression.rs`. Phase 5 pulls it back into an active location (probably `pattern_provider/src/compose/compression.rs` given the composer-side role, or `pattern_runtime/src/compression.rs` if it serves the runtime side — decide based on where call-sites actually live after Phase 3+4 work; likely the composer side).

The four strategies (Truncate, RecursiveSummarization, ImportanceBased, TimeDecay) preserved structurally. Token-counting changes:

```rust
// Before (heuristic):
fn should_compress(batch: &[Message], max_tokens: usize) -> bool {
    let estimated = batch.iter().map(|m| m.word_count * 4 / 3).sum::<usize>();
    estimated > max_tokens
}

// After (async, provider-reported):
async fn should_compress(
    batch: &[Message],
    max_tokens: usize,
    token_counter: &TokenCounter,
    auth: &ResolvedCredential,
    shaper: &RequestShaper,
) -> Result<bool, ProviderError> {
    let count_req = CountTokensRequest::from_messages(batch, shaper)?;
    let count = token_counter.count(auth, shaper, &count_req).await?;
    Ok(count.input > max_tokens as u64)
}
```

**Step 1:** Pull compression.rs back from staging.

**Step 2:** Migrate all token-counting call sites. Strategies that need to compare message sizes (e.g., ImportanceBased scoring) can use the heuristic internally as a ranking heuristic (not a gate), but the decision-to-compress threshold uses `count_tokens`.

**Step 3:** AC8.4 — compression strategies preserve `MessageBatch` integrity (never archive incomplete batches per `MessageBatch::is_complete`). Verify this invariant still holds when pseudo-messages are present in the stream: a batch containing a `[memory:updated]` pseudo-message is either fully included or fully excluded from archival. Add a test.

**Step 4:** Pseudo-message ordering — when compaction occurs mid-history, the `[memory:updated]` pseudo-messages that were emitted at specific turn boundaries must maintain their ordering relative to the real messages. Test this explicitly.

**Commit:**

```bash
jj describe -m "[pattern-provider] compression: migrate call sites to async count_tokens; preserve batch integrity with pseudo-messages (AC5b.3, AC8.4)"
jj new
```
<!-- END_TASK_13 -->

<!-- START_TASK_14 -->
### Task 14: Remove block-rendering code from pre-v3 system-prompt path

**Verifies:** AC7.2 (segment 1 contains no block content).

**Files:**
- Remove: the portion of `context/builder.rs:226-316` that renders blocks into the system prompt, if any of that code surfaces in the post-Phase-2 pattern_core. (Phase 2 staged context/ out, so this is mostly a no-op in pattern_core; the reshape happens in pattern_provider's composer.)
- Verify: no code path in pattern_core or pattern_provider produces system-prompt blocks containing `[memory:…]` content, block viewport renders, or anything from `pattern_core::memory::*`.

**Step 1:** Audit.

```bash
rg '\[memory:' crates/pattern_core/src/ crates/pattern_provider/src/ crates/pattern_runtime/src/ | grep -v test | grep -v compose
```

Expected: matches only in `pattern_provider/src/compose/` (the composer, intentional). If any other location renders block content into system-prompt-adjacent places, migrate or delete.

**Step 2:** Regression test — compose a request with non-empty memory blocks, assert NO system_block text contains any block label or viewport content.

**Commit:**

```bash
jj describe -m "[pattern-provider] ensure block content never appears in segment 1 (AC7.2)"
jj new
```
<!-- END_TASK_14 -->
<!-- END_SUBCOMPONENT_E -->

<!-- START_SUBCOMPONENT_F (tasks 15-18) -->
<!-- START_TASK_15 -->
### Task 15: End-to-end integration test — memory-edit cache preservation

**Verifies:** AC8.1, AC8.2, AC8.3 together.

**Files:**
- Create: `crates/pattern_provider/tests/memory_edit_cache_preservation.rs`

**Implementation:**

Full pipeline test using wiremock only. Live Anthropic verification of the same flow is handled by the AC9.1 CLI checklist Step 5-6 (human operator, not env-gated test file):

1. Open a session with a non-trivial memory block set (3+ blocks, ~1KB of content each).
2. Run turn 1 with a user message. Capture `cache_creation_input_tokens` per segment (first-turn all segments are fresh).
3. Run turn 2 without modifying memory. Assert `cache_read_input_tokens` covers segment 1 + segment 2 + segment 3 (high hit ratio).
4. **Edit one memory block between turns.**
5. Run turn 3 with a user message. Assert:
   - Segment 1 `cache_read_input_tokens` is still high (AC8.1) — system blocks unchanged
   - Segment 3 `cache_read_input_tokens` dropped (AC8.2) — memory pseudo-turn invalidated
   - `[memory:updated]` pseudo-message appears in the segment 2 prepended to turn 3's history (AC8.3)

With wiremock, mock the `/v1/messages` endpoint to return deterministic usage numbers reflecting the expected cache behavior; with live endpoint, assertions use relative comparisons (segment-1-hit-rate-high, segment-3-drops-meaningfully).

**Commit:**

```bash
jj describe -m "[pattern-provider] e2e test: memory edit preserves segment 1 cache, invalidates segment 3, emits pseudo-message (AC8.1, AC8.2, AC8.3)"
jj new
```
<!-- END_TASK_15 -->

<!-- START_TASK_16 -->
### Task 16: Zero-loaded-blocks edge case (AC7.6)

**Verifies:** AC7.6 explicitly.

**Files:**
- Extend: `crates/pattern_provider/tests/memory_edit_cache_preservation.rs` or separate test file

**Implementation:**

- Construct a session with NO loaded blocks
- Compose a request; assert segment 3 contains exactly one pseudo-turn message with `[memory:current_state]\n(no blocks loaded)` content wrapped in `<system-reminder>`
- Assert segment 3's cache_control marker is still placed (cache-boundary consistency)
- Edit: load a block, compose next turn; segment 3 now contains the block and the cache marker

**Commit:**

```bash
jj describe -m "[pattern-provider] zero-loaded-blocks edge case: segment 3 present but empty (AC7.6)"
jj new
```
<!-- END_TASK_16 -->

<!-- START_TASK_17 -->
### Task 17: Zero-warning close + audit + docs

**Verifies:** cleanliness gates.

**Step 1:** Compile / clippy / doc.

```bash
cargo check -p pattern_provider -p pattern_runtime -p pattern_core 2>&1 | tee /tmp/phase5-check.log
cargo clippy --all-features --all-targets -- -D warnings 2>&1 | tee /tmp/phase5-clippy.log
cargo doc -p pattern_provider -p pattern_runtime -p pattern_core --no-deps 2>&1 | tee /tmp/phase5-doc.log
```

All zero-warning.

**Step 2:** Update `crates/pattern_provider/CLAUDE.md`:
- Composer pipeline architecture + extension points (where to add passes for cache_reference, cache_edits, etc.)
- `CacheProfile` semantics + latching-at-session-open rule
- Break-detection hashing + how to diagnose a cache bust
- Segment-3 `[memory:current_state]` rendering + empty-case semantics

**Step 3:** Update `crates/pattern_runtime/CLAUDE.md`:
- MemoryStoreAdapter architecture — delegation to pattern_core::memory storage
- ChangeLog lifecycle (record on write, advance_turn at boundary, prune to retention window)
- Pseudo-message emission via renderer

**Step 4:** Audit script.

```bash
bash scripts/audit-rewrite-state.sh
```

**Step 5:** Full test suite.

```bash
cargo nextest run --workspace 2>&1 | tail
cargo test --doc --workspace
```

**Commit:**

```bash
jj describe -m "[pattern-provider] phase 5 close: zero warnings; audit clean; docs updated

AC6.* memory storage adapter preserving existing behavior: PASS
AC7.* three-segment cache layout: PASS
AC8.* cache preservation across block edits: PASS"
jj new
```
<!-- END_TASK_17 -->

<!-- START_TASK_18 -->
### Task 18: Live cache verification — deferred to AC9.1 CLI flow

**Verifies:** AC8.1, AC8.2, AC8.5 against a real Anthropic subscription tier, via the AC9.1 CLI checklist Step 5-6 rather than an env-gated test file.

**Rationale:** Same pattern as Phase 4 Task 11 — no env-gated `PATTERN_V3_LIVE_AUTH=1` live-credential test files. The AC9.1 manual checklist exercises the full three-turn scenario (pre-edit metrics → block edit → post-edit metrics) against the real Anthropic endpoint, with the human operator confirming cache preservation / invalidation bounds.

**Files:** None added.

**Coverage mapping:**
- AC8.1 (segment 1 preserved across block edits): AC9.1 Step 6 records pre-edit `seg1`, compares to post-edit
- AC8.2 (segment 3 invalidates): same step, compares `seg3`
- AC8.5 (break-detection fires on unexpected segment 1 bust): operator observes `tracing::warn!` output during the post-edit turn; if seg1 drops unexpectedly and the break-detection diff doesn't surface a plausible cause, AC8.5 fails

See `test-requirements.md` AC8.* section for the full mapping.

**No commit** — intentional absence.
<!-- END_TASK_18 -->

<!-- START_TASK_19 -->
### Task 19: Scope-aware search + recall + block-read functionality (v3 port)

**Verifies:** functional parity with v2's scoped `SearchTool` / `ConstellationSearchTool` / `RecallTool` / `BlockTool` read-ops, rebuilt in the v3 effect-handler architecture.

**Rationale:** v2 agents could (a) search their own conversation history + archival memory, (b) search across constellation peers under permission, (c) search with FTS + time/role filters, (d) retrieve blocks shared to them by other agents. v3's foundation must preserve all of that agent UX — the backend schema (message FTS indexing, `archive_summaries`, `shared_blocks`, `agent_groups` / `group_members`) all already support it; v3 just needs to expose the operations through its Haskell-effect / Rust-handler model. The `AiTool`-trait / `ToolContext` Rust-side framework from v2 is **deliberately not ported** — v3's SDK structure is the Haskell effect GADT + handler dispatch path, not `dyn AiTool`.

**What's in-scope:**
- Extend the Haskell SDK with scope-aware search + recall operations (new modules, or extensions to existing `Memory`).
- Extend the Rust handler side to resolve those operations against pattern_db FTS + archive_summaries + shared_blocks + group-membership tables.
- Permission model: decide + implement the scope → allowed-agents resolution logic (same semantics v2 had — self always allowed, cross-agent requires group membership or explicit share, constellation-wide requires broad permission).
- Block-read scope: expose shared-to-me blocks through the existing `MemoryStore::list_shared_blocks` + `get_shared_block` methods via the Memory effect's load/info ops.

**What's explicitly NOT in-scope:**
- Porting the `AiTool` trait, `ToolContext`, `ToolRegistry` Rust infrastructure — v3 uses effects + handlers, not `dyn AiTool` dispatch.
- Porting the v2 `ImportanceScoringConfig` / keyword-bonus machinery from the old search tools — if importance scoring is wanted, it surfaces later as its own concern.
- New schema migrations — existing pattern_db schema supports everything below.
- Cross-constellation-constellation search (if that's ever a thing) — only intra-constellation scope is wired.

**Files:**
- Create / extend: `crates/pattern_runtime/haskell/Pattern/Search.hs` — new SDK module exposing scoped search over message history + archival entries. GADT shape up to the implementer; v2's `SearchDomain` (ArchivalMemory / Conversations / ConstellationMessages / All) is a good starting point.
- Create: `crates/pattern_runtime/haskell/Pattern/Recall.hs` — new SDK module for archival-entry insert/search/get/delete. Search op takes `Maybe Scope` (optional per user: recall is usually per-agent; scope is occasional).
- Extend: `crates/pattern_runtime/haskell/Pattern/Memory.hs` — Block load/info ops optionally take an `Owner` parameter so agents can load blocks shared to them by peers. Existing `Memory.Search` currently returns a handler-unimplemented error; decide whether to absorb it into the new `Pattern/Search.hs` (lean: yes, deprecate `Memory.Search`) or keep it as memory-only search within the current agent.
- Extend: `crates/pattern_runtime/src/sdk/handlers/memory.rs` — wire block-read ops to consult `shared_blocks` when an owner is specified; reject cross-agent access when no sharing record exists.
- Create: `crates/pattern_runtime/src/sdk/handlers/search.rs` — new handler implementing the Search effect. Dispatches to `pattern_db::queries` FTS helpers, filtering by resolved agent set based on scope + permission.
- Create: `crates/pattern_runtime/src/sdk/handlers/recall.rs` — new handler for Recall effect. Thin layer over `MemoryStore::{insert_archival, search_archival, delete_archival}`; scope resolution mirrors the Search handler.
- Extend: `crates/pattern_core/src/traits/memory_store.rs` — if any new DB-level method is needed (e.g. cross-agent message search by query), add to the trait with a default impl that delegates to existing helpers. `search_archival` already takes `agent_id` so cross-agent works by parameter swap.
- Extend: `crates/pattern_runtime/src/sdk/mod.rs` (or wherever the `SdkBundle` HList lives) — register the new handlers in the canonical handler order (after `Memory`, before `Spawn` — call it out explicitly in the bundle docs since handler position drives the JIT effect tag).
- Extend: `crates/pattern_core/src/types/` or `crates/pattern_runtime/src/types/` — a `SearchScope` type (port from `rewrite-staging/agent_runtime/runtime/tool_context.rs:29`) mirroring v2's enum (CurrentAgent / Agent(AgentId) / Agents(Vec<AgentId>) / Constellation). Lives in pattern_core if any cross-crate consumer needs it; pattern_runtime otherwise.
- Permission-resolution helper: `crates/pattern_runtime/src/sdk/handlers/scope.rs` (or equivalent) — takes a `SearchScope` + caller `AgentId` + `MemoryStore` trait handle, returns `Vec<AgentId>` (the resolved set) or a permission-denied error. Implements the scope → allowed-agents logic:
  - `CurrentAgent` → `[caller]`
  - `Agent(target)` → `[target]` iff (a) `target == caller`, OR (b) `target` has shared ≥1 block with `caller` (tolerable heuristic for "these agents cooperate"), OR (c) both are in the same `agent_group`, OR (d) the agent's trust level or group has a `cross_agent_search` flag set
  - `Agents(ids)` → per-id same check; filters out unpermitted without erroring
  - `Constellation` → all constellation agents if caller has constellation-wide-search permission (v2's "Archive agent" role), else error
  Final policy decision on ordering + which signals count (shared-blocks vs group-membership vs explicit flag) made during implementation; the shape matches v2, the exact policy is updatable.

**Implementation notes:**

1. **Haskell module style.** Match the existing `Pattern.Memory` / `Pattern.Message` conventions: GADT with constructor prefixes that avoid `Prelude` collisions (e.g. `SearchMessages` / `SearchArchival` / `SearchAll`). Register each SDK request variant with `#[core(module = "Pattern.Search", name = "Messages")]` on the Rust decode side for arity-aware disambiguation.

2. **Effect row ordering.** Handler position in `SdkBundle` HList determines JIT effect tag. Current order: `Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn`. Add `Search` + `Recall` at a deliberate position — suggested: `Memory, Search, Recall, Message, …` (adjacent to Memory since they're all storage-adjacent) — and update `pattern_runtime/CLAUDE.md`'s canonical-ordering section accordingly.

3. **Permission model is configurable, not hardcoded.** Permission signals + their ordering are decided at implementation time; future phases may tune them. The scope resolver function should be testable in isolation (accepts a `MemoryStore` trait object + `AgentId` + scope, returns allowed `Vec<AgentId>`).

4. **Search result shapes.** v2 returned `serde_json::Value` blobs from the tool surface. v3's Haskell SDK should return typed results (e.g. `[MessageHit { id :: Text, agentId :: Text, preview :: Text, position :: Text, createdAt :: Text, role :: Text }]`). Handler-side does the shaping against pattern_db's FTS query results.

5. **Porting v2's `SearchDomain` + `ConstellationSearchDomain` precedent:** The union of those (archival / conversations / constellation-messages / constellation-archival / all) is the set of primary operations. Decide whether to collapse into one effect variant (Search takes `domain + scope`) or split (SearchArchival / SearchConversations / SearchAll as separate constructors). Either is defensible; match the style of existing effect modules.

6. **Recall scope is optional.** Per user input: recall tool is "optionally scoped" — the search op takes `Maybe Scope`, defaulting to `CurrentAgent` semantics when absent.

7. **Block read ops on shared blocks.** Existing `Memory.Get` / `Memory.Info` / `Memory.Viewport` ops implicitly target the caller's own blocks. Add an `owner :: Maybe AgentId` parameter (or dedicated ops `GetShared` / `InfoShared` / etc.) so agents can access shared blocks without needing to know the owner's internal representation. Permission check lives in the handler: if `owner != caller`, verify `shared_block_agents` grants the caller read access.

**Testing:**

- Unit tests for the scope resolver (pure function; in-memory `MemoryStore` test fixture with configured sharing / group memberships).
- Handler tests for Search over message FTS: write N messages across 2 agents, search with each scope variant, assert correct result set.
- Handler tests for Recall: insert archival entries for 2 agents, search with optional scope, assert results.
- Integration test that exercises the full effect path: Haskell agent program → JIT → Rust handler → pattern_db query → results back to agent → assertion in Rust.
- Permission-denied paths: caller tries to search an agent they have no relationship with, handler returns `EffectError::Permission` (or appropriate variant).

**Docs:**
- Update `crates/pattern_runtime/CLAUDE.md`:
  - Canonical handler order (new positions for Search + Recall).
  - New SDK modules + their intent.
  - Permission model summary + where the policy lives.
- Update `crates/pattern_provider/CLAUDE.md` only if any composer-side consumer changes (unlikely for Task 19).
- Update `docs/architecture/message-batching-design.md` or similar to note that scoped search is the agent-facing way to access historical archives.

**Commit:**

```bash
jj describe -m "[pattern-runtime] Task 19: scope-aware Search + Recall SDK modules + handlers

Ports v2's scoped SearchTool / ConstellationSearchTool / RecallTool
functionality to v3's Haskell-effect + Rust-handler architecture.
Adds Pattern.Search and Pattern.Recall Haskell modules with scope-aware
effect constructors; new handlers in pattern_runtime that dispatch
against pattern_db FTS + archive_summaries + shared_blocks +
group-membership tables. Permission model preserves v2 semantics:
self-always, cross-agent via shared-blocks or group membership,
constellation requires broad permission.

AiTool Rust trait + ToolContext framework deliberately NOT ported —
v3 uses effects + handlers exclusively.

Schema migrations: none (existing pattern_db schema supports all paths)."
jj new
```
<!-- END_TASK_19 -->

<!-- START_TASK_20 -->
### Task 20: Agent loop + `code` tool + router registry

**Verifies:** the keystone human↔agent integration. Wires `Session::step` to a Rust-driven orchestrator that calls pattern_provider's `PatternGatewayClient`, evaluates LLM-emitted Haskell snippets via `tidepool_runtime::compile_and_run`, routes agent output through a scheme-dispatched router registry, and **drives the multi-wire-turn tool-use loop honestly** — each wire turn is one complete provider request/response (matching Anthropic's protocol: every tool_result batch is a new HTTP request, not a mid-stream injection). One `Session::step` call = one user-visible exchange = N wire turns chained by tool_result feedback.

**Rationale:** Phase 4 built pattern_provider (the gateway) but did NOT wire it into `MessageHandler`. Phase 3's stub returned an `EffectError::Handler`. The Anthropic tool-use model is explicit in the docs: "Send a new request containing the original messages, the assistant's response, and a user message with the `tool_result` blocks. Repeat while `stop_reason` is `tool_use`." This means:

- a "turn" in the **wire sense** is one provider call (one composer pass, one TurnRecord, one checkpoint boundary);
- a "turn" in the **user sense** is the full loop from human input to final assistant text;
- the composer runs fresh on each wire turn — pseudo-messages (`[memory:updated]` etc.) naturally flow as the agent's own prior-turn writes become visible in segment 2 of the next wire turn's composition. No mid-turn re-rendering needed.

v2's `Pattern.Message.Ask` (agent "asks" an LLM and blocks) doesn't fit v3: v3 agents don't call LLMs via effects; LLMs drive agent work via `Session::step`, and within each wire turn they use a **single tool** (`code`) to invoke SDK capabilities. `Ask` stays stubbed as a candidate for removal.

**Depends on:**
- Tasks 4 + 5 (`MemoryStoreAdapter` + `TurnHistory` + DB wiring; adapter pending-buffer + session drains). Pending-writes flow into each wire turn's `TurnOutput.block_writes` via these.
- Tasks 8 + 9 + 10 (composer passes + finalize). Orchestrator calls `compose::compose` to build each wire turn's `CompletionRequest`.
- Task 6 (pseudo-message renderer). Prior wire turn's drained block_writes get rendered as `[memory:updated]` pseudo-messages in the NEXT wire turn's segment 2.

**Blocks:**
- Task 15 (e2e memory-edit cache preservation test) — exercises the full step → wire turns → final answer round-trip.
- Task 16 (zero-blocks edge) — same.

**Files:**

Types-layer additions (pattern_core):
- Add enum: `crates/pattern_core/src/types/turn.rs::StopReason` — `EndTurn` / `ToolUse` / `MaxTokens` / `StopSequence` / `Refusal` / `PauseTurn`. Serde for cross-boundary. Includes `is_terminal()` helper (everything except `ToolUse`).
- Add struct: `crates/pattern_core/src/types/turn.rs::StepReply` — `{ turns: Vec<TurnOutput>, final_stop_reason: StopReason, total_usage: Usage }` with convenience accessors (`all_messages`, `all_block_writes`, `final_text`, `turn_count`).
- Extend struct: `crates/pattern_core/src/types/turn.rs::TurnOutput` — add `tool_calls: Vec<ToolCall>`, `tool_results: Vec<ToolResult>`, `stop_reason: StopReason` fields. Document invariant: `tool_calls.len() == tool_results.len()` and both nonempty iff `stop_reason == ToolUse`.
- Add enum: `crates/pattern_core/src/types/provider.rs::ToolOutcome` — `Success(serde_json::Value)` / `Error(String)`. Maps to Anthropic's `tool_result { is_error: bool, content: ... }` wire shape.
- Add struct: `crates/pattern_core/src/types/provider.rs::ToolResult` — `{ call_id: String, outcome: ToolOutcome }`.
- Add constructor: `crates/pattern_core/src/types/turn.rs::TurnInput::from_tool_results(prior: &TurnOutput, batch_id: BatchId) -> Self` — mints a fresh `TurnId`, preserves `batch_id`, builds ONE `ChatRole::User` message whose content is the sequence of tool_result content blocks (one per `prior.tool_results`). Same batch = same user-visible exchange.
- Add trait + enum: `crates/pattern_core/src/traits/turn_sink.rs::{TurnSink, TurnEvent, DisplayKind}`:
  - `TurnEvent::Text(String)` — LLM text chunk as it streams. No sub-kind; UIs concatenate into a streaming buffer.
  - `TurnEvent::ToolCall(ToolCall)` — LLM requested a tool (pre-dispatch)
  - `TurnEvent::ToolResult(ToolResult)` — eval completed, result in hand
  - `TurnEvent::Display { kind: DisplayKind, text: String }` — from Haskell `Pattern.Display.*`. `kind` is `Chunk` / `Final` / `Note` so UIs can render the three styles distinctly (streaming assembled text vs terminal assembled content vs side-channel status).
  - `TurnEvent::Stop(StopReason)` — wire turn ended (after all evals settled)
  - Default impl: `NoOpSink` (for tests or headless runs); `VecSink` for test assertions.
  - **UX invariant:** `TurnEvent::Text` (LLM-authored stream) and `TurnEvent::Display` (agent-authored deliberate output) MUST render differently in the CLI / TUI. CLI convention: `Text` in default style; `Display::Chunk` / `::Final` prefixed with a glyph or framed block; `Display::Note` dimmed or parenthesised. Documented on the types themselves for implementers.
- Update signature: `crates/pattern_core/src/traits/session.rs::Session::step` — return type changes from `TurnOutput` → `StepReply`. Doc-comment updated to explain "one user-visible exchange (may comprise multiple wire turns driven by tool_use cycles)".
- Add constant: `crates/pattern_core/src/lib.rs::PERSONA_LABEL: &str = "persona"` — reserved memory-block label read by Segment 1.

Runtime-layer additions (pattern_runtime):
- Create: `crates/pattern_runtime/src/agent_loop.rs` — async orchestrator for **one wire turn**. No inner loop. Returns `TurnOutput`.
- Create: `crates/pattern_runtime/src/sdk/code_tool.rs` — `CODE_TOOL` static + genai schema + Haskell-source templating adapted from `tidepool-mcp`.
- Create: `crates/pattern_runtime/src/sdk/preamble.rs` — Haskell preamble assembler; uses each handler's `DescribeEffect` impl.
- Create: `crates/pattern_runtime/src/router/mod.rs` — `Router` trait + `RouterRegistry`.
- Create: `crates/pattern_runtime/src/router/cli.rs` — `CliRouter` implementation.
- Modify: `crates/pattern_runtime/src/sdk/handlers/*.rs` — add `impl DescribeEffect` for each of the 11 handlers.
- Modify: `crates/pattern_runtime/src/sdk/handlers/message.rs` — replace `Send/Reply/Notify` stubs with real router dispatch; `Ask` stays stubbed with a "candidate for removal" comment.
- Modify: `crates/pattern_runtime/src/sdk/handlers/display.rs` — forward text into the session's `TurnSink` as `TurnEvent::Display`.
- Modify: `crates/pattern_runtime/src/session.rs` — `TidepoolSession` gains `router_registry: Arc<RouterRegistry>`, `pending_messages: Arc<Mutex<Vec<Message>>>`, `turn_sink: Arc<dyn TurnSink>`, `agent_helpers_dir: Option<PathBuf>`. `SessionContext` exposes the same fields.
- Modify: `crates/pattern_runtime/src/session.rs::TidepoolSession::step` — becomes **the wire-turn loop driver**. Reads `BatchId` from input (caller-provided), invokes `agent_loop::orchestrate` per wire turn, uses `TurnInput::from_tool_results` to chain tool_use cycles, returns `StepReply` when `stop_reason.is_terminal()`.
- Modify: `crates/pattern_provider/src/compose/passes/segment_1.rs` (from Task 8) — injects `CODE_TOOL.clone()` into `partial.tools`.

**Retire** (no longer applicable): the "run_turn replaces SessionMachine.run" framing. There's no longer a `run_turn`; `Session::step` is the public entry. The static-program eval path (`SessionMachine.run`) stays available via an internal method for test fixtures but is not the production path. Full retirement to Phase 6.

**Implementation details:**

1. **`impl DescribeEffect` for each handler.** Borrow tidepool-mcp's `DescribeEffect` trait + `EffectDecl` struct (or import them from tidepool-mcp if feasible; else copy the pattern). Each handler declares:
   - `type_name`: Haskell GADT name (`"Memory"`, `"Message"`, etc.)
   - `description`: what the effect does
   - `constructors`: e.g. `&["Get :: BlockHandle -> Memory Content", "Put :: BlockHandle -> Content -> Maybe Text -> Memory ()", ...]`
   - `type_defs`: supporting types like `data BlockType = ...`
   - `helpers`: curried helper fns agents use idiomatically (`get`, `put`, etc.)

2. **Preamble assembler.** `pattern_runtime::sdk::preamble::build(decls: &[EffectDecl]) -> String` produces the static Haskell boilerplate shared by every `code` tool eval:
   - Module header + `{-# LANGUAGE ... #-}` pragmas
   - Standard imports (`Control.Monad.Freer`, `Data.Aeson`, etc.)
   - Each handler's `type_defs` emitted in order
   - `data Memory a where ...` / `data Message a where ...` GADT declarations
   - `type M = '[Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn]` — the canonical handler row matching the `SdkBundle` HList ordering documented in `pattern_runtime/CLAUDE.md`
   - Each handler's `helpers` emitted after the type alias

3. **`code` tool schema.** Static const in `sdk::code_tool`:
   ```rust
   pub static CODE_TOOL: LazyLock<Tool> = LazyLock::new(|| Tool {
       name: "code".into(),
       description: r#"Execute a Haskell code snippet against the Pattern SDK.
   The snippet runs with Pattern.Prelude imported and full access to the
   effect stack: Memory, Message, Display, Time, Log, Shell, File,
   Sources, Mcp, Rpc, Spawn. Return the value to surface it to the next
   turn; the value serialises to JSON via Aeson."#.into(),
       schema: json!({
           "type": "object",
           "properties": {
               "code": { "type": "string", "description": "Haskell snippet in do-notation" },
               "imports": { "type": "string", "description": "Extra `import X.Y.Z` lines (optional)" },
               "helpers": { "type": "string", "description": "Extra helper definitions, compiled before the snippet (optional)" }
           },
           "required": ["code"]
       }),
       cache_control: None,  // tool list is stable within a session; cache_control applied at segment boundary by the composer
   });
   ```

4. **Source templating.** `sdk::code_tool::template_source(preamble, code, imports, helpers) -> String` — directly adapts `tidepool_mcp::template_haskell`. Inserts user imports after the standard ones, appends helpers, wraps the user code in:
   ```haskell
   result :: Eff M Value
   result = do
     _r <- do
       <user code lines>
     paginateResult 4096 (toJSON _r)
   ```

5. **Orchestrator structure — one wire turn, no inner loop:**

   ```rust
   // agent_loop.rs
   pub async fn orchestrate(
       input: TurnInput,
       ctx: Arc<SessionContext>,
       eval_tx: &EvalSender,
       preamble: &str,
   ) -> Result<TurnOutput, RuntimeError> {
       // 1. Push input.messages into ctx.pending_messages (for this wire
       //    turn's TurnRecord; composer reads TurnHistory which will
       //    include them after persistence).
       ctx.pending_messages().lock().extend(input.messages.clone());

       // 2. Build CompletionRequest via composer pipeline (segments 1-3).
       let req = build_request(&ctx, &input).await?;

       // 3. Stream the provider response.
       let mut stream = ctx.provider().complete(req).await?;
       let mut text_accum = String::new();
       let mut tool_calls: Vec<ToolCall> = Vec::new();
       let mut pending_evals: FuturesUnordered<
           JoinHandle<(String, ToolOutcome)>
       > = FuturesUnordered::new();
       let mut stop_reason = StopReason::EndTurn; // default until stream tells us
       let mut usage = Usage::default();

       while let Some(event) = stream.next().await {
           match event? {
               ChatTurnEvent::Chunk(c) => {
                   text_accum.push_str(&c.content);
                   ctx.turn_sink().emit(TurnEvent::Text(c.content.clone()));
               }
               ChatTurnEvent::ToolCall(tc) => {
                   ctx.turn_sink().emit(TurnEvent::ToolCall(tc.clone()));
                   tool_calls.push(tc.clone());
                   // 4. Dispatch eval IN PARALLEL with remaining stream work.
                   if tc.fn_name == "code" {
                       pending_evals.push(dispatch_eval(
                           tc.clone(), preamble, eval_tx.clone()
                       ));
                   } else {
                       // Unknown tool — synthesize error outcome.
                       pending_evals.push(pre_error(
                           tc.call_id.clone(),
                           format!("unsupported tool: {}", tc.fn_name),
                       ));
                   }
               }
               ChatTurnEvent::End(end) => {
                   stop_reason = map_stop_reason(&end);
                   if let Some(u) = end.captured_usage { usage = u; }
               }
               _ => {}
           }
       }

       // 5. Stream closed. Drain pending evals in arrival order.
       let mut results_by_id: HashMap<String, ToolOutcome> = HashMap::new();
       while let Some(joined) = pending_evals.next().await {
           let (call_id, outcome) = joined?;
           ctx.turn_sink().emit(TurnEvent::ToolResult(
               ToolResult { call_id: call_id.clone(), outcome: outcome.clone() }
           ));
           results_by_id.insert(call_id, outcome);
       }

       // 6. Build tool_results paired 1:1 with tool_calls (preserved order).
       let tool_results: Vec<ToolResult> = tool_calls.iter().map(|tc| {
           let outcome = results_by_id.remove(&tc.call_id).unwrap_or_else(||
               ToolOutcome::Error("eval did not complete".into())
           );
           ToolResult { call_id: tc.call_id.clone(), outcome }
       }).collect();

       // 7. Emit final Stop event.
       ctx.turn_sink().emit(TurnEvent::Stop(stop_reason));

       // 8. Drain block_writes + messages. Assemble TurnOutput.
       let block_writes = ctx.adapter().drain_pending();
       let messages: Vec<Message> = std::mem::take(
           &mut *ctx.pending_messages().lock()
       );
       if !text_accum.is_empty() || !tool_calls.is_empty() {
           // Push assistant message (text + tool_calls) into the returned
           // messages so TurnRecord captures it.
           messages.push(make_assistant_message(
               text_accum, tool_calls.clone(), ctx.agent_id(), input.batch_id.clone()
           ));
       }

       Ok(TurnOutput {
           messages, block_writes,
           tool_calls, tool_results, stop_reason,
           usage: Some(usage),
           cache_metrics: TurnCacheMetrics::default(), // Task 12 populates
           completed_at: Timestamp::now(),
       })
   }
   ```

6. **`TidepoolSession::step` — the wire-turn loop driver:**

   ```rust
   // session.rs
   #[async_trait]
   impl Session for TidepoolSession {
       async fn step(&mut self, input: TurnInput)
           -> Result<StepReply, RuntimeError>
       {
           let batch_id = input.batch_id.clone();
           let mut turns: Vec<TurnOutput> = Vec::new();
           let mut cur_input = input;

           loop {
               let out = agent_loop::orchestrate(
                   cur_input,
                   self.ctx.clone(),
                   &self.eval_tx,
                   &self.preamble,
               ).await?;

               // Persist this wire turn: TurnRecord → pattern_db,
               // TurnHistory update, checkpoint if due.
               self.persist_wire_turn(&out).await?;

               let terminal = out.stop_reason.is_terminal();
               let needs_next = matches!(out.stop_reason, StopReason::ToolUse)
                   && !out.tool_calls.is_empty();

               turns.push(out);
               if terminal || !needs_next { break; }

               // Build the next wire turn's input from this turn's tool_results.
               // batch_id preserved; fresh turn_id.
               cur_input = TurnInput::from_tool_results(
                   turns.last().unwrap(),
                   batch_id.clone(),
               );
           }

           let total_usage = turns.iter().filter_map(|t| t.usage.as_ref())
               .fold(Usage::default(), |acc, u| acc + *u);
           let final_stop_reason = turns.last()
               .map(|t| t.stop_reason)
               .unwrap_or(StopReason::EndTurn);

           Ok(StepReply { turns, final_stop_reason, total_usage })
       }
   }
   ```

   Key properties:
   - Each wire turn runs the full composer pipeline → cache boundaries respected.
   - Each wire turn produces its own TurnRecord + checkpoint → crash-safe resume.
   - Prior wire turn's `block_writes` naturally flow into NEXT wire turn's segment 2 via TurnHistory + pseudo-message renderer (no special-case wiring).
   - `batch_id` is load-bearing: all wire turns in one step share it (same user-visible exchange); a new `Session::step` call mints a fresh batch externally.

7. **`dispatch_eval` — mid-stream parallel dispatch:**

   ```rust
   fn dispatch_eval(
       tc: ToolCall, preamble: &str, eval_tx: EvalSender,
   ) -> JoinHandle<(String, ToolOutcome)> {
       let call_id = tc.call_id.clone();
       tokio::spawn(async move {
           // Parse code tool input; template the source; send to worker.
           let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
           let params: CodeToolInput = match serde_json::from_value(tc.fn_arguments) {
               Ok(p) => p,
               Err(e) => return (call_id, ToolOutcome::Error(
                   format!("invalid code tool input: {e}")
               )),
           };
           let source = template_source(
               preamble, &params.code,
               params.imports.as_deref(), params.helpers.as_deref(),
           );
           if eval_tx.send(EvalRequest { source, reply: reply_tx }).is_err() {
               return (call_id, ToolOutcome::Error("eval worker closed".into()));
           }
           match reply_rx.await {
               Ok(Ok(v)) => (call_id, ToolOutcome::Success(v)),
               Ok(Err(e)) => (call_id, ToolOutcome::Error(format!("{e}"))),
               Err(_) => (call_id, ToolOutcome::Error("eval dropped reply".into())),
           }
       })
   }
   ```

6. **Router registry:**

   ```rust
   #[async_trait]
   pub trait Router: Send + Sync {
       fn scheme(&self) -> &str;
       async fn route(&self, recipient: &str, body: &Message) -> Result<(), RouterError>;
   }

   pub struct RouterRegistry {
       routers: DashMap<String, Arc<dyn Router>>, // keyed by scheme
   }

   impl RouterRegistry {
       pub fn register(&self, router: Arc<dyn Router>);
       pub async fn route(&self, recipient: &str, body: &Message) -> Result<(), RouterError> {
           let (scheme, _target) = recipient.split_once(':').ok_or(RouterError::MalformedRecipient)?;
           let router = self.routers.get(scheme).ok_or(RouterError::NoRouterForScheme(scheme.into()))?;
           router.route(recipient, body).await
       }
   }
   ```

7. **`CliRouter`:**
   - Holds `Arc<UnboundedSender<Message>>` (the CLI subscribes with the matching `Receiver`)
   - `scheme() == "cli"`
   - `route()` parses `cli:<id>` (for Phase 5 foundation, all `cli:*` go to the one registered sink; mapping to multiple CLI consumers is future), pushes to the sender

8. **Handler integration for `Send/Reply/Notify`:**
   - `MessageHandler::handle(Send(recipient, body))` → reads `RouterRegistry` from `EffectContext`, calls `registry.route(recipient, &msg)`, returns `Value::Unit` on success or maps error → `EffectError::Handler(...)`
   - Same for `Reply` and `Notify` (the differences between them are in the Message's metadata, not the routing)
   - `Ask` stays stubbed: returns a handler error noting "Pattern.Message.Ask is a candidate for removal in a future plan; v3 agents don't call LLMs via effects — LLMs drive agent turns via the `code` tool. Use Memory / Send / Reply for inter-agent communication instead."

9. **Router registry + `CliRouter`:**

   ```rust
   #[async_trait]
   pub trait Router: Send + Sync {
       fn scheme(&self) -> &str;
       async fn route(&self, recipient: &str, body: &Message)
           -> Result<(), RouterError>;
   }

   pub struct RouterRegistry {
       routers: DashMap<String, Arc<dyn Router>>,
   }

   impl RouterRegistry {
       pub fn register(&self, router: Arc<dyn Router>);
       pub async fn route(&self, recipient: &str, body: &Message)
           -> Result<(), RouterError>
       {
           let (scheme, _target) = recipient.split_once(':')
               .ok_or(RouterError::MalformedRecipient)?;
           let router = self.routers.get(scheme)
               .ok_or(RouterError::NoRouterForScheme(scheme.into()))?;
           router.route(recipient, body).await
       }
   }
   ```

   `CliRouter` holds `Arc<UnboundedSender<Message>>`, `scheme() == "cli"`, pushes all `cli:*` recipients to the single registered sink.

10. **Handler integration for `Send/Reply/Notify`:**
    - `MessageHandler::handle(Send(recipient, body))` → reads `RouterRegistry` from `EffectContext`, calls `registry.route(recipient, &msg)`, returns `Value::Unit` or maps error → `EffectError::Handler(...)`
    - Same for `Reply` and `Notify` (differences are in metadata, not routing)
    - `Ask` stays stubbed: "Pattern.Message.Ask is a candidate for removal in a future plan; v3 agents don't call LLMs via effects — LLMs drive agent turns via `Session::step`, and within each wire turn they use the `code` tool. Use Memory / Send / Reply for inter-agent communication instead."

11. **`DisplayHandler` → `TurnSink` wiring:**
    - `DisplayHandler` gains `turn_sink: Arc<dyn TurnSink>` constructor argument.
    - On `Display.Show text`, emit `TurnEvent::Display { text }` to the sink.
    - Orchestrator uses the same sink for `TurnEvent::Text/ToolCall/ToolResult/Stop`.
    - CLI registers a sink that prints to stdout (with ANSI colour coding per event variant in a later CLI task).
    - Tests register a `VecSink` (collects events into a Vec for assertions).

12. **Hybrid pre-baked helpers (plumbing only):**
    - `TidepoolSession::open` accepts optional `agent_helpers_dir: Option<PathBuf>`.
    - Stored on `SessionContext`; eval worker appends it to `include_paths` for each `compile_and_run` call.
    - GHC's `.hi`/`.o` cache makes this essentially free after first use.
    - No CLI-level wiring in Phase 5; exposed for future specialized-agent scenarios.

**Tests:**

Types-layer:
- `StopReason::is_terminal` returns correct values for each variant.
- `StepReply` convenience accessors (`all_messages`, `all_block_writes`, `final_text`, `turn_count`) work across 0-turn, 1-turn, multi-turn cases.
- `TurnInput::from_tool_results` preserves batch_id, mints fresh turn_id, and produces a single user-role message with N content blocks when given N results.
- `TurnOutput` tool_calls/tool_results pairing invariant enforced (ideally via constructor; at minimum documented + test).
- `ToolOutcome` serde round-trips.

Runtime-layer:
- `preamble::build` with a synthetic handler set (2-3 mock `DescribeEffect` impls) produces expected Haskell string.
- `template_source` matches expected wrapper shape (regression against tidepool-mcp's template).
- `RouterRegistry::route` with mock routers: scheme dispatch, missing-scheme error, malformed-recipient error.
- `CliRouter`: register, send a message, assert receiver side got it.
- `dispatch_eval`: a task with good input returns `ToolOutcome::Success`; bad JSON returns `ToolOutcome::Error`; dropped reply returns `ToolOutcome::Error("eval dropped reply")`.
- `VecSink` test fixture: asserts TurnEvent ordering during a simulated orchestrate run (mock provider).
- Stubbed-Ask test: `Ask` effect still returns the "candidate for removal" error.

Integration (wiremock):
- **Single wire turn, no tools:** mock provider returns final text; `Session::step` returns `StepReply` with one TurnOutput, `stop_reason=EndTurn`.
- **One tool round-trip (two wire turns):** mock provider turn 1 returns `code` tool_use; orchestrator evaluates `put "notes" "hello"`; turn 2 returns final text. Assert `StepReply.turns.len() == 2`, `turns[0].stop_reason == ToolUse`, `turns[0].tool_results[0].outcome` is Success, `turns[0].block_writes` contains the write, `turns[1].stop_reason == EndTurn`, `all_block_writes()` aggregates correctly, all turns share `batch_id`, each has distinct `turn_id`.
- **Tool error recovery:** turn 1 returns `code` tool_use with invalid Haskell; orchestrator returns `ToolOutcome::Error`; turn 2 receives the error tool_result and returns an apology; assert loop terminates cleanly.
- **Pseudo-message flow across wire turns:** turn 1 writes a block; turn 2's ChatRequest (captured by the mock provider harness) contains a `[memory:updated]` pseudo-message in segment 2.

**Commit:**

```
[pattern-runtime] Task 20: agent loop + code tool + router

Wires Pattern.Session::step to a Rust-driven wire-turn loop driver that
calls pattern_provider per wire turn, dispatches LLM-emitted `code`
tool_use to a Haskell eval worker, and chains tool_result → next wire
turn the way Anthropic's protocol requires (every tool_result batch is
a new HTTP request).

Model:
- One user-visible exchange = Session::step = N wire turns = one
  StepReply containing Vec<TurnOutput>.
- One wire turn = one provider call = one composer pass = one
  TurnRecord + checkpoint. Block_writes from turn N flow naturally
  into turn N+1's segment 2 via TurnHistory + pseudo-message renderer
  (no special-case wiring).
- BatchId stable across all wire turns within one step.
- Mid-stream parallel eval dispatch: we push ToolCall evals onto a
  FuturesUnordered as the stream emits them; join at wire-turn
  boundary before building TurnOutput.

Types-layer additions (pattern_core):
- StopReason enum (EndTurn / ToolUse / MaxTokens / StopSequence /
  Refusal / PauseTurn) with is_terminal().
- StepReply struct for the user-visible-exchange return.
- TurnOutput gains tool_calls, tool_results, stop_reason fields.
- ToolResult { call_id, outcome: ToolOutcome::{Success, Error} }.
- TurnInput::from_tool_results(&TurnOutput, BatchId) constructor.
- TurnSink trait + TurnEvent enum (Text, ToolCall, ToolResult,
  Display, Stop) for streaming display. Named Turn* rather than
  Stream* to avoid colliding with traits::data_stream::StreamEvent.
  DisplayHandler forwards to the sink; orchestrator emits the
  others. Default NoOpSink; VecSink for tests.

Runtime-layer new modules:
- agent_loop: one wire turn per call. Stream → accumulate →
  dispatch evals in parallel → join → build TurnOutput.
- sdk::preamble: Haskell boilerplate from each handler's
  DescribeEffect impl.
- sdk::code_tool: CODE_TOOL genai::Tool static + source templating
  adapted from tidepool_mcp::template_haskell.
- router: Router trait + RouterRegistry; CliRouter ships as the
  first scheme handler. Send/Reply/Notify dispatch via registry.

Each of the 11 SDK handlers impl DescribeEffect. DisplayHandler gains
a TurnSink constructor arg; forwards Display.Show to the sink.

TidepoolSession::step becomes the wire-turn loop driver: runs
agent_loop::orchestrate, builds TurnInput::from_tool_results for
continuation, loops until stop_reason.is_terminal() or no tool_calls.

Ask stays stubbed as candidate-for-removal: v3 agents don't call LLMs
via effects. LLMs drive agent work via Session::step; the `code` tool
lets them invoke SDK capabilities.

Hybrid pre-baked helpers: sessions accept optional agent_helpers_dir,
threaded through compile_and_run's include paths. GHC's module cache
handles the rest.

Depends on Task 4+5 (adapter + TurnHistory + DB wiring) and Task 8-10
(composer). Blocks Task 15 (e2e test) and Task 16 (zero-blocks edge).
```

### Task 20 execution log

The task landed as a seven-commit chain rather than a single monolithic commit. Documenting the split here so reviewers can walk them in order:

1. **Part 1** — `DescribeEffect` trait + impls on all 11 SDK handlers + `sdk::preamble::build()` assembler.
2. **Part 2** — `CODE_TOOL` static `genai::Tool` + `template_source()` adapted from `tidepool_mcp::template_haskell` + `CodeToolInput`.
3. **Part 3** — `Router` trait (takes TARGET only, not full recipient) + `RouterRegistry` with **default-scheme fallback** for malformed/unknown-scheme recipients + `CliRouter` + `MessageHandler.{Send,Reply,Notify}` routing + `Ask` stubbed as candidate-for-removal.
4. **Part 4 prep** — `PERSONA_LABEL` const + `RuntimeError::ProviderError` variant + `SessionContext::model_id` field + plan re-scope for the new wire-turn-loop architecture (Anthropic's protocol forces one provider call per "turn" — see rationale above).
5. **Part 5a** — types-layer: `StopReason` + `ToolOutcome`/`ToolResult` + `TurnSink`/`TurnEvent` with `DisplayKind` (Chunk/Final/Note) + Thinking variant + `TurnInput.batch_id` + `TurnOutput.{tool_calls,tool_results,stop_reason}` + `StepReply` + `TurnInput::from_tool_results(prior, batch_id, owner_id)`.
6. **Part 5b** — `Session::step` signature flip to `StepReply` + `SessionContext::turn_sink` field + `DisplayHandler::forward_to_turn_sink` bridge (TurnSinkForwarder). `TidepoolSession::step` interim wrapper: calls legacy `run_turn`, wraps in one-entry StepReply. Plan entry logs the **genai fork gap** (Anthropic adapter drops thinking content parts on outbound) → tracked in phase 6.
7. **Part 5c** — `agent_loop::orchestrate` (one wire turn: compose → stream → emit TurnEvents → dispatch evals → build TurnOutput) + `drive_step` (wire-turn-loop driver: chain tool_results via `from_tool_results` until `stop_reason.is_terminal()`) + `EvalDispatcher` trait + `MockProviderClient` (scriptable ProviderClient for integration tests). 10 integration tests cover single-turn, thinking+text, tool_use round-trip, two-turn chain, tool-error recovery.
8. **Part 5d** — `EvalWorker`: real `EvalDispatcher` backed by `std::thread` + 256 MiB stack + multi-thread tokio runtime (worker_threads=2, needed so `MemoryHandler`-driven async sqlx calls don't deadlock during sync `compile_and_run`) + `tidepool_runtime::compile_and_run`. Per-request: parse `CodeToolInput` → `template_source` → mpsc → worker reconstructs fresh `SdkBundle` with `DisplayHandler` forwarding to session sink → compile + run → oneshot reply. Drop-safe (channel close → worker exits at next `rx.recv()`). Phase 6 notes added for `TIDEPOOL_PRELUDE_DIR` env setup + evaluate rusqlite migration.
9. **Part 5e** — `TidepoolSession::open_with_agent_loop` spawns `EvalWorker` + builds preamble + wires `DisplayHandler::forward_to_turn_sink` + injects turn_sink. `step_with_agent_loop` method drives the full wire-turn loop via `drive_step`. Legacy `Session::step` stays on the SessionMachine path for test-fixture compat; retirement in phase 6.

### Still deferred from Task 20 (tracked as separate follow-ups)

- **Full composer integration**: `orchestrate` currently builds a minimal `CompletionRequest` (just input messages + `CODE_TOOL`). Segments 1/2/3 (system + persona, conversation history + pseudo-messages, current_state) get wired via `compose::compose` with the pass set in a follow-up refinement — the wire-loop shape doesn't change, just the request construction.
- **genai fork patch**: Anthropic adapter needs to preserve `ContentPart::{ReasoningContent, ThoughtSignature}` on outbound (currently drops them → Extended Thinking breaks across tool cycles). Phase 6 follow-up.
- **`TIDEPOOL_PRELUDE_DIR` env wiring**: Nix devshell + preflight + session-open defaults. Phase 6 follow-up.
- **Rusqlite migration evaluation**: sync MemoryStore would let the eval worker drop the tokio runtime entirely. Phase 6 decision.
<!-- END_TASK_20 -->

---

## Phase 5 "Done when" checklist

- [x] `pattern_core::types::provider` re-exports `genai::chat::CacheControl` directly (Phase 4 Task 18 already did this; Task 1 is a no-op on arrival at Phase 5)
- [ ] `CacheProfile` latched at session open; `allow_extended_ttl` respects subscription status
- [ ] Composer pipeline: `ComposerPass` trait + `PartialRequest` + `BreakpointTracker` + `finalize()` with validation
- [ ] Types-layer prep: `BlockCreate` struct bundling `MemoryStore::create_block` args; `BlockWrite::previous_rendered_content` field for diff-style pseudo-messages
- [ ] `MemoryStoreAdapter` wraps preserved pattern_core storage as Phase 2's trait + holds a pending `Vec<BlockWrite>` buffer that handlers push into as they mutate; session drains at turn close
- [ ] `TurnHistory` holds in-memory active turns (unbounded at its layer; compaction manages size), `summary_head` vector of recent `ArchiveSummary`s loaded from pattern_db, and a running `estimated_tokens: u64` that combines real counts + heuristic fallback (no `Option`-wrapping at the API)
- [ ] Each wire turn produces a real `TurnOutput`: `messages` drained from pending + synthesized assistant msg, `block_writes` from adapter drain, `tool_calls` + `tool_results` from stream/eval, `stop_reason` from stream End, `usage` from provider response, `cache_metrics` populated (Task 12 feeds in). `Session::step` aggregates wire turns into `StepReply`.
- [ ] Per-wire-turn message persistence to pattern_db `messages` table (`is_archived=0` at turn close; set to `1` during compaction). All wire turns from one `step` share `batch_id`; each has distinct `turn_id`.
- [ ] Hierarchical archive summaries wired to pattern_db's `archive_summaries` table — depth-0 rows created during compaction, depth-N rollups generated when depth-(N-1) chain grows too long for the summary-head prepend budget
- [ ] Pseudo-message renderer emits `[memory:written]` / `[memory:updated]` in `<system-reminder>` tags; uses `similar::TextDiff` (existing pattern_core dep) against `previous_rendered_content` for update diffs
- [ ] `[memory:current_state]` pseudo-turn renderer: block-aware rendering per schema type; empty-case preserved (AC7.6)
- [ ] Segment 1 / 2 / 3 passes with cache_control marker placement; segment 2 prepends summary-head vector as synthesized "earlier context" + recent messages + BlockWrite pseudo-messages
- [ ] Break-detection hashing captures system/tools/cache_control/betas/model state per turn
- [ ] Cache-hit metrics emitted via tracing; segment-1 bust detection fires loud warning (AC8.5)
- [ ] Compression strategies migrated to async `count_tokens`; budget policy = `context_window - max_output - explicit_buffer`; compaction activates when `TurnHistory.estimated_tokens` approaches budget; batch integrity preserved with pseudo-messages
- [ ] Block-rendering code removed from all system-prompt paths (AC7.2 guarantee)
- [ ] Task 19: scope-aware Search + Recall SDK modules + handlers; v2 functional parity preserved (self-always, cross-agent via shared-blocks or group-membership, constellation via broad permission). Recall scope is optional; block-read ops honor `shared_blocks` for cross-agent access. `AiTool` / `ToolContext` Rust infrastructure NOT ported — v3 uses effects + handlers exclusively.
- [ ] Task 20: agent loop + `code` tool + router. Types-layer: `StopReason` / `StepReply` / `ToolResult` / `ToolOutcome` / `TurnSink` + `TurnEvent` in pattern_core; `TurnOutput` extended with `tool_calls` / `tool_results` / `stop_reason`; `TurnInput::from_tool_results(prior, batch_id)` constructor; `Session::step` returns `StepReply`. Runtime-layer: `agent_loop::orchestrate` runs **one wire turn** (no inner loop) with mid-stream parallel eval dispatch via FuturesUnordered; `TidepoolSession::step` drives the **wire-turn loop** — repeatedly calls orchestrate, threads tool_results through `TurnInput::from_tool_results`, preserves `batch_id` across all wire turns in one step, returns `StepReply` when `stop_reason.is_terminal()`. `MessageHandler.{Send,Reply,Notify}` dispatch through scheme-keyed router registry; `CliRouter` is the first scheme handler. `DisplayHandler` forwards to the session's `TurnSink`. LLM-emitted `code` tool_use snippets evaluated via `tidepool_runtime::compile_and_run` against the SDK bundle. `Ask` stubbed as candidate-for-removal. Compile hot path minimized via include-path discipline (GHC module cache + tidepool CBOR cache).
- [ ] `cargo check`, `clippy`, `doc` all zero-warning across the narrowed workspace
- [ ] `bash scripts/audit-rewrite-state.sh` passes
- [ ] `just pre-commit-all` passes
- [ ] Live cache-hit behaviour verified via AC9.1 CLI checklist Step 5-6 (manual operator confirmation; no env-gated test)

## What this phase deliberately does NOT do

- Does not implement `cache_reference` for tool_result stitching (future compaction-enhancements plan; hook exists as a `ComposerPass` attachment point)
- Does not implement `cache_edits` / microcompact deletions (same future plan)
- Does not implement per-tool cache hashing for cache-bust attribution (future observability plan; hook: `CacheProfile::per_tool_hashes` field addition)
- Does not implement `CacheStrategy::McpAware` — MCP integration is a future plan; variant declared for API stability only
- Does not implement `CacheStrategy::BedrockExtraBody` — Bedrock provider is future work
- Does not implement overage-based TTL downgrade — Pattern's billing-aware logic lives in a future plan
- Does not implement 24h TTL selection by default — `CacheControl::Ephemeral24h` is declared and serializes correctly but Phase 5's profile defaults to 1h for segment 1
- Does not change memory storage behavior in any way — the preserved `pattern_core::memory` crate is untouched; Phase 5 only adds an adapter wrapper
- Does not redesign compression strategies — the four existing strategies preserved; only the token-counting call path changes from heuristic to async provider-reported
- Does not touch the block schema types — Text / Map / List / Log / Composite remain as pre-v3
- Does not implement automatic cache-strategy flipping based on MCP tool discovery — that's the `CacheStrategy::McpAware` hook, future work
- Does not implement bespoke block-level diff algorithms beyond the minimal "content replaced, previous hash X" shown in the pseudo-message renderer — proper diffs are a future UX-polish item
