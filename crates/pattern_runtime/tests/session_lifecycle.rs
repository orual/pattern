//! Session lifecycle tests.
//!
//! The tests that exercised the legacy static-program path — `open_session`
//! compiling a Haskell `program` string, `Session::step` driving
//! `SessionMachine.run`, checkpoint/restore round-trips through that path — were
//! retired in Phase 6 Task B alongside the static-program machinery.
//!
//! The `update_block_description_on_missing_block_returns_not_found` test, which
//! exercised the in-memory store directly without any session machinery, is moved
//! inline to `testing/in_memory_store.rs` where the implementation lives.
//!
//! Agent-loop lifecycle tests live in `src/session.rs`'s inline test module
//! (see `open_with_agent_loop_and_step_drives_two_wire_turns` and
//! `open_with_agent_loop_wires_turn_sink_into_ctx`).
