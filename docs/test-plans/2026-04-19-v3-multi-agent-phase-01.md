# Human Test Plan — v3-multi-agent Phase 1 (Capability + Policy + Broker)

**Scope:** Phase 1 of 7. Covers AC1.1–AC1.6 (CapabilitySet + prelude
filtering) and AC2.1–AC2.9 (runtime approval + policy + broker).
Automated coverage is complete (see
`docs/test-plans/` coverage report companion output). This plan is for
human operators validating end-to-end feel, doc alignment, and
configuration error surfaces.

## Prerequisites

- Nix devshell active: `nix develop` (provides `tidepool-extract`,
  `TIDEPOOL_EXTRACT` env).
- Clean `cargo` state: `cargo fmt --check && cargo clippy
  --all-features --all-targets` green.
- Full test suite passing:
  - `cargo nextest run -p pattern-core capability permission`
    (30 tests).
  - `cargo nextest run -p pattern-runtime --tests` (448 tests).
  - `cargo test --doc` green.
- **Do NOT run the `pattern` CLI against the live data dir** — production
  agents may be running. Use a scratch `TMPDIR` for every manual run.

## Phase A — Configuration sanity

Verify the KDL schema surfaces parse errors clearly and that a happy-path
persona with `capabilities {}` / `policy {}` blocks loads and shapes the
session correctly.

| Step | Action | Expected |
|------|--------|----------|
| A.1 | Create `/tmp/phase1-cap/persona.kdl` with a valid `capabilities { effects { - "memory" \n - "message" } flags { - "spawn-new-identities" } }` block | File writes without issue |
| A.2 | Create `/tmp/phase1-cap/bad-effect.kdl` that lists `effects { - "teleport" }` | File writes |
| A.3 | In a fresh Rust unit or a scratch binary, call `pattern_runtime::persona_loader::load_persona_kdl("/tmp/phase1-cap/bad-effect.kdl")` | Returns a `miette::Report`-style error whose rendered span points at the `"teleport"` argument and names the valid effect set |
| A.4 | Repeat A.3 with a `flags { - "wake-aliens" }` block | Rendered error points at the bad flag, lists `spawn-new-identities / wake-condition-registration / fronting-control` |
| A.5 | Call `load_persona_kdl(...)` on the **valid** file from A.1 | Parses successfully; resulting `PersonaSnapshot.capabilities` holds `Some(CapabilitySet)` with exactly `{Memory, Message}` categories and `{SpawnNewIdentities}` flag |
| A.6 | Valid file with policy block: `policy { rule "allow-git-push" effect="shell" action="allow" { matcher "shell-command" pattern="git push*" } }` | Parses; `PersonaSnapshot.policy_rules` has one `PolicyRule` with `Precedence::KdlConfig` |

**Why manual:** tests already cover valid/invalid parse paths
programmatically; this phase confirms the miette/knus error rendering
stays human-readable when developers edit real KDL files.

## Phase B — Shell gate smoke

Verify that an agent program invoking `Shell.execute` against the real
session machinery (Shell handler + PermissionBroker + bridge) surfaces
the correct markers and messages in tracing output.

| Step | Action | Expected |
|------|--------|----------|
| B.1 | In Nix devshell, spin up a test-only harness session: construct `TidepoolSession::open_with_agent_loop(persona, ...)` with `persona.policy_rules = rust_defaults()` and no override. Use `MockProviderClient::with_turns(...)` to script a single `tool_use(shell, "rm -rf /tmp/testdir")` turn | Session opens; agent loop drives one turn |
| B.2 | Observe the `TurnEvent::ToolResult` that reaches the `VecSink` | `content` is a `serde_json::Value::String` containing `"PermissionDenied: "` prefix (no responder is wired, broker times out to denial) |
| B.3 | Wire a broker subscriber that resolves every request with `PermissionDecisionKind::ApproveOnce` and rerun B.1 | `ToolResult` contains `"GateApproved:"` prefix; message mentions "not implemented in v3 foundation" |
| B.4 | Change the scripted command to `"ls"` (not matched by `rust_defaults`) | `ToolResult` is the plain `"Pattern.Shell.Execute is not implemented"` stub error — NO `GateApproved:` marker (proves gate skipped cleanly, not over-firing) |

## Phase C — File.Write locked-default smoke

Verify the config-KDL shape guard genuinely blocks writes even when
policy would loosen them.

| Step | Action | Expected |
|------|--------|----------|
| C.1 | Construct a persona whose policy is `[PolicyRule::new(File, FilePath{pattern:"*"}, Allow, RuntimeOverride)]` — the most permissive possible | Session opens |
| C.2 | Script a turn with `tool_use(file_write, "/tmp/.pattern.kdl", "mount mode=\"A\"\n")` and no broker responder | `ToolResult` surfaces `PermissionDenied:` prefix (broker times out). This proves the shape guard short-circuited before policy evaluation |
| C.3 | Add a responder that responds `Deny` and rerun | Same `PermissionDenied:` prefix; broker observed exactly one request with scope `PermissionScope::FileWrite { path: "/tmp/.pattern.kdl" }` |
| C.4 | Script two sequential writes: `("/tmp/.pattern.kdl", "")` then `("/tmp/other.kdl", "mount mode=\"B\"\n")` — both are config-shape matches. Responder approves for `jiff::Span::new().minutes(5)` on both | Broker observes exactly 2 prompts (one per distinct path); handler surfaces `GateApproved:` on both |
| C.5 | Script a write to `"/tmp/notes.txt"` (non-config) with empty policy | `ToolResult` is `GateApproved:` marker with NO broker request observed — proves the guard only fires on config shapes |

**Why manual:** every step has an automated test equivalent
(`config_kdl_write_escalates_to_broker_and_can_be_denied`,
`config_kdl_write_locked_against_kdl_allow_all`,
`config_kdl_write_locked_against_runtime_override_allow`,
`approve_for_duration_caches_per_path`,
`non_config_write_does_not_escalate`). This phase confirms the real
session machinery (vs direct handler invocation in the unit tests)
produces the same markers, catching any regression in the SDK bundle
or router wiring.

## Phase D — Documentation alignment

Verify the locked-default semantics and partner-bypass contract are
documented consistently with the code.

| Step | Action | Expected |
|------|--------|----------|
| D.1 | Read `crates/pattern_core/src/permission.rs` module docstring | Calls out that `scope_cache` is intentionally session-lifetime and must not gain a persist path |
| D.2 | Grep for `"partner_bypass"` literal | Appears in `PermissionGrant::synthesized_partner` metadata AND in tests that assert on it (`partner_origin_short_circuits_without_broadcast` in permission.rs; `partner_origin_short_circuits_at_handler_level` in shell.rs and file.rs) |
| D.3 | Read the phase_01.md narrative on dispatch-origin-vs-turn-origin (Task 7 section) | Matches `agent_loop::drive_step`: origin installed per-iteration is `Author::Agent { agent_id: … }` (NOT the activating Partner) |
| D.4 | Read `crates/pattern_runtime/src/sdk/handlers/file.rs` handler comment | States that `PolicySet` is NOT consulted for config-shape writes — handler short-circuits to broker directly. Matches Task 12 plan |
| D.5 | Open `docs/design-plans/2026-04-19-v3-multi-agent.md` §AC2.7 | Design spec prose for AC2.7 still matches the handler-level enforcement (NOT a `LockedDefault` precedence tier). Confirms phase_01.md's mid-execution revision is reflected in the design doc, or at least not contradicted by it |

## Phase E — Partner-bypass direct execution

Verify the Partner-bypass predicate is wired and inert during normal
model-driven turns.

| Step | Action | Expected |
|------|--------|----------|
| E.1 | Open a session; script a turn that reaches `File.Write("/tmp/.pattern.kdl", "")` via tool_use (normal agent loop path) | `drive_step` installs `Author::Agent(...)` origin; broker gates normally (Deny or Approve via responder). NO Partner bypass fires |
| E.2 | Construct an `EffectContext` directly — bypassing `drive_step` — with `user.origin = Some(Partner origin)` and invoke `FileHandler::handle(Write("/tmp/.pattern.kdl", ""), ...)` | Handler returns `GateApproved:` marker WITHOUT broadcasting to the broker (verified in `partner_origin_short_circuits_at_handler_level`) |
| E.3 | Confirm Phase 1 ships no code path that sets a Partner origin during the normal agent loop | Grep for `Author::Partner(` in `crates/pattern_runtime/src/agent_loop/` — should find no callsite that installs Partner origin into `current_dispatch_origin`. Only tests and the helper type appear |

## Phase F — Two-broker independence (AC2.9 cross-check)

Verify two concurrently-open sessions have independent broker state
when driven by real traffic.

| Step | Action | Expected |
|------|--------|----------|
| F.1 | Open two sessions in the same process (`TidepoolSession::open_with_agent_loop(...)` twice) with distinct `agent_id`s | Each session has its own `PermissionBroker` Arc (check `ctx.permission_broker()` identity with `Arc::ptr_eq`) |
| F.2 | In session A, drive a `File.Write("/tmp/.pattern.kdl", "")` turn with a responder that approves-for-scope | Broker A's scope cache is populated; session A's next write to the same path skips the broker |
| F.3 | In session B, drive the identical `File.Write`. Do NOT wire a responder | Broker B has an empty cache; request times out; handler surfaces `PermissionDenied:` |

## End-to-End: Capability-scoped program lifecycle (AC1.2 composition)

**Purpose:** prove the capability set truly flows all the way from
`PersonaSnapshot.capabilities` → `from_persona` → session preamble →
Tidepool compiler error.

**Steps:**

1. Author `crates/pattern_runtime/tests/fixtures/cap_scoped_persona.kdl`
   with `capabilities { effects { - "memory" \n - "message" } }`.
2. Programmatically load the persona via `load_persona_kdl`.
3. Open a session via `TidepoolSession::open_with_agent_loop(persona,
   ..., Some(persona.capabilities.clone().unwrap()))`.
4. Compose a Haskell source by reading `session.preamble()` and
   appending a body that calls `Shell.execute "echo hi"`.
5. Call `tidepool_runtime::compile_and_run` on that source.
6. **Expected:** compile fails; error string contains "scope" /
   "not in scope" / "undefined" / "unknown" / "shell" (implementation
   lets upstream Tidepool phrasing drift).
7. Swap the body to call `Memory.put "k" "v"` and rerun.
8. **Expected:** compiles and runs; `EvalResult::into_value()` is a unit
   constructor.

This is the integration test `excluded_effect_fails_at_tidepool_compile`
/ `permitted_effects_compile_and_run` reproduced manually so an operator
can see the Tidepool error text first-hand.

## Human Verification Required

None of the acceptance criteria require manual verification — every AC1.x
/ AC2.x case has automated coverage. The manual phases above are
defense-in-depth:

| Area | Why Manual | Steps |
|------|-----------|-------|
| KDL parse error rendering | miette/knus rendering stays readable only if a human reads it | Phase A.3 / A.4 |
| Documentation/code alignment | Docs drift in ways tests cannot detect | Phase D |
| Session-level wiring (vs direct handler) | Catches SDK bundle / router-registry regressions that unit tests miss | Phase B, C, F |
| Upstream Tidepool error text drift | Error wording is not an AC; an operator inspection catches silent degradation | E2E step 6 |

## Traceability

| AC | Automated Test | Manual Step |
|----|----------------|-------------|
| AC1.1 | `filtered_decls_excludes_absent_categories`, `build_for_minimal_capability_set_excludes_filtered_imports` | E2E step 4 (preamble inspection) |
| AC1.2 | `excluded_effect_fails_at_tidepool_compile` | E2E step 5–6 |
| AC1.3 | `permitted_effects_compile_and_run` | E2E step 7–8 |
| AC1.4 | `build_for_full_capability_set_matches_unfiltered_build` | (none — structural) |
| AC1.5 | `restrict_to_err_when_adding_*` (three tests) | (none — pure data) |
| AC1.6 | `build_for_empty_capability_set_produces_pure_computation_prelude` | (none) |
| AC2.1 | `deny_action_returns_permission_denied_prefix`, `require_approval_with_*_bridge_returns_*` | Phase B.2 / B.3 |
| AC2.2 | `ac2_2_persona_kdl_allow_reaches_shell_handler_and_skips_broker` | Phase A.6 (parse) + Phase B (runtime) |
| AC2.3 | `ac2_3_persona_kdl_require_approval_reaches_file_handler_and_invokes_broker` | Phase C |
| AC2.4 | `approve_once_does_not_populate_cache` | (none — deterministic) |
| AC2.5 | `approve_for_scope_caches_subsequent_requests` | (none) |
| AC2.6 | `approve_for_duration_expires_via_injected_clock` | (none — clock-injected) |
| AC2.7 | `config_kdl_write_escalates_to_broker_and_can_be_denied`, `config_kdl_write_locked_against_kdl_allow_all`, `config_kdl_write_locked_against_runtime_override_allow`, `approve_for_duration_caches_per_path`, `non_config_write_does_not_escalate` | Phase C |
| AC2.8 | `timeout_path_cleans_up_pending_state` | (none — deterministic) |
| AC2.9 | `two_brokers_have_independent_state`, `per_agent_scope_grants_do_not_cross_pollinate` | Phase F |

## Sign-off

- [ ] Phase A: KDL parse errors are readable and accurate.
- [ ] Phase B: Shell gate markers observed under live session.
- [ ] Phase C: File.Write locked-default holds under live session.
- [ ] Phase D: Docs, module comments, and design plan are mutually consistent.
- [ ] Phase E: Partner-bypass is wired but inert during normal turns.
- [ ] Phase F: Two-broker independence holds in the real session machinery.
- [ ] End-to-end walkthrough verifies Tidepool compile-time rejection.
- [ ] `cargo nextest run` full suite green.
- [ ] `cargo test --doc` green.
- [ ] `just pre-commit-all` green.
