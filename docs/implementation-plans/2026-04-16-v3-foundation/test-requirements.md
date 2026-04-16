# Pattern v3 Foundation — Test Requirements

Generated from the AC list in `docs/design-plans/2026-04-16-v3-foundation.md` and the per-task "Verifies:" annotations in `phase_0{1..6}.md`. Every AC sub-case maps to either:

- **Automated**: a named test or script lives in the tree (unit, integration, doctest, snapshot, or build-gate).
- **Human**: manual verification via CLI checklist or spot-check; justification provided.

Pattern v3 foundation **deliberately does not use env-gated automated live tests** (`PATTERN_V3_LIVE_AUTH=1`-style gating). Live credential + network tests are either:
- Verified transitively by the AC9.1 / AC9.2 manual CLI checklist (which exercises the full stack end-to-end), OR
- Replaced with mocked automated tests using `wiremock` when the AC is specifically about code-path correctness rather than real-backend behaviour.

This keeps live-credential handling isolated to operator-driven manual runs; CI never requires credentials.

Planning decisions reflected throughout:

- `claude-code-literal` system-prompt slot[0] content (ShaperCompatMode) is **structural**, not an identity claim. Pattern's real identity lives in slots [1] and [2]. AC5.2 verification targets structural placement of pattern-specific persona content, not exclusion of the claude-code literal string.
- Subscription session-pickup tests are feature-gated manual. Phase 6 runs the full DoD flow through a documented CLI checklist rather than an autonomous CI job — deliberate divergence from AC9.1's literal "on CI" phrasing, intentional per planning (see Phase 6 architecture note).
- Streaming reaches consumers via `DisplayHandler` subscribers (Phase 3 Task 10); no separate streaming AC exists, but display plumbing is exercised indirectly by the hello-world and smoke tests.
- The intermediate-state audit script (`scripts/audit-rewrite-state.sh`) is the enforcement mechanism for AC1.7/1.8/1.9/1.10 across all phases.

All paths below are absolute-to-repo-root unless otherwise noted.

---

## AC1: pattern_core traits are defined, satisfiable, and documented (Phase 2)

### v3-foundation.AC1.1 — zero-warning `cargo check -p pattern_core`
- **Mode**: Automated (build gate).
- **Test**: `cargo check -p pattern_core` with warnings-as-error treatment; final verification at Phase 2 Task 22 and Task 26.
- **Path**: Invoked from `just pre-commit-all`; log captured to `/tmp/final-check.log` during Task 26.
- **Notes**: Single-gate; pre-existing warnings cleaned incrementally across Phase 2 staging tasks.

### v3-foundation.AC1.2 — `cargo doc -p pattern_core` complete, no warnings
- **Mode**: Automated (doc build + doctest).
- **Tests**:
  - `cargo doc -p pattern_core` (Phase 2 Task 23) — captured to `/tmp/final-doc.log`, fails on any `warning:`.
  - `cargo test --doc -p pattern_core` (Phase 2 Task 14, Task 20, Task 26) — doctests on every public type + error variant + trait.
- **Path**: Doctests live inline in `crates/pattern_core/src/types/**`, `src/error/**`, `src/traits/*.rs`.

### v3-foundation.AC1.3 — dummy trait impls compile
- **Mode**: Automated (doctest).
- **Test**: Trait-level doctest dummy-impl in each of `crates/pattern_core/src/traits/{agent_runtime,session,memory_store,provider_client,message_router,data_stream,source_manager}.rs` (Phase 2 Tasks 16–20).
- **Path**: `cargo test --doc -p pattern_core`.
- **Notes**: Every dummy `unimplemented!()` body carries an `AC1.3` marker to satisfy AC1.8 simultaneously.

### v3-foundation.AC1.4 — port-list doc exists with deferral notes
- **Mode**: Automated (file-existence check) + **Human** content review.
- **Tests**:
  - Existence: `test -f docs/plans/rewrite-v3-portlist.md` (Phase 1 Task 6, Phase 2 Task 26).
  - Content audit: human review that all 9 excluded crates are listed with fate + deferred-plan field; run at Phase 2 close.
- **Justification for partial human**: the presence of entries is checkable with `grep`, but the adequacy of the deferral note ("target-plan reference is the right one") requires a reviewer.

### v3-foundation.AC1.5 — removing a required method fails compile clearly
- **Mode**: Human spot-check (one-shot manual verification per phase close).
- **Procedure**: Phase 2 Task 20 Step 3 and Task 26 Step 2 — comment out one method in a dummy-impl doctest, run `cargo test --doc -p pattern_core`, confirm the error names the missing method, restore.
- **Justification**: The proof is a compile-time negative assertion; automating this would require a sibling crate dedicated to "should-not-compile" tests (e.g., `trybuild`). Planning declined to add that dependency for a single AC; the manual procedure is recorded in the phase-close commit message for auditability.

### v3-foundation.AC1.6 — referencing a retired crate fails workspace
- **Mode**: Human spot-check.
- **Procedure**: Phase 2 Task 26 Step 3 and Phase 4 Task 7 — add `pattern_auth = { path = "../pattern_auth" }` to `pattern_provider/Cargo.toml`, run `cargo check`, confirm workspace error, remove.
- **Justification**: Same negative-assertion rationale as AC1.5. Verified twice: once against a trait-only `pattern_core` and once after `pattern_auth` retirement commit.

### v3-foundation.AC1.7 — fate markers on in-flight code
- **Mode**: Automated (grep audit script).
- **Test**: `scripts/audit-rewrite-state.sh` (Phase 2 Task 25) scans `rewrite-staging/` and requires every file to carry a `// MOVING TO:`, `// REPLACED BY:`, or `// MOVING WITHIN CRATE:` header.
- **Path**: Runs at every phase close (Tasks 22–26 for phases 2–6).

### v3-foundation.AC1.8 — no unmarked `unimplemented!()` / `todo!()`
- **Mode**: Automated (audit script).
- **Test**: `scripts/audit-rewrite-state.sh` checks that every `unimplemented!()` / `todo!()` in workspace crates has a nearby `phase|AC` reference.

### v3-foundation.AC1.9 — dangling fate markers fail audit
- **Mode**: Automated (audit script).
- **Test**: `scripts/audit-rewrite-state.sh` ensures every fate marker is inside a comment attached to a real item.

### v3-foundation.AC1.10 — commented-out code blocks fail audit
- **Mode**: Automated (audit script).
- **Test**: `scripts/audit-rewrite-state.sh` flags `^\s*// (pub )?(fn|struct|enum|impl|use)` patterns.

---

## AC2: Tidepool runtime embedded with bounded execution (Phase 3)

Phase 3 Task 23 ships the per-AC test-mapping table; the entries below mirror it.

### v3-foundation.AC2.1 — hello-world Haskell program runs end-to-end
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/hello_world.rs::hello_world_runs_end_to_end` (Phase 3 Task 13), with fixture `tests/fixtures/hello.hs`.
- **Notes**: Skips gracefully if `tidepool-extract` is not on PATH; CI must install it (Phase 3 Task 4 provides `flake.nix` recipe).

### v3-foundation.AC2.2 — `ctx.time.now` effect round-trips
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/time_log_effects.rs::time_now_returns_current_epoch` (Phase 3 Task 20).

### v3-foundation.AC2.3 — `ctx.log` effect observable via tracing
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/time_log_effects.rs::log_info_observable_via_tracing` (Phase 3 Task 20).

### v3-foundation.AC2.4 — checkpoint/restore round-trip determinism
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/checkpoint.rs::checkpoint_restore_roundtrip_is_deterministic` (Phase 3 Task 15).

### v3-foundation.AC2.5 — wall-clock timeout fires before 1.5× budget
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/timeout.rs::wall_clock_timeout_fires` (Phase 3 Task 16).

### v3-foundation.AC2.6 — CPU timeout fires with `cpu_ms`
- **Mode**: Automated (integration, Linux-only).
- **Test**: `crates/pattern_runtime/tests/timeout.rs::cpu_timeout_fires` (Phase 3 Task 16). Gated `#[cfg(target_os = "linux")]` due to CPU sampling implementation.
- **Notes**: macOS/Windows would ship with CPU sampling as a separate task; explicit scope-out for v3 foundation.

### v3-foundation.AC2.7 — Tidepool 10K-node response overflow
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/effect_overflow.rs::oversized_response_fails` (Phase 3 Task 17). Also unit-covered at `tidepool/error_map.rs` tests (Task 3).

### v3-foundation.AC2.8 — GHC crash surfaces as `RuntimeError::RuntimeCrashed`
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/ghc_crash.rs::ghc_crash_poisons_session` (Phase 3 Task 18).

### v3-foundation.AC2.9 — stub `spawn`/`mcp`/`ipc` return specific errors, not hang
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/stub_effects.rs` with one test per stubbed namespace (Phase 3 Task 19). Plus unit tests in `tidepool/error_map.rs` (Task 3).

### v3-foundation.AC2.10 — concurrent sessions are isolated
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/session_lifecycle.rs::concurrent_sessions_are_isolated` (Phase 3 Task 14 Step 3).

---

## AC3: Subscription session-pickup authentication (Phase 4)

### v3-foundation.AC3.1 — valid session makes authenticated request
- **Mode**: Automated (mocked) + human-trigger-automated (live).
- **Tests**:
  - Mocked: `crates/pattern_provider/tests/provider_client_integration.rs` session-pickup happy path via wiremock (Phase 4 Task 19).
  - Live: verified transitively by AC9.1 (API-key variant) / AC9.2 (OAuth variant) CLI checklist. The CLI smoke flow makes real `/v1/messages` calls against the Anthropic endpoint; when those succeed, AC3.1's "provider makes an authenticated request ... returns a real response" is demonstrated.
- **Notes**: The live test requires a valid `~/.claude/.credentials.json` on the running machine.

### v3-foundation.AC3.2 — atomic read under concurrent write
- **Mode**: Automated (unit).
- **Test**: `crates/pattern_provider/src/auth/session_pickup.rs` unit tests (Phase 4 Task 8) — spawn a thread rewriting the file in a tight loop while the reader runs; no torn read.
- **Notes**: Uses a tempfile-based harness; no live dependency.

### v3-foundation.AC3.3 — missing session file → skip tier, no error
- **Mode**: Automated (unit).
- **Test**: `session_pickup.rs` unit test pointing the picker at a nonexistent directory; asserts `Ok(None)` (Phase 4 Task 8).

### v3-foundation.AC3.4 — malformed JSON → warning + skip
- **Mode**: Automated (unit).
- **Test**: `session_pickup.rs` unit test writing garbage bytes; asserts `Ok(None)` plus `tracing` warning captured via a test subscriber.

### v3-foundation.AC3.5 — expired token → skip tier
- **Mode**: Automated (unit).
- **Test**: `session_pickup.rs` unit test writing credentials with `expires_at < now`; asserts `Ok(None)`.

### v3-foundation.AC3.6 — keyring absent but session valid → succeeds
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_provider/src/creds_store.rs` + resolver integration tests (Phase 4 Task 6, Task 10) — resolver uses the session-pickup tier without touching the keyring; test constructs a resolver with a stub keyring that errors on any call and confirms success.

---

## AC4: PKCE and API-key fallback (Phase 4)

### v3-foundation.AC4.1 — PKCE flow end-to-end
- **Mode**: Human + human-trigger-automated.
- **Tests**:
  - Manual: Phase 4 Task 9 ships the manual-paste PKCE variant; user pastes the code, token stored in keyring; subsequent request succeeds. Documented in `pattern_provider/CLAUDE.md`.
  - Live: verified by AC9.2 CLI checklist — the PKCE flow runs once as Step 0, then the rest of the DoD demonstrates the stored token works.
- **Justification**: The interactive consent step requires a human at the browser. The post-consent token-use path is automated.

### v3-foundation.AC4.2 — near-expiry auto-refresh
- **Mode**: Automated (integration via wiremock).
- **Test**: `crates/pattern_provider/tests/provider_client_integration.rs` (Phase 4 Task 19) — stored near-expiry token → resolver triggers refresh → new token stored → request succeeds.

### v3-foundation.AC4.3 — `ANTHROPIC_API_KEY` path works
- **Mode**: Automated (integration).
- **Tests**:
  - Mocked: `tests/provider_client_integration.rs` (Phase 4 Task 19).
  - Live: verified by AC9.1 CLI checklist (API-key tier) — demonstrates the full code path end-to-end against the Anthropic endpoint.

### v3-foundation.AC4.4 — PKCE callback timeout surfaces
- **Mode**: Automated (unit, future loopback variant) + documented deferral for manual-paste.
- **Test**: Phase 4 Task 9 notes the manual-paste flow is not meaningfully timeout-testable (user pastes when they paste). The future loopback variant has a 5-minute deadline surfaced as `ProviderError::AuthFlowTimeout`; unit test for the loopback listener lands with it.
- **Notes**: Recorded in phase design as not-directly-testable-in-manual-mode.

### v3-foundation.AC4.5 — refresh-endpoint error surfaces
- **Mode**: Automated (integration).
- **Test**: `tests/provider_client_integration.rs` wiremock test — refresh endpoint returns 400/500, resolver surfaces `ProviderError::RefreshFailed` (Phase 4 Task 10 Step, Task 19).

### v3-foundation.AC4.6 — keyring + JSON fallback both unavailable
- **Mode**: Automated (unit).
- **Test**: `crates/pattern_provider/src/creds_store.rs` unit tests — primary returns error, fallback path unreadable → `ProviderError::CredentialStoreUnavailable` (Phase 4 Task 6).

### v3-foundation.AC4.7 — concurrent refresh serialized by mutex
- **Mode**: Automated (integration).
- **Test**: `tests/provider_client_integration.rs` concurrent-refresh test — 10 tokio tasks call `resolve()` against a near-expiry stored token; wiremock asserts exactly one refresh call reached the endpoint (Phase 4 Task 10).

---

## AC5: Request shaping and rate limiting (Phase 4)

### v3-foundation.AC5.1 — honest pattern-identification headers + session UUID
- **Mode**: Automated (unit + integration).
- **Tests**:
  - Unit: shaper header-population tests in `crates/pattern_provider/src/shaper.rs` (Phase 4 Task 12).
  - Integration: `crates/pattern_provider/tests/shaper_ratelimit_integration.rs` via wiremock asserting outbound headers (Phase 4 Task 15).

### v3-foundation.AC5.2 — pattern-specific persona content in system-prompt slot
- **Mode**: Automated (unit).
- **Test**: Shaper unit tests (Phase 4 Task 12) assert slots [1] and [2] contain pattern persona content. Slot [0] may contain the claude-code literal under `ShaperCompatMode` — planning classified this as **structural**, not identity; `pattern_provider/CLAUDE.md` documents the framing.
- **Notes**: Rationale for structural-not-identity decision is explicit in the phase 4 design notes and tests assert on the behaviour-driving slots, not slot [0] exclusion.

### v3-foundation.AC5.3 — session UUID rotates on caller signal
- **Mode**: Automated (unit).
- **Test**: `crates/pattern_provider/src/session_uuid.rs` unit tests — explicit rotation call changes UUID; non-rotation calls preserve it (Phase 4 Task 13).

### v3-foundation.AC5.4 — rate-bucket exhaustion queues with jitter
- **Mode**: Automated (integration).
- **Tests**:
  - Unit: `crates/pattern_provider/src/ratelimit.rs` TPM exhaustion + refill test (Phase 4 Task 14).
  - Integration: `tests/shaper_ratelimit_integration.rs` end-to-end retry-after-refill (Phase 4 Task 15).

### v3-foundation.AC5.5 — misconfigured shaper fails at construction
- **Mode**: Automated (unit).
- **Test**: Shaper unit test constructs `ShaperConfig` with empty `x_app`; asserts `new()` errors before any request shape (Phase 4 Task 12).

### v3-foundation.AC5.6 — independent buckets across providers
- **Mode**: Automated (unit).
- **Test**: `ratelimit.rs` unit test — two `ProviderRateLimiter` instances with different tiers operate independently (Phase 4 Task 14).

### v3-foundation.AC5.7 — tokens-per-day bucket independent of per-minute
- **Mode**: Automated (unit).
- **Test**: `ratelimit.rs` unit test — deplete TPD, wait past TPM window, confirm next request still blocked on TPD (Phase 4 Task 14).

---

## AC5b: Provider-reported token counting (Phase 4 API + Phase 5 consumption)

### v3-foundation.AC5b.1 — `count_tokens` returns Anthropic-reported counts
- **Mode**: Automated (integration, mocked) + human-trigger-automated (live).
- **Tests**:
  - Mocked: `tests/provider_client_integration.rs` + wiremock fixture (Phase 4 Task 16, Task 19).
  - Live: AC9.1 / AC9.2 CLI checklist exercises count_tokens transitively when the provider computes pre-request budgets during the smoke flow. If the counts are wildly wrong, context-length decisions go wrong, which surfaces in checklist observations.

### v3-foundation.AC5b.2 — post-response `usage` capture and exposure
- **Mode**: Automated (integration).
- **Test**: `tests/provider_client_integration.rs` asserts `ChatResponse::usage` is populated (Phase 4 Task 17).

### v3-foundation.AC5b.3 — call-site migration from heuristic to provider counts
- **Mode**: Automated (integration, Phase 5).
- **Test**: Compression-strategy tests in `crates/pattern_provider/` consume the async `count_tokens` API (Phase 5 Task 13); assert the code path invokes `ProviderClient::count_tokens` rather than the retired heuristic.
- **Notes**: Phase 4 lands the API; Phase 5 migrates the call sites. Divergence from AC-as-written is noted in Phase 4 design as intentional.

### v3-foundation.AC5b.4 — `TokenCountFailed` surfaces explicitly
- **Mode**: Automated (integration).
- **Test**: `tests/provider_client_integration.rs` wiremock non-2xx response → `ProviderError::TokenCountFailed { status, body }`; no silent heuristic fallback (Phase 4 Task 16).

### v3-foundation.AC5b.5 — count bucket independent of completion bucket
- **Mode**: Automated (unit).
- **Test**: `ratelimit.rs` unit test — `acquire_completion(1000)` does not consume from the count-tokens bucket (Phase 4 Task 14).

---

## AC6: Memory storage adapter preserves existing behavior (Phase 5)

### v3-foundation.AC6.1 — `ctx.memory.write` persists
- **Mode**: Automated (integration).
- **Test**: `MemoryStoreAdapter` integration tests in `crates/pattern_runtime/src/memory/adapter.rs` (Phase 5 Task 4).

### v3-foundation.AC6.2 — `ctx.memory.read` returns current content
- **Mode**: Automated (integration).
- **Test**: Same adapter test file (Phase 5 Task 4) — write then read round-trip.

### v3-foundation.AC6.3 — `ctx.memory.search` returns hybrid FTS + vector
- **Mode**: Automated (integration).
- **Test**: Adapter test with `pattern_db` fixture (Phase 5 Task 4) — asserts both FTS and vector paths contribute.

### v3-foundation.AC6.4 — content survives restart
- **Mode**: Automated (integration).
- **Test**: Adapter test (Phase 5 Task 4) — write, tear down adapter, rebuild from storage, read; simulates process restart without actually exec'ing a new process.

### v3-foundation.AC6.5 — write to non-existent handle → `BlockNotFound`
- **Mode**: Automated (unit).
- **Test**: Adapter unit test (Phase 5 Task 4) asserting `MemoryError::BlockNotFound { handle, available }` with populated `available` list.

### v3-foundation.AC6.6 — concurrent writes merge via loro CRDT
- **Mode**: Automated (integration).
- **Test**: Adapter test using `tokio::spawn` for two concurrent writers (Phase 5 Task 4); asserts both changes survive merge.

---

## AC7: Three-segment cache layout structure (Phase 5)

### v3-foundation.AC7.1 — exactly three `cache_control` markers
- **Mode**: Automated (integration + unit).
- **Tests**:
  - Composer finalize unit tests in `crates/pattern_provider/src/compose.rs` (Phase 5 Task 10).
  - End-to-end integration: `crates/pattern_provider/tests/memory_edit_cache_preservation.rs` asserts 3 markers in composed `ChatRequest` (Phase 5 Task 15).

### v3-foundation.AC7.1b — per-breakpoint TTL selection
- **Mode**: Automated (unit).
- **Test**: `CacheProfile` tests (Phase 5 Task 1, Task 2) — default 1h for seg 1, 5m for segs 2/3; caller-override round-trip.

### v3-foundation.AC7.2 — segment 1 has identity + base instructions + tools, no blocks
- **Mode**: Automated (unit + integration).
- **Tests**:
  - Unit: composer-pass-1 test (Phase 5 Task 8) scans `system_blocks[..=marker_idx]` for absence of `[memory:…]` substrings.
  - Regression: block-rendering removal verified in Phase 5 Task 14 with a grep-style assertion over the pre-v3 builder path.

### v3-foundation.AC7.3 — segment 3 contains `[memory:current_state]` pseudo-turn
- **Mode**: Automated (unit + snapshot).
- **Test**: `current_state.rs` pseudo-turn renderer tests (Phase 5 Task 7).

### v3-foundation.AC7.4 — `DEFAULT_BASE_INSTRUCTIONS` byte-for-byte in segment 1
- **Mode**: Automated (snapshot).
- **Test**: Composer Task 8 snapshot test — exact substring match and marker-coverage assertion against `crates/pattern_core/src/base_instructions.rs` (Phase 5 Task 8 Step 2).

### v3-foundation.AC7.5 — 5th `cache_control` marker → composition-time validation error
- **Mode**: Automated (unit).
- **Test**: `BreakpointTracker::place` test in `compose.rs` (Phase 5 Task 10 Step 2) — pipeline with 5 passes is rejected with `CacheBreakpointBudgetExceeded` at placement, not at API boundary.

### v3-foundation.AC7.5b — unsupported TTL → configuration error
- **Mode**: Automated (type-level + unit).
- **Test**: TTL is an enum (`CacheControl::Ephemeral5m` / `Ephemeral1h`); invalid values are unrepresentable. Downgrade path (session doesn't support 1h) tested in Phase 5 Task 1 with a `tracing::warn` assertion.

### v3-foundation.AC7.6 — zero loaded blocks → segment 3 present but empty
- **Mode**: Automated (unit + integration).
- **Tests**:
  - Unit: `current_state.rs` empty-block-set test (Phase 5 Task 7).
  - Integration: `tests/memory_edit_cache_preservation.rs` zero-blocks variant (Phase 5 Task 16).

---

## AC8: Cache preservation across block edits (Phase 5)

### v3-foundation.AC8.1 — segment 1 stays cached across block edit
- **Mode**: Automated (integration, mocked) + human-trigger-automated (live).
- **Tests**:
  - Mocked: `crates/pattern_provider/tests/memory_edit_cache_preservation.rs` — wiremock response includes `cache_read_input_tokens`; assertion checks seg1 hit rate unchanged (Phase 5 Task 15).
  - Live: AC9.1 Step 5-6 CLI checklist captures real `seg1` / `seg3` cache metrics before/after block edit; operator confirms the expected behaviour on real Anthropic responses.

### v3-foundation.AC8.2 — segment 3 invalidates as expected
- **Mode**: Automated (integration) + human-trigger-automated (live).
- **Test**: Same `memory_edit_cache_preservation.rs` — asserts seg3 `cache_read_input_tokens` drops after block edit (Phase 5 Task 15). Live verification Phase 5 Task 18.

### v3-foundation.AC8.3 — `[memory:updated]` pseudo-message in segment 2
- **Mode**: Automated (integration).
- **Test**: `memory_edit_cache_preservation.rs` asserts pseudo-message presence in turn 3's segment 2 (Phase 5 Task 15). Also unit-covered in `pseudo_messages.rs` renderer tests (Phase 5 Task 6).

### v3-foundation.AC8.4 — compression preserves batch integrity with pseudo-messages
- **Mode**: Automated (integration).
- **Test**: Compression strategy tests (Phase 5 Task 13 Step 3) — batch containing `[memory:updated]` is archived fully or not at all.

### v3-foundation.AC8.5 — unexpected segment 1 invalidation fails loudly
- **Mode**: Automated (integration) + human-trigger-automated (live).
- **Tests**:
  - Unit: break-detection hashing tests in Phase 5 Task 11.
  - Integration: cache-hit metrics capture tests (Phase 5 Task 12) — fire `tracing::warn` on seg1 miss when a hit was expected, captured by test subscriber.
  - Live: AC9.1 Step 6 observation — operator verifies break-detection warning fires (via tracing output) when `seg1` invalidation is unexpected.

### v3-foundation.AC8.6 — non-local-agent source renders with correct attribution
- **Mode**: Automated (unit).
- **Test**: `change_log.rs` and `pseudo_messages.rs` unit tests (Phase 5 Tasks 5, 6) — record Written event with `Caller::Agent(other_persona_id)`; assert renderer attributes correctly.

---

## AC9: End-to-end foundation demonstration (Phase 6)

**Planning divergence** from AC text: AC9.1 says "API-key smoke test in CI passes deterministically". Phase 6 makes the full DoD smoke test **manual** via a documented CLI checklist, while keeping AC9.5's per-step error-clarity checks automated in CI. This is a deliberate, user-approved divergence — live-credential smoke tests are not suitable for CI under Pattern's operational risk model, so "deterministic" is satisfied by a repeatable documented procedure instead.

The manual procedures below are the authoritative test specification. `pattern_runtime/CLAUDE.md` carries a copy for operator-facing reference, but this document (`test-requirements.md`) is what verifies AC coverage.

### v3-foundation.AC9.1 — API-key smoke test passes the full flow

- **Mode**: Human (CLI checklist).
- **Justification**: Requires live Anthropic credentials and outbound network. Planning decision: no live-credential tests in CI; procedure documented + exercised manually at phase-close and before any merge to `main`.

#### Prerequisites
- `tidepool-extract` on `$PATH` (or `TIDEPOOL_EXTRACT` set). Confirm with `which tidepool-extract`.
- `ANTHROPIC_API_KEY` exported in the current shell, set to a valid API key.
- Pattern built: `cargo build -p pattern_runtime --bin pattern-v3`.
- Clean data dir: `TMPDIR=$(mktemp -d)`.

#### Procedure

**Step 1 — Open session + first turn.**
```bash
cargo run -p pattern_runtime --bin pattern-v3 -- \
    spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --data-dir "$TMPDIR" \
    --auth api-key
```
At the `pattern>` prompt, type: `hello; what's your role?`

**Expected**: agent responds with a role description consistent with the smoke-persona TOML. Stream chunks render live (typewriter effect). After the turn completes, CLI prints `[cache: seg1=0% seg2=0% seg3=0%]` (all cold on first turn). No errors.

**Step 2 — Write a memory block.**

Type: `please remember in your scratchpad: favorite color is teal.`

**Expected**: agent confirms and its reply references the write. CLI prints a cache-metrics line after the turn. Agent invoked the `memory.write` effect internally (the change log will reflect this when we inspect at Step 5).

**Step 3 — Exit + re-spawn against same data dir (simulated restart).**

Type `:q` or Ctrl+D. CLI exits cleanly.

Re-run the same spawn command with the same `--data-dir "$TMPDIR"`.

**Expected**: CLI restarts. Same persona, same data dir, fresh machine. Prompt returns.

**Step 4 — Recall the stored value.**

Type: `what's my favorite color?`

**Expected**: agent replies with "teal" (or a clear recall of the stored value). Persistence across the simulated restart worked. If the agent says "I don't know" or hallucinates a different color, AC9.1 FAILS — memory persistence is broken.

**Step 5 — Capture pre-edit cache metrics.**

Type: `ok, thanks`

Observe the `[cache: seg1=X% seg2=Y% seg3=Z%]` line. Record `X` (seg1) and `Z` (seg3). Expect `X` > 80% (segment 1 is now hot across turns), `Z` > 50% (segment 3 is hot because memory blocks haven't changed).

**Step 6 — Edit a block externally, then next turn.**

From another shell:
```bash
cargo run -p pattern_runtime --bin pattern-v3 -- \
    edit-block --data-dir "$TMPDIR" scratchpad \
    "favorite color is actually indigo"
```

Back in the REPL, type: `confirm update`.

**Expected cache behavior**:
- `seg1` ratio within 5% of pre-edit `X` (system prefix preserved).
- `seg3` ratio substantially lower than pre-edit `Z` (memory pseudo-turn invalidated because block content changed).
- The `<system-reminder>` pseudo-message for the block edit appears in the LLM's view of segment 2 history.

**Step 7 — Agent confirms the new value.**

Type: `what's my favorite color now?`

**Expected**: agent responds with "indigo" (reflecting the edit). The agent saw the block-updated pseudo-message, recalls the new value.

**Step 8 — Shutdown and cleanup.**

Exit with `:q`. Remove `$TMPDIR`.

#### Success criteria (all required)

- [ ] All 8 steps complete without errors.
- [ ] Agent writes + reads memory blocks correctly (Step 2, 4).
- [ ] Memory survives simulated restart (Step 4).
- [ ] Agent picks up block edits via pseudo-messages (Step 7).
- [ ] `seg1` cache ratio unchanged across block edit (Step 6).
- [ ] `seg3` cache ratio drops across block edit (Step 6).
- [ ] No unhandled errors or panics during the flow.

If any criterion fails, AC9.1 FAILS. Record the failing step + error in the operator's test log and file a blocker.

### v3-foundation.AC9.2 — subscription-OAuth smoke test passes manually

- **Mode**: Human (CLI checklist, OAuth variant).
- **Justification**: Requires live Claude Pro/Max subscription. Can't run in CI for both security and account-type reasons.

#### Prerequisites
- All AC9.1 prerequisites EXCEPT `ANTHROPIC_API_KEY`.
- One of:
  - (a) Active claude-code session at `~/.claude/.credentials.json` (session-pickup tier will resolve it automatically), OR
  - (b) Previous `pattern-v3 spawn ... --auth pkce` run that cached a token via keyring (see Step 0 below).

#### Procedure

**Step 0 — (If using PKCE) one-time OAuth.**

```bash
cargo run -p pattern_runtime --bin pattern-v3 -- \
    spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --data-dir "$TMPDIR" \
    --auth pkce
```

CLI prints an authorization URL. Visit in browser. Approve. Page displays an authorization code. Copy the full `code#state` string. Paste into the CLI prompt.

**Expected**: "Authentication successful" message. Token stored in keyring (or JSON fallback). CLI enters the REPL.

(If Step 0 already happened previously, skip to Step 1.)

**Step 1-8** — Identical to AC9.1 Steps 1-8, but re-spawn using `--auth session-pickup` (or no `--auth` flag, letting the resolver pick the highest tier — should resolve to SubscriptionOAuth / SessionPickup).

#### Success criteria
- All AC9.1 success criteria apply.
- Additionally: session header inspection (via tracing logs or provider's debug output) should confirm the auth tier used was `SessionPickup` or `Pkce`, NOT `ApiKey`.
- Anthropic subscription usage accrues (verify via the subscriber dashboard if desired).

If the auth tier is `ApiKey` instead of `SessionPickup` / `Pkce`, AC9.2 FAILS — the OAuth path isn't being selected even though credentials are present.

### v3-foundation.AC9.3 — minimal CLI entry point drives full flow

- **Mode**: Human + automated infrastructure.
- **Justification**: Functional correctness of the CLI (argument parsing, subcommand dispatch, basic structure) is verifiable via build gates; behaviour-under-live-creds is covered by AC9.1/9.2 manual checklists.

#### Automated verification
- `cargo build -p pattern_runtime --bin pattern-v3` succeeds without warnings.
- `cargo run -p pattern_runtime --bin pattern-v3 -- --help` prints a `Usage:` block with `spawn` and `edit-block` subcommands.
- `cargo run -p pattern_runtime --bin pattern-v3 -- spawn --help` lists `--data-dir`, `--auth` flags.

#### Manual verification
Part of the AC9.1 / AC9.2 checklists: the existence and correct operation of the `spawn` subcommand driving the full DoD flow is the test.

Additional spot-check:
- Run `cargo run -p pattern_runtime --bin pattern-v3 -- spawn nonexistent.toml` — expect a specific error pointing at the TOML loading step (AC9.5 category).
- Run `cargo run -p pattern_runtime --bin pattern-v3 -- spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml --auth api-key` with `ANTHROPIC_API_KEY` unset — expect a specific auth-error message pointing at the resolver step.

### v3-foundation.AC9.4 — cache-hit metric assertions

- **Mode**: Automated (mocked, Phase 5) + Human (live via checklist).
- **Justification**: Metric-shape correctness is automated via mocked tests; live-response cache behaviour requires real Anthropic backend responses and is verified by human observation of metrics during AC9.1 Step 6.

#### Automated tests
- `crates/pattern_provider/tests/memory_edit_cache_preservation.rs` (Phase 5 Task 15) — uses wiremock responses with canned `usage` fields. Asserts segment_1_hit_ratio stays above 0.95 after simulated block-edit, segment_3_hit_ratio drops below 0.5.
- `crates/pattern_provider/src/compose/break_detection.rs` tests — verify the hash-diff machinery correctly identifies which component changed when a bust is detected.

#### Live verification procedure
Embedded in AC9.1 Step 6. Record pre-edit `seg1`, `seg3` values; record post-edit values; verify:
- Post-edit `seg1` ∈ [pre_edit_seg1 × 0.95, pre_edit_seg1 × 1.05] (segment 1 preserved).
- Post-edit `seg3` < pre_edit_seg3 × 0.5 (segment 3 invalidated).

If either bound is violated, AC9.4 FAILS.

Additionally verify: the break-detection warning in `tracing::warn!` output identifies "cache_control block at index 2 changed" (or similar diagnostic) — not a generic "cache miss" message.

### v3-foundation.AC9.5 — per-step failure produces specific error
- **Mode**: Automated (integration).
- **Test**: `crates/pattern_runtime/tests/error_clarity.rs` (Phase 6 Task 5) — one test case per smoke step (persona parse, auth, provider-build, session-open, memory-write, restart, read-back, edit path). Runs in CI without credentials; each test induces a failure and asserts the error's display message uniquely identifies which step failed.

---

## Summary table

| AC | Mode | Location |
|---|---|---|
| AC1.1 | Automated (build) | `cargo check -p pattern_core` (Phase 2 Task 22, 26) |
| AC1.2 | Automated (doc + doctest) | `cargo doc -p pattern_core`; `cargo test --doc -p pattern_core` |
| AC1.3 | Automated (doctest) | `crates/pattern_core/src/traits/*.rs` trait-level doctests |
| AC1.4 | Automated + Human review | `test -f docs/plans/rewrite-v3-portlist.md` + content audit |
| AC1.5 | Human spot-check | Phase 2 Task 20 Step 3; recorded in commit |
| AC1.6 | Human spot-check | Phase 2 Task 26 Step 3; Phase 4 Task 7 |
| AC1.7 | Automated (audit) | `scripts/audit-rewrite-state.sh` |
| AC1.8 | Automated (audit) | `scripts/audit-rewrite-state.sh` |
| AC1.9 | Automated (audit) | `scripts/audit-rewrite-state.sh` |
| AC1.10 | Automated (audit) | `scripts/audit-rewrite-state.sh` |
| AC2.1 | Automated (integration) | `crates/pattern_runtime/tests/hello_world.rs` |
| AC2.2 | Automated (integration) | `tests/time_log_effects.rs::time_now_returns_current_epoch` |
| AC2.3 | Automated (integration) | `tests/time_log_effects.rs::log_info_observable_via_tracing` |
| AC2.4 | Automated (integration) | `tests/checkpoint.rs::checkpoint_restore_roundtrip_is_deterministic` |
| AC2.5 | Automated (integration) | `tests/timeout.rs::wall_clock_timeout_fires` |
| AC2.6 | Automated (integration, linux) | `tests/timeout.rs::cpu_timeout_fires` |
| AC2.7 | Automated (integration) | `tests/effect_overflow.rs::oversized_response_fails` |
| AC2.8 | Automated (integration) | `tests/ghc_crash.rs::ghc_crash_poisons_session` |
| AC2.9 | Automated (integration) | `tests/stub_effects.rs::*` |
| AC2.10 | Automated (integration) | `tests/session_lifecycle.rs::concurrent_sessions_are_isolated` |
| AC3.1 | Automated (mock) + Human (AC9.1/9.2 CLI) | `tests/provider_client_integration.rs`; smoke-flow CLI |
| AC3.2 | Automated (unit) | `src/auth/session_pickup.rs` |
| AC3.3 | Automated (unit) | `src/auth/session_pickup.rs` |
| AC3.4 | Automated (unit) | `src/auth/session_pickup.rs` |
| AC3.5 | Automated (unit) | `src/auth/session_pickup.rs` |
| AC3.6 | Automated (integration) | `src/creds_store.rs` + resolver tests |
| AC4.1 | Human (AC9.2 CLI) | Manual-paste PKCE exercised during AC9.2 Step 0 |
| AC4.2 | Automated (integration) | `tests/provider_client_integration.rs` |
| AC4.3 | Automated (mock) + Human (AC9.1 CLI) | `tests/provider_client_integration.rs`; API-key smoke flow |
| AC4.4 | Documented deferral | Loopback variant future task; manual-paste not meaningfully timeout-testable |
| AC4.5 | Automated (integration) | `tests/provider_client_integration.rs` |
| AC4.6 | Automated (unit) | `src/creds_store.rs` |
| AC4.7 | Automated (integration) | `tests/provider_client_integration.rs` concurrent-refresh |
| AC5.1 | Automated (unit + integration) | `src/shaper.rs`; `tests/shaper_ratelimit_integration.rs` |
| AC5.2 | Automated (unit) | `src/shaper.rs` |
| AC5.3 | Automated (unit) | `src/session_uuid.rs` |
| AC5.4 | Automated (unit + integration) | `src/ratelimit.rs`; `tests/shaper_ratelimit_integration.rs` |
| AC5.5 | Automated (unit) | `src/shaper.rs` |
| AC5.6 | Automated (unit) | `src/ratelimit.rs` |
| AC5.7 | Automated (unit) | `src/ratelimit.rs` |
| AC5b.1 | Automated (mock) + Human (AC9.1/9.2 CLI) | `tests/provider_client_integration.rs`; smoke-flow observation |
| AC5b.2 | Automated (integration) | `tests/provider_client_integration.rs` |
| AC5b.3 | Automated (integration, Phase 5) | Compression call-site tests |
| AC5b.4 | Automated (integration) | `tests/provider_client_integration.rs` |
| AC5b.5 | Automated (unit) | `src/ratelimit.rs` |
| AC6.1 | Automated (integration) | `pattern_runtime/src/memory/adapter.rs` |
| AC6.2 | Automated (integration) | `pattern_runtime/src/memory/adapter.rs` |
| AC6.3 | Automated (integration) | `pattern_runtime/src/memory/adapter.rs` |
| AC6.4 | Automated (integration) | `pattern_runtime/src/memory/adapter.rs` |
| AC6.5 | Automated (unit) | `pattern_runtime/src/memory/adapter.rs` |
| AC6.6 | Automated (integration) | `pattern_runtime/src/memory/adapter.rs` |
| AC7.1 | Automated (unit + integration) | `pattern_provider/src/compose.rs`; `tests/memory_edit_cache_preservation.rs` |
| AC7.1b | Automated (unit) | `CacheProfile` tests |
| AC7.2 | Automated (unit) | composer pass-1 test; block-rendering regression (Phase 5 Task 14) |
| AC7.3 | Automated (unit) | `current_state.rs` |
| AC7.4 | Automated (snapshot) | composer Task 8 snapshot |
| AC7.5 | Automated (unit) | `BreakpointTracker::place` tests |
| AC7.5b | Automated (type + unit) | `CacheControl` enum + `CacheProfile` downgrade tests |
| AC7.6 | Automated (unit + integration) | `current_state.rs`; `tests/memory_edit_cache_preservation.rs` |
| AC8.1 | Automated (mock) + Human (AC9.1 Step 5-6) | `tests/memory_edit_cache_preservation.rs`; CLI checklist |
| AC8.2 | Automated (mock) + Human (AC9.1 Step 5-6) | `tests/memory_edit_cache_preservation.rs`; CLI checklist |
| AC8.3 | Automated (integration) | `tests/memory_edit_cache_preservation.rs`; `pseudo_messages.rs` |
| AC8.4 | Automated (integration) | Compression strategy tests (Phase 5 Task 13) |
| AC8.5 | Automated (unit + integration) + Human (AC9.1 Step 6) | break-detection + metrics tests; CLI tracing observation |
| AC8.6 | Automated (unit) | `change_log.rs`; `pseudo_messages.rs` |
| AC9.1 | Human (CLI checklist) | `pattern_runtime/CLAUDE.md` smoke-test procedure |
| AC9.2 | Human (CLI checklist, OAuth) | `pattern_runtime/CLAUDE.md` smoke-test procedure |
| AC9.3 | Human + build-gate | `pattern-v3` bin links; checklist exercises subcommands |
| AC9.4 | Automated (mock) + Human (live metrics) | Phase 5 Task 15 assertions; checklist Step 7 |
| AC9.5 | Automated (integration) | `crates/pattern_runtime/tests/error_clarity.rs` |
