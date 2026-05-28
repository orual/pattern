// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Targeted end-to-end effect tests for Task 20 (AC2.2, AC2.3).
//!
//! These tests go beyond the generic hello-world smoke path by asserting
//! observable behaviour of the two fully-implemented Prelude-5 effects:
//!
//! * `Time.now` — the returned Int must sit within a reasonable window
//!   around `jiff::Timestamp::now()` readings taken on the Rust side
//!   before and after the agent runs (AC2.2).
//! * `Log.info` — a `tracing-test` subscriber attached on the Rust side
//!   must observe the agent-originated event, including the
//!   marker message, the `source=agent` structured field, and the
//!   `session` field emitted by `LogHandler` (AC2.3).
//!
//! Both tests use `tidepool_runtime::compile_and_run` directly against a
//! reduced bundle so the assertions remain focused on the handlers under
//! test (no `Session` / watchdog / runtime machinery in the way).

use std::io;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use pattern_runtime::sdk::handlers::log::LogHandler;
use pattern_runtime::sdk::handlers::time::TimeHandler;
use tidepool_eval::value::Value;
use tidepool_repr::Literal;
use tracing_subscriber::fmt::MakeWriter;

/// Unwrap a freshly returned `Int` from the JIT result. The tidepool
/// bridge returns Haskell `Int` as a single-field `Value::Con` wrapping
/// `Value::Lit(LitInt(..))` (Haskell's @I#@ boxing). We don't round-trip
/// via `FromCore` here because we want to assert the raw wire value and
/// surface a descriptive panic on shape drift.
fn extract_int(v: &Value) -> i64 {
    match v {
        Value::Con(_, fields) if fields.len() == 1 => match &fields[0] {
            Value::Lit(Literal::LitInt(n)) => *n,
            other => panic!("expected boxed LitInt inside I#, got {other:?}"),
        },
        other => panic!("expected Value::Con(I#, [_]), got {other:?}"),
    }
}

/// AC2.2: `Time.now` inside the JIT returns the current wall clock in
/// epoch nanoseconds. We bracket the call with Rust-side readings and
/// assert the returned value lies in the interval (with a small fudge
/// factor since the three clock reads happen on different crossings of
/// the FFI boundary).
#[test]
fn time_now_returns_current_epoch_nanos() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/time_now_returns_int.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Bundle with Time at tag 0 only — matches `Eff '[Time]`.
    type TimeOnlyBundle = frunk::HList![TimeHandler];
    let mut bundle: TimeOnlyBundle = frunk::hlist![TimeHandler];

    // 1 second tolerance on either side to absorb compile / JIT warm-up
    // latency on the very first run on a cold machine.
    let fudge_ns: i64 = 1_000_000_000;
    let before = i64::try_from(Timestamp::now().as_nanosecond()).expect("i64 ns") - fudge_ns;

    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include_path = sdk_dir;
            tidepool_runtime::compile_and_run(
                source,
                "agent",
                &[include_path.as_path()],
                &mut bundle,
                &(),
            )
        })
        .expect("thread spawn")
        .join()
        .expect("thread did not panic");

    let after = i64::try_from(Timestamp::now().as_nanosecond()).expect("i64 ns") + fudge_ns;
    let eval_result = result.expect("compile_and_run should succeed");
    let value = eval_result.into_value();
    let ns = extract_int(&value);

    assert!(
        ns >= before && ns <= after,
        "Time.now ns {ns} not in [{before}, {after}]",
    );
}

/// Shared capture buffer writer for the custom fmt subscriber used by
/// the Log test. `tracing-test` filters events by the integration-test
/// binary's crate name, which drops events emitted from the
/// `pattern_runtime` crate (the LogHandler's own tracing calls). We
/// side-step that by installing a minimal `fmt::Subscriber` scoped to
/// this test with no env-filter, writing into a shared `Vec<u8>`.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("buffer mutex").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// AC2.3: `Log.info` emits a `tracing` event observable by a Rust-side
/// subscriber. We install a custom fmt subscriber (writing into a
/// shared buffer) as the default for this test's scope via
/// `with_default` so events from the `pattern_runtime::sdk::handlers::log`
/// path are captured; `tracing-test`'s default env filter is keyed to
/// the integration-test binary name and silently drops those events.
#[test]
fn log_info_observable_via_tracing() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/log_info_marker.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Tag the session so we can assert the `session` field lands
    // correctly on emitted events.
    let session_id = "log-info-marker-session";

    type LogOnlyBundle = frunk::HList![LogHandler];
    let mut bundle: LogOnlyBundle = frunk::hlist![LogHandler::for_session(session_id)];

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(CaptureWriter(buf.clone()))
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let include_path = sdk_dir;
        tidepool_runtime::compile_and_run(
            source,
            "agent",
            &[include_path.as_path()],
            &mut bundle,
            &(),
        )
        .expect("compile_and_run should succeed");
    });

    let captured =
        String::from_utf8(buf.lock().expect("buffer mutex").clone()).expect("utf-8 log output");

    // The LogHandler emits: level=info, session=<id>, source="agent",
    // message=<payload>. Check all three axes.
    assert!(
        captured.contains("structured-log-assertion-marker"),
        "expected subscriber to capture the marker message; capture:\n{captured}",
    );
    assert!(
        captured.contains("source=\"agent\""),
        "expected LogHandler to tag emitted events with source=\"agent\"; capture:\n{captured}",
    );
    assert!(
        captured.contains(session_id),
        "expected LogHandler to tag emitted events with session={session_id}; capture:\n{captured}",
    );
}
