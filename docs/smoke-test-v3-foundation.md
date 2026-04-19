# v3 foundation smoke test

Manual end-to-end verification procedure for the v3 foundation rewrite
(design plan: `docs/design-plans/2026-04-16-v3-foundation.md`, AC9.*).

The canonical step-by-step procedure lives in
`crates/pattern_runtime/CLAUDE.md` under "Smoke-test procedure (v3
foundation AC9.*)". This document is a cover sheet that adds:
prerequisites with troubleshooting, cache-metric interpretation,
failure diagnosis, a completion checklist, and known gaps.

Last verified: 2026-04-19

## Prerequisites

### tidepool-extract

The Haskell runtime binary must be reachable. Resolution order:

1. `$TIDEPOOL_EXTRACT` env var (absolute path).
2. `tidepool-extract` on `$PATH`.

**With nix (recommended):**

```sh
nix develop   # enters devshell, exports $TIDEPOOL_EXTRACT
which tidepool-extract   # should print /nix/store/...
```

If `tidepool-extract` points at a stale derivation (symptom: `CASE TRAP`
or `Jit(Yield(Undefined))` errors), see the "Stale-harness
troubleshooting" section in `crates/pattern_runtime/CLAUDE.md`.

**Without nix:**

Clone and build from
`https://github.com/tidepool-heavy-industries/tidepool` (GHC 9.12 +
Cabal). Export `TIDEPOOL_EXTRACT=/abs/path/to/tidepool-extract`.

### Credential selection

Pick one auth path:

| Path | Setup | AC |
|------|-------|----|
| API key | `export ANTHROPIC_API_KEY=sk-ant-...` | AC9.1 |
| Session pickup | Have an active claude-code session at `~/.claude/.credentials.json` | AC9.2 |
| PKCE (one-time) | Run `cargo run -p pattern-runtime --bin pattern-test-cli -- auth` | AC4.1 |

The `auth` subcommand prints which tier resolved. Use this to verify
your environment before running the full flow.

### Build

```sh
cargo build -p pattern-runtime --bin pattern-test-cli
```

### Automated test gate

```sh
cargo nextest run --workspace
```

All 677 tests must pass before manual smoke testing. If any fail,
diagnose before proceeding -- a broken automated suite invalidates the
manual procedure.

## Procedure

Follow steps 1-8 from `crates/pattern_runtime/CLAUDE.md` "DoD flow"
section. The summary below is for quick reference; the CLAUDE.md
version is authoritative.

### Step 1: fresh session

```sh
TMPDIR=$(mktemp -d)
cargo run -p pattern-runtime --bin pattern-test-cli -- \
    spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --data-dir "$TMPDIR"
```

Add `--auth api-key | session-pickup | pkce` to force a tier.

### Step 2: chat

Type: `hello; what's your role?`

Expected: response consistent with the smoke persona. A cache-metrics
line prints: `[cache: fresh=N read=N create=N ratio=NN%]`.

### Step 3: write a memory block

Type: `please remember in your scratchpad: favorite color is teal.`

Expected: agent confirms the write.

### Step 4: exit and re-spawn

`:q` or Ctrl+D. Re-run the same `spawn` command with the same
`--data-dir`.

### Step 5: recall

Type: `what's my favorite color?`

Expected: `teal` in the response (memory persisted across restart).

### Step 6: capture pre-edit metrics

Type: `ok, thanks`. Record the `read` and `ratio` values.

### Step 7: edit block and confirm cache preservation

```
pattern> :edit-block scratchpad favorite color is actually indigo
```

Then type: `confirm the update`.

Expected: agent references `indigo`.

### Step 8: exit

`:q`. Session shuts down cleanly.

## Cache-metric interpretation

The `[cache: fresh=N read=N create=N ratio=NN%]` line appears after
every turn. The fields:

| Field | Meaning |
|-------|---------|
| `fresh` | Input tokens not covered by any cached segment |
| `read` | Tokens served from an existing cache hit |
| `create` | Tokens written to cache for the first time this turn |
| `ratio` | `read / (read + create + fresh)` as a percentage |

### Expected behavior per step

| Step | Expected pattern |
|------|-----------------|
| 2 (first chat) | `ratio` low or 0% (nothing cached yet); `create` high |
| 3 (write block) | `ratio` rises (system prompt cached from step 2) |
| 5 (post-restart recall) | `ratio` moderate; segments rebuild from DB state |
| 6 (pre-edit baseline) | Note `ratio` and `read` values for comparison |
| 7 (post-edit confirm) | `ratio` should be within 5% of step 6 value (AC8.1: segment 1 preserved). `create` spikes (AC8.2: segment 3 invalidated by block edit) |

### Tolerances

- **AC8.1 (segment 1 preserved):** `ratio` drop from step 6 to step 7
  must be <= 5 percentage points. A larger drop indicates segment 1 was
  unexpectedly invalidated.

- **AC8.2 (segment 3 bust on edit):** `create` in step 7 should spike
  compared to step 6. This is expected -- the edited block content
  requires fresh caching.

- If `ratio` collapses dramatically (e.g. from 60% to 5%), inspect the
  `tracing::warn!` break-detection output. The composer's
  `BreakDetectionSnapshot` diff identifies which subsystem changed.

## Failure diagnosis

| Symptom | Check |
|---------|-------|
| Session fails to open | `preflight::check()` -- is `tidepool-extract` reachable? See `tests/error_clarity.rs::ac9_5_session_open_bad_sdk_path_returns_sdk_not_found` |
| Auth fails | Run `pattern-test-cli auth` to see which tier resolves. See `tests/error_clarity.rs::ac9_5_auth_no_api_key_returns_no_auth_available` |
| Persona TOML fails | Error message should name the failing field. See `tests/error_clarity.rs::ac9_5_persona_*` tests |
| Memory not found after restart | Verify `--data-dir` matches between runs. Check `constellation.db` exists |
| `ratio` collapses on block edit | Inspect `tracing::warn!` break-detection output. Diff composed requests for segment-1 differences |
| Unclear error at any step | This is an AC9.5 regression. File an issue before debugging further |

## What this test does NOT exercise

- Cross-provider routing (foundation is single-provider per session).
- Constellation / multi-agent paths (foundation is single-agent).
- UX polish (pattern-test-cli is a throwaway driver).
- Compaction under load (exercised by automated `tests/compaction.rs`).
- Concurrent CRDT merges (depends on loro's own guarantees; AC6.6).
- Live-credential CI (intentionally manual; see rationale in
  `crates/pattern_runtime/CLAUDE.md`).

## Completion checklist

Tick each box after completing the step with acceptable results. Record
the cache metrics in the "value" column for the post-mortem record.

- [ ] **Prerequisite:** `cargo nextest run --workspace` passes (677/677)
- [ ] **Prerequisite:** `tidepool-extract` reachable (`which tidepool-extract` prints a path)
- [ ] **Prerequisite:** Auth tier confirmed (`pattern-test-cli auth` prints the expected tier)
- [ ] **Step 1:** Session opens without error
- [ ] **Step 2:** Agent responds coherently; cache-metrics line prints
  - `ratio` = _______ `read` = _______ `create` = _______
- [ ] **Step 3:** Agent confirms memory write
- [ ] **Step 4:** Re-spawn succeeds against same `--data-dir`
- [ ] **Step 5:** Agent recalls `teal` from persisted memory
- [ ] **Step 6:** Pre-edit baseline captured
  - `ratio` = _______ `read` = _______ `create` = _______
- [ ] **Step 7a:** `:edit-block` command succeeds
- [ ] **Step 7b:** Agent references `indigo` after edit
- [ ] **Step 7c:** `ratio` within 5% of step 6 (AC8.1)
  - `ratio` = _______ `read` = _______ `create` = _______
  - delta from step 6: _______
- [ ] **Step 7d:** `create` spiked compared to step 6 (AC8.2 seg3 bust)
- [ ] **Step 8:** Clean exit

**Tester:** _______________
**Date:** _______________
**Auth tier used:** _______________
**Notes:**

## Known gaps and follow-ups

### Medium/low confidence ACs needing better test coverage post-foundation

| AC | Current state | Follow-up |
|----|---------------|-----------|
| AC3.2 (atomic read) | Code uses `tokio::fs::read_to_string` (single syscall for small files); no dedicated concurrency test | Add a stress test simulating concurrent claude-code writes during pickup |
| AC4.1 (PKCE flow) | Unit tests cover config + refresh; full browser-callback flow is manual-only | Consider a headless-browser integration test for CI |
| AC6.4 (restart persistence) | Covered by `turn_history_restore` tests + `session_lifecycle::memory_round_trip_through_session`; uses in-memory store, not full DB round-trip with process restart | Add a subprocess-spawn test that exercises actual process restart |
| AC6.6 (concurrent CRDT merge) | No test; relies on loro's own guarantees | Add a concurrent-write test once multi-agent paths exist |
| AC8.1/8.2 (cache metrics) | Composer pipeline tests verify marker placement; actual cache-hit metrics require live Anthropic responses | Manual smoke test is the verification vehicle; consider wiremock response headers for simulated cache metrics |
| AC9.1/9.2 (e2e smoke) | Manual procedure only (by design) | No change needed; live-credential CI is a foot-gun |
| AC9.4 (cache preservation) | Same as AC8.1/8.2 | Same follow-up |

### Manual-only ACs that could be automated later

| AC | Blocker for automation |
|----|----------------------|
| AC9.1 (API-key smoke) | Requires live Anthropic credentials; rate-limit noise + cost |
| AC9.2 (OAuth smoke) | Requires active subscription session |
| AC9.4 (cache metrics) | Requires Anthropic cache-hit response headers |
| AC4.1 (PKCE callback) | Requires browser automation |

### Known flakes

The two previously-named flaky tests
(`open_step_twice_does_not_recompile` and
`hard_abandon_await_enforces_cancel_grace_ceiling`) were deleted when
the SessionMachine static-program path retired. The underlying
concurrent-`tidepool-extract` contention hypothesis may still apply to
surviving tests. See `crates/pattern_runtime/CLAUDE.md` "Known flakes"
section for investigation vectors.

**Before GA:** re-audit the full suite under parallel load
(`cargo nextest run --workspace` repeated 10x) to confirm no surviving
flakes.

### Under-exercised edges

- **Segment2Pass index-correspondence fragility:** the mapping between
  Pattern `Message`s and composed `ChatMessage`s depends on
  `summary_count` offset math. A future pass that reorders messages
  would silently break splice logic. Tracked for follow-up in
  `crates/pattern_runtime/CLAUDE.md` and
  `crates/pattern_provider/CLAUDE.md`.

- **`BreakDetectionSnapshot` gap between compose-time and wire-time:**
  Phase 5 added `message_markers_hash` and `compute_from_chat()` to
  close this gap, but the hash is advisory (warn-only), not enforced.

- **Compression pseudo-message interleaving:** the four compression
  strategies are tested individually via `tests/compaction.rs` but
  the interaction between pseudo-messages and compression-then-reload
  across a process restart is not exercised end-to-end.
