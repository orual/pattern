# v3-sandbox-io Test Requirements

Each acceptance criterion in the design plan maps below to either an
automated test (with file path, test type, and expected name) or a
documented human-verification step (with justification).

Source: `docs/design-plans/2026-04-19-v3-sandbox-io.md` (46 ACs).
Test names are taken verbatim from the implementation plan tasks
(Phase 1 Task 5, Phase 2 Task 10, Phase 3 Task 9, Phase 4 Task 9, Phase 5 Task 4).

---

## AC1: LoroSyncedFile infrastructure (Phase 1)

All AC1 tests are unit tests in-crate at `crates/pattern_memory/src/loro_sync/tests.rs`,
exercising `LoroSyncedFile::open` (standalone mode) over real tempfiles + real notify.
No Plan 3 prerequisite.

### v3-sandbox-io.AC1.1 Success: open seeds doc + starts watcher
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `open_seeds_doc_and_starts_watcher`
- **Mechanism:** Write `"hello"` to tempfile; `LoroSyncedFile::open` succeeds; `read()` returns `"hello"`; external edit fires `subscribe_external_changes` event.

### v3-sandbox-io.AC1.2 Success: write updates doc + disk
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `write_updates_doc_and_disk`
- **Mechanism:** Open empty tempfile; `write("agent content")`; disk content and `read()` both match.

### v3-sandbox-io.AC1.3 Success: external edit merges into doc
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `external_edit_merges_into_doc`
- **Mechanism:** Open tempfile `"abc\n"`; agent `write("abcXYZ\n")`; external `std::fs::write` `"abc\ndef\n"`; wait for merge; assert final content has both `XYZ` and `def`.

### v3-sandbox-io.AC1.4 Success: self-emit echo suppressed
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `self_echo_is_suppressed`
- **Mechanism:** Subscribe; `write("once")`; wait 750ms; assert no event arrived (mtime + hash dedupe).

### v3-sandbox-io.AC1.5 Success: close drops watcher + doc
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `close_drops_watcher_and_doc`
- **Mechanism:** Open, close, external edit after close; wait; verify subscribe receiver disconnected (no panics; double-close is no-op).

### v3-sandbox-io.AC1.6 Failure: nonexistent file → NotFound
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `open_nonexistent_returns_not_found`
- **Mechanism:** `LoroSyncedFile::open("/tmp/nope-<rand>")` → `Err(LoroSyncError::NotFound(_))`.

### v3-sandbox-io.AC1.7 Edge: concurrent edits to different regions merge cleanly
- **Type:** Automated, unit.
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `concurrent_edits_different_regions_merge`
- **Mechanism:** Tempfile with three lines; agent edits line1; external edits line3; both EDITED tokens preserved post-merge.

### v3-sandbox-io.AC1.8 Edge: concurrent edits to same region — deterministic LWW per position
- **Type:** Automated, unit (insta snapshot).
- **File:** `crates/pattern_memory/src/loro_sync/tests.rs`
- **Test:** `concurrent_edits_same_region_lww_per_position_deterministic`
- **Mechanism:** Agent writes `"aXcdef"`, external writes `"abcdYf"`; assert deterministic merge result via `insta::assert_snapshot!` (regression lock against loro version drift).

---

## AC2: File handler (Phase 2)

**Requires:** Plan 3 Phase 1 landed (`CapabilitySet`, per-instance `PermissionBroker`).

Integration tests live at `crates/pattern_runtime/tests/file_handler.rs`;
FileManager-level unit tests in `crates/pattern_runtime/src/file_manager/manager.rs`.
Policy unit tests in `crates/pattern_runtime/src/file_manager/policy.rs`.

### v3-sandbox-io.AC2.1 Success: Read does not open a LoroDoc
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `read_does_not_open_loro`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `fm.read(path)`; external `std::fs::write`; wait 750ms; assert `session.drain_async_reminders()` empty.

### v3-sandbox-io.AC2.2 Success: Open returns content + subscribes
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `open_returns_content_and_subscribes`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `fm.open` content matches disk; external edit → `session.drain_async_reminders()` non-empty; body contains path + `you had open`.

### v3-sandbox-io.AC2.3 Success: Write on open file goes through loro
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `write_on_open_file_goes_through_loro`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Open + `fm.write("new")` with concurrent external edit → both preserved (delegates to AC1.3 mechanism). Write on un-opened file: direct `atomic_write`, no loro.

### v3-sandbox-io.AC2.4 Success: Close drops watcher
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `close_drops_watcher`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Open, close, external edit; wait; `session.drain_async_reminders()` empty.

### v3-sandbox-io.AC2.5 Success: List with glob
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `list_with_glob`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Tempdir with `a.rs`, `b.py`, `c.rs`; `fm.list(dir, "*.rs")` returns 2 entries.

### v3-sandbox-io.AC2.6 Success: Watch subscribes without LoroDoc
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Tests:** `watch_does_not_create_loro` and `watcher_pooling_shares_dir_watchers` (pool correctness).
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `fm.watch`; external edit; reminder body contains `you were watching`; `fm.open_files` does not contain path; `fm.watch_only_paths` does. Pooling test: open 3 files in same dir; exactly 1 `dir_watchers` entry; close 2, still 1; close last, GC'd.

### v3-sandbox-io.AC2.7 Success: external edit on open file → attachment on next turn
- **Type:** Automated, integration (full SessionContext + agent_loop drain + Segment2Pass render).
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `external_edit_on_open_file_becomes_attachment`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Open session + persona; agent `Pattern.File.Open(path)`; external `std::fs::write`; advance one turn; assert next turn's first user message has `MessageAttachment::FileEdit { path, kind: Open, diff: Some(_), .. }`.

### v3-sandbox-io.AC2.8 Failure: write outside allowed directories → PermissionDenied
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `write_outside_rules_denied`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Policy `allow /project/**`; `fm.write("/etc/passwd", ...)` → `FileError::PermissionDenied { reason: "no matching rule (default deny)" }`.

### v3-sandbox-io.AC2.9 Failure: config-KDL write requires human approval
- **Type:** Automated, integration (scripted broker).
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `config_write_triggers_broker`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Content parses as pattern config KDL. Scripted broker auto-approves → write succeeds; scripted broker denies → `FileError::ConfigApprovalDenied`.

### v3-sandbox-io.AC2.10 Edge: ordered KDL deny rule beats allow
- **Type:** Automated, integration (3 scenarios in one test).
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `ordered_rules_last_match_wins`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** (a) `allow /project/**` then `deny /project/.env` → `.env` denied; (b) `deny /project/**` then `allow /project/notes/*.md` → `notes/foo.md` allowed; (c) nested re-allow inside deny. Denial reason names the losing rule.
- **Note:** Plan also has policy unit tests in `crates/pattern_runtime/src/file_manager/policy.rs`: `last_match_wins_allow_then_deny`, `last_match_wins_deny_then_allow`, `nested_re_allow_inside_re_deny`, `default_deny_when_no_rules`, `default_deny_when_no_match`, `invalid_glob_fails_loudly`, `canonicalisation_resists_dotdot_escape`, `kdl_round_trip_preserves_order`.

### v3-sandbox-io.AC2.11 Edge: snapshot serializes open paths; restore re-opens with fresh LoroDoc
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/file_handler.rs`
- **Test:** `snapshot_restores_open_files`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Open two files → snapshot → drop session → restore → both files open and readable. Plus serde round-trip unit test in `crates/pattern_core/src/types/snapshot.rs`.

---

## AC3: Shell handler (Phase 3)

**Requires:** Plan 3 Phase 1 landed (`CapabilitySet`, `cap.has_shell()`).

Integration tests at `crates/pattern_runtime/tests/shell_handler.rs`;
PTY-level unit tests in `crates/pattern_runtime/src/process_manager/local_pty.rs`
(ported from v2 reference at `rewrite-staging/runtime_subsystems/data_source/process/tests.rs`).

### v3-sandbox-io.AC3.1 Success: Execute returns output + exit code
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `execute_returns_output_and_exit_code`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `pm.execute(cap, "echo hello", 30s)` → output `"hello\n"`, exit_code `Some(0)`, duration_ms reasonable.

### v3-sandbox-io.AC3.2 Success: Execute auto-spawns + reuses session
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `execute_auto_spawns_then_reuses_session`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** First execute initialises session; second reuses (cwd cache hit). Verify with two `pwd` calls.

### v3-sandbox-io.AC3.3 Success: Spawn returns id + streams output via reminders
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `spawn_streams_output_via_attachments`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Spawn `for i in 1 2 3; do echo line$i; sleep 0.05; done`; wait one turn boundary; assert next turn's first user message has `MessageAttachment::ShellOutput { kind: ShellOutputKind::Output(text), .. }` for `line1`/`line2`/`line3` plus an `Exit` entry.

### v3-sandbox-io.AC3.4 Success: Kill terminates process; Status reflects it
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `kill_terminates_running_process`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Spawn `sleep 60`; immediately `kill(task_id)`; verify `status()` no longer lists task; verify broadcast `Exit` chunk arrives.

### v3-sandbox-io.AC3.5 Success: Status lists running tasks
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `status_lists_running_tasks`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Spawn two long-running processes; assert `status()` returns both task IDs.

### v3-sandbox-io.AC3.6 Success: cwd persists across executions
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `cwd_persists_across_executions`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `execute("cd /tmp")` then `execute("pwd")` → output contains `/tmp`.

### v3-sandbox-io.AC3.7 Failure: Execute timeout → backgrounded (not killed)
- **Type:** Automated, integration (two tests).
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Tests:** `execute_timeout_backgrounds_not_kills` and `execute_timeout_emits_backgrounded_sentinel`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `execute("sleep 2 && echo done", 1s)` returns `ExecuteResult { exit_code: None, backgrounded_as: Some(task_id), .. }` quickly; `pm.status()` lists it; later, bridge enqueues `ShellOutput` containing `done` + `Exit`. Sentinel test: immediately after call, `drain_async_reminders()` has `ShellOutput { kind: Backgrounded { partial_output }, .. }`.
- **Note:** Plan deliberately diverges from AC3.7's "killed" wording (Claude Code parity) — see Phase 3 Q1 resolution. Surface as design-decision deviation, not gap.

### v3-sandbox-io.AC3.8 Failure: Kill nonexistent → ProcessNotFound
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `kill_unknown_task_returns_error`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `kill(TaskId("not-a-real-id"))` → `ShellError::UnknownTask("not-a-real-id")`.
- **Note:** Plan reuses v2's `UnknownTask` instead of renaming to `ProcessNotFound` — see Phase 3 Q3.

### v3-sandbox-io.AC3.9 Edge: OSC marker exit-code parser resists injection
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Test:** `exit_code_parser_resists_injection`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Run command whose output contains `__PATTERN_EXIT_deadbeef__:1`; nonce is unique per call so spurious string does not match; verify exit_code is the actual exit code.

### v3-sandbox-io.AC3.10 Edge: process output logged to file as backstop
- **Type:** Automated, integration + unit.
- **Integration file:** `crates/pattern_runtime/tests/shell_handler.rs`
- **Integration test:** `process_output_logged_to_file`
- **Unit file:** `crates/pattern_runtime/src/process_manager/logger.rs`
- **Unit tests:** `appends_output_lines`, `appends_exit_record`, `flush_persists_after_drop`, `concurrent_appends_dont_interleave`
- **Requires:** Plan 3 Phase 1 landed (integration only; unit tests have no Plan 3 dep).
- **Mechanism:** Spawn process with known output; wait for completion; read `<cache_dir>/shell/<task_id>.log`; verify each output line + EXIT line are present.

---

## AC4: Port trait + registry (Phase 4)

**Requires:** Plan 3 Phase 1 landed (`CapabilitySet` with **per-port granularity** —
Phase 4 Task 6 has explicit prereq check; if Plan 3 only exposes `has_port_effect()`,
AC4.7/4.9 cannot be verified).

Integration tests at `crates/pattern_runtime/tests/port_handler.rs` using `MockPort`
helper (in `crates/pattern_runtime/src/testing/mock_port.rs`).
Registry CRUD unit tests in `crates/pattern_runtime/src/port_registry/registry.rs`.

### v3-sandbox-io.AC4.1 Success: Port trait defined with required methods
- **Type:** Automated, doctest.
- **File:** `crates/pattern_core/src/traits/port.rs`
- **Test:** Module doctest showing `Dummy` Port impl (mirrors v2 DataStream doctest).
- **Requires:** None (Plan 3 not needed for trait shape).
- **Mechanism:** Doctest compiles → trait shape verified.

### v3-sandbox-io.AC4.2 Success: PortRegistry resolves ports; List returns metadata
- **Type:** Automated, unit + integration.
- **Unit file:** `crates/pattern_runtime/src/port_registry/registry.rs`
- **Unit tests:** `register_then_get_returns_port`, `register_duplicate_fails_with_already_registered`, `unregister_removes_entry`, `list_returns_all_metadata`
- **Integration file:** `crates/pattern_runtime/tests/port_handler.rs`
- **Integration test:** `port_list_returns_registered_metadatas`
- **Requires:** Plan 3 Phase 1 landed (integration only).
- **Mechanism:** Register 3 MockPorts; `Port.List` returns 3 entries.

### v3-sandbox-io.AC4.3 Success: Call dispatches to correct port impl
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_call_dispatches_to_registered_port`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** MockPort with `call_response = {"ok": true}`; `Port.Call("mock", "ping", "{}")` returns response.

### v3-sandbox-io.AC4.4 Success: Subscribe delivers events as system reminders
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_subscribe_delivers_events_via_attachments`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Subscribe; push 3 events via MockPort tx; await scheduler tick + one turn boundary; assert next turn's first user message has 3 `MessageAttachment::PortEvent { port_id, payload, .. }` matching pushed events.

### v3-sandbox-io.AC4.5 Success: Unsubscribe stops event delivery
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_unsubscribe_stops_event_delivery`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** Subscribe + push 1 event + drain. Unsubscribe + push another event + tick + drain — second event NOT present (AbortHandle stopped the task).

### v3-sandbox-io.AC4.6 Success: library() compiled into prelude when port in CapabilitySet
- **Type:** Automated, unit.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_library_appended_to_preamble_when_capable`
- **Requires:** Plan 3 Phase 1 landed (per-port granularity).
- **Mechanism:** MockPort with `library_src = Some("module Mock where mockFn = ...")`; build preamble with capability granted; assert preamble contains `mockFn`.
- **Supporting unit tests** (in `crates/pattern_runtime/src/sdk/preamble.rs`): `library_appended_when_provided`, `no_library_block_when_empty`, `multiple_libraries_each_get_header`.

### v3-sandbox-io.AC4.7 Failure: Call to port not in CapabilitySet → effect constructors absent (compile-time)
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_call_capability_denied_blocks_dispatch`
- **Requires:** Plan 3 Phase 1 landed (per-port granularity).
- **Mechanism:** Capability set without port; `Port.Call("mock", ...)` returns `PortError::CapabilityDenied`.
- **Note:** Plan verifies the runtime-side denial path. AC4.7's "compile-time rejection in prelude" is verified compositionally with AC4.9 (library exclusion) plus the dispatch denial — agent code referencing the missing constructor cannot type-check because the library was never spliced in.

### v3-sandbox-io.AC4.8 Failure: Call to unregistered PortId → NotFound
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_call_unknown_port_returns_not_found`
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** `Port.Call("does-not-exist", ...)` → `PortError::NotFound`.

### v3-sandbox-io.AC4.9 Edge: library excluded from prelude when port not in CapabilitySet
- **Type:** Automated, integration.
- **File:** `crates/pattern_runtime/tests/port_handler.rs`
- **Test:** `port_library_excluded_when_not_capable`
- **Requires:** Plan 3 Phase 1 landed (per-port granularity).
- **Mechanism:** MockPort with library; capability set excludes port; preamble does NOT contain library.

### v3-sandbox-io.AC4.10 Edge: DataStream + SourceManager removed; workspace compiles
- **Type:** Automated, compile gate.
- **File:** Workspace-wide; `cargo check --workspace` after Phase 4 Task 8 deletions.
- **Test:** No named test — compile success is the verification.
- **Requires:** None directly; Phase 4 Task 8 must complete.
- **Mechanism:** `grep -rn "DataStream\|SourceManager\|SourcesHandler\|RpcHandler" crates/` returns no matches; `cargo check --workspace` succeeds.

---

## AC5: Integration and cleanup (Phase 5)

**Requires:** Plan 3 Phase 1 landed (smoke test exercises full CapabilitySet path).

End-to-end smoke at `crates/pattern_runtime/tests/sandbox_io_smoke.rs`.
Cleanup ACs verified by grep + `cargo nextest run`.

### v3-sandbox-io.AC5.1 Success: HttpPort registered; Call("http", "get", {url}) works
- **Type:** Automated, unit + smoke.
- **Unit file:** `crates/pattern_runtime/src/ports/http.rs`
- **Unit tests:** `metadata_advertises_methods`, `subscribe_returns_not_subscribable`, `unknown_method_returns_unsupported`, `configure_persists`, plus a wiremock-backed test if `wiremock` is available (open question Q1).
- **Registration unit:** `crates/pattern_runtime/src/runtime.rs` — test asserts `runtime.port_registry().get(&PortId::new("http"))` returns Some.
- **Smoke file:** `crates/pattern_runtime/tests/sandbox_io_smoke.rs`
- **Smoke test:** Single `#[tokio::test]` covering full sequence (one named test; sub-steps via `with_context` labels per AC5.6).
- **Requires:** Plan 3 Phase 1 landed (smoke only).

### v3-sandbox-io.AC5.2 Success: file/shell/port reminders all in segment 2 of next turn
- **Type:** Automated, smoke.
- **File:** `crates/pattern_runtime/tests/sandbox_io_smoke.rs`
- **Test:** Smoke test steps 4 (FileEdit), 5/7 (ShellOutput), 7 (PortEvent) — all assert attachments on next turn's first user message via Segment2Pass render.
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** All three sources flow through shared `SessionContext::async_reminder_queue` → compose-time drain → first-user-message attachment → Segment2Pass render. Phases 2/3/4 each contribute their `MessageAttachment` variant; smoke test exercises all three in one flow.

### v3-sandbox-io.AC5.3 Success: smoke test passes deterministically
- **Type:** Automated, smoke (e2e).
- **File:** `crates/pattern_runtime/tests/sandbox_io_smoke.rs`
- **Test:** Single `#[tokio::test]` exercising shell execute, file open+write+external-edit+merge, port call+subscribe; uses tempdirs, mock provider, mock port.
- **Requires:** Plan 3 Phase 1 landed; Phases 1-4 complete.
- **Mechanism:** Run 5x to confirm non-flake; condition-based waits (5s deadlines) per existing pattern. Cleanup verifies no leaked threads.

### v3-sandbox-io.AC5.4 Success: Sources + Rpc stubs deleted
- **Type:** Automated, grep + compile gate.
- **File:** Workspace-wide.
- **Test:** Phase 5 Task 5 audit: `grep -rn "is not implemented" crates/pattern_runtime/src/sdk/handlers/ | grep -v 'mcp\|spawn'` returns no matches; `cargo check --workspace` succeeds with `SourcesHandler`/`RpcHandler` absent from SdkBundle.
- **Requires:** None.
- **Mechanism:** Phase 4 Task 8 deletes; Phase 5 Task 3 + Task 5 audit.

### v3-sandbox-io.AC5.5 Success: canonical_effect_decls() updated for Shell/File/Port; Sources/Rpc removed
- **Type:** Automated, unit.
- **File:** `crates/pattern_runtime/src/sdk/bundle.rs`
- **Test:** Existing canonical-row test updated — `assert_eq!(decls.len(), 15)` (was 16 pre-Phase 4); test in `sdk::bundle::tests` module.
- **Requires:** None.
- **Mechanism:** SdkBundle HList declaration replaces `SourcesHandler`/`RpcHandler` with `PortHandler`; canonical-row count is 15 (16 - Sources - Rpc + Port).

### v3-sandbox-io.AC5.6 Failure: smoke test failures identify step + assertion
- **Type:** Automated, structural property of smoke test.
- **File:** `crates/pattern_runtime/tests/sandbox_io_smoke.rs`
- **Test:** Property of the smoke test's assertion style — each step uses `.with_context(|| format!("step N: ..."))` so any failure surfaces step + assertion in the panic message.
- **Requires:** Plan 3 Phase 1 landed (smoke prerequisite).
- **Mechanism:** No separate test; verified by reading the smoke source for labeled assertions on each of the 12 steps.

### v3-sandbox-io.AC5.7 Edge: smoke test runs concurrently with other tests
- **Type:** Automated, run-mode property.
- **File:** `crates/pattern_runtime/tests/sandbox_io_smoke.rs`
- **Test:** Run with `cargo nextest run --test-threads=4` alongside other integration tests; smoke uses tempdirs everywhere, no shared globals.
- **Requires:** Plan 3 Phase 1 landed.
- **Mechanism:** All paths are tempdir-scoped; verified by running the full integration suite parallel without flakes.

---

## Human verification

None. All 46 ACs are amenable to automation:

- AC1 ACs use real notify-watcher + tempfiles in unit tests.
- AC2/3/4 ACs are integration-tested through full SessionContext + handler dispatch
  (with Plan 3 Phase 1 prereq).
- AC5 ACs are covered by the smoke test, grep audits, and the canonical-row count test.

Two ACs deserve extra scrutiny by the test-analyst even though they are automatable:

- **AC3.7** — Plan deliberately diverges from the AC text ("killed") to a "backgrounded" semantic (Claude Code parity). The two tests verify the actual implementation; reviewer must confirm the design-deviation is acceptable.
- **AC4.7** — Verified compositionally (runtime denial via `port_call_capability_denied_blocks_dispatch` + library exclusion via `port_library_excluded_when_not_capable`) rather than directly compiling agent Haskell against a missing constructor. If a stricter compile-rejection test is wanted, a fixture-Haskell-file gate would be needed (currently no such gate in the plan).

---

## Coverage summary

- **Total ACs:** 46 (AC1: 8, AC2: 11, AC3: 10, AC4: 10, AC5: 7)
- **Automated:** 46
- **Human-verified:** 0
- **Plan-3-Phase-1 prereq:** 38 (all of AC2.1-2.11 [11], AC3.1-3.10 [10], AC4.2-4.9 [8], AC5.1-5.3+5.6-5.7 [5] integration paths; some AC4 unit tests + AC4.1 doctest + AC4.10 + AC5.4-5.5 are independent — 8 ACs do not require Plan 3)

### Test files introduced by this plan

Phase 1 (no Plan 3 prereq):
- `crates/pattern_memory/src/loro_sync/tests.rs` (new — AC1.1-1.8)
- Plus unit tests inline in `crates/pattern_memory/src/loro_sync/dir_watcher.rs`
  (`dir_watcher_routes_events_to_subscriber`, `dir_watcher_drops_unsubscribed_events`,
  `subscription_drop_removes_entry`, `multiple_subscribers_in_same_dir`).

Phase 2 (requires Plan 3 Phase 1):
- `crates/pattern_runtime/tests/file_handler.rs` (new — AC2.1-2.11 integration)
- Unit tests in `crates/pattern_runtime/src/file_manager/manager.rs` (FileManager impl)
- Unit tests in `crates/pattern_runtime/src/file_manager/policy.rs` (policy)
- Unit tests in `crates/pattern_runtime/src/file_manager/config_detect.rs` (config-shape detection)

Phase 3 (requires Plan 3 Phase 1):
- `crates/pattern_runtime/tests/shell_handler.rs` (new — AC3.1-3.10 integration)
- Unit tests in `crates/pattern_runtime/src/process_manager/local_pty.rs` (PTY backend, ported from v2)
- Unit tests in `crates/pattern_runtime/src/process_manager/logger.rs` (logger)

Phase 4 (requires Plan 3 Phase 1 with per-port `cap.has_port`):
- `crates/pattern_runtime/tests/port_handler.rs` (new — AC4.2-4.9 integration)
- `crates/pattern_runtime/src/testing/mock_port.rs` (MockPort helper)
- Unit tests in `crates/pattern_runtime/src/port_registry/registry.rs` (registry CRUD)
- Unit tests in `crates/pattern_runtime/src/sdk/preamble.rs` (library splicing)
- Doctest in `crates/pattern_core/src/traits/port.rs` (AC4.1)

Phase 5 (requires Plan 3 Phase 1):
- `crates/pattern_runtime/tests/sandbox_io_smoke.rs` (new — AC5.1-5.3, 5.6-5.7 e2e)
- Unit tests in `crates/pattern_runtime/src/ports/http.rs` (HttpPort)

### Out-of-scope (not part of this plan's verification surface)

Per the design plan's "Explicitly OUT OF SCOPE" list:
- Spawn handler (Plan 3: v3-multi-agent)
- Mcp handler (Plan 4: v3-extensibility)
- Plugin-registered ports (Plan 4 consumes the Port trait from this plan)
- `Message.Ask` implementation
- TUI rendering of shell output or file diffs
