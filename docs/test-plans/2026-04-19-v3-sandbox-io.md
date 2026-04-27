# v3-sandbox-io human test plan

Merge-readiness verification for `docs/implementation-plans/2026-04-19-v3-sandbox-io/`.

The test-requirements doc declares all 46 ACs automatable, and they are.
This plan exists for the things automation cannot exhaustively check:
end-to-end behaviour under fresh-shell load, design-deviation ACs that
need a human eye on the rationale, and the daemon-side HTTP-port
registration that was the final-review critical fix.

Total runtime: ~10 min on a warm machine, ~15 min cold (first build).

---

## Setup (one-time per machine)

1. Worktree at `/home/orual/Projects/PatternProject/pattern-v3-sandbox-io`
   on change `yuttmrpoqyvlkmwsmsmzrtyzlktlrzkx` (HEAD of the branch).
2. Devshell active: `nix develop` (must print a `/nix/store/...`
   path for `which tidepool-extract`). If absent, see
   `crates/pattern_runtime/CLAUDE.md` § "Stale-harness troubleshooting".
3. Workspace builds clean: `cargo check --workspace`.
4. No `pattern` daemon running in the foreground (the production
   warning in the root `CLAUDE.md` applies).

If any setup step fails, stop. Don't move on to the per-AC procedures.

---

## Procedure

Each section names the AC(s) being verified, the exact command, the
acceptance criterion, and the timing/tolerance budget. Skip sections
only if a previous one failed and the failure is upstream of the skip
(e.g. setup broke).

### Step 1 — smoke test triple-run (AC5.1, AC5.2, AC5.3, AC5.6, AC5.7)

The single end-to-end test exercises shell + file + ports through the
agent loop. Three consecutive passes prove non-flakiness for merge.

```sh
cd /home/orual/Projects/PatternProject/pattern-v3-sandbox-io
for i in 1 2 3; do
  echo "── run $i ──"
  cargo nextest run -p pattern-runtime --test sandbox_io_smoke || break
done
```

**Pass criteria:** all 3 runs print `1 test run: 1 passed, 0 skipped`.
Each run takes ~55 s on the reference machine.

**Tolerance:** if a single run takes > 90 s, suspect tidepool-extract
re-resolution; check `which tidepool-extract` is unchanged. If a run
fails, the panic message names the step (`step N: ...`). That AC5.6
property is what makes diagnosis fast — record the step number before
re-running.

### Step 2 — handler integration suites (AC2.*, AC3.*, AC4.*)

Confirms the per-handler integration tests pass under parallel load,
including the design-deviation tests called out below.

```sh
cargo nextest run -p pattern-runtime \
  --test file_handler --test shell_handler --test port_handler
```

**Pass criteria:** `39 tests run: 39 passed`.
**Tolerance:** ~2 s wall-clock on a warm build.

### Step 3 — design-deviation review (AC3.7, AC4.7)

These ACs ship with deliberate departures from the original design plan
text. Read the rationale; confirm acceptance.

**AC3.7 — `execute_via_handler_timeout_kills_and_surfaces_error`.**
Open `crates/pattern_runtime/src/sdk/handlers/shell.rs` and grep for
`Amendment 2026-04-26`. The shipped behaviour is **timeout = kill**,
not "background and continue streaming via reminders" as the original
AC text and test-requirements.md described. The `Backgrounded` enum
variant in `MessageAttachment` is forward-compat scaffolding; no code
path emits it today.

**Pass criterion:** the amendment comment is present, the behaviour
matches (run `cargo nextest run -p pattern-runtime
execute_via_handler_timeout_kills_and_surfaces_error`), and you accept
the v2-parity rationale.

**AC4.7 — capability-denied port call.** The original AC asks for
"compile-time rejection in prelude" — i.e. agent Haskell that
references a port not in the capability set should fail to type-check.
The shipped tests verify this *compositionally*:

- `port_call_capability_denied_blocks_dispatch` (runtime denial path)
- `port_library_excluded_when_not_capable` (library not spliced into
  preamble when port is absent from caps)

Together these mean an agent's Haskell can't reference the missing
port (no library → no constructors in scope → type error) AND the
runtime denies the call if the agent somehow obtains a constructor.
There is no fixture-Haskell-file gate that compiles a denied program
and asserts a type error.

**Pass criterion:** you accept the compositional argument. If you
want a stricter compile-rejection gate, file it as follow-up work
rather than blocking merge.

### Step 4 — daemon-side HTTP port registration (AC5.1)

The final-review fix wired `with_runtime_ports` into BOTH the runtime
factory AND the daemon's session factory. Verify both call sites.

```sh
grep -n "with_runtime_ports" \
  crates/pattern_server/src/main.rs \
  crates/pattern_runtime/src/runtime.rs
```

**Pass criterion:** both files show one match each, neither falls
back to `PortRegistryImpl::new(...)` for the daemon-facing sessions.
The integration assertion lives in
`crates/pattern_runtime/src/port_registry/registry.rs::with_runtime_ports_registers_http_port`.

```sh
cargo nextest run -p pattern-runtime \
  port_registry::registry::tests::with_runtime_ports_registers_http_port
```

**Pass criterion:** 1 test, passed.

### Step 5 — Sources/Rpc cleanup (AC5.4, AC5.5)

```sh
grep -rn "DataStream\|SourceManager\|SourcesHandler\|RpcHandler" crates/
grep -rn "is not implemented" crates/pattern_runtime/src/sdk/handlers/ \
  | grep -v 'mcp\|spawn'
cargo nextest run -p pattern-runtime sdk::bundle::tests::canonical_decls_has_15_entries
```

**Pass criteria:**
- First grep returns at most ONE hit, the historical comment in
  `crates/pattern_runtime/tests/stub_effects.rs`. No source matches.
- Second grep returns zero matches. (Mcp and Spawn stubs are expected
  and explicitly filtered out.)
- Bundle test passes — the canonical row is exactly 15 entries
  (`Memory, Search, Recall, Tasks, Skills, Message, Display, Time, Log,
  Shell, File, Mcp, Spawn, Diagnostics, Port`).

### Step 6 — full crate test sweep (regression backstop)

```sh
cargo nextest run -p pattern-memory -p pattern-runtime
```

**Pass criterion:** all green. This is the safety net for "smoke test
passes but I broke something else" — slower than the targeted runs
(~3 min cold) but proves nothing else regressed.

---

## Failure-diagnosis matrix

| Symptom | First check | Likely cause |
|---------|-------------|--------------|
| Smoke test panics with no `step N:` prefix | Read the smoke source for step labels | New assertion missed `.with_context(\|\| ...)` — surface as AC5.6 regression. |
| Smoke fails at step 0 (registry register) | `grep with_runtime_ports crates/pattern_server/src/main.rs` | Daemon registry construction regressed; HTTP port double-registered. |
| Smoke fails at step 2 (FileEdit attachment) | `cargo nextest run -p pattern-runtime ac2_7_clean_external_edit_produces_file_edit_attachment` | DirWatcher → SessionContext drain wiring; check `crates/pattern_runtime/src/file_manager/manager.rs` `external_edit_produces_reminder` test. |
| Smoke fails at step 5 or 7 (PortEvent) | `cargo nextest run -p pattern-runtime port_subscribe_delivers_events_via_attachments` | Dispatcher actor / unsubscribe race. Check whether the test fails standalone too. |
| Smoke flaky across the 3 runs | `which tidepool-extract`; `direnv reload` | Stale `$TIDEPOOL_EXTRACT` symlink. Re-run setup step 2. |
| `0 tests run` from any nextest invocation | Filter typo. Verify the test name with `grep -n "^async fn\|^fn " <file>` | Misspelled test filter — nextest matches as substring; "no match" silently runs nothing. |
| Bundle test reports != 15 decls | `git diff -- crates/pattern_runtime/src/sdk/bundle.rs` | Someone added/removed an effect without updating the locked count test. |
| Sources/Rpc grep returns multiple hits | Inspect each hit — comments are fine, source isn't | Phase 4 Task 8 deletion incomplete. Stop and surface. |
| Smoke run > 90 s | `cargo build -p pattern-runtime --tests` first, retry | Cold target dir; not a regression. |
| `tidepool-extract: command not found` | `nix develop`; verify `which tidepool-extract` | Devshell not active. |

---

## Completion checklist

Tick each item only when its step has passed cleanly. Don't tick from
memory.

- [ ] Setup (devshell active, `cargo check --workspace` clean,
      no production daemon running).
- [ ] Step 1: 3 consecutive smoke runs all pass; each reports
      `1 test run: 1 passed, 0 skipped`.
- [ ] Step 2: 39/39 handler integration tests pass.
- [ ] Step 3: AC3.7 amendment reviewed and accepted; AC4.7
      compositional verification reviewed and accepted.
- [ ] Step 4: HTTP port registered in both `pattern_server` and
      `pattern_runtime`; `with_runtime_ports_registers_http_port`
      passes.
- [ ] Step 5: no Sources/Rpc residue; canonical-row count is 15.
- [ ] Step 6: full `pattern-memory` + `pattern-runtime` test sweep is
      green.

When every box is ticked, the branch is merge-ready against the
stated design plan.

---

## Notes on automated coverage

This plan complements (does not replace) the automated suites:

- 21 loro_sync unit tests (`crates/pattern_memory/src/loro_sync/tests.rs`,
  `dir_watcher.rs`)
- 117 runtime unit tests covering file_manager, port_registry,
  process_manager, sdk::preamble, sdk::bundle, ports::http
- 39 handler integration tests
- 1 end-to-end smoke test
- canonical-row gate, Sources/Rpc grep audit

Total automated coverage: 178 tests covering all 46 ACs. The human
plan's purpose is operator-facing merge confidence, not coverage gap
filling.
