# pattern_runtime

Agent runtime for Pattern v3. Houses Tidepool (Haskell-in-Rust) embedding, the
agent turn loop, `freer-simple` effect handlers, and turn-level checkpoint
machinery. Depends only on `pattern_core` trait definitions.

Last verified: 2026-04-26 (post v3-multi-agent Phase 4 + v3-sandbox-io Phases 1-5)

v3-TUI integration note: the runtime is consumed by `pattern_server`'s actor
via `TidepoolSession`, `MultiplexSink`, and per-batch `TurnSinkBridge`. The
session open path runs in spawned tasks (not the actor loop) and wire-safe
events are emitted as `WireTurnEvent` for IRPC transport. No runtime public
API changes landed during v3-TUI — what changed was who holds sessions and
how events are routed out.

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
  `stop_reason.is_terminal()`. Accepts `on_turn: Option<TurnObserver>`
  (Phase 2): an optional per-turn callback invoked after each turn is
  recorded. `TurnObserver = Arc<dyn Fn(&TurnOutput) + Send + Sync>`.
  Existing callers pass `None`; ephemeral spawn uses it for progress-log
  entries.

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
Omit the `compression` block in the persona KDL (or
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

`LIVE_EVAL_WORKERS: AtomicUsize` (Phase 2): global counter incremented
on thread spawn, decremented via RAII guard inside the worker closure.
`live_eval_workers()` accessor exposed for tests. Used by AC3.6 leak-
detection tests to assert that all child eval workers terminate after
the parent session resolves.

### `<mount>/lib/` include-path extension

When a mount provides a `lib/` directory, each `.hs` file is
probe-compiled individually via `tidepool_runtime::compile_haskell`
(Approach A). The probe generates a minimal Haskell source that imports
the module qualified and calls `pure ()`, exercising GHC's parser and
type-checker without executing effects. Modules that pass the probe
cause `lib/` to be added to the eval worker's include path; modules
that fail are recorded as `LibCompileFailure` and surfaced to agents
via `Pattern.Diagnostics`.

Entry point: `crate::sdk::lib_modules::validate_and_resolve(mount_path, base_include_paths)`.

### Pattern.Diagnostics + WriteToPersona (Phase 8)

**Pattern.Diagnostics:** `GetDiagnostics` returns a JSON-encoded list of
session diagnostic events (lib-compile failures, handler errors). Handler
at `sdk/handlers/diagnostics.rs`; Haskell module at
`haskell/Pattern/Diagnostics.hs`. The diagnostics list is accumulated
during session construction and exposed read-only to agents.

**WriteToPersona:** Part of `Pattern.Memory` — allows explicit writes to
the persona scope when `IsolatePolicy::None` is active. Under
`CoreOnly` or `Full`, returns `MemoryError::IsolationDenied`.

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
path -- the SDK effect modules plus vendored utility modules are compiled
and linked together.

The SDK uses a hybrid qualified/unqualified import scheme. Modules with
unambiguous terse verbs are used unqualified; modules with generic verbs
(get, read, error, search, etc.) are used qualified to avoid collision:

```haskell
-- Unqualified: Message, Time, Display, Spawn (terse, no conflicts)
import Pattern.Message
import Pattern.Time
import Pattern.Log   -- use qualified: Log.error avoids shadowing the error shim

-- Qualified: Memory, File, Log, Search, Recall, Shell, Mcp, Port
import qualified Pattern.Memory as Memory
import qualified Pattern.File as File
import qualified Pattern.Log as Log
import qualified Pattern.Port as Port

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
- `File.List` is `ListDir` — avoids ambiguity with generic `List`.
- `Port.Call` (request/response to external services) — leaves `Send` to `Message`.

Defense-in-depth at the host-runtime decode boundary is provided by the
derive layer (arity disambiguation + `#[core(module = "Pattern.<Module>",
name = "...")]` on every SDK request variant).

Effect-row ordering matters: handler position in the `SdkBundle` HList
determines the JIT effect tag. The canonical order is storage-adjacent
first (`Memory, Search, Recall, Tasks, Skills`), then messaging/display
(`Message, Display, Time, Log`), then rarer effects (`Shell, File, Mcp,
Spawn, Diagnostics`), then `Port` last (the unified external-service
port from v3-sandbox-io Phase 4 — replaces the retired `Sources` and
`Rpc` effects):

```
Memory, Search, Recall, Tasks, Skills, Message, Display, Time, Log,
Shell, File, Mcp, Spawn, Diagnostics, Port
```

Agent `Eff '[...]` rows must line up with this prefix. The
`canonical_decls_has_15_entries` test in `sdk/bundle.rs` is the source
of truth for the ordering and entry count.

### Vendored utility modules

The SDK vendors several utility modules so agents are fully
self-contained (no tidepool-mcp dependency):

- `Pattern.Prelude` — curated prelude (Text-returning `show`, list/Map
  helpers, Aeson construction). Does NOT re-export the SDK effect modules.
- `Pattern.Aeson`, `Pattern.Aeson.Value`, `Pattern.Aeson.KeyMap`,
  `Pattern.Aeson.Lens` — JSON construction + traversal.
- `Pattern.Table` — tabular text formatting.
- `Pattern.Text` — Text utilities.

Notable: `Instant` and `Duration` (from `Pattern.Time`) derive `Show`,
so agents can `show now` in log lines.

### Code-tool description and preamble

The `code` tool's description (`sdk/code_tool.rs`) is ~6.4 KB and built
once at process startup from `canonical_effect_decls()`. It contains:
- Full API reference (every helper signature across the SDK effects).
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

### `populated_spawn_test_table()` (`testing.rs`)

**Feature gate:** `#[cfg(any(test, feature = "test-support"))]`.
Hand-curated `DataConTable` registration for all `ToCore` wire types
used by the spawn handler (Phase 3 adds `WireForkOpResult`,
`WireForkOpKind`, `WireForkHandle`). Integration tests in `tests/`
that exercise handler dispatch through `tidepool_eval` must call
`populated_spawn_test_table()` to get a table with the spawn-specific
DataCon entries, rather than `standard_datacon_table()` which lacks them.

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

### SDK handler visibility (`sdk/handlers/`)

The tasks and skills handler functions are `pub` (not `pub(crate)`) so
they can be called from integration tests in `tests/`. This is
intentional: direct handler calls let integration tests exercise the full
Rust SDK surface without going through the Haskell eval path (which requires
`preflight::check()` and `tidepool-extract` on PATH).

**Changed to `pub`:**
- `sdk/handlers/tasks.rs` — `handle_create`, `handle_update`,
  `handle_transition`, `handle_add_comment`, `handle_link`, `handle_unlink`,
  `handle_list_tasks`, `handle_query_graph`, and `TaskHandlerError`.
- `sdk/handlers/skills.rs` — `handle_list`, `handle_get_metadata`,
  `handle_get_usage_stats`, `handle_search`, `handle_load`, and
  `SkillHandlerError`.

### End-to-end smoke test (`tests/task_skill_smoke.rs`)

Four `#[test]` functions exercising the Tasks + Skills SDK surface
end-to-end (v3-task-skill-blocks AC10.1, AC10.3, AC10.5, AC10.8):

1. **`smoke_tasks_surface`** — `InMemoryMemoryStore` + `reconcile_task_list`
   + `handle_create`/`update`/`transition`/`link`/`list_tasks`/`query_graph`.
2. **`smoke_skills_surface`** — `Arc<MemoryCache>` (FTS5 backend) +
   `handle_list`/`get_metadata`/`search`/`load`; verifies pseudo-message
   injection and blake3 content-hash stability across loads.
3. **`smoke_cross_schema_fts`** — `Arc<MemoryCache>` + Text/TaskList/Skill
   blocks all with a shared keyword; `MemoryStore::search` returns hits from
   all three schemas.
4. **`smoke_scope_enforcement`** — `MemoryScope::Full` isolation; persona
   blocks are hidden (even from the persona), project blocks visible to all
   callers; persona write is `IsolationDenied`.

Each test uses its own fresh in-memory sqlite + store — no shared state,
safe under `--test-threads=N` (AC10.5).

Note: `InMemoryMemoryStore::search()` has no FTS5 backend and always
returns empty. Tests needing real FTS5 search must use `Arc<MemoryCache>`
backed by `ConstellationDb::open_in_memory()`.

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
persona KDL path; use the smoke fixture at
`crates/pattern_runtime/tests/fixtures/smoke_persona.kdl` as a baseline.

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
- If the persona KDL fails to load, `persona_loader`'s error messages
  should name the failing field or step; if they don't, tighten them.

### What the CLI deliberately does NOT do

- No auto-run smoke test with live credentials. The checklist above IS
  the smoke test.
- No polished UX. `pattern-test-cli` is a throwaway driver; the real
  CLI lives in a post-foundation plan (likely rebuilt on ratatui).
- No cross-provider routing demo. Same provider per session.
- No constellation / multi-agent paths. Foundation is single-agent.

## Open work: CliRouter TUI integration (Phase 5+)

**Status (post Phase 4):** `AgentRegistry`, `RouterRegistry`, and `WakeRegistry`
are now wired in `pattern_server::get_or_open_session` via `SessionRegistries`.
Agent-to-agent routing (`agent:` scheme) works end-to-end. The remaining gap is
the `CliRouter` for surfacing agent-to-cli messages in the TUI.

**Problem:** The `Router` trait (`router.rs`) does not carry origin information
(who sent the message, from which session/batch). A correct `CliRouter` for the
daemon needs origin metadata to tag outbound `WireTurnEvent::MessageSent` events
for the TUI.

**Remaining required changes:**

1. **Fix Router trait**: `route()` should receive origin context — at minimum the
   sender's agent_id. Design decision needed on whether this is a parameter, a
   field on `Message`, or a wrapper struct.

2. **Add `WireTurnEvent::MessageSent`** variant to `protocol.rs`:
   `MessageSent { recipient: String, body: String }`. This is a wire-only
   concept — no internal `TurnEvent` variant needed.

3. **Add `WireTurnEvent::Text` agent name prefix**: Text events should render
   with `[agent-name]` prefix in the TUI. Thread agent name through `RenderBatch`.

4. **Implement `CliRouter`**: holds a channel to the daemon's event bus. On
   `route()`, constructs `TaggedTurnEvent` with `MessageSent` and sends it.
   Registered as the default scheme in the daemon's `RouterRegistry`.

**Current state (Phase 4):** `RouterRegistry` is created per session in
`get_or_open_session`. The `AgentRouter` (`agent:` scheme) is registered and
routes to other agent mailboxes. No `CliRouter` registered yet — `Message.Send`
to `"cli:..."` targets will return "no router found for scheme cli".

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

## Capability + permission system (v3-multi-agent Phase 1)

### `permission` module

Sync-to-async bridge between the eval-worker thread and the
async `pattern_core::permission::PermissionBroker`. Same channel
shape as `RouterBridge` (`router.rs`):

- `PermissionBridge::spawn(broker)` registers a long-lived tokio task
  that drains an `mpsc::UnboundedSender<PermissionBridgeRequest>` and
  invokes `broker.request(...)`. Replies travel back via
  `std::sync::mpsc::sync_channel` so the eval-worker thread can block
  on the result without needing tokio context.
- `request_sync(...)` is the handler-facing entry point. Returns
  `None` on bridge-closed, broker denial, or broker timeout — handlers
  treat all three as denial.
- Per-session: each `SessionContext` owns one bridge instance,
  spawned in `open_with_agent_loop` after the broker is constructed.

### `policy` module

Composes `pattern_core::PolicySet` for each session and ships the
runtime-side helpers consumed by the gated handlers:

- `rust_defaults()` — conservative baseline: Shell `RequireApproval`
  on `rm -rf*` / `sudo*` / `mkfs*` / `dd if=*` / `chmod -R 000*`,
  Spawn `RequireApproval`. Pattern config KDL writes are NOT a
  default rule — they're enforced at the File handler level.
- `is_pattern_config_kdl(path, content) -> ConfigGuardVerdict` — shape
  detection used by the File handler. Filename rule + top-level
  identifier scan; prefers false positives for safety.
- `PERMISSION_DENIED_PREFIX` / `GATE_APPROVED_PREFIX` — string
  prefixes handlers attach to `EffectError::Handler` messages so
  tests (and the eventual UI) can discriminate denial / approval /
  pure stub paths without parsing prose.

### `SessionContext` extensions

New fields for the capability + permission machinery:

- `capabilities: Option<pattern_core::CapabilitySet>` — `None` means
  full power; `Some` restricts the prelude effect row.
- `policies: Arc<pattern_core::PolicySet>` — composed from
  `rust_defaults() ++ persona.policy_rules` at session open via
  `merge_policies`. Reads via `cx.user().policies()`.
- `permission_broker: Arc<PermissionBroker>` — per-session, no
  global singleton.
- `permission_bridge: Option<Arc<PermissionBridge>>` — wired in
  `open_with_agent_loop` (async context required for the spawn).
- `current_dispatch_origin: Arc<RwLock<Option<MessageOrigin>>>` —
  immediate-dispatcher origin slot. **Critical security invariant:**
  populated by `agent_loop::drive_step` per orchestrate iteration to
  `Author::Agent(self)` (NOT the activating turn's origin). Handlers
  read this for the broker's partner-bypass predicate. The
  distinction prevents an agent's autonomous activity from
  inheriting Partner authority on a Partner-activated turn — the
  classic "user typed a message, so the agent can now `rm -rf`
  without prompting" failure mode. Future direct-execution paths
  (admin REPL, audited sandboxed code) may explicitly override the
  slot to a Partner origin before invoking a handler.

Phase 2-3 spawn fields:

- `spawn_registry: Arc<SpawnRegistry>` — per-parent child tracking with
  semaphore-bounded ephemeral concurrency (default limit 8). Cancel-on-
  drop: all children cancelled when parent session ends.
- `tokio_handle: tokio::runtime::Handle` — explicit runtime handle for
  sync-to-async `block_on` in the spawn handler (and future sandbox-io
  PortRegistry). See `block_on` safety policy below.
- `include_paths: Arc<Vec<PathBuf>>` — GHC include paths inherited by
  child sessions. Extended with synthesized lib dirs for ephemerals.
- `sibling_resolver: Arc<dyn SiblingPersonaResolver>` — maps PersonaId
  to KDL path. Default: `UnconfiguredSiblingResolver` (all lookups fail).
  Phase 6 replaces with `pattern_db`-backed resolver.
- `drafts_dir: PathBuf` — root for draft persona KDL files. Default:
  `<XDG_DATA_HOME>/pattern/drafts`.
- `fork_registry: Arc<dyn ForkRegistry>` (Phase 3) — per-session fork
  handle tracking. Default: `InMemoryForkRegistry`. Accessed via
  `fork_registry()`. Forks are session-scoped; when the session drops
  the registry drops and all outstanding handles are discarded.
- `memory_cache: Option<Arc<MemoryCache>>` (Phase 3) — the session's
  live `MemoryCache`. Wired by daemon callers via `with_memory_cache()`.
  Required for fork dispatch (`handle_fork` fails with an informative
  error if missing). `fork_for_ephemeral` clones the Arc into child
  contexts.
- `mount_info: Option<MountInfo>` (Phase 3) — mount-level metadata
  (repo_root, workspace_root, StorageMode, jj_enabled). Required for
  persistent forks; `None` means persistent forks fail with
  `ForkError::PersistentNotAvailable`, lightweight forks proceed
  against the in-memory cache only.

New builders: `with_sibling_resolver`, `with_drafts_dir`,
`with_memory_cache`, `with_mount_info`, `with_fork_registry`. New
method: `fork_for_ephemeral(&self)` (constructs child `SessionContext`).
Test-only: `replace_spawn_registry_for_test(usize)`.

New traits:

- `HasPolicySet { fn policies() -> &PolicySet }` — implemented for
  `SessionContext` and `()` (empty set, Allow-everything).
- `HasPermissionBridge { fn permission_bridge(); fn current_dispatch_origin(); fn dispatch_agent_id() }`
  — `dispatch_agent_id` is **load-bearing for per-agent isolation**:
  the broker's `scope_cache` is keyed `(agent_id, scope)`, so
  hardcoding a synthetic id would silently collapse two agents'
  grants. The `()` shim returns `None`; handlers fail closed on
  missing identity (return `PERMISSION_DENIED_PREFIX` rather than
  proceeding without attribution).
- `HasSpawnRegistry { fn spawn_registry() -> &Arc<SpawnRegistry> }` —
  `SessionContext` exposes the live registry; `()` shim returns a
  zero-limit registry (no accidental spawns in unit tests).

### `agent_loop::drive_step` dispatch-origin discipline

`CurrentDispatchOriginGuard` (RAII) sets
`ctx.current_dispatch_origin = Some(MessageOrigin::new(Author::Agent { agent_id }, sphere))`
at the top of each orchestrate iteration; clears on Drop (panic-safe).
The same `dispatch_origin` value is reused for the existing
`output_origin` persistence in the same iteration, so handlers and
persistence see identical attribution.

### Shell + File handler gating

Both gated handlers (`sdk/handlers/shell.rs` for `Pattern.Shell.Execute`,
`sdk/handlers/file.rs` for `Pattern.File.Write`) share the shape:

1. Read `cx.user().policies()` and evaluate.
2. On `Deny`, return `PERMISSION_DENIED_PREFIX`-marked `EffectError::Handler`.
3. On `RequireApproval`, escalate via `permission_bridge().request_sync(...)`.
   On grant, return `GATE_APPROVED_PREFIX`-marked stub. On denial /
   timeout, return `PERMISSION_DENIED_PREFIX`-marked stub.
4. On `Allow`, return the existing "not implemented" stub error
   without `GateApproved` marker (lets tests discriminate gate-skip
   from gate-fire-then-allow).

The File handler additionally short-circuits config-KDL writes via
`is_pattern_config_kdl` **before** consulting `PolicySet` — the
locked invariant is structural: no rule of any precedence can loosen
it because the policy is never consulted on that path.

### Persona KDL: `capabilities {}` and `policy {}` blocks

`persona_loader` parses two new top-level blocks:

```kdl
capabilities {
    effects { memory; message; tasks }
    flags { spawn-new-identities }
}

policy {
    rule "allow-git-push" effect="shell" action="allow" {
        matcher "shell-command" pattern="git push*"
    }
    rule "gate-all-file-writes" effect="file" action="require-approval" {
        matcher "file-path" pattern="*"
        reason "all file writes gated for this persona"
    }
}
```

Decoded into `PersonaSnapshot.capabilities` (`Option<CapabilitySet>`)
and `PersonaSnapshot.policy_rules` (`Vec<PolicyRule>` with
`Precedence::KdlConfig`). `merge_policies(persona)` layers the rules
over `rust_defaults()` at session open.

## Spawn infrastructure (v3-multi-agent Phases 2-3)

### `spawn` module

Child session lifecycle: registry, ephemeral runner, sibling resolver,
draft writer, fork lifecycle, fork registry. Module layout:

- `spawn::registry` — `SpawnRegistry` (tokio `Semaphore`-bounded,
  `parking_lot::Mutex`-guarded, cancel-on-drop via `Drop` impl).
  `ChildSessionHandle` holds `cancel_state`, `Shared<BoxFuture<Result<
  SpawnResult, SpawnError>>>`, and optional `OwnedSemaphorePermit`.
  Methods: `try_acquire_ephemeral_slot`, `register`, `wait_for(id)`
  (async), `cancel_one(id)`, `cancel_all`, `install_watcher`.
  `SpawnKind { Ephemeral, Fork, Sibling }`, `TerminationReason
  { EndTurn, ToolUse, MaxTurns, Timeout, Cancelled, Error }`,
  `SpawnResult { child_id, final_text, turns, terminated,
  progress_log_label }` (`#[non_exhaustive]`, `SpawnResult::new`
  constructor).
- `spawn::ephemeral` — `run_ephemeral` (drives child's `drive_step`
  inside `tokio::time::timeout`), `fork_for_ephemeral` (method on
  `SessionContext`, constructs child context with inherited state),
  `synthesize_program_lib` (writes `lib/Pattern/SpawnHelpers.hs` to a
  `tempfile::TempDir`), `compute_child_caps` (intersection via
  `restrict_to`; escalation is `SpawnError::CapabilityEscalation`),
  `child_include_paths`, `MAX_EPHEMERAL_TURNS = 32`,
  `create_progress_log_block`, `build_progress_log_observer`.
- `spawn::sibling` — `SiblingPersonaResolver` trait (seam for Phase 6
  `pattern_db`-backed resolver). `UnconfiguredSiblingResolver` (prod
  default; all lookups fail) + `StubSiblingResolver` (test, `HashMap`).
  `spawn_sibling_existing` — resolves persona, loads KDL snapshot,
  returns `SiblingExistingOutcome { persona_id, capabilities }` (caps
  from the sibling's own KDL, NOT inherited from parent). Siblings are
  NOT registered in the parent's `SpawnRegistry`.
  `spawn_sibling_new` — writes draft KDL via `RuntimeConfigWriter`,
  returns `SiblingNewOutcome { persona_id, status, kdl_path }`. Status
  is `Active` (parent holds `SpawnNewIdentities`) or `Draft` (pending
  human promote). Both emit `tracing::info!` with
  `source = "runtime.spawn.sibling"`.
- `spawn::draft` — `RuntimeConfigWriter` writes draft persona KDL to
  `drafts_dir/<id>.kdl`. Creates directories lazily. Writes bypass the
  `Pattern.File` handler policy gate (runtime-authorised bookkeeping).
- `spawn::fork` — `ForkHandle`, `ForkIsolationState { Resolved |
  Lightweight | Persistent }`, `ForkError` (16 variants, `#[non_exhaustive]`),
  `WireForkHandle` (`ToCore` derive). `check_promote_capability` gates
  promote on `SpawnNewIdentities`. Resolution helpers:
  `merge_back_lightweight` (CRDT import via `LoroDoc::export_snapshot` +
  `apply_updates`), `merge_back_persistent` (jj merge commit via
  `jj new <workspace>@- @` + loro snapshot import), `discard` (consumes
  handle; lightweight = cancel + drop; persistent = cancel + best-effort
  `workspace_forget` + `bookmark_delete`), `promote` (consumes handle;
  writes draft KDL + seed cache to `<drafts_dir>/<persona_id>.cache/`).
  `Drop` impl aborts the `cancel_watcher` `JoinHandle` on all resolution
  paths (explicit resolution methods `take()` the watcher first; Drop
  is a cheap no-op after them; bare-drop aborts to prevent parked-task leak).
  Persistent forks intentionally do NOT run jj cleanup in Drop (requires
  async I/O); callers must call `discard` explicitly.
- `spawn::fork_registry` — `ForkRegistry` trait + `InMemoryForkRegistry`.
  Per-session tracking of outstanding `ForkHandle`s by id. `insert`,
  `get` (returns `Arc<Mutex<ForkHandle>>`), `remove` (returns
  `Option<Option<ForkHandle>>` — outer None = unknown id, inner None =
  Arc still shared; entry is re-inserted on contention so retry works),
  `list_ids`. Phase 6 swaps in a DB-backed implementation.
- `spawn::merge` — `MergeReport { blocks_merged: u32 }`. Returned by
  both `merge_back_lightweight` and `merge_back_persistent`.

### `CancelState` Notify-based waiting (Phase 2)

`CancelState` gained a `notify: tokio::sync::Notify` field.
`request_cancel()` flips the atomic AND calls `notify.notify_waiters()`.
`wait_for_cancel()` is an `async fn` that parks on `notify.notified()`
instead of polling every 50 ms. This eliminates watcher-task leaks in
long-lived parents: the watcher wakes exactly once and completes.

Watcher tasks spawned by `fork_for_ephemeral` capture
`Weak<SpawnRegistry>` to break the Arc cycle that previously prevented
`Drop::abort()` from firing. The registry's `Drop` impl aborts the
watcher task via the stored `JoinHandle`.

### `Pattern.Spawn` wire grammar (Phase 2-3)

7 GADT variants: `Ephemeral | AwaitSpawn | AwaitAll | Fork | Sibling |
Stop | ForkOp`. Typed records for return values in
`sdk::requests::spawn`: `WireEphemeralSpawn`, `WireSpawnResult`,
`WireSpawnAwaitOutcome` (sum: `Ok(WireSpawnResult) | Fail(String)`),
`WireForkHandle`, `WireSiblingSpawn` (sum:
`ExistingActive | NewActive | NewDraft`), `WireForkOpKind` (sum:
`MergeBack | Discard | Promote(WirePersonaConfig)`), `WireForkOpResult`
(sum: `Unit | MergeReport(String) | PersonaId(String)`). No
JSON-over-string — typed Core values via `FromCore` (incoming) + `ToCore`
(outgoing). Wire types in `sdk/requests/spawn.rs`; Haskell counterpart
in `haskell/Pattern/Spawn.hs`.

Haskell helpers for fork resolution: `mergeBack` (non-consuming;
handle stays in registry), `discardFork` (consuming), `promoteFork`
(consuming; requires `SpawnNewIdentities`). No `AwaitResult` for
forks — they are memory snapshots, not running sessions.

### Spawn handler (`sdk/handlers/spawn.rs`)

Tightened to `EffectHandler<SessionContext>` (was generic
`<U: HasCancelState>`). Uses `cx.user().tokio_handle().block_on(
registry.wait_for(...))` for sync-to-async glue from the eval-worker
thread. The await target is bounded by `tokio::time::timeout` on the
child's `run_ephemeral` future — no plugin code in the await path.

Phase 3 additions: `handle_fork` dispatches lightweight and persistent
paths via `parent.memory_cache()` + `parent.mount_info()`. Spawns a
parent-to-child cancel-propagation watcher (holds `Weak<CancelState>`
for the child to break reference cycles). Inserts the handle into
`parent.fork_registry()`. `handle_fork_persistent` sequence: verify
mount + jj available, compute bookmark name via
`fork_bookmark_name(agent, task_ref)`, pre-check bookmark collision,
`workspace_add` + `bookmark_set` (rollback on failure), fork parent
cache (rollback workspace + bookmark on failure).

`handle_fork_op` dispatches `WireForkOpKind`:
- `MergeBack` — non-consuming (`registry.get`); dispatches to
  `merge_back_lightweight` or `merge_back_persistent` based on
  isolation mode; returns `WireForkOpResult::MergeReport`.
- `Discard` — consuming (`registry.remove`); calls `handle.discard()`;
  returns `WireForkOpResult::Unit`.
- `Promote` — consuming (`registry.remove`); calls
  `handle.promote(cfg, drafts_dir)`; returns
  `WireForkOpResult::PersonaId`.

### `tokio_handle` threading and `block_on` safety

`TidepoolRuntime::new` and `with_default_sdk` take
`tokio_handle: tokio::runtime::Handle` as an explicit parameter
(preempted from sandbox-io Phase 3 Task 5). Stored on both
`TidepoolRuntime` and `SessionContext`. First consumer: the spawn
handler's `block_on` path.

**Decision rule** (when to use `block_on` vs. a bridge):
- `block_on` is safe when: the await path is bounded by enforced
  time/memory limits, no plugin-provided code in the await path, and
  blocking matches the semantic contract.
- A bridge is required when: network calls lack a top-level timeout,
  plugin code could appear in the await path, or the sync wait would
  prevent necessary concurrent work.
- `PermissionBridge` is a candidate to migrate to `block_on` later
  (broker is bounded, no plugin code). `RouterBridge` stays as a
  bridge — routers may dispatch to plugin-provided endpoints.

See the memory note at
`~/.claude/projects/.../memory/project_eval_worker_block_on_safety.md`
for the full rationale and migration-state-of-the-world.

### `TidepoolRuntime` constructor change

Both `TidepoolRuntime::new(...)` and `with_default_sdk(...)` now require
a `tokio_handle: tokio::runtime::Handle` parameter. All call sites
(pattern_server, tests) updated. The sandbox-io plan inherits this
threading.

## Wake system (v3-multi-agent Phase 4)

### `Pattern.Wake` effect

Four wake condition types implemented in `sdk/requests/wake.rs` and
`sdk/handlers/wake.rs`:

- **Interval** — fires every `period_ms` milliseconds. Backed by
  `tokio::time::interval` in `wake::rust_primitives`.
- **TaskDep** — fires when a task block item transitions to a terminal
  status. Backed by `wake::task_dep::TaskDepCondition`, which polls
  `MemoryStore::get_block` on a configurable period.
- **BlockChanged** — fires when any block matching a label/scope changes.
  Backed by `wake::block_changed::BlockChangedCondition`, which hooks into
  `pattern_memory::subscriber::BlockChangeNotifier`.
- **Custom** — caller-defined predicate evaluated against a memory snapshot.
  Backed by a parked evaluator in `WakeRegistry`.

`WAKE_REGISTRY_MISSING_PREFIX: &str = "WakeRegistryMissing: "` is the
error prefix returned when `Pattern.Wake.Register` is invoked but no
`WakeRegistry` is wired (e.g. in test sessions that don't wire it).
Tests can check for this prefix to distinguish missing-registry errors
from capability-denied errors.

### `WakeRegistry` (`wake/registry.rs`)

Per-session registry. Manages a `DashMap` of `WakeHandle`s keyed by
`WakeId` (UUID-based). Each handle wraps an `Arc<dyn WakeCondition>` plus
a tokio `JoinHandle` for the evaluator task. `register(condition)` spawns
the evaluator and inserts the handle. `unregister(id)` aborts the evaluator.
Registry drop aborts all outstanding evaluators.

`WakeRegistry` requires:
- `tokio_handle: Handle` — to spawn evaluator tasks from the sync handler context.
- `mailbox_tx: UnboundedSender<MailboxInput>` — the session's own mailbox,
  so activated wake conditions can deliver a wake-up message.
- Optional `BlockChangeNotifier` (for BlockChanged conditions).
- Optional `Arc<dyn MemoryStore>` (for TaskDep and Custom conditions).

### `AgentRegistry` — single-map consolidation (cycle-3 TOCTOU fix)

`AgentRegistry` uses a single `DashMap<PersonaId, AgentSlot>` where `AgentSlot` is
a sum type (`Active { tx }` | `Draft { queue: Mutex<VecDeque<MailboxInput>> }`).
Reads use `DashMap::get()`'s `Ref` (shard read lock held for the `Ref`'s lifetime);
writes use `DashMap::insert()` (shard write lock). `insert` cannot acquire the
write lock while any reader holds a `Ref` on the same shard, so the status check
and dispatch run under a stable view.

This closes the TOCTOU race that existed in the cycle-1/cycle-2 two-map design
(`entries: DashMap` + `draft_queues: DashMap`), where a sender could observe
`Draft` status, the promoter could complete (swap entry + drain + remove queue),
and the sender would then find a removed queue and silently drop the message.
The cycle-2 reorder narrowed but did not close the race (~1 loss per 6M sends
remained, confirmed by the heavy probe at `tests/probe_consolidation.rs`).

With the single-map design:
- `route_or_queue` holds the entry guard for the full status-check + queue-push
  (Draft path) or status-check + tx-clone (Active path). No window exists for the
  promoter to remove the slot between the check and the push.
- `register_active` swaps the slot to `Active` via `DashMap::insert`, then drains
  the previous Draft queue. The queue is uniquely owned after the swap; no
  concurrent push is possible because any sender that sees the new `Active` slot
  sends directly to `tx`, and any sender that held a Draft entry guard before the
  swap will push into the queue that is now being drained.
- Zero message loss and zero `PersonaNotFound` errors verified by
  `tests/probe_consolidation.rs` (64×500×200 sends, 5-yield promoter, 8 runs).

`route_or_queue` is the preferred routing entry point. `queue_for_draft` is
retained as a lower-level method for callers that have already confirmed Draft
status outside this function.

### Session wiring: `SessionRegistries` and `WakeRegistryExtras`

`open_with_agent_loop` now accepts `registries: Option<SessionRegistries>`.

```rust
pub struct SessionRegistries {
    pub agent_registry: Option<Arc<AgentRegistry>>,
    pub router_registry: Option<Arc<RouterRegistry>>,
    pub wake_registry_extras: Option<WakeRegistryExtras>,
}
pub struct WakeRegistryExtras {
    pub block_change_notifier: Option<BlockChangeNotifier>,
    pub memory_store: Option<Arc<dyn MemoryStore>>,
}
```

When `Some(registries)` is passed:
- `agent_registry` → `ctx.with_agent_registry(...)` (session registers itself
  as Active on open, unregisters on drop via `RegistryGuard`).
- `router_registry` → `ctx.with_router(registry)` (after wiring `AgentRouter`
  into the registry).
- `wake_registry_extras` → `WakeRegistry` is built from the session's own
  mailbox sender + the extras, then wired into `SessionContext`.

All existing callers pass `None`; only `pattern_server::get_or_open_session`
passes `Some(...)`.

### Daemon wiring (`pattern_server/src/server.rs`)

`ProjectMount` gains `agent_registry: Arc<AgentRegistry>` (shared across all
sessions for the same project). `ProjectMount.cache` is stored as
`Arc<MemoryCache>` (narrowed from `Arc<dyn MemoryStore>`) to expose
`block_change_notifier()`.

`get_or_open_session` builds per-session `RouterRegistry` + `SessionRegistries`
and passes them with `CapabilitySet::all()` (fail-closed: no partial capability
grants from daemon sessions). The `WakeRegistry` is built inside
`open_with_agent_loop` (needs the session's mailbox sender).

## Shell subsystem (Phase 3 Tasks 1-9)

### Architecture overview

The shell subsystem is layered: `LocalPtyBackend` → `ProcessManager` →
`ShellHandler`. Each layer is independently testable.

- **`LocalPtyBackend`** (`process_manager/local_pty.rs`) — sync PTY driver.
  Allocates a pty pair, forks a shell (`$SHELL` → `/bin/bash` fallback),
  writes command strings, reads until `PROMPT_MARKER` (injected via
  `PROMPT_COMMAND`), strips ANSI, returns trimmed output. Stateful: the
  backend owns the shell process for the lifetime of the session and
  environment is preserved between `execute()` calls.

- **`ProcessManager`** (`process_manager/manager.rs`) — per-session wrapper.
  Owns one `LocalPtyBackend` for interactive shell execution plus a
  `ProcessLogger` for process-log persistence. `spawn()` forks background
  tasks via `std::thread::spawn` with a bounded output queue; `execute()`
  forwards synchronously to the backend. `kill()` / `status()` manage the
  background task registry. Every `SessionContext` owns exactly one
  `ProcessManager` — no runtime-global singleton.

- **`ProcessLogger`** (`process_manager/logger.rs`) — append-only log of
  completed shell executions. Each entry records timestamp, command,
  output, exit status, and duration. Persists to a `process_log.ndjson`
  file in the session's cache dir.

- **`ShellHandler`** (`sdk/handlers/shell.rs`) — maps `ShellReq` variants
  to `ProcessManager` calls. Handles `Execute`, `Spawn`, `Kill`, `Status`,
  `Cwd`, and `Env`. Enforces the Phase 1 policy gate (Allow / Deny /
  RequireApproval) before delegating. Pushes `ShellOutput` attachments
  (output chunks, exit events, kill events) to the session's
  `SystemCommunicationsQueue` for asynchronous delivery to agents.

### `ShellOutput` attachments

Background spawns stream output via `MessageAttachment::ShellOutput`
pushed to `SessionContext.system_comms_queue`. The bridge thread
(`spawn_output_bridge`) runs on `std::thread::spawn` (not a tokio task)
and drains the pty output queue, pushing attachments until an `Exit` or
`Killed` terminal event is observed. Tests poll the queue with
`wait_for_queue` / `drain_shell_outputs` helpers (condition-based, no
arbitrary `sleep`).

### `SessionContext.with_process_manager`

Builder method added for test fixture control:

```rust
ctx.with_process_manager(Arc::new(ProcessManager::new(cwd, cache_dir)))
```

Replaces the default manager (constructed at session open with the
persona's cache dir) with an injected one. Only needed in integration tests
that need to inspect the process log path or inject a controlled cache dir.

### Kill handler

`ShellReq::Kill(TaskId)` takes the opaque handle string returned by `Spawn`'s
JSON response (`{"task_id":"...","pid":N}`). Recycle-safe: lookup goes
through the running map, the actual SIGTERM dispatch uses the reader thread's
owned `Child` handle. PID recycling cannot misroute kills to unrelated
processes.

The integration test `kill_via_handler_terminates_running_process` (AC3.4)
exercises the full honest path: Spawn → parse task_id from JSON → Kill(task_id)
→ Status confirms removal.

### AC3 integration tests (`tests/shell_handler.rs`)

Eighteen handler-level integration tests covering AC3.1–AC3.10 plus
capability-denial and policy-gate paths (including a wired-broker test
that observes the `ToolExecution` scope shape):

| Test | AC | What it verifies |
|------|----|-----------------|
| `execute_via_handler_returns_output_and_exit_code` | AC3.1 | Execute dispatches and returns JSON ExecuteResult |
| `execute_via_handler_persists_session_state` | AC3.2 | cd then pwd; same handler/context |
| `spawn_streams_output_via_attachments` | AC3.3 | Background spawn pushes ShellOutput attachments |
| `kill_via_handler_terminates_running_process` | AC3.4 | Spawn → Kill(task_id) via handler → Status confirms removal |
| `status_via_handler_lists_running_tasks` | AC3.5 | Status returns both task IDs from two Spawns |
| `cwd_persists_across_handler_executions` | AC3.6 | pm.cwd() reflects `cd /tmp` after Execute |
| `execute_via_handler_timeout_kills_and_surfaces_error` | AC3.7 | Timeout → Err; session recovers |
| `kill_unknown_task_via_handler_returns_error` | AC3.8 | Kill(bogus) → Err with "not found" |
| `exit_marker_resists_command_output_injection` | AC3.9 | Spurious marker in output; exit_code correct |
| `spawn_output_logged_to_file` | AC3.10 | ProcessLogger writes OUT/EXIT lines to ndjson |
| `execute_via_handler_denied_without_shell_capability` | cap | Restricted caps → PERMISSION_DENIED_PREFIX |
| `spawn_via_handler_denied_without_shell_capability` | cap | Restricted caps deny Spawn |
| `kill_via_handler_denied_without_shell_capability` | cap | Restricted caps deny Kill |
| `status_via_handler_denied_without_shell_capability` | cap | Restricted caps deny Status |
| `execute_via_handler_denies_when_policy_denies` | policy | Deny rule → PERMISSION_DENIED_PREFIX before PM |
| `execute_via_handler_escalates_to_broker_on_require_approval` | policy | rm -rf* rule → broker consulted |
| `spawn_via_handler_also_gates_on_policy` | policy | Deny rule also fires on Spawn |

Tests use `#[tokio::test]` for context construction (async DB) then invoke
the handler synchronously. `tidepool_testing::gen::standard_datacon_table()`
provides the Haskell constructor table (NOT `pattern_runtime::testing`,
which is `#[cfg(test)]`-gated and unavailable from integration test files).

## File subsystem (v3-sandbox-io Phase 2)

### Architecture overview

The file subsystem is layered: `LoroSyncedFile` (CRDT model from
`pattern_memory::loro_sync`) → `FileManager` → `FileHandler`.

- **`FileManager`** (`file_manager/manager.rs`) — per-session file lifecycle
  manager. Owns a pooled `DirWatcher` (shared with other open files in the
  same session), per-file `LoroSyncedFile` handles, and file-state tracking
  (open, watched, closed). `open()` starts CRDT sync for a file;
  `watch()` subscribes to external edits; `close()` tears down both.
  Pushes `MessageAttachment::FileEdit` reminders to the session's
  `async_reminder_queue` for delivery at the next turn boundary.

- **`FilePolicy`** (`file_manager/policy.rs`) — default-deny access control
  for file operations. Loaded from `.pattern.kdl`'s `file_policy {}` block
  via `FilePolicySection` (in `pattern_memory::config`). Last-match-wins
  rule evaluation via `check_access(path)`. `FilePolicy::from_section()`
  converts the config representation; `FilePolicy::from_rules()` accepts
  pre-built rules for test fixtures.

- **`FileHandler`** (`sdk/handlers/file.rs`) — maps `FileReq` variants
  (`Read`, `Write`, `Open`, `Watch`, `Close`, `List`) to `FileManager`
  calls. The config-KDL shape guard fires before policy evaluation on
  `Write`. `FilePolicy` deny fires before the permission broker.

### `SessionContext.async_reminder_queue`

A `Mutex<Vec<MessageAttachment>>` shared between the `FileManager`'s
background watcher bridge and the agent loop. Background threads push
`FileEdit` / `FileConflict` attachments; `drive_step` drains them at
the start of each orchestrate iteration and splices them onto the
current turn's messages.

### `HasFileManager` trait

`HasFileManager { fn file_manager() -> Option<&Arc<FileManager>> }` —
implemented for `SessionContext` (returns the wired manager) and `()`
(returns `None`). Handlers call this; the `()` shim provides a
closed-by-default path for test doubles without a file subsystem.

## Port subsystem (v3-sandbox-io Phases 4-5)

### Architecture overview

The unified Port subsystem replaces the retired `Sources` and `Rpc` effects.
Three layers: `Port` trait (in `pattern_core`) → `PortRegistryImpl` + dispatcher
actor (in this crate) → `PortHandler` (SDK handler).

- **`Port` trait** (`pattern_core::traits::port`) — one `id()`, `metadata()`,
  `capabilities()`, `call()`, `subscribe()`, `unsubscribe()`, plus
  `library()` for optional Haskell wrapper code spliced into the agent's
  prelude at session open.

- **`PortRegistryImpl`** (`port_registry/registry.rs`) — concrete
  `PortRegistry` impl. Owns a `DashMap<PortId, Arc<dyn Port>>` of
  registered ports plus a tokio-spawned dispatcher actor
  (`port_registry/dispatcher.rs`) that serialises `call()` and
  `subscribe()` invocations. The dispatcher uses `mpsc` channels;
  handlers send requests via `blocking_send` and wait via
  `recv_timeout` (sync-safe from the eval worker thread).

- **`PortRegistryImpl::with_runtime_ports(handle)`** — factory that
  constructs a registry pre-loaded with runtime-provided ports (currently
  `HttpPort`). Both `TidepoolRuntime::new` and `pattern_server::main` build
  the registry through this helper so `HttpPort` is always registered.

- **`HttpPort`** (`ports/http.rs`) — first concrete Port impl. Methods:
  `get`, `post`, `put`, `patch`, `delete`, `head`, `options`. Backed by
  a `reqwest::Client` with connection pooling.

- **`PortHandler`** (`sdk/handlers/port.rs`) — maps `PortReq` variants
  (`List`, `Call`, `Subscribe`, `Unsubscribe`) to dispatcher messages.
  Per-port capability gating via `CapabilitySet::has_port(port_id)`.

### Port library materialization (Phase 5)

Ports can ship Haskell wrapper code via `Port::library()`. At session
open, `open_with_agent_loop` materializes each registered port's library
into a per-session tempdir. The tempdir path is added to the eval
worker's include path so agents can `import qualified Pattern.Http as
Http` (or any other port library). The source for runtime-provided port
libraries lives at `crates/pattern_runtime/haskell/ports/` (NOT in the
SDK include tree). `SessionContext._port_lib_tempdir` holds the tempdir
handle to keep it alive for the session's lifetime.

### `PortEvent` attachment streaming

Ports that support subscriptions push `PortEvent`s on a `BoxStream`.
The dispatcher bridges these onto `SessionContext.async_reminder_queue`
as `MessageAttachment::PortEvent` attachments. Rendering goes through
`pattern_provider::compose::render::render_port_event_attachment`.

## `open_with_agent_loop` signature (current)

```rust
pub async fn open_with_agent_loop(
    persona: PersonaSnapshot,
    sdk: &SdkLocation,
    memory_store: Arc<dyn MemoryStore>,
    provider: Arc<dyn ProviderClient>,
    db: Arc<ConstellationDb>,
    turn_sink: Arc<dyn TurnSink>,
    prelude_dir: Option<PathBuf>,
    mount_path: Option<PathBuf>,
    capabilities: Option<CapabilitySet>,
    port_registry: Arc<PortRegistryImpl>,
    file_policy: Option<FilePolicy>,
) -> Result<Self, RuntimeError>
```

The `port_registry` and `file_policy` parameters were added in
v3-sandbox-io. `file_policy`, when `Some`, causes a `FileManager` to be
constructed and wired into the `SessionContext` before the eval worker
spawns. Port library materialization and dispatcher start-up also happen
in this function.

## End-to-end sandbox-io smoke test (`tests/sandbox_io_smoke.rs`)

Drives the real session machinery via scripted `MockProvider.tool_use_turn`
calls: File.Read, File.Write (policy-allowed path), Shell.Execute,
Port.List, Port.Call (HttpPort). Validates the full handler → manager →
backend chain without needing `tidepool-extract` on PATH (mock provider
injects tool_use turns directly). Runs as a single `#[tokio::test]`.
