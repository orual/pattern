//! Task 17 — AC2.7: effect-response overflow surfaces as
//! `RuntimeError::EffectOverflow`.
//!
//! Tidepool enforces a 10K-node limit on the Value returned from any
//! effect handler (`jit_machine.rs::MAX_EFFECT_RESPONSE_NODES`). Real
//! Phase-3 handlers don't organically produce responses that large, so
//! we synthesise the condition by substituting a test-only handler for
//! `Pattern.Time` that ignores the request and returns a deeply-nested
//! `Value::Con` chain exceeding the limit.
//!
//! The overflow detection is at the JIT/FFI boundary — the agent code
//! doesn't need to consume the oversized value; the runtime aborts the
//! turn before handing it back to Haskell code.

use pattern_runtime::sdk::requests::TimeReq;
use pattern_runtime::tidepool::error_map::{JitOutcome, map_jit_error};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;
use tidepool_repr::DataConId;

/// Test-only handler occupying the `Time` slot in a reduced bundle.
/// Ignores the request and returns a `Value::Con` tree whose total
/// node count exceeds the 10K limit enforced by
/// `tidepool_codegen::jit_machine::MAX_EFFECT_RESPONSE_NODES`.
struct OverflowHandler;

impl OverflowHandler {
    /// Build a single `Value::Con` whose `fields` list already has
    /// `n` entries (each a unit constructor of node_count = 1). Total
    /// node count = `1 + n`.
    ///
    /// Node-count is the only property tidepool checks at the overflow
    /// boundary; the concrete `DataConId`s don't need to resolve
    /// against any real `DataConTable` because the check runs before
    /// any heap conversion that would hit the table.
    fn oversized_value(n: usize) -> Value {
        let outer = DataConId(9_000);
        let leaf = DataConId(9_001);
        let fields = (0..n).map(|_| Value::Con(leaf, vec![])).collect();
        Value::Con(outer, fields)
    }
}

impl<U> EffectHandler<U> for OverflowHandler {
    type Request = TimeReq;

    fn handle(&mut self, _req: TimeReq, _cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // 12_000 > 10_000 (MAX_EFFECT_RESPONSE_NODES). Total node count
        // = 1 (outer Con) + 12_000 (leaves) = 12_001.
        Ok(Self::oversized_value(12_000))
    }
}

/// AC2.7 (integration path). We drive a real agent program through the
/// tidepool pipeline with `OverflowHandler` in the Time slot. The agent
/// calls `Time.now`; tidepool invokes the handler, detects the 12K-node
/// response is over budget, and returns `JitError::EffectResponseTooLarge`.
/// `compile_and_run` bubbles that up as a `CompileError` equivalent —
/// here we assert on the raw JitError via the error-map layer.
#[test]
fn oversized_response_fails() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let source = include_str!("fixtures/time_now_returns_int.hs");
    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    type TimeSlotOverflow = frunk::HList![OverflowHandler];
    let mut bundle: TimeSlotOverflow = frunk::hlist![OverflowHandler];

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

    let err = result.expect_err("oversized response should abort the turn");

    // `CompileError::JitError(JitError::EffectResponseTooLarge { .. })`
    // is how tidepool surfaces the overflow at the run boundary. We
    // cover the public Pattern surface by asserting the Debug form
    // mentions the tidepool node-count message — guarantees the error
    // is bubbling through untouched and Pattern's error-map layer has
    // a path to map it to `RuntimeError::EffectOverflow`.
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("response too large") || rendered.contains("EffectResponseTooLarge"),
        "expected tidepool overflow error in Debug rendering, got: {rendered}",
    );
}

/// AC2.7 (map surface). The single-shot error-map assertion. The full
/// unit-test matrix in `src/tidepool/error_map.rs` covers every JitError
/// branch; this pins the *public* mapping from the dedicated
/// `EffectResponseTooLarge` variant to `RuntimeError::EffectOverflow`
/// so a refactor can't silently redirect it elsewhere.
#[test]
fn effect_response_too_large_maps_to_effect_overflow() {
    use pattern_core::error::RuntimeError;
    use tidepool_codegen::jit_machine::JitError;

    let outcome = map_jit_error(JitError::EffectResponseTooLarge {
        nodes: 20_000,
        limit: 10_000,
    });
    assert!(
        matches!(outcome, JitOutcome::Runtime(RuntimeError::EffectOverflow)),
        "expected RuntimeError::EffectOverflow from EffectResponseTooLarge",
    );
}
