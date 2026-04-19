//! Timeout and cancellation tests.
//!
//! The legacy static-program path (`SessionMachine.run` + `run_turn` watchdog)
//! was retired in Phase 6 Task B. The tests that exercised `CancelPath::Soft`,
//! `CancelPath::HardAbandon`, and the `cancel_grace` ceiling through the legacy
//! path were deleted alongside it — those code paths no longer exist in the
//! production session. The agent-loop path has its own timeout behaviour that
//! will be covered by agent-loop-specific tests in a future phase.
//!
//! Budget construction from `PersonaSnapshot` is still tested in
//! `src/timeout.rs`'s inline unit-test module.
