# pattern_runtime

Agent runtime for Pattern v3. Houses Tidepool (Haskell-in-Rust) embedding, the
agent turn loop, `freer-simple` effect handlers, and turn-level checkpoint
machinery. Depends only on `pattern_core` trait definitions.

Last verified: 2026-04-19

See the v3 foundation design at
`docs/design-plans/2026-04-16-v3-foundation.md` for the substrate choice,
SDK hierarchy, and phase ordering.

## Runtime setup

`pattern_runtime` compiles agent Haskell programs via the `tidepool-runtime`
Rust crate, which shells out to the **`tidepool-extract`** GHC plugin binary
(~300 MB, GHC 9.12). The binary must be available at runtime or
`compile_haskell()` fails. Resolution order:

1. `$TIDEPOOL_EXTRACT` env var if set (absolute path to the binary).
2. `tidepool-extract` on `$PATH` otherwise.

### With Nix (recommended)

```sh
nix develop   # enters pattern-shell with tidepool-extract on PATH
                # and $TIDEPOOL_EXTRACT exported to the absolute store path
which tidepool-extract   # should print a /nix/store/... path
```

The devshell module at `nix/modules/devshell.nix` pulls the
`github:orual/tidepool` flake input (our fork — see `flake.nix` for the
reasoning) and surfaces the binary via the `tidepool-extract` derivation.
The pinned revision lives in `flake.lock`; bump it with
`nix flake update tidepool` when chasing updated fixes on our fork or to
swap back to upstream once our patches merge.

Developers iterating on tidepool itself can override the input:

```sh
nix develop --override-input tidepool path:../tidepool
```

This picks up uncommitted local changes and skips the GitHub fetch.

### Stale-harness troubleshooting

**Symptom:** `test_cross_module_effect_runs` or other multi-module agent
compilation fails with `CASE TRAP` / `Jit(Yield(Undefined))`, *despite*
`flake.lock` pinning tidepool at a commit that contains the fix.

**Cause:** `$TIDEPOOL_EXTRACT` in the active devshell / direnv cache
points at an older `tidepool-extract` derivation built from a pre-fix
harness snapshot. The symlink chain
(wrapper → harness → haskell-snapshot) may be pinned to a stale store
path even after `flake.lock` moves forward.

**Recovery:**

```sh
# 1. Force eval of the pinned harness (no-op if cache already has it).
nix build github:orual/tidepool/$(jq -r '.nodes.tidepool.locked.rev' flake.lock)#tidepool-extract

# 2. Reload direnv — this is what actually refreshes $TIDEPOOL_EXTRACT.
direnv reload

# 3. Verify the resolved binary.
readlink -f "$TIDEPOOL_EXTRACT"
# Must match the path produced by step (1).
```

Or, for one-off runs: `TIDEPOOL_EXTRACT=$(nix build --print-out-paths .#tidepool-extract)/bin/tidepool-extract cargo nextest run ...`

Hardening opportunity (upstream, not urgent): add a
`tidepool-extract --version` endpoint whose commit-hash output
`tidepool-runtime::compile_haskell` cross-checks against its own
`EXPECTED_HARNESS_VERSION` constant at session open. Self-diagnosing
error instead of silent CASE TRAP.

### Without Nix

Clone and build tidepool-extract from
`https://github.com/tidepool-heavy-industries/tidepool` (requires GHC 9.12 +
Cabal; see that repo's `README.md` for build instructions). Then either
place the resulting binary on `$PATH` or export
`TIDEPOOL_EXTRACT=/abs/path/to/tidepool-extract`.

### Preflight

`pattern_runtime::preflight::check()` (Phase 3 Task 5) verifies the binary is
reachable and returns a structured error pointing at this section when the
setup is wrong. Run it at binary startup before opening any Session.

## Agent loop architecture (`agent_loop.rs`)

The agent loop is split into two layers:

- **`orchestrate`** — executes one wire turn: compose request, stream
  provider response, emit `TurnEvent`s to the session's `TurnSink`,
  dispatch tool_use evals, synthesize a `ChatRole::Tool` message with
  all `ToolResponse` parts, and append it to `TurnOutput.messages`.
  The tool_result message is built by `orchestrate` after dispatch --
  NOT by the caller.

- **`drive_step`** — wire-turn loop driver. Chains tool_use cycles via
  `TurnInput::continuation(batch_id, agent_id)` (empty messages --
  prior tool_result lives in TurnHistory). Records `(input, output)`
  pairs atomically via `hist.record()`. Returns `StepReply` when
  `stop_reason.is_terminal()`.

### Batch-anchored snapshot attachments

Memory snapshots are NOT a separate Segment 3 composer pass (the old
`Segment3Pass` pseudo-message approach is retired from the agent loop).
Instead, snapshots are attached to batch-opening user messages as
`MessageAttachment::BatchOpeningSnapshot` and spliced onto the wire at
compose-time by `compose_request_for_turn` (step 8). This eliminates
the cache-busting problem where the old seg3 pseudo-message changed
"last message" identity across turns.

Snapshot kind decision (`build_snapshot_attachment`):
- **Full** — emitted when `batches_since_last_full` hits threshold, or
  `post_compaction_pending` is set, or history is empty.
- **Delta** — emitted otherwise; includes only blocks whose
  `content_hash` changed since the prior full/delta baseline.

The delta baseline is computed by `collect_last_tracked_hashes`, which
walks the full history latest-wins per label (not just the most recent
attachment). `content_hash` uses `blake3::hash(...).as_bytes()[..8]`
(not `DefaultHasher`) for cross-process stability.

`Segment3Pass` still exists in `pattern_provider` for standalone
compose-pipeline tests, but the agent loop does NOT use it -- it places
the seg3 cache marker directly on the last message that had an
attachment spliced (see `last_spliced_idx` in `compose_request_for_turn`).

**MessageId origin tagging:** the runtime locates composed messages
for attachment splicing via `PartialRequest.message_origins`, a parallel
vector populated by `Segment2Pass` that maps each composed message back
to its Pattern `MessageId`. The splice loop builds a `HashMap<SmolStr,
usize>` from `ComposeOutput.message_origins` for O(1) lookup instead of
computing indices from `summary_count` offsets. This is robust against
future pass reordering or insertion.

### MemoryStoreAdapter (`memory/adapter.rs`)

Thin wrapper over `Arc<dyn MemoryStore>` with a pending `BlockWrite`
buffer. Handlers call `record_write()` explicitly after mutations
(they hold the semantic context: Create vs Replace, pre-content state).
The session drains the buffer at turn close to populate
`TurnOutput.block_writes` and feed pseudo-message emission.

Design choice: the adapter does NOT intercept trait-method calls to
auto-record writes. It is a simple, auditable passthrough plus a
pending buffer.

### TurnHistory (`memory/turn_history.rs`)

`TurnRecord` stores both `input: TurnInput` and `output: TurnOutput`
for each turn. `active_messages()` interleaves input and output
messages in order so `Segment2Pass` replays the complete conversational
context.

Snapshot-related state tracked by `TurnHistory`:
- `batches_since_last_full: u32` — reset on Full, incremented on new batch.
- `post_compaction_pending: bool` — set by compaction layer, consumed
  by `drive_step` to force a Full on next batch.
- `most_recent_batch_id: Option<BatchId>` — detects new-batch transitions.

### Compaction (`compaction.rs`)

`maybe_compact(ctx, turn_history, context_policy)` is called from
`drive_step` before each wire turn's compose step. It checks the
persona's `ContextPolicy` gate and applies the configured
`CompressionStrategy` when the gate fires.

**Gate logic** (short-circuits in order):
1. `compression` is `None` → skip (compression disabled for this persona).
2. `active_len < compress_check_message_floor` (default 100) → skip.
3. `count_tokens` (async provider call) below `compress_token_threshold`
   → skip. Default threshold: `context_window - max_tokens - 8192 buffer`,
   where context_window falls back to 128k when per-model metadata is
   unavailable.

**Strategy dispatch matrix:**

| Strategy | Provider call | Summary row | Notes |
|---|---|---|---|
| Truncate | gate only | no | keeps N most recent turns |
| ImportanceBased | gate only | no | scores older turns heuristically |
| TimeDecay | gate only | no | archives turns older than cutoff |
| RecursiveSummarization | gate + complete() | depth=0 | calls provider to summarize oldest chunk |

**Post-strategy invariants:**
- `archive_messages` marks `is_archived=1` for messages with
  `position < boundary` in pattern_db.
- `TurnHistory::take_oldest` drops archived turns from the active deque.
- `post_compaction_pending` is set to `true`, causing the next batch's
  snapshot to be Full (ensures the model gets a complete context view).
- For RecursiveSummarization: an `archive_summaries` row (depth=0) is
  created and `summary_head` is reloaded from `get_summary_head`.

**Session-UUID rotation on compaction:** when `maybe_compact` returns
`CompactionOutcome::Fired`, the compaction driver calls
`ctx.provider().rotate_session_uuid()` to cycle the Anthropic session
UUID. This prevents the post-compaction (shorter) context from being
confused with the pre-compaction context by Anthropic's server-side
cache. `ProviderClient::rotate_session_uuid` has a default no-op
implementation; `PatternGatewayClient` provides the real rotation.

**How to disable compression for a persona:**
Set `context.compression = None` in the persona TOML (or
`ContextPolicy::default()` which has `compression: None`).

**Future work:** depth->=1 summary rollup (running RecursiveSummarization
on accumulated depth=0 summaries) is out of scope for foundation.

### Eval worker (`agent_loop/eval_worker.rs`)

Eval worker is a plain OS thread spawned via `std::thread::spawn` with a
256 MiB stack (GHC continuation frames need it). Intake channel is
`std::sync::mpsc::Sender<EvalRequest>` owned by `EvalWorker`; reply
channel is `tokio::sync::oneshot::Sender<ToolOutcome>` per request. The
worker runs Tidepool's Haskell evaluator directly against the sync
`MemoryStore` surface — no nested tokio runtime, no `block_in_place`,
no `Handle::current().block_on`.

Panic handling: worker thread panic terminates the thread; session
becomes unusable (channel closed); callers observe channel-closed errors
on the next dispatch. This is the intended failure mode (fail loud; no
silent deadlock).

Freshness date: 2026-04-19 (v3-memory-rework Phase 3).

### SessionContext (`session.rs`)

Gains `snapshot_policy: SnapshotPolicy` field wrapping:
- `selection: SnapshotSelection` — which block types/labels appear in
  batch-opening snapshot attachments. Defaults to Core + Working.
- `mid_batch: MidBatchDeltaBehavior` — controls whether a turn's own
  tool-initiated `block_writes` trigger mid-batch delta attachments on
  tool_result messages. `IncludeSelfEdits` (default) preserves the
  agent-trust signal at cache cost; `FilterSelfEdits` skips self-edits
  for cache-efficient intra-batch turns, relying on tool_result content
  to confirm the edit landed.

`snapshot_selection()` is retained as a convenience accessor returning
`&self.snapshot_policy.selection` to minimize call-site churn.

## Authoring agent programs

### SDK imports

Agent programs import from the `Pattern.*` SDK module tree (installed at
`$PATTERN_SDK_DIR` or `crates/pattern_runtime/haskell/Pattern/` by default).
`tidepool-extract` compiles agents with the SDK directory on its include
path -- all 13 effect modules plus vendored utility modules are compiled
and linked together.

The SDK uses a hybrid qualified/unqualified import scheme. Modules with
unambiguous terse verbs are used unqualified; modules with generic verbs
(get, read, error, search, etc.) are used qualified to avoid collision:

```haskell
-- Unqualified: Message, Time, Display, Spawn (terse, no conflicts)
import Pattern.Message
import Pattern.Time
import Pattern.Log   -- use qualified: Log.error avoids shadowing the error shim

-- Qualified: Memory, File, Log, Search, Recall, Sources, Shell, Rpc, Mcp
import qualified Pattern.Memory as Memory
import qualified Pattern.File as File
import qualified Pattern.Log as Log

agent = do
  Memory.put "notes" "hello"        -- Memory.Put
  File.write "/tmp/f" "contents"    -- File.Write
  _ <- File.read "/tmp/f"           -- File.Read (renamed from read_)
  _ <- Memory.get "notes"           -- Memory.Get
  send "agent:orual" "ping"         -- Message.send (renamed from send_)
  Log.error "oops"                  -- Log.Error (renamed from error_)
```

For code-tool (`code` tool eval) programs, the preamble builds the
hybrid import scheme automatically — agents write bare `send`, `now`,
`chunk`, `start` for unqualified modules and `Memory.put`, `File.read`,
`Log.info`, `Search.messages`, `Recall.get` for qualified ones.

Collision-avoidance decisions on the Haskell side:

- `Memory` uses `Get`/`Put` constructors (KV semantics) — leaving
  `Read`/`Write` constructors to `File`.
- `Search` helpers are `messages`/`archival`/`all_` (prefix dropped;
  GADT constructors `SearchMessages`/`SearchArchival`/`SearchAll` retain
  unique names for the Rust decode layer).
- `Recall` helpers are `insert`/`search`/`get`/`delete` (prefix dropped;
  both `Memory.get` and `Recall.get` exist so qualified import is required
  when both are in scope).
- `File.read` renamed from `read_` — use qualified `File.read` to avoid
  shadowing `Prelude.read` in files without `NoImplicitPrelude`.
- `Message.send` renamed from `send_`; `Log.error` renamed from `error_`.
- `File.List` is `ListDir` — leaves `List` to `Sources`.
- `Rpc.Call` (request/response) — leaves `Send` to `Message`.

Defense-in-depth at the host-runtime decode boundary is provided by the
derive layer (arity disambiguation + `#[core(module = "Pattern.<Module>",
name = "...")]` on every SDK request variant).

Effect-row ordering matters: handler position in the `SdkBundle` HList
determines the JIT effect tag. The canonical order is storage-adjacent
first (`Memory, Search, Recall`), then messaging/display (`Message,
Display, Time, Log`), then rarer effects (`Shell, File, Sources, Mcp,
Rpc, Spawn`):

```
Memory, Search, Recall, Message, Display, Time, Log, Shell, File,
Sources, Mcp, Rpc, Spawn
```

Agent `Eff '[...]` rows must line up with this prefix.

### Vendored utility modules

The SDK vendors several utility modules so agents are fully
self-contained (no tidepool-mcp dependency):

- `Pattern.Prelude` — curated prelude (Text-returning `show`, list/Map
  helpers, Aeson construction). Does NOT re-export the 13 effect modules.
- `Pattern.Aeson`, `Pattern.Aeson.Value`, `Pattern.Aeson.KeyMap`,
  `Pattern.Aeson.Lens` — JSON construction + traversal.
- `Pattern.Table` — tabular text formatting.
- `Pattern.Text` — Text utilities.

Notable: `Instant` and `Duration` (from `Pattern.Time`) derive `Show`,
so agents can `show now` in log lines.

### Code-tool description and preamble

The `code` tool's description (`sdk/code_tool.rs`) is ~6.4 KB and built
once at process startup from `canonical_effect_decls()`. It contains:
- Full API reference (every helper signature across all 13 effects).
- Effect-row and import-scheme conventions.
- Common gotchas section (e.g. `Memory.get` returns `Content` not
  `Maybe`, `pure ()` not `return unit`, `Show Instant` works,
  `Memory.list` does not exist).

The preamble (`sdk/preamble.rs`) builds the Haskell module header for
each eval: pragmas, `Pattern.Prelude` import, SDK effect imports via
the hybrid qualified/unqualified scheme, `type M` effect-row alias,
and an API documentation comment block assembled from `EffectDecl.helpers`
for LLM discoverability. GADT declarations are NOT inlined -- the
effect modules are imported directly (viable since the tidepool
multi-module compilation bug was fixed in our fork).

### In-memory test double (`testing/in_memory_store.rs`)

**Feature gate:** `pub mod testing` is gated behind
`#[cfg(any(test, feature = "test-support"))]`. External crates that
import `pattern_runtime::testing::InMemoryMemoryStore` (e.g.
`pattern-test-cli`) must declare `features = ["test-support"]` on
their `pattern-runtime` dependency. The `pattern-test-cli` binary
already uses `required-features = ["test-support"]` in its
`[[bin]]` manifest entry.

Minimal `MemoryStore` implementation for integration tests. Phase 5
wired previously-stubbed methods:
- `set_block_pinned` — mutates metadata via Arc-shared `metadata_mut`.
- `insert_archival` / `search_archival` / `delete_archival` — stored
  in `Vec<ArchivalRecord>` with naive `contains()` search.
- `update_block_schema` — mutates `metadata.schema`.

### Search, recall, and shared-block access

`Pattern.Search` provides scoped search across message history and
archival entries. Search scope is an optional `Maybe Scope` parameter:

- `Nothing` or `"current"` — current agent only (always allowed).
- `"agent:<id>"` — specific agent (requires shared-blocks or group
  membership).
- `"agents:<id1>,<id2>"` — multiple agents (filters unpermitted).
- `"constellation"` — all agents in the constellation.

`Pattern.Recall` provides archival-entry CRUD (insert/search/get/delete).
The search operation takes an optional scope with the same semantics.

`Pattern.Memory.GetShared` allows agents to read blocks shared to them by
other agents. Permission is checked against the `shared_blocks` table.

#### Permission model

The scope resolver (`handlers/scope.rs`) implements the permission
checks. For cross-agent access, the ordering of permission signals is:

1. **Self** — always allowed (short-circuit).
2. **Shared blocks** — if the target agent has shared at least one block
   with the caller, cross-agent search is allowed.
3. **Group membership** — if both agents are in the same `agent_group`,
   cross-agent search is allowed.

This policy is configurable; future phases may add trust-level gates or
explicit capability flags.

## Smoke-test procedure (v3 foundation AC9.*)

> **Also see:** `docs/smoke-test-v3-foundation.md` — polished smoke-test
> cover sheet with tolerances table, failure-diagnosis matrix, and a
> completion checklist. The section below is the primary source of
> truth for the procedure; the companion doc references it.

The v3 foundation smoke test is a **manual procedure** driven through the
`pattern-test-cli spawn` subcommand. Live-credential tests in CI are a
foot-gun — credentials rotate + expire, rate-limit noise swamps real
failures, per-run API cost accumulates — so Phase 6 ships a CLI binary + this
checklist as the verification vehicle rather than an auto-run
live-credential test.

The failure-mode tests at `tests/error_clarity.rs` run in CI and verify
error specificity per step (AC9.5). Everything below is manual; design plan
AC9.1's "deterministically" is satisfied by the repeatable documented
procedure rather than an auto-run smoke_e2e.rs.

### Setup (one-time per machine)

1. Ensure `tidepool-extract` is reachable (see Runtime setup §).
2. Build the bin: `cargo build -p pattern-runtime --bin pattern-test-cli`.
3. Pick an auth path:
   - **API key:** export `ANTHROPIC_API_KEY=sk-ant-...`.
   - **OAuth (subscription):** have an active claude-code session at
     `~/.claude/.credentials.json` (session-pickup tier resolves it), or
     run the one-time PKCE flow via `pattern-test-cli auth`.

### DoD flow — AC9.1 (API-key) / AC9.2 (OAuth) / AC9.3 (CLI drives it) / AC9.4 (cache behavior)

**Step 1 — start a fresh session.** The `spawn` subcommand takes a
persona TOML path; use the smoke fixture at
`crates/pattern_runtime/tests/fixtures/smoke_persona.toml` as a baseline.

```bash
TMPDIR=$(mktemp -d)
cargo run -p pattern-runtime --bin pattern-test-cli -- \
    spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --data-dir "$TMPDIR"
# CLI prints "pattern> "
```

Add `--auth api-key | session-pickup | pkce` to force a specific tier;
default is whatever `build_chain()` resolves.

**Step 2 — talk to Claude.** Type `hello; what's your role?`. Expect a
response consistent with the smoke persona. A one-line cache summary
prints after each turn: `[cache: fresh=N read=N create=N ratio=NN%]`.

**Step 2a (one-time PKCE flow if using `--auth pkce`).** CLI prints an
auth URL, opens browser, paste back the `code#state` string. Token is
stored in keyring (or JSON fallback at
`$XDG_CONFIG_HOME/pattern/creds/anthropic.json`). Subsequent runs reuse
the stored token.

**Step 3 — write a memory block (AC9.1 step 4).** Type:
`please remember in your scratchpad: favorite color is teal.`
Expect: agent confirms + the cache-metrics line. If `verbose=true` the
change-log debug output shows the `memory.put` effect firing.

**Step 4 — exit + re-spawn against the same data dir (AC9.1 step 5).**
`:q` or Ctrl+D to exit. Re-run the same `spawn` command with the same
`--data-dir`. Memory persists across restart via the DB-backed
MemoryCache; `--data-dir/constellation.db` is the store.

**Step 5 — recall the stored value (AC9.1 step 6).** Type:
`what's my favorite color?`. Expect: `teal` in the response.

**Step 6 — capture pre-edit cache metrics (AC9.1 step 7).** Type:
`ok, thanks`. Note the `read` and `ratio` values printed after the turn.

**Step 7 — edit the block mid-session (AC9.1 step 8, AC9.4).** Use the
`:edit-block <label> <content>` REPL command:

```
pattern> :edit-block scratchpad favorite color is actually indigo
[edit-block] 'scratchpad' updated (35 chars)
```

The Arc-shared memory store means the session picks up the edit on
its next turn without explicit reload.

Type: `confirm the update`. Expect:
- `ratio` ≥ the pre-edit ratio minus 5% (AC8.1 / AC9.4: segment 1
  prefix preserved across the memory edit)
- `create` token count spikes (AC8.2: segment 3 invalidated — the
  new block content has to be cached fresh)

Record the numbers. If `ratio` drops dramatically beyond the
expected seg3 invalidation, check `tracing::warn!` logs for
break-detection output (Phase 5 Task 11).

**Step 8 — exit.** `:q`. Session shuts down cleanly.

### When things fail

- Any unclear error surfaced at the CLI is an AC9.5 regression — add a
  test case at `tests/error_clarity.rs` before debugging further.
- If `ratio` collapses unexpectedly during step 7, inspect the
  break-detection warnings and diff the composed requests for
  segment-1 differences.
- If the persona TOML fails to load, `persona_loader`'s error messages
  should name the failing field or step; if they don't, tighten them.

### What the CLI deliberately does NOT do

- No auto-run smoke test with live credentials. The checklist above IS
  the smoke test.
- No polished UX. `pattern-test-cli` is a throwaway driver; the real
  CLI lives in a post-foundation plan (likely rebuilt on ratatui).
- No cross-provider routing demo. Same provider per session.
- No constellation / multi-agent paths. Foundation is single-agent.

## Known flakes — historical note

Two tests previously flaked intermittently under
`cargo nextest run --workspace` parallel load:

- `session_lifecycle::open_step_twice_does_not_recompile`
- `timeout::hard_abandon_await_enforces_cancel_grace_ceiling`

Both touched the `tidepool-extract` subprocess path. Hypothesis was
concurrent `tidepool-extract` spawns contending on shared cache paths /
lockfiles / wall-clock margins.

**Both were deleted during Phase 6 Task B** when the SessionMachine
static-program path retired. The 677-test suite has run clean under
full parallel load across the final review cycles without recurrence.

If new tests that shell out to `tidepool-extract` land later and show
similar parallel-load flakes, these investigation vectors apply:

1. Tracing-level logging on subprocess spawn / cache-lookup to identify
   which shared resource is contending.
2. `cargo nextest run --test-threads=1` to confirm single-threaded runs
   are clean — distinguishes contention from a second bug.
3. Audit per-test tempdirs for accidental collapse to a shared
   `/tmp` or `$XDG_CACHE_HOME` path.
4. For wall-clock-timing assertions: widen grace ceilings or switch to
   a deterministic tokio-test clock.
