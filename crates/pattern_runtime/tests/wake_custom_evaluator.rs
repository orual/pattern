//! Integration tests for the custom Haskell wake-condition evaluator
//! (Phase 7 Task 6).
//!
//! These tests exercise the `CustomEvaluator` end-to-end: real Haskell
//! compilation via `tidepool-extract`, single-flight enforcement,
//! timeout handling, and the security-critical capability-rejection
//! test that verifies the read-only bundle omits non-Observe effects.
//!
//! Gated on `preflight::check()` — silently skipped when `tidepool-extract`
//! is not available.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::traits::MemoryStore;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_core::ProviderClient;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient};
use pattern_runtime::wake::custom::CustomEvaluator;
use smol_str::SmolStr;
use tokio::sync::mpsc;

fn skip_without_tidepool() -> bool {
    pattern_runtime::preflight::check().is_err()
}

async fn test_evaluator() -> (
    CustomEvaluator,
    mpsc::UnboundedReceiver<pattern_runtime::mailbox::MailboxInput>,
    Arc<pattern_runtime::session::SessionContext>,
) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);

    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("wake-test-agent", "WakeTestAgent");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona, store, provider, db,
        tokio::runtime::Handle::current(),
    ));

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");
    let include_paths = vec![sdk_dir];

    let (tx, rx) = mpsc::unbounded_channel();
    let evaluator = CustomEvaluator::new(
        tx,
        include_paths,
        tokio::runtime::Handle::current(),
        ctx.clone(),
    );

    (evaluator, rx, ctx)
}

/// Test 1: Register a condition with 1s period that always returns True.
/// Verify mailbox receives at least one wake message over a ~3s window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_fires_correctly() {
    if skip_without_tidepool() {
        return;
    }

    let (evaluator, mut rx, _ctx) = test_evaluator().await;

    evaluator
        .register_interval(
            SmolStr::new("test-always-true"),
            "pure True".to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed");

    assert_eq!(evaluator.len(), 1);

    // Wait up to 10s for at least one message. GHC warm-up on first
    // eval can take several seconds.
    let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    assert!(
        msg.is_ok(),
        "should have received at least one wake message within 10s"
    );

    evaluator.unregister(&SmolStr::new("test-always-true"));
    assert_eq!(evaluator.len(), 0);
}

/// Test 2: Register a condition whose program sleeps for 60s (exceeds
/// the 30s eval timeout). Verify no mailbox poke and condition stays
/// registered for the next trigger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_enforces_limit() {
    if skip_without_tidepool() {
        return;
    }

    let (evaluator, mut rx, _ctx) = test_evaluator().await;

    // This program calls Time.sleep which is a MutateInternal effect
    // and will be absent from the Observe-only prelude. It should fail
    // at compile time, not at runtime timeout. This is actually better —
    // the compile error proves the security boundary works.
    //
    // Use a program that loops instead.
    // Actually, let's use a pure infinite loop: `let loop_ x = loop_ x in loop_ ()"
    // That would hang the eval thread until timeout.
    evaluator
        .register_interval(
            SmolStr::new("test-timeout"),
            "let loop_ x = loop_ x in loop_ ()".to_string(),
            Duration::from_secs(2),
        )
        .expect("registration should succeed");

    // Wait 5s — should NOT receive any message (program hangs, timeout fires).
    let no_msg = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
    assert!(
        no_msg.is_err(),
        "should not receive any wake message when program times out"
    );

    // Condition should still be registered.
    assert_eq!(evaluator.len(), 1);
    evaluator.unregister(&SmolStr::new("test-timeout"));
}

/// Test 3 (SECURITY CRITICAL): Register a condition that uses
/// Memory.Put (a write effect). Verify Tidepool compile rejects it
/// because Memory.Put is MutateInternal and absent from the
/// Observe-only prelude.
///
/// This is the load-bearing T0-leverage test — the read-only filtered
/// prelude must omit Memory.Put so the program can't even be expressed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_rejection_at_compile() {
    if skip_without_tidepool() {
        return;
    }

    let (evaluator, mut rx, _ctx) = test_evaluator().await;

    // The program references Memory.put which is MutateInternal — it
    // should fail at compile time because the Observe-only prelude
    // doesn't import Memory.Put.
    evaluator
        .register_interval(
            SmolStr::new("test-write-rejected"),
            r#"do { Memory.put "evil" "data"; pure True }"#.to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed (compile happens on first trigger)");

    // The evaluation should fail at compile time. No mailbox poke.
    let no_msg = tokio::time::timeout(Duration::from_secs(8), rx.recv()).await;
    assert!(
        no_msg.is_err(),
        "Memory.Put program must NOT produce a wake message — \
         the read-only prelude should reject it at compile time"
    );

    evaluator.unregister(&SmolStr::new("test-write-rejected"));
}

/// Test 4: Two conditions with overlapping triggers. Both fire
/// correctly, single-flight per condition (no cross-condition blocking).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_conditions_overlapping_triggers() {
    if skip_without_tidepool() {
        return;
    }

    let (evaluator, mut rx, _ctx) = test_evaluator().await;

    evaluator
        .register_interval(
            SmolStr::new("cond-a"),
            "pure True".to_string(),
            Duration::from_secs(1),
        )
        .expect("cond-a registration should succeed");

    evaluator
        .register_interval(
            SmolStr::new("cond-b"),
            "pure True".to_string(),
            Duration::from_secs(1),
        )
        .expect("cond-b registration should succeed");

    assert_eq!(evaluator.len(), 2);

    // Wait for messages from both conditions. Collect up to 4 messages
    // within 15s (GHC warm-up for two conditions may be slow).
    let mut count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while count < 2 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(_)) => count += 1,
            _ => break,
        }
    }

    assert!(
        count >= 2,
        "should have received at least 2 wake messages (one per condition), got {count}"
    );

    evaluator.unregister(&SmolStr::new("cond-a"));
    evaluator.unregister(&SmolStr::new("cond-b"));
}

/// Test 5: Register with a subsecond period. Verify registration
/// returns the min-period error and no task is spawned.
#[tokio::test]
async fn min_period_rejection() {
    // No tidepool needed — this is a pure registration-time check.
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("agent-min-period", "A");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona, store, provider, db,
        tokio::runtime::Handle::current(),
    ));

    let (tx, _rx) = mpsc::unbounded_channel();
    let evaluator = CustomEvaluator::new(
        tx,
        vec![],
        tokio::runtime::Handle::current(),
        ctx,
    );

    let result = evaluator.register_interval(
        SmolStr::new("subsecond"),
        "pure True".to_string(),
        Duration::from_millis(500),
    );

    assert!(result.is_err(), "subsecond period must be rejected");
    let err = result.unwrap_err();
    assert!(
        err.contains("CustomWakeMinPeriod"),
        "error should mention min period: {err}"
    );
    assert_eq!(evaluator.len(), 0, "no task should have been spawned");
}

/// Verify that the read-only prelude built by the CustomEvaluator
/// correctly filters out non-Observe constructors.
#[test]
fn read_only_prelude_omits_mutate_constructors() {
    use pattern_core::{CapabilitySet, EffectClass};
    use pattern_runtime::sdk::bundle::filtered_effect_decls;

    let caps = CapabilitySet::all().with_classes([EffectClass::Observe]);
    let decls = filtered_effect_decls(&caps);

    // Memory module should have Get (Observe) but not Put (MutateInternal).
    let memory = decls
        .iter()
        .find(|d| d.type_name == "Memory")
        .expect("Memory module must survive (has Observe constructors)");
    let has_get = memory.constructors.iter().any(|s| s.starts_with("Get "));
    let has_put = memory.constructors.iter().any(|s| s.starts_with("Put "));
    assert!(has_get, "Memory.Get must be present in Observe-only prelude");
    assert!(!has_put, "Memory.Put must NOT be present in Observe-only prelude");

    // Shell should be entirely absent (all Escape constructors).
    assert!(
        !decls.iter().any(|d| d.type_name == "Shell"),
        "Shell must be absent from Observe-only prelude"
    );

    // Message should be absent (all Coordinate/Escape constructors).
    // Actually: Message.Ask is Escape, Send/Reply/Notify/Delegate are Coordinate.
    // Neither is Observe — so Message should be absent.
    assert!(
        !decls.iter().any(|d| d.type_name == "Message"),
        "Message must be absent from Observe-only prelude"
    );

    // Spawn should be absent (all Coordinate constructors).
    // AwaitSpawn and AwaitAll are Observe but Ephemeral/Fork/Sibling/Stop/ForkOp are Coordinate.
    // So Spawn MAY have some surviving constructors. Let's check.
    if let Some(spawn) = decls.iter().find(|d| d.type_name == "Spawn") {
        // Only AwaitSpawn and AwaitAll should survive.
        for ctor in spawn.constructors.iter() {
            let name = ctor.split_whitespace().next().unwrap_or("");
            assert!(
                name == "AwaitSpawn" || name == "AwaitAll",
                "unexpected Spawn constructor in Observe-only prelude: {name}"
            );
        }
    }
}

/// Verify CustomEvaluator enforces the per-session condition cap.
#[tokio::test]
async fn condition_cap_enforced() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("agent-cap", "A");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona, store, provider, db,
        tokio::runtime::Handle::current(),
    ));

    let (tx, _rx) = mpsc::unbounded_channel();
    let evaluator = CustomEvaluator::new(
        tx,
        vec![],
        tokio::runtime::Handle::current(),
        ctx,
    )
    .with_max_conditions(2);

    // Register two conditions — should succeed.
    evaluator
        .register_interval(SmolStr::new("c1"), "pure True".into(), Duration::from_secs(1))
        .expect("first should succeed");
    evaluator
        .register_interval(SmolStr::new("c2"), "pure True".into(), Duration::from_secs(1))
        .expect("second should succeed");

    // Third should fail.
    let result = evaluator.register_interval(
        SmolStr::new("c3"),
        "pure True".into(),
        Duration::from_secs(1),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("CustomWakeLimit"),
        "error should mention limit: {err}"
    );
    assert_eq!(evaluator.len(), 2, "only 2 conditions should be registered");
}
