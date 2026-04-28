//! Validation test: can Pattern SDK modules be compiled with the multi-module
//! path now that tidepool fork commit 6120c51 fixes the DataConTable/CoreExpr
//! inconsistency at JIT time?
//!
//! Three test cases:
//!
//! 1. `qualified_imports_direct` — qualified imports (`import qualified Pattern.Time as Time`)
//!    compiled with NO inliner preprocessing. SDK dir passed as include path to
//!    `tidepool_runtime::compile_and_run`. This is the post-deprecation authoring style.
//!
//! 2. `unqualified_imports_direct` — unqualified imports (`import Pattern.Time`, as the
//!    Prelude-5 bundle currently uses), compiled with NO inliner preprocessing. SDK dir
//!    passed as include path. If this passes, the inliner is fully redundant.
//!
//! 3. `delegation_round_robin_module_compiles` — import `Pattern.Delegation.RoundRobin` and
//!    verify `roundRobin` has the expected polymorphic type (AC10.4). The agent's `agent`
//!    binding is a trivial `Time`-only program so this test reuses the `TimePlusLogBundle`;
//!    a top-level `_checkRoundRobinType` helper verifies the import and type at GHC level.
//!
//! Tests 1 and 2 fail loudly (via `.expect`) if `tidepool-extract` is not available —
//! they are environment-gated, not silently skipped.
//! Test 3 skips gracefully if `tidepool-extract` is not available (same skip-pattern as
//! `spawn_wire_round_trip.rs`).

use pattern_runtime::sdk::handlers::log::LogHandler;
use pattern_runtime::sdk::handlers::time::TimeHandler;

/// Reduced bundle matching `Eff '[Time, Log]`.
/// Effect tag 0 -> Time, tag 1 -> Log.
type TimePlusLogBundle = frunk::HList![TimeHandler, LogHandler];

/// Test 1: qualified imports, multi-module path (no inliner).
///
/// The agent uses `import qualified Pattern.Time as Time` and
/// `import qualified Pattern.Log as Log`, which is the idiomatic Haskell
/// style when module namespacing is available. Qualified aliases require the
/// modules to be genuinely separate (not inlined), so this is the definitive
/// test that tidepool's multi-module DataCon fix works.
///
/// `sdk_dir` is passed as the single include path; the modules are found at
/// `sdk_dir/Pattern/Time.hs` and `sdk_dir/Pattern/Log.hs`.
#[test]
fn qualified_imports_direct() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Agent using qualified imports — these would FAIL with the inliner
    // (qualified aliases become dangling references after module flattening).
    let source = r#"{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
module Agent where
import Control.Monad.Freer (Eff)
import qualified Pattern.Time as Time
import qualified Pattern.Log as Log

agent :: Eff '[Time.Time, Log.Log] ()
agent = do
  _t <- Time.now
  Log.info "hello via qualified imports; no inliner"
"#;

    let mut bundle: TimePlusLogBundle = frunk::hlist![TimeHandler, LogHandler::default()];

    // Spawn on a larger stack — tidepool JIT needs it. Move sdk_dir into the
    // closure so lifetime is self-contained ('static bound on the closure).
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
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let eval_result = result.expect("compile_and_run should succeed for qualified imports");
    let value = eval_result.into_value();

    match &value {
        tidepool_eval::value::Value::Con(_, fields) if fields.is_empty() => {
            eprintln!("qualified_imports_direct: got unit () as expected");
        }
        other => panic!("expected unit, got: {other:?}"),
    }
}

/// Test 2: unqualified imports, multi-module path (no inliner).
///
/// The agent uses `import Pattern.Time` and `import Pattern.Log` without
/// qualifiers — exactly the style that Prelude-5 agents currently rely on
/// via the inliner. If this test passes, the inliner is fully redundant:
/// the multi-module path handles both qualified and unqualified import styles.
///
/// Note: unqualified imports only cause ambiguity when two modules expose
/// the same unqualified constructor. Pattern's current SDK is collision-
/// free at the unqualified layer (Memory uses `Get`/`Put`, File uses
/// `Read`/`Write`, etc.), but mixing Haskell-level re-exports could still
/// reintroduce ambiguity — this test confirms the simple `Time` + `Log`
/// pair compiles cleanly without any inliner preprocessing.
#[test]
fn unqualified_imports_direct() {
    pattern_runtime::preflight::check()
        .expect("tidepool-extract must be available; see crates/pattern_runtime/CLAUDE.md");

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // Agent using unqualified imports — the Prelude-5 authoring style.
    // The SDK dir is passed as a GHC include path; no source preprocessing.
    let source = r#"{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
module Agent where
import Control.Monad.Freer (Eff)
import Pattern.Time
import Pattern.Log

agent :: Eff '[Time, Log] ()
agent = do
  _t <- now
  info "hello via unqualified imports; no inliner"
"#;

    let mut bundle: TimePlusLogBundle = frunk::hlist![TimeHandler, LogHandler::default()];

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
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let eval_result = result.expect("compile_and_run should succeed for unqualified imports");
    let value = eval_result.into_value();

    match &value {
        tidepool_eval::value::Value::Con(_, fields) if fields.is_empty() => {
            eprintln!("unqualified_imports_direct: got unit () as expected");
        }
        other => panic!("expected unit, got: {other:?}"),
    }
}

/// Test 3: `Pattern.Delegation.RoundRobin` module compiles and exports `roundRobin`
/// with the correct polymorphic type (AC10.4).
///
/// The agent source imports the module qualified and defines `_checkRoundRobinType`
/// as a type-only alias for `roundRobin`. GHC checks that the alias typechecks —
/// proving the import resolves and the type signature is consistent — even though
/// the binding is never called.  The `agent` entrypoint is a plain `Time`-only
/// program so the test can reuse the existing `TimePlusLogBundle` and `()` context.
///
/// Skips gracefully when `tidepool-extract` is not on PATH (same policy as
/// `spawn_wire_round_trip.rs`).
#[test]
fn delegation_round_robin_module_compiles() {
    if pattern_runtime::preflight::check().is_err() {
        eprintln!("delegation_round_robin_module_compiles: skipping (tidepool-extract not available)");
        return;
    }

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");

    // The agent's `agent` binding uses only `Time` so `TimePlusLogBundle`
    // satisfies the effect row.  `_checkRoundRobinType` is a top-level
    // type-alias binding that GHC checks even though it is never called —
    // this is the compile-time import verification for AC10.4.
    let source = r#"{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings, FlexibleContexts #-}
module Agent where

import Control.Monad.Freer (Eff, Member)
import qualified Pattern.Time as Time
import qualified Pattern.Log as Log
import qualified Pattern.Spawn as Spawn
import qualified Pattern.Delegation.RoundRobin as RR

-- Type-only alias: GHC verifies that RR.roundRobin has the expected signature.
-- Never called; exists only for compile-time verification of AC10.4.
_checkRoundRobinType
    :: Member Spawn.Spawn effs
    => [Spawn.EphemeralConfig]
    -> [task]
    -> (task -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
    -> Eff effs [Spawn.SpawnAwaitOutcome]
_checkRoundRobinType = RR.roundRobin

agent :: Eff '[Time.Time, Log.Log] ()
agent = do
  _t <- Time.now
  Log.info "delegation/round-robin module import verified"
"#;

    let mut bundle: TimePlusLogBundle = frunk::hlist![TimeHandler, LogHandler::default()];

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
        .expect("thread spawn should succeed")
        .join()
        .expect("thread should not panic");

    let eval_result = result
        .expect("compile_and_run should succeed: Pattern.Delegation.RoundRobin must be importable");
    let value = eval_result.into_value();

    match &value {
        tidepool_eval::value::Value::Con(_, fields) if fields.is_empty() => {
            eprintln!("delegation_round_robin_module_compiles: got unit () as expected");
        }
        other => panic!("expected unit from agent, got: {other:?}"),
    }
}
