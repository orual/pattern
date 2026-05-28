// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! AC14.3 integration test: broken lib/ module → probe compile captures failure
//! → `Pattern.Diagnostics.diagnostics` sees the compile failure in the result.
//!
//! The test exercises the full path:
//!
//! 1. A tempdir `lib/` directory with a broken `.hs` file (type error).
//! 2. `validate_and_resolve` probe-compiles each module and records failures.
//! 3. The failure is converted to a `DiagnosticEvent` via `From<LibCompileFailure>`.
//! 4. A `DiagnosticsHandler` pre-populated with that event is wired into
//!    `compile_and_run` with the `diagnostics_query.hs` fixture.
//! 5. The agent program calls `Pattern.Diagnostics.diagnostics` and returns
//!    the JSON-encoded diagnostics list as `Text`.
//! 6. The test asserts the returned JSON contains the broken module name.
//!
//! Gated on `preflight::check()` — silently skipped when `tidepool-extract`
//! is not available (like the existing `lib_modules` integration tests do).

use std::sync::{Arc, Mutex};

use pattern_runtime::sdk::handlers::diagnostics::{DiagnosticEvent, DiagnosticsHandler};
use pattern_runtime::sdk::lib_modules::{LibCompileFailure, validate_and_resolve};

/// `DiagnosticsQuery` agent: calls `Pattern.Diagnostics.diagnostics` and
/// returns the JSON list as `Text`. Effect row: `Eff '[Diagnostics] Text`.
/// Handler at position 0 (tag 0) must be `DiagnosticsHandler`.
type DiagnosticsOnlyBundle = frunk::HList![DiagnosticsHandler];

/// Run the `diagnostics_query.hs` fixture through `compile_and_run` with
/// a pre-populated handler and return the decoded JSON string.
///
/// The Haskell `diagnostics` helper returns `Text` (JSON-encoded). The
/// tidepool `value_to_json` renderer decodes the `Text` constructor into a
/// `serde_json::Value::String`. We extract that string and return it.
fn run_diagnostics_agent(events: Vec<DiagnosticEvent>) -> String {
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should resolve");

    let handler = DiagnosticsHandler::new(Arc::new(Mutex::new(events)));
    let mut bundle: DiagnosticsOnlyBundle = frunk::hlist![handler];

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include_path = sdk_dir;
            tidepool_runtime::compile_and_run(
                include_str!("fixtures/diagnostics_query.hs"),
                "agent",
                &[include_path.as_path()],
                &mut bundle,
                &(),
            )
        })
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let eval_result = result.expect("compile_and_run should succeed for diagnostics_query");

    // The agent returns `Text` (JSON-encoded). `value_to_json` renders
    // the tidepool `Text` constructor as a `serde_json::Value::String`.
    let json_val = eval_result.to_json();
    match json_val {
        serde_json::Value::String(s) => s,
        other => panic!("expected Text value to render as JSON string, got: {other:?}"),
    }
}

/// AC14.3: a broken lib module is surfaced through the full path.
///
/// Broken `.hs` file → `validate_and_resolve` probe captures failure →
/// `From<LibCompileFailure> for DiagnosticEvent` converts it →
/// `DiagnosticsHandler` returns it → agent sees it in the JSON result.
#[test]
fn broken_lib_module_surfaces_in_diagnostics() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should resolve");

    // Build a tempdir with a lib/ directory containing one broken module.
    // The broken module has a type error: assigning a String literal to
    // an Int-typed binding. GHC's type-checker will reject this.
    let tmp = tempfile::TempDir::new().expect("tempdir should create");
    let lib_dir = tmp.path().join("lib");
    std::fs::create_dir(&lib_dir).expect("lib dir should create");
    std::fs::write(
        lib_dir.join("BrokenModule.hs"),
        "module BrokenModule where\n\nbad :: Int\nbad = \"not an int\"\n",
    )
    .expect("broken module file should write");

    // Probe-compile: validate_and_resolve uses the mount path (parent of lib/).
    let validation = validate_and_resolve(tmp.path(), &[sdk_dir]);

    // The broken module must be recorded as a failure.
    assert_eq!(
        validation.failures.len(),
        1,
        "expected exactly one probe compile failure, got: {:?}",
        validation
            .failures
            .iter()
            .map(|f| &f.module_name)
            .collect::<Vec<_>>(),
    );
    let failure: LibCompileFailure = validation.failures.into_iter().next().unwrap();
    assert_eq!(
        failure.module_name, "BrokenModule",
        "failure module name should match"
    );

    // Convert to a DiagnosticEvent via the From impl. This is the wiring
    // that open_with_agent_loop exercises when it pushes lib failures into
    // the session diagnostics.
    let event = DiagnosticEvent::from(failure);
    assert_eq!(
        event.source, "lib-compile",
        "DiagnosticEvent source should be 'lib-compile'"
    );
    assert!(
        event.message.contains("BrokenModule"),
        "DiagnosticEvent message should contain the module name, got: {:?}",
        event.message,
    );

    // Run the agent program. The handler is pre-populated with the event;
    // calling `diagnostics` inside the JIT returns the JSON-encoded list.
    let json_text = run_diagnostics_agent(vec![event]);

    // The returned text is a JSON string. Parse it and verify the event
    // is present and contains the broken module name.
    let parsed: serde_json::Value =
        serde_json::from_str(&json_text).expect("diagnostics result should be valid JSON");

    let events_arr = parsed
        .as_array()
        .expect("diagnostics result should be a JSON array");

    assert_eq!(
        events_arr.len(),
        1,
        "expected exactly one diagnostic event in the result, got: {events_arr:?}",
    );

    let event_obj = &events_arr[0];
    let message = event_obj
        .get("message")
        .and_then(|v| v.as_str())
        .expect("diagnostic event should have a 'message' field");

    assert!(
        message.contains("BrokenModule"),
        "diagnostic event message should contain 'BrokenModule', got: {message:?}",
    );

    let source = event_obj
        .get("source")
        .and_then(|v| v.as_str())
        .expect("diagnostic event should have a 'source' field");

    assert_eq!(
        source, "lib-compile",
        "diagnostic event source should be 'lib-compile'"
    );

    let severity = event_obj
        .get("severity")
        .and_then(|v| v.as_str())
        .expect("diagnostic event should have a 'severity' field");

    assert_eq!(
        severity, "error",
        "diagnostic event severity should be 'error'"
    );
}

/// AC14.3 (empty case): no lib modules means diagnostics returns an empty list.
///
/// Validates the other side of the path: when the session has no diagnostics
/// the agent sees an empty JSON array. This guards against regressions where
/// a non-empty default is accidentally baked in.
#[test]
fn empty_diagnostics_returns_empty_json_array() {
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let json_text = run_diagnostics_agent(vec![]);

    let parsed: serde_json::Value =
        serde_json::from_str(&json_text).expect("diagnostics result should be valid JSON");

    assert_eq!(
        parsed,
        serde_json::Value::Array(vec![]),
        "empty diagnostics should produce an empty JSON array, got: {parsed:?}",
    );
}
