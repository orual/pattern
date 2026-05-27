// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Scripted turn fixtures for the multi-agent smoke test.
#![allow(dead_code)] // consumed by multi_agent_smoke.rs (Task 5)
//!
//! Builds `Vec<Vec<ChatStreamEvent>>` provider scripts for the two-session
//! constellation exercised by `multi_agent_smoke.rs`: a supervisor that
//! delegates a task and a specialist that completes it. Both scripts are
//! deterministic — no live model required (AC10.2).
//!
//! # Consuming this module
//!
//! At the top of `tests/multi_agent_smoke.rs`:
//!
//! ```rust,ignore
//! #[path = "support/multi_agent_scripts.rs"]
//! mod scripts;
//! ```
//!
//! Then call the builder functions:
//!
//! ```rust,ignore
//! let supervisor_turns = scripts::supervisor_script("agent-specialist");
//! let specialist_turns = scripts::specialist_script("agent-supervisor");
//! let supervisor_provider = MockProviderClient::with_turns(supervisor_turns);
//! let specialist_provider = MockProviderClient::with_turns(specialist_turns);
//! ```
//!
//! # Script anatomy
//!
//! Each "exchange" is a pair of wire turns:
//!
//! - Turn 1: `tool_use_turn` — the agent emits a `code` tool call encoding a
//!   Haskell program. The eval worker compiles and runs it.
//! - Turn 2: `text_turn` — a brief summary message that ends the wire-turn loop
//!   with `stop_reason = EndTurn`.
//!
//! For tests that do not exercise the Haskell eval path (e.g. tests that drive
//! the handler layer directly without `tidepool-extract`), use `text_turn`-only
//! exchanges; the smoke test itself always uses both turns.
//!
//! # Extending fixtures
//!
//! Each named function returns an independent `Vec`. Callers that need a
//! longer script can concatenate:
//!
//! ```rust,ignore
//! let mut turns = scripts::supervisor_script("specialist-id");
//! turns.extend(scripts::supervisor_summary_exchange());
//! let provider = MockProviderClient::with_turns(turns);
//! ```
//!
//! Functions that return a single exchange (`Vec<Vec<...>>` with 2 entries)
//! are named `*_exchange` to make the two-turn shape explicit.

use genai::chat::ChatStreamEvent;
use pattern_runtime::testing::MockProviderClient;
use serde_json::json;

// ── Supervisor script ─────────────────────────────────────────────────────────

/// Full script for the supervisor session in the multi-agent smoke test.
///
/// Three exchanges (six wire turns total):
///
/// 1. **Routing turn** — supervisor receives the human message and emits a
///    `send` that delegates the task to the specialist.
/// 2. **Observation turn** — a no-op text turn so the supervisor's session
///    stays live while the specialist is running.
/// 3. **Summary turn** — supervisor reads the specialist's result from the
///    `"specialist-result"` block and produces the final human-facing response.
///
/// The Haskell programs in turns 1 and 3 are intentionally minimal so they
/// compile quickly during CI runs. They exercise the `Message` effect (routing)
/// and `Memory` effect (read result).
pub fn supervisor_script(specialist_id: &str) -> Vec<Vec<ChatStreamEvent>> {
    let mut turns = Vec::new();

    // Exchange 1: supervisor routes the human message to the specialist.
    turns.extend(supervisor_routing_exchange(specialist_id));

    // Exchange 2: supervisor waits for the specialist's result.
    turns.extend(supervisor_observation_exchange());

    // Exchange 3: supervisor summarises the result back to the human.
    turns.extend(supervisor_summary_exchange());

    turns
}

/// A single exchange where the supervisor delegates a task to `specialist_id`.
///
/// The agent program calls `send` (from `Pattern.Message`) to forward the task
/// and writes a delegation note to the `"delegation-log"` memory block so
/// tests can assert the write landed.
///
/// Returns 2 wire turns (`tool_use_turn` + `text_turn`).
pub fn supervisor_routing_exchange(specialist_id: &str) -> Vec<Vec<ChatStreamEvent>> {
    // The code payload forwards the task to the specialist. Flush-left
    // (four-space prefix is added by the code-tool template engine).
    let program = format!(
        "_ <- Log.info \"supervisor: routing task to specialist\"\n\
         Memory.put \"delegation-log\" \"delegated: compute 2+2\"\n\
         send \"agent:{specialist_id}\" \"compute 2+2\""
    );
    vec![
        MockProviderClient::tool_use_turn("toolu_sup_01_route", "code", json!({ "code": program })),
        MockProviderClient::text_turn("I have delegated the computation task to the specialist."),
    ]
}

/// A single exchange where the supervisor advances with a plain-text reply.
///
/// Used as a placeholder turn that keeps the supervisor session advancing while
/// the specialist is being driven in the test. The text content is informational
/// and stable — assertions that search for this string can use it as a
/// synchrony fence.
///
/// Returns 1 wire turn (`text_turn`). The wire-turn loop ends immediately on
/// `EndTurn`.
pub fn supervisor_observation_exchange() -> Vec<Vec<ChatStreamEvent>> {
    vec![MockProviderClient::text_turn(
        "Waiting for the specialist to complete the task.",
    )]
}

/// A single exchange where the supervisor reads the specialist's result and
/// produces the final human-facing summary.
///
/// The program reads the `"specialist-result"` block written by the specialist
/// and composes a summary response. This verifies that the specialist's memory
/// write is visible to the supervisor (shared-block or same-store semantics,
/// depending on test setup).
///
/// Returns 2 wire turns (`tool_use_turn` + `text_turn`).
pub fn supervisor_summary_exchange() -> Vec<Vec<ChatStreamEvent>> {
    let program = "_ <- Log.info \"supervisor: summarising specialist result\"\n\
         result <- Memory.get \"specialist-result\"\n\
         pure (T.concat [\"The answer is: \", result])";
    vec![
        MockProviderClient::tool_use_turn(
            "toolu_sup_02_summarise",
            "code",
            json!({ "code": program }),
        ),
        MockProviderClient::text_turn("The specialist computed the answer: 4. Task complete."),
    ]
}

// ── Specialist script ─────────────────────────────────────────────────────────

/// Full script for the specialist session in the multi-agent smoke test.
///
/// Two exchanges (three wire turns total):
///
/// 1. **Task execution turn** — specialist receives the delegated task, computes
///    the result, and writes it to the `"specialist-result"` memory block.
/// 2. **Completion turn** — specialist confirms completion with a text reply.
///
/// The specialist's capability set excludes `Shell`, so the `Shell` GADT is
/// absent from its preamble (Phase 1 capability filtering). Step 5 of the
/// smoke test verifies this by attempting `Shell.execute` and expecting a
/// compile-time GHC error. That step uses a separate
/// [`specialist_capability_probe_exchange`], not this script.
pub fn specialist_script(supervisor_id: &str) -> Vec<Vec<ChatStreamEvent>> {
    let mut turns = Vec::new();

    // Exchange 1: specialist receives and executes the task.
    turns.extend(specialist_task_exchange(supervisor_id));

    // Exchange 2: specialist confirms completion (text only).
    turns.extend(specialist_completion_exchange());

    turns
}

/// A single exchange where the specialist processes the delegated computation.
///
/// The program writes the result `"4"` to the `"specialist-result"` block and
/// notifies the supervisor via `send`. The memory write is the primary
/// assertion target for the smoke test.
///
/// Returns 2 wire turns (`tool_use_turn` + `text_turn`).
pub fn specialist_task_exchange(supervisor_id: &str) -> Vec<Vec<ChatStreamEvent>> {
    let program = format!(
        "_ <- Log.info \"specialist: executing computation task\"\n\
         Memory.put \"specialist-result\" \"4\"\n\
         send \"agent:{supervisor_id}\" \"result: 4\""
    );
    vec![
        MockProviderClient::tool_use_turn(
            "toolu_spec_01_compute",
            "code",
            json!({ "code": program }),
        ),
        MockProviderClient::text_turn(
            "I computed 2+2 = 4 and stored the result in specialist-result.",
        ),
    ]
}

/// A single exchange where the specialist sends a plain-text completion reply.
///
/// Used as the terminal turn for the specialist session. A single `text_turn`
/// ending with `EndTurn` closes the session's wire-turn loop.
///
/// Returns 1 wire turn (`text_turn`).
pub fn specialist_completion_exchange() -> Vec<Vec<ChatStreamEvent>> {
    vec![MockProviderClient::text_turn(
        "Task complete. Result 4 written to memory and supervisor notified.",
    )]
}

/// A scripted turn that attempts to call `Shell.execute` from the specialist.
///
/// The specialist does not have the `Shell` capability, so this program must
/// fail at **compile time** — GHC cannot find the `Shell.execute` constructor
/// because the Phase 1 prelude filter omitted it from the specialist's `type M`
/// row. The smoke test's step 5 calls this exchange and expects a non-`Ok`
/// `ToolOutcome` with an error string containing `"not in scope"` or a
/// comparable GHC diagnostic.
///
/// Returns 1 wire turn (`tool_use_turn`). The wire-turn loop will not advance
/// to a second turn because the eval worker returns an error immediately.
/// Callers append a `text_turn` (e.g. [`specialist_completion_exchange`]) if
/// the session must continue after this probe.
pub fn specialist_capability_probe_exchange() -> Vec<Vec<ChatStreamEvent>> {
    // The specialist intentionally calls Shell.execute. Without the Shell
    // effect in scope, GHC rejects this at compile time with something like
    // "Variable not in scope: execute :: ...".
    let program = "Shell.execute \"echo capability-probe\"";
    vec![MockProviderClient::tool_use_turn(
        "toolu_spec_probe_shell",
        "code",
        json!({ "code": program }),
    )]
}

// ── Fork-and-merge helpers ────────────────────────────────────────────────────

/// Script for the forked session used in the fork-and-merge step of the smoke.
///
/// The fork writes `"fork-note"` to the `"notes"` memory block and exits.
/// After `fork_op merge_back` the parent must observe this value in `"notes"`.
///
/// Returns 2 wire turns (`tool_use_turn` + `text_turn`).
pub fn fork_write_exchange() -> Vec<Vec<ChatStreamEvent>> {
    let program = "_ <- Log.info \"fork: writing fork-note\"\n\
         Memory.put \"notes\" \"fork-note\"";
    vec![
        MockProviderClient::tool_use_turn(
            "toolu_fork_01_write",
            "code",
            json!({ "code": program }),
        ),
        MockProviderClient::text_turn("Fork wrote fork-note to notes block."),
    ]
}

/// Script for the parent session to verify the fork-and-merge outcome.
///
/// Reads the `"notes"` block and produces a summary. If the merge did not
/// land, `Memory.get "notes"` will return an empty or pre-fork value — the
/// test's `assert!(result.contains("fork-note"), ...)` catches this.
///
/// Returns 2 wire turns (`tool_use_turn` + `text_turn`).
pub fn parent_merge_verify_exchange() -> Vec<Vec<ChatStreamEvent>> {
    let program = "_ <- Log.info \"parent: verifying merge outcome\"\n\
         notes <- Memory.get \"notes\"\n\
         pure notes";
    vec![
        MockProviderClient::tool_use_turn(
            "toolu_parent_01_verify",
            "code",
            json!({ "code": program }),
        ),
        MockProviderClient::text_turn("Parent verified the merge outcome from the fork."),
    ]
}
