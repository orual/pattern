# v3-multi-agent: Human Test Plan

**Implementation:** `docs/implementation-plans/2026-04-19-v3-multi-agent/` (Phases 1–7 complete)
**Test requirements:** `docs/implementation-plans/2026-04-19-v3-multi-agent/test-requirements.md`
**Companion automated suite:** see Coverage Validation section below.
**Generated:** 2026-04-28 (head: jj `pqypxlkq` / commit `a0102d1d`)

This document is the human-driven verification companion to the automated
test suite. Every Acceptance Criterion (AC1.1–AC10.6) listed in
test-requirements.md has automated coverage; the manual steps below
exercise the same criteria end-to-end through the daemon + TUI to
confirm they hold under real-world execution paths and that the
visible-to-the-partner surfaces (constellation panel, fronting
display, sibling spawn UX, error messages) render correctly.

---

## Coverage Validation

Test-requirements doc lists 56 ACs across 10 axes. Every AC has at
least one automated test asserting its behaviour. The actual landed
test layout differs from the *expected paths* in test-requirements.md
(many tests were consolidated into broader fixture files), but coverage
is complete.

| AC group | ACs | Status | Evidence |
|----------|-----|--------|----------|
| AC1 (CapabilitySet + prelude filtering) | 1.1–1.6 | covered | `pattern_core::capability` units; `pattern_runtime::sdk::bundle` snapshots; `tests/capability_compile.rs::excluded_effect_fails_at_tidepool_compile` + `permitted_effects_compile_and_run`; runtime gate via `tests/wake_custom_evaluator.rs::wake_eval_*_rejected_at_compile` |
| AC2 (runtime approval + policy) | 2.1–2.9 | covered | `policy/defaults.rs` units (`defaults_gate_destructive_shell_commands` + `defaults_allow_benign_shell_commands`); `policy/config_guard.rs` units (5 tests + proptest fuzz); `pattern_core::permission::tests` (8 tests covering approve-once / scope / duration / timeout / two-broker isolation / partner short-circuit); `tests/shell_handler.rs` policy tests (`execute_via_handler_denies_when_policy_denies`, `…escalates_to_broker_on_require_approval`); persona KDL `policy { rule … }` parsing in `persona_loader::tests` |
| AC3 (ephemeral spawn) | 3.1–3.7 | covered | `tests/ephemeral_spawn.rs` (13 tests including `parent_cancel_propagates_through_three_level_chain`, `eval_worker_count_returns_to_baseline_after_ephemeral`, `ac3_4_timeout_fires_cancel_and_returns_timeout_error`, `ac3_5_handler_side_concurrency_limit_returns_handler_error`, `costume_overrides_system_prompt_and_preserves_persona_identity`); `spawn::registry` units; `LIVE_EVAL_WORKERS` accessor in `agent_loop::eval_worker` |
| AC4 (fork + isolation) | 4.1–4.10 | covered | `tests/fork_lightweight.rs` (4 tests); `tests/fork_merge_lightweight.rs` (5 tests + proptest `merge_back_convergence_all_appends_survive`); `tests/fork_discard.rs` (4 tests, including double-discard); `tests/fork_persistent.rs` (`*_jj_gated` tests for persistent merge + discard + bookmark format); `tests/fork_promote.rs` (3 tests covering with/without flag + persistent synthetic); `tests/fork_dispatch.rs` end-to-end via handler dispatch including bookmark-collision (I-5) and rollback (I-6); `pattern_server::tests::constellation_rpc::promote_draft_*` (5 tests including `promote_draft_seed_cache_version_mismatch_is_best_effort`) |
| AC5 (sibling spawn + identity) | 5.1–5.7 | covered | `tests/sibling_spawn.rs` (`ac5_1`/`ac5_2`/`ac5_3`/`ac5_4`/`ac5_6` + 3 wire-shape tests); `tests/sibling_autoregister.rs` (`ac5_5_*`/`ac5_7_*` + no-registry-wired); `tests/sibling_resolver_constellation.rs` (4 tests for C-1 production resolver) |
| AC6 (mailbox + delivery) | 6.1–6.6 | covered | `agent_registry::tests` (15 tests; FIFO under single+concurrent senders; busy-agent queueing; persona-not-found; draft queue+drain; **`route_or_queue_active_swap_does_not_lose_messages`** I-4 regression); `tests/agent_registry_promote_race.rs::route_or_queue_never_returns_persona_not_found_during_promotion`; `sdk::handlers::message::tests` (6 tests including `delegate_pins_task_block_ref_in_message` and `send_to_nonexistent_agent_produces_router_error_prefix`); `router::agent::tests` (4 tests) |
| AC7 (wake conditions) | 7.1–7.7 | covered | `wake::rust_primitives::tests` (5 tests covering interval, task_timeout, multiple-conditions, unregister, registry-drop); `wake::block_changed::tests`; `tests/wake_task_dep.rs::task_dep_resolved_fires_on_completion`; `tests/wake_handler_capability.rs` (5 tests including registry-missing-prefix); `tests/wake_custom_evaluator.rs` (15 tests including capability-rejection-at-compile, condition cap enforced, two-conditions overlap, min-period rejection, abort-aborts-evaluator) |
| AC8 (fronting + routing) | 8.1–8.8 | covered | `pattern_db::queries::fronting::tests` (5 tests round-trip + clear + migration + regex rule); `fronting_dispatch::tests` (8 tests: rule-match, fallback, fan-out, **`rule_update_applies_to_subsequent_dispatches`** I-3/AC8.8, `in_flight_routing_uses_snapshot_at_dispatch_time`, default-system); `tests/fronting_handler_capability.rs` (set/current/route/clear capability gating; `none_caps_is_denied_fail_closed`); `tests/fronting_supervisor.rs` (`supervisor_pattern_routes_messages_correctly`, `fronting_set_survives_restart`); origin-author tagging via `pattern_core::permission::tests::partner_origin_short_circuits_without_broadcast` + `non_partner_origins_broadcast_normally`; agent-loop `Author::Agent`/`Author::System`/`Author::Partner`/`Author::Human` thread-through visible at `agent_loop.rs:720` (BatchType mapping) |
| AC9 (constellation registry) | 9.1–9.6 | covered | `pattern_db::queries::constellation::tests` (10+ tests covering `list_all`, `list_project_filters_via_json_each`, `list_project_unknown_path_returns_empty_not_error` for AC9.5, `find_by_project_and_kind_filters_correctly`, `get_returns_some_then_none`); `tests/constellation_sdk.rs` (8 tests: capability-denied, missing-registry, list/find/groups dispatches, project-filter, unknown-kind clear-error); `pattern_server::tests::constellation_rpc` (list, list-empty, list-without-init, add_relationship paths, create_group + duplicate) |
| AC10 (e2e integration) | 10.1–10.6 | covered | **`tests/multi_agent_smoke.rs::multi_agent_smoke`** — 9-step e2e smoke (assertion messages contain `"step N: …"` context for AC10.5); fixtures at `tests/fixtures/multi_agent/{supervisor,specialist}.kdl` + `*.hs`; mock provider via `tests/support/multi_agent_scripts.rs` (AC10.2); fork+merge in step 6 (AC10.3); compile-time capability rejection in step 5 (AC10.4 + AC1.2 re-verification); unique tempdir per test invocation (AC10.6) |

**Pre-flight automated suite:** `cargo nextest run -p pattern-runtime -p pattern-core -p pattern-server -p pattern-db` shows 1323 / 1323 passing as of head `a0102d1d`.

**Gaps:** none observed. Every AC mapped to at least one automated assertion that actually exercises the behaviour (not just file existence).

---

## Pre-flight setup

### Environment

- [ ] `nix develop` (or equivalent) so `tidepool-extract` is on `$PATH`
      and `$TIDEPOOL_EXTRACT` resolves to a non-stale store path. Confirm
      with `readlink -f "$TIDEPOOL_EXTRACT"`.
- [ ] `which jj` resolves a binary; `jj --version` ≥ pinned minimum (see
      `crates/pattern_memory/src/jj/adapter.rs::detect`).
- [ ] Working tree clean; `cargo check --workspace` succeeds.
- [ ] `cargo nextest run -p pattern-runtime -p pattern-core -p pattern-server -p pattern-db`
      reports `1323/1323` passing.
- [ ] `cargo nextest run -p pattern-memory` reports `448/448` passing
      against a warm fs cache. (A documented cold-build fs-watcher
      flake cluster exists; rerun warm if the first run shows fs-watcher
      tests failing together — see project memory note
      `project_pattern_memory_cold_build_flakes`.)
- [ ] `cargo clippy --all-targets` reports zero warnings across all five
      crates.

### Test fixtures + scratch dirs

- [ ] `crates/pattern_runtime/tests/fixtures/multi_agent/supervisor.kdl`
      and `…/specialist.kdl` exist. Inspect them: supervisor has Memory,
      Message, Spawn, Constellation, FrontingControl, SpawnNewIdentities;
      specialist has Memory + Message but not Shell / Spawn / File.
      These are the canonical "restricted persona" fixtures for negative-
      case verification.
- [ ] `mktemp -d` for a fresh data dir to avoid contamination from prior
      manual runs. Export as `$PATTERN_TEST_DATA`.

### Daemon startup sanity

> **Critical:** if a partner's production daemon is running, do NOT use
> the production data dir (`~/.local/share/pattern/`). Use `$PATTERN_TEST_DATA`.

- [ ] Start the daemon binary in test mode against `$PATTERN_TEST_DATA`.
- [ ] Connect with the CLI; confirm a clean state (no personas, no fronting set).

---

## Phase A: Constellation registry + TUI rendering (AC9.*, AC5.5, AC5.7)

These steps verify the visual surfaces a partner uses to see their
constellation. Coverage of the underlying data is automated; what
matters here is that the CLI/TUI renders it correctly.

| Step | Action | Expected |
|------|--------|----------|
| A1 | From a clean daemon, register two personas with `--mount` configured. Use the supervisor + specialist fixtures, copying KDLs into the test data dir. | Both personas appear in `ctx.constellation.list()` output (see step A3). |
| A2 | Open a session for the supervisor; let it initialise. | Supervisor appears in the TUI's constellation panel with `status: Active`. |
| A3 | Use the `/constellation` slash command (or whatever the cli surfaces — see `pattern_cli::tui::constellation`) to render the panel. | Panel shows both personas, their relationships (none yet), groups (none yet), and statuses. Specialist still shows `status: Active` if its session is running, otherwise `Inactive`. |
| A4 | From the supervisor session, run the equivalent of `ctx.constellation.find(<project>, SupervisorOf)` via a delegated agent program (or direct API). Verify the response. | Empty list, since no relationships exist yet. |
| A5 | Issue `/relate <supervisor> SupervisorOf <specialist>` (or the equivalent constellation API call). | Edge appears in the panel. Re-running A4 returns `[specialist]`. |
| A6 | Spawn a sibling persona ("new identity" path) from the supervisor with `SpawnNewIdentities` in caps and `relationship=PeerOf`. | Sibling auto-registers as Active; appears in panel with `PeerOf` edge. (Verifies AC5.5 + AC5.7 + AC9.4 end-to-end through the CLI.) |
| A7 | Spawn a sibling without `SpawnNewIdentities` flag (use a persona whose capabilities lack it). | Sibling appears in panel with `status: Draft`. Send a message to the draft via `@<draft-id>` — it should queue, not error. (AC5.7 / AC6.5) |
| A8 | Promote the draft via `/promote <draft-id>` slash command. | Sibling status flips to Active in the panel. Queued messages drain (verifiable via the sibling's session log). (Cross-references Phase 6 Task 6 + automated `agent_registry_promote_race.rs`.) |

**Negative cases for Phase A:**

- [ ] Query `ctx.constellation.find(<bogus-project-path>, SupervisorOf)` via an agent program — must return an empty `Vec`, not an error. Visually, panel filtering by an unknown project should show "no personas in this project" rather than an error toast. (AC9.5)
- [ ] Try `/relate <supervisor> InvalidKind <specialist>` — must surface a clear error message naming the kind ("unknown relationship kind: InvalidKind"). Automated: `pattern_server::tests::constellation_rpc::add_relationship_unknown_kind_returns_error`.

---

## Phase B: Sibling resolution and spawn (C-1 regression scenario)

This phase mirrors the cycle-1 audit failure that motivated
`tests/sibling_resolver_constellation.rs`: before the fix, the daemon
wired no resolver, so `ctx.spawn.sibling(SiblingPersona::Existing(id))`
**always failed with `RegistryError::PersonaNotFound`** even for
auto-registered personas. The automated tests cover the resolver +
end-to-end spawn path through `ConstellationSiblingResolver`; the
manual scenario verifies it from the partner's seat.

| Step | Action | Expected |
|------|--------|----------|
| B1 | Have two Active personas registered (Phase A). Note the specialist's persona-id. | Both appear in the panel. |
| B2 | From the supervisor's session, send an agent program that invokes `Spawn.sibling(Existing("<specialist-id>"))` with `relationship=SupervisorOf`. (Use the `code` tool or a scripted handler call.) | Spawn succeeds. The supervisor gains an Outgoing `SupervisorOf` edge to the specialist; visible in the constellation panel. |
| B3 | Without restarting, send another `Spawn.sibling(Existing("not-a-real-id"))`. | Spawn fails with `RegistryError::PersonaNotFound("not-a-real-id")` carrying the persona id verbatim. (Surfaces as a clear error in the agent's tool result, not a panic or generic "spawn failed".) |
| B4 | Restart the daemon. Re-open the supervisor session. Repeat B2. | Spawn still succeeds. Confirms the resolver wiring survives a restart and that auto-registered personas keep their `config_path` set in the DB. |

**Why this is worth manually verifying:** the C-1 bug looked fine in
isolated tests (the resolver had unit coverage) but failed in
integration because the *daemon* was constructing
`UnconfiguredSiblingResolver`. The manual restart in B4 simulates the
exact condition that surfaced the bug originally.

---

## Phase C: Fronting + routing (AC8.*)

Fronting is partner-visible: the active persona drives which agent
hears unrouted messages. The TUI typically surfaces the active
persona somewhere in chrome.

| Step | Action | Expected |
|------|--------|----------|
| C1 | With supervisor + specialist Active, set fronting to `[supervisor]` only. | TUI chrome shows "fronting: supervisor" or equivalent indicator. |
| C2 | Type a message with no `@` prefix into the chat. | Supervisor receives the message; specialist does not. |
| C3 | Type `@specialist <body>`. | Specialist receives `<body>`; supervisor does not. (AC8.4 direct-delivery bypass.) |
| C4 | Add a routing rule: `pattern="^math:"` → `target=specialist`. Send `math: 2+2`. | Specialist receives it. |
| C5 | Send `hello world` (no rule match). | Supervisor receives (fallback to fronting persona). (AC8.3) |
| C6 | Set fronting to `[supervisor, specialist]` (co-fronting). Send an unrouted message. | Both personas receive it (fan-out). (AC8.5) |
| C7 | Update routing rules mid-conversation while a message is in flight. | Queued messages use the prior routing; new messages use the new routing. The TUI should render the rule update as it lands; a brief "fronting updated" event is acceptable. (AC8.8 — automated coverage in `fronting_dispatch::tests::rule_update_applies_to_subsequent_dispatches` + `in_flight_routing_uses_snapshot_at_dispatch_time`.) |
| C8 | **Restart the daemon.** Reconnect. | Fronting set + routing rules persist exactly. (AC8.1 — automated in `fronting_supervisor.rs::fronting_set_survives_restart`; manual restart confirms the daemon wires the persisted state at startup.) |

---

## Phase D: Fork → merge → restart durability

This phase mirrors the cycle-1 audit concern about fork durability
across daemon restart. The automated `fork_persistent.rs` and
`fork_promote.rs` tests cover the pieces; the integration-level scenario
is human-driven.

| Step | Action | Expected |
|------|--------|----------|
| D1 | From the supervisor session, fork its memory cache (lightweight). Write to a memory block in the fork. | Parent's view of the block is unchanged. (AC4.1) |
| D2 | Call `merge_back` on the fork. | Parent now sees the merged content. The TUI memory panel (if open) shows the change attribution. (AC4.3) |
| D3 | Spawn a *persistent* fork (requires jj on PATH; the automated `*_jj_gated` tests cover this). Verify a workspace appears under `<mount>/.pattern/forks/<bookmark>`. | jj `bookmark list` shows the new bookmark with format `<agent>/<task-id>`. (AC4.10) |
| D4 | **Restart the daemon** with the persistent fork still outstanding. Reconnect. | The fork bookmark + workspace still exist on disk. Re-opening the parent session should surface the existing fork in the registry (or at least not crash; the cycle-1 fix in I-6 ensures rollback works correctly on collision retry). |
| D5 | Discard the persistent fork via the handler. | jj workspace removed; bookmark deleted. Both partial-failure paths are tested in automated coverage but a clean discard should leave no stale state. (AC4.6) |
| D6 | Promote a lightweight fork into a Draft persona via `fork.promote(cfg)` from a session with `SpawnNewIdentities`. | New Draft persona appears in `constellation.list()` with seeded memory blocks; KDL written under `<drafts_dir>/<id>.kdl`. (AC4.7) |
| D7 | Try the same promote without `SpawnNewIdentities`. | Returns `CapabilityError::Denied` — agent's tool result must name the missing flag, not a generic permission error. (AC4.8) |
| D8 | Promote a draft (via the server's promote-draft RPC) where the seed cache version is incompatible. | Promote still succeeds, but the response carries a `warning` field naming the failing step (`"seed cache migration failed"`), the persona id, and the consequence (`"empty memory"`). Automated: `constellation_rpc::promote_draft_seed_cache_version_mismatch_is_best_effort` (M-1). Manually: confirm the warning surfaces in the CLI's promote-draft output, not silently swallowed. |

---

## Phase E: Negative cases — capability-restricted persona

These mirror the cycle-1 / cycle-2 audit's regression-shaped concerns.
The "specialist" fixture is intentionally restricted: Memory + Message
only, no Shell / Spawn / File / FrontingControl.

| Step | Action | Expected |
|------|--------|----------|
| E1 | From the specialist session, attempt an agent program that calls `Shell.execute "ls"`. | **Compile-time** rejection from tidepool-extract; the CLI surfaces a scope error naming `Shell`, not a runtime denial. (AC1.2, AC1.3 — automated in `tests/capability_compile.rs`.) |
| E2 | From specialist, attempt `Spawn.ephemeral(...)`. | Same compile-time rejection naming `Spawn`. |
| E3 | From specialist, attempt `Memory.put "scratch" "x"` (an allowed effect). | Compiles and runs. (AC1.3 / AC1.4) |
| E4 | From specialist, attempt `Wake.register(...)`. | Compile-time rejection (capability not present). |
| E5 | From the supervisor (which lacks `WakeConditionRegistration` unless explicitly granted in fixture), attempt `Wake.register(...)`. | Returns `CAPABILITY_DENIED_PREFIX`-marked error from the runtime gate. (AC7.5 — automated in `tests/wake_handler_capability.rs::register_without_capability_is_denied`.) |
| E6 | Configure a persona with `policy { rule "deny-rm-rf" effect="shell" action="deny" { matcher "shell-command" pattern="rm -rf*" } }`. From a session, attempt `rm -rf /tmp/something`. | Returns `PERMISSION_DENIED_PREFIX`-marked error before the process manager is consulted. (AC2.1, AC2.3) |
| E7 | From the same persona, attempt a benign `ls`. | Falls through to allow; ProcessManager runs the command. (AC2.2) |
| E8 | Configure the persona's policy to gate a write with `action="require-approval"`. Attempt the write. | Permission broker fires. From the partner seat: an approval prompt should appear in the TUI; approving lets the write proceed; denying / timing out returns a deny stub. (AC2.4–2.6, AC2.8) |
| E9 | Attempt a `Pattern.File.Write` to a `.pattern.kdl` file (matching the locked config-shape predicate). | **Always** denied regardless of policy. (AC2.7 — automated via `policy/config_guard.rs::tests` + `is_pattern_config_kdl` short-circuit in the file handler.) |

**Supervisor-delegation negative case:**

- [ ] From supervisor, send an agent program that calls
      `Message.delegate("agent:nonexistent-sibling", task)`. Expect the
      tool result carries `RouterError::PersonaNotFound` with the
      persona id named in the error. Confirm: it must NOT crash the
      session, NOT silently drop the message, NOT mis-route to a
      fallback. Automated: `sdk::handlers::message::tests::send_to_nonexistent_agent_produces_router_error_prefix`.

---

## Phase F: Wake conditions (AC7.*)

| Step | Action | Expected |
|------|--------|----------|
| F1 | From a session with `WakeConditionRegistration` capability, register an `Interval` wake every 2s. Wait 6s. | Three wake events delivered to the session's mailbox, each carrying `WakeReason::Interval`. (AC7.4) |
| F2 | Register a `TaskTimeout` for `now + 1s`. Wait 2s. | One wake event with `WakeReason::TaskTimeout`. (AC7.1) |
| F3 | Register a `BlockChanged` for label `"scratch"`. From another session (or the same one in a later turn), modify the `scratch` block. | Wake fires with `WakeReason::BlockChanged`. (AC7.2) |
| F4 | Register multiple conditions; wait for one to fire. | First-to-fire pokes the mailbox; remaining conditions persist until separately triggered. (AC7.6) |
| F5 | Register a Custom (Haskell) wake condition with an `Observe`-only program. Verify it fires. | Wake fires; the read-only restricted bundle prevents the Haskell predicate from initiating side effects. (AC7.5 / EffectClass observe-vs-escape — covered in `tests/wake_custom_evaluator.rs::wake_eval_*_rejected_at_compile`.) |
| F6 | Trigger a wake during a turn that has not yet completed. | Wake is queued; delivered after the current turn closes. (AC7.7) |

---

## Phase G: End-to-end smoke (AC10.*)

The automated `multi_agent_smoke` test exercises 9 steps. As a manual
end-to-end, this confirms the smoke runs deterministically from a
fresh checkout:

- [ ] `cargo nextest run -p pattern-runtime --test multi_agent_smoke -- --nocapture`
- [ ] All assertions carry `"step N: …"` context (AC10.5). If a future
      regression breaks step 5 (capability rejection) or step 6 (fork +
      merge), the failure message identifies the step.
- [ ] Run the smoke alongside other pattern-runtime tests (default
      nextest parallelism). It uses a unique tempdir per invocation
      (AC10.6) and must not interfere with other tests.

---

## Traceability matrix

For each AC, this maps the *automated* coverage and the *manual* phase
that re-exercises it from a partner-visible angle.

| AC | Automated test | Manual step |
|----|----------------|-------------|
| 1.1 | `sdk::bundle` snapshot + `bundle_non_prelude5.rs` | E1–E2 (compile-time scope error visibility) |
| 1.2 | `tests/capability_compile.rs::excluded_effect_fails_at_tidepool_compile` | E1, E2, E4 |
| 1.3 | `tests/capability_compile.rs::permitted_effects_compile_and_run` | E3 |
| 1.4 | `sdk::bundle` snapshot for `CapabilitySet::all()` | (snapshot only) |
| 1.5 | `pattern_core::capability::tests::restrict_to_*` | (unit only) |
| 1.6 | `sdk::bundle` empty-set snapshot | (snapshot only) |
| 2.1 | `policy::defaults::tests::defaults_gate_destructive_shell_commands` | E6 |
| 2.2 | persona-loader policy parsing + `defaults_allow_benign_shell_commands` | E7 |
| 2.3 | persona-loader + Phase E config | E6 |
| 2.4 | `pattern_core::permission::tests::approve_once_does_not_populate_cache` | E8 |
| 2.5 | `…::approve_for_scope_caches_subsequent_requests` | E8 |
| 2.6 | `…::approve_for_duration_expires_via_injected_clock` | (clock-injection — unit only) |
| 2.7 | `policy::config_guard::tests` (5 tests + proptest) | E9 |
| 2.8 | `…::timeout_path_cleans_up_pending_state` | (timeout — unit only) |
| 2.9 | `…::two_brokers_have_independent_state` | (broker isolation — unit only) |
| 3.1 | `tests/ephemeral_spawn.rs::ephemeral_success_returns_final_text_and_logs_progress` | (smoke) |
| 3.2 | `…::capability_subset_is_accepted` + `…::capability_escalation_*` | (smoke) |
| 3.3 | `…::costume_overrides_system_prompt_and_preserves_persona_identity` | (smoke) |
| 3.4 | `…::ac3_4_timeout_fires_cancel_and_returns_timeout_error` | (smoke) |
| 3.5 | `…::ephemeral_concurrency_limit_saturates` + `…::ac3_5_handler_side_concurrency_limit_returns_handler_error` | (smoke) |
| 3.6 | `…::eval_worker_count_returns_to_baseline_after_ephemeral` + `…::watcher_tasks_are_aborted_on_child_registry_drop` | (smoke) |
| 3.7 | `…::parent_cancel_propagates_through_three_level_chain` | (smoke) |
| 4.1 | `tests/fork_lightweight.rs::lightweight_fork_isolates_writes_ac4_1` | D1 |
| 4.2 | `tests/fork_persistent.rs::merge_back_persistent_reconciles_crdt_state_jj_gated` | D3 |
| 4.3 | `tests/fork_merge_lightweight.rs::merge_back_imports_fork_write_ac4_3` | D2 |
| 4.4 | `tests/fork_persistent.rs::merge_back_persistent_reconciles_crdt_state_jj_gated` | D3 |
| 4.5 | `tests/fork_discard.rs::discard_drops_child_state_does_not_propagate_ac4_5` + `already_resolved_error_is_displayable` | (smoke) |
| 4.6 | `tests/fork_persistent.rs::persistent_discard_round_trip_jj_gated` | D5 |
| 4.7 | `tests/fork_promote.rs::promote_lightweight_with_flag_creates_draft` + `promote_draft_*` server tests | D6, D8 |
| 4.8 | `tests/fork_promote.rs::promote_without_flag_is_capability_denied` | D7 |
| 4.9 | `tests/fork_merge_lightweight.rs::merge_back_convergence_all_appends_survive` (proptest) + `…::diamond_concurrent_edit_merges_both_sides_ac4_9` | (proptest only) |
| 4.10 | `tests/fork_persistent.rs::fork_bookmark_name_format_is_agent_slash_task` + `handle_fork_returns_bookmark_conflict_when_bookmark_exists_i5` | D3 |
| 5.1 | `tests/sibling_spawn.rs::ac5_1_existing_persona_adoption_returns_ok` | A6 (existing-id path) |
| 5.2 | `tests/sibling_spawn.rs::ac5_2_new_sibling_with_spawn_new_identities_writes_draft` + `tests/sibling_autoregister.rs::ac5_5_new_identity_active_registers_with_relationship` | A6 |
| 5.3 | `tests/sibling_spawn.rs::ac5_3_new_sibling_without_flag_writes_draft_no_live_session` + `tests/sibling_autoregister.rs::ac5_7_new_identity_without_flag_registers_as_draft` | A7 |
| 5.4 | `tests/sibling_spawn.rs::ac5_4_capabilities_come_from_sibling_own_config` | (smoke) |
| 5.5 | `tests/sibling_autoregister.rs::ac5_5_*` | A8 |
| 5.6 | `tests/sibling_spawn.rs::ac5_6_unknown_persona_id_returns_persona_not_found` + `tests/sibling_resolver_constellation.rs::constellation_resolver_reports_unknown_id_as_persona_not_found` | B3 |
| 5.7 | `tests/sibling_autoregister.rs::ac5_7_*` + `pattern_server` promote-draft tests | A7, A8 |
| 6.1 | `agent_registry::tests::route_or_queue_active_delivers_message` + `active_sender_delivers_message` | (chat) |
| 6.2 | `agent_registry::tests::register_active_replays_queued_draft_messages` (busy-agent semantics covered by Active+Draft swap) | A8 (queue → drain on promote) |
| 6.3 | `sdk::handlers::message::tests::delegate_pins_task_block_ref_in_message` | (delegation in supervisor session) |
| 6.4 | `agent_registry::tests::queue_for_draft_unknown_persona_returns_persona_not_found` + `sdk::handlers::message::tests::send_to_nonexistent_agent_produces_router_error_prefix` | "supervisor-delegation negative case" in Phase E |
| 6.5 | `agent_registry::tests::route_or_queue_draft_queues_message` + `router::agent::tests::draft_persona_mailbox_is_not_triggered` | A7 |
| 6.6 | `agent_registry::tests::route_or_queue_active_swap_does_not_lose_messages` (I-4 regression) + `tests/agent_registry_promote_race.rs` | (heavy probe under load) |
| 7.1 | `wake::rust_primitives::tests::task_timeout_fires_after_deadline` | F2 |
| 7.2 | `wake::block_changed::tests::block_change_fires_wake` | F3 |
| 7.3 | `tests/wake_task_dep.rs::task_dep_resolved_fires_on_completion` | (manual via task block) |
| 7.4 | `wake::rust_primitives::tests::interval_fires_repeatedly` | F1 |
| 7.5 | `tests/wake_handler_capability.rs::register_without_capability_is_denied` | E5 |
| 7.6 | `wake::rust_primitives::tests::multiple_conditions_fire_independently` | F4 |
| 7.7 | (queueing semantic, exercised through `WakeRegistry` mailbox plumbing) | F6 |
| 8.1 | `pattern_db::queries::fronting::tests::round_trip_save_and_load` + `tests/fronting_supervisor.rs::fronting_set_survives_restart` | C8 |
| 8.2 | `fronting_dispatch::tests::rule_match_routes_to_target` + `tests/fronting_supervisor.rs::supervisor_pattern_routes_messages_correctly` | C4 |
| 8.3 | `fronting_dispatch::tests::fallback_receives_unmatched_message` | C5 |
| 8.4 | (direct-delivery via `@persona-name` parsing in router) | C3 |
| 8.5 | `fronting_dispatch::tests::fan_out_delivers_to_all_active` | C6 |
| 8.6 | `pattern_core::permission::tests::partner_origin_short_circuits_without_broadcast` + `non_partner_origins_broadcast_normally` (covers Author author-tag distinctions); agent_loop.rs:720 BatchType mapping per Author variant | C1 (visible "fronting:" indicator implies origin tagging works) |
| 8.7 | `pattern_core::permission::tests::partner_origin_short_circuits_without_broadcast` (human-as-caller short-circuit) + `tests/fronting_supervisor.rs` | C1 |
| 8.8 | `fronting_dispatch::tests::rule_update_applies_to_subsequent_dispatches` (I-3) + `…::in_flight_routing_uses_snapshot_at_dispatch_time` | C7 |
| 9.1 | `pattern_db::queries::constellation::tests::list_all_returns_all_seeded_personas` + `tests/constellation_sdk.rs::list_dispatches_through_registry_with_three_personas` + `pattern_server::tests::constellation_rpc::list_personas_returns_seeded_records` | A3 |
| 9.2 | `…::find_by_project_and_kind_filters_correctly` + `tests/constellation_sdk.rs::find_with_supervisor_of_kind_dispatches` | A4–A5 |
| 9.3 | `…::list_project_filters_via_json_each` + `tests/constellation_sdk.rs::list_with_project_filter_dispatches` | A3 (project filter UI) |
| 9.4 | `tests/sibling_autoregister.rs::ac5_5_existing_sibling_adds_relationship_edge` | A6 |
| 9.5 | `…::list_project_unknown_path_returns_empty_not_error` | "Negative cases for Phase A" |
| 9.6 | `tests/constellation_sdk.rs::missing_registry_returns_not_wired_marker` (Draft visibility tested via `agent_registry` Draft slot) | A7 |
| 10.1 | `tests/multi_agent_smoke.rs::multi_agent_smoke` | G |
| 10.2 | `tests/support/multi_agent_scripts.rs` (mock provider) | G |
| 10.3 | `tests/multi_agent_smoke.rs::multi_agent_smoke` step 6 | G |
| 10.4 | `tests/multi_module_sdk.rs` + smoke step 4 | G |
| 10.5 | smoke step assertion-message contract | G |
| 10.6 | smoke unique-tempdir + nextest parallel | G |

---

## Eyeball-only checks

These have no automated coverage but are quick visual confirmations a
human can do during the walkthrough:

- [ ] **Constellation panel rendering** (Phase A3, A6, A7): personas
      render with the right status indicator (Active vs Draft vs
      Inactive); relationship edges are displayed; the panel updates
      live as personas spawn.
- [ ] **Fronting indicator chrome** (Phase C1): the active persona is
      visible in TUI chrome (status line, header, or wherever the
      design lands); changes when fronting is updated.
- [ ] **Sibling spawn UX** (Phase A6, A7): the partner-visible flow for
      "spawn a new sibling" clearly distinguishes between "Active"
      (with `SpawnNewIdentities`) and "Draft" (without). The Draft case
      should explicitly explain the persona requires `/promote` before
      it can step.
- [ ] **Promote-draft warning surfacing** (D8): when a seed-cache
      version mismatch occurs, the warning text appears in the partner-
      visible response, not just in server logs. (Cycle-2 M-1 fix.)
- [ ] **Permission broker prompt** (E8): when policy hits
      `RequireApproval`, the partner sees a TUI prompt with the
      command/path/scope clearly named; approving / denying / timing
      out all produce sensible messaging.
- [ ] **Error message clarity** for `RegistryError::PersonaNotFound`,
      `CapabilityError::Denied`, and the sibling-resolver path: errors
      name the missing persona id / capability flag, not generic prose.

---

## Known follow-ups (not gaps)

- **Phase 8 deferrals** (documented in `pattern_runtime/CLAUDE.md`):
  `BlockChanged` triggers for custom wake evaluators, per-effect broker
  escalation, GFS-style log rotation, tidepool-effect version
  cross-check. Each is annotated with a `// FUTURE WORK (Phase 8+,
  2026-04-28)` comment. None block the v3-multi-agent acceptance.
- **Cold-build fs-watcher flakes** in `pattern_memory` (project memory
  note `project_pattern_memory_cold_build_flakes`): rerun warm to
  confirm not a regression.
- **Eval-worker priority queue** (project memory note
  `project_eval_worker_priority_queue`): Phase 7 Task 6 ships
  fresh-thread for simplicity; long-term shape is queue-on-session.
  Not a v3-multi-agent acceptance gap.

---

**Test plan owner:** the partner running this walkthrough.
**Escalation path:** if a manual step fails where the corresponding
automated test passes, treat it as a wiring/integration regression
(not an AC gap) — the daemon, server RPC, or TUI is missing a hookup.
File against the appropriate phase plan with reproduction steps.
