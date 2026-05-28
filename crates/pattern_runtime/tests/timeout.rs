// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
