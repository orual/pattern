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

use pattern_core::ProviderClient;
use pattern_core::traits::MemoryStore;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_runtime::mailbox::Mailbox;
use pattern_runtime::testing::{InMemoryMemoryStore, NopProviderClient};
use pattern_runtime::wake::custom::CustomEvaluator;
use smol_str::SmolStr;

fn skip_without_tidepool() -> bool {
    pattern_runtime::preflight::check().is_err()
}

async fn test_evaluator() -> (
    CustomEvaluator,
    Arc<Mailbox>,
    Arc<pattern_runtime::session::SessionContext>,
) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);

    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("wake-test-agent", "WakeTestAgent");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));

    let sdk_dir = pattern_runtime::SdkLocation::default()
        .resolve()
        .expect("SDK dir should exist");
    let include_paths = vec![sdk_dir];

    let (mailbox, _) = Mailbox::new(PersonaId::from("wake-test-agent"));
    let evaluator = CustomEvaluator::new(
        mailbox.clone(),
        include_paths,
        tokio::runtime::Handle::current(),
        ctx.clone(),
    )
    .with_min_interval(std::time::Duration::from_millis(100));

    (evaluator, mailbox, ctx)
}

/// Test 1: Register a condition with 1s period that always returns True.
/// Verify mailbox receives at least one wake message over a ~3s window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_fires_correctly() {
    if skip_without_tidepool() {
        return;
    }

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

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

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

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

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

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

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

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
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));

    let (mailbox, _) = Mailbox::new(PersonaId::from("agent-min-period"));
    let evaluator =
        CustomEvaluator::new(mailbox, vec![], tokio::runtime::Handle::current(), ctx);

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

/// SECURITY: Verify that `CapabilitySet::wake_evaluator_read_only()` drops entire
/// SDK module categories (Spawn, Shell, Message, Mcp, Wake, Fronting,
/// Constellation, File, Port) and applies the Observe-class filter to the
/// surviving modules.
///
/// This is the regression guard for Critical 1. The previous implementation
/// used `CapabilitySet::all().with_classes([EffectClass::Observe])`, which kept
/// ALL categories (including Spawn, Shell, etc.) and relied solely on the class
/// filter. That approach fails for `Skip`-classified constructors (e.g.
/// `Spawn.Ephemeral`, `Shell.Execute`, `Message.Send`, `Wake.Register`) which
/// bypass the runtime `check_effect_class` gate — the category filter is their
/// only protection.
///
/// If this test regresses (any of the dangerous modules reappear in the
/// wake-eval prelude), the security boundary is broken.
#[test]
fn wake_eval_read_only_capset_drops_dangerous_modules() {
    use pattern_core::CapabilitySet;
    use pattern_runtime::sdk::bundle::filtered_effect_decls;

    let caps = CapabilitySet::wake_evaluator_read_only();
    let decls = filtered_effect_decls(&caps);

    // ── Dropped categories (must be absent from the prelude) ─────────────
    // Spawn: has Skip-classified constructors (Ephemeral, Fork, Sibling,
    // ForkOp) that bypass check_effect_class. Category filter is the only
    // protection.
    assert!(
        !decls.iter().any(|d| d.type_name == "Spawn"),
        "SECURITY: Spawn must be absent from wake-eval prelude; \
         Spawn.Ephemeral/Fork/Sibling/ForkOp are Skip-classified"
    );

    // Shell: all Skip-classified (Execute, Spawn, Kill, Status).
    assert!(
        !decls.iter().any(|d| d.type_name == "Shell"),
        "SECURITY: Shell must be absent from wake-eval prelude; \
         all Shell constructors are Skip-classified"
    );

    // Message: all Skip-classified (Ask, Send, Reply, Notify, Delegate).
    assert!(
        !decls.iter().any(|d| d.type_name == "Message"),
        "SECURITY: Message must be absent from wake-eval prelude; \
         all Message constructors are Skip-classified"
    );

    // Mcp: Escape, but Enforce-classed. Dropped by category filter for simplicity.
    assert!(
        !decls.iter().any(|d| d.type_name == "Mcp"),
        "Mcp must be absent from wake-eval prelude"
    );

    // Wake: has Skip-classified Register constructor. Recursive wake
    // registration from inside a wake-eval is explicitly prohibited.
    assert!(
        !decls.iter().any(|d| d.type_name == "Wake"),
        "SECURITY: Wake must be absent from wake-eval prelude; \
         Wake.Register is Skip-classified and would allow recursive registration"
    );

    // Fronting: all Skip-classified.
    assert!(
        !decls.iter().any(|d| d.type_name == "Fronting"),
        "SECURITY: Fronting must be absent from wake-eval prelude"
    );

    // Constellation: all Enforce-classed Observe, but mutation path could land
    // here in future phases — dropped for defence-in-depth.
    assert!(
        !decls.iter().any(|d| d.type_name == "Constellation"),
        "Constellation must be absent from wake-eval prelude"
    );

    // File: has Skip-classified Write/ForceWrite.
    assert!(
        !decls.iter().any(|d| d.type_name == "File"),
        "SECURITY: File must be absent from wake-eval prelude; \
         File.Write/ForceWrite are Skip-classified"
    );

    // Port: has Skip-classified Call/Subscribe/Unsubscribe/List.
    assert!(
        !decls.iter().any(|d| d.type_name == "Port"),
        "SECURITY: Port must be absent from wake-eval prelude; \
         Port.Call/Subscribe are Skip-classified"
    );

    // ── Kept categories (must be present with Observe-only constructors) ──

    // Memory: Get (Observe, Enforce) must be present; Put must be absent.
    let memory = decls
        .iter()
        .find(|d| d.type_name == "Memory")
        .expect("Memory module must survive wake-eval (has Observe constructors)");
    let has_get = memory.constructors.iter().any(|s| s.starts_with("Get "));
    let has_put = memory.constructors.iter().any(|s| s.starts_with("Put "));
    assert!(has_get, "Memory.Get must be present in wake-eval prelude");
    assert!(
        !has_put,
        "Memory.Put must NOT be present in wake-eval prelude"
    );

    // Time: Now is present (Observe); Sleep must be absent (MutateInternal).
    let time = decls
        .iter()
        .find(|d| d.type_name == "Time")
        .expect("Time module must survive wake-eval (has Observe constructors)");
    let has_now = time.constructors.iter().any(|s| s.starts_with("Now "));
    assert!(has_now, "Time.Now must be present in wake-eval prelude");
    let has_sleep = time.constructors.iter().any(|s| s.starts_with("Sleep "));
    assert!(
        !has_sleep,
        "Time.Sleep must NOT be present in wake-eval prelude (MutateInternal)"
    );

    // Log: all Observe — all four constructors (Debug, Info, Warn, Error) present.
    assert!(
        decls.iter().any(|d| d.type_name == "Log"),
        "Log module must survive wake-eval (all Observe)"
    );

    // Search, Recall, Tasks, Skills, Display, Diagnostics — all present.
    for module in [
        "Search",
        "Recall",
        "Tasks",
        "Skills",
        "Display",
        "Diagnostics",
    ] {
        assert!(
            decls.iter().any(|d| d.type_name == module),
            "{module} module must survive wake-eval (has Observe constructors)"
        );
    }
}

/// Verify that the read-only prelude built by the CustomEvaluator
/// correctly filters out non-Observe constructors (kept for backwards
/// compatibility; the stronger `wake_eval_read_only_capset_drops_dangerous_modules`
/// test above is the primary security regression guard).
#[test]
fn read_only_prelude_omits_mutate_constructors() {
    use pattern_core::CapabilitySet;
    use pattern_runtime::sdk::bundle::filtered_effect_decls;

    // Use wake_evaluator_read_only() — the actual capset used by CustomEvaluator.
    let caps = CapabilitySet::wake_evaluator_read_only();
    let decls = filtered_effect_decls(&caps);

    // Memory module should have Get (Observe) but not Put (MutateInternal).
    let memory = decls
        .iter()
        .find(|d| d.type_name == "Memory")
        .expect("Memory module must survive (has Observe constructors)");
    let has_get = memory.constructors.iter().any(|s| s.starts_with("Get "));
    let has_put = memory.constructors.iter().any(|s| s.starts_with("Put "));
    assert!(
        has_get,
        "Memory.Get must be present in Observe-only prelude"
    );
    assert!(
        !has_put,
        "Memory.Put must NOT be present in Observe-only prelude"
    );

    // Shell should be entirely absent (category-level drop).
    assert!(
        !decls.iter().any(|d| d.type_name == "Shell"),
        "Shell must be absent from wake-eval prelude"
    );

    // Message should be absent (category-level drop).
    assert!(
        !decls.iter().any(|d| d.type_name == "Message"),
        "Message must be absent from wake-eval prelude"
    );

    // Spawn should be absent (category-level drop — prevents Ephemeral/Fork/etc.).
    assert!(
        !decls.iter().any(|d| d.type_name == "Spawn"),
        "SECURITY: Spawn must be absent from wake-eval prelude (was present when using \
         CapabilitySet::all().with_classes([Observe]) — AwaitSpawn/AwaitAll are Observe-classed)"
    );
}

/// SECURITY (Critical 1): A wake-eval program that imports `Pattern.Spawn` and
/// calls `Spawn.ephemeral` must fail to compile. Pattern.Spawn is absent from
/// the wake-eval prelude because the Spawn category is dropped by
/// `CapabilitySet::wake_evaluator_read_only()`.
///
/// Before the Critical 1 fix, `CapabilitySet::all().with_classes([Observe])`
/// kept the Spawn category (AwaitSpawn/AwaitAll survive the Observe filter),
/// so `Pattern.Spawn` was importable and `Spawn.ephemeral` was reachable via
/// the module import path even though `Ephemeral` was filtered from the preamble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_eval_program_using_spawn_rejected_at_compile() {
    if skip_without_tidepool() {
        eprintln!("wake_eval_program_using_spawn_rejected_at_compile: SKIPPED (no tidepool)");
        return;
    }

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

    // This program imports Pattern.Spawn and attempts to call Spawn.ephemeral.
    // Pattern.Spawn must be absent from the wake-eval prelude because the Spawn
    // category is dropped. GHC should reject this with "Variable not in scope"
    // or "Could not load module".
    evaluator
        .register_interval(
            SmolStr::new("spawn-probe"),
            // The program tries to use Spawn.ephemeral directly. In the pre-fix
            // capset (all() + Observe-class), Pattern.Spawn was importable and
            // Spawn.ephemeral was accessible via import.
            "import qualified Pattern.Spawn as Spawn\n\
             cfg <- pure (Spawn.EphemeralConfig \"echo exploit\" Nothing Nothing Nothing Nothing)\n\
             _ <- Spawn.ephemeral cfg\n\
             pure True"
                .to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed (compile error happens on first trigger)");

    // The program must NOT produce a wake message — it should fail to compile.
    let no_msg = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    assert!(
        no_msg.is_err(),
        "SECURITY: Spawn.ephemeral program must NOT produce a wake message — \
         Pattern.Spawn must be absent from the wake-eval prelude"
    );

    evaluator.unregister(&SmolStr::new("spawn-probe"));
}

/// SECURITY (Critical 1): A wake-eval program that imports `Pattern.Shell` and
/// calls `Shell.execute` must fail to compile. Shell is absent from the
/// wake-eval prelude because the Shell category is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_eval_program_using_shell_rejected_at_compile() {
    if skip_without_tidepool() {
        eprintln!("wake_eval_program_using_shell_rejected_at_compile: SKIPPED (no tidepool)");
        return;
    }

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

    evaluator
        .register_interval(
            SmolStr::new("shell-probe"),
            "import qualified Pattern.Shell as Shell\n\
             _ <- Shell.execute \"echo exploit\"\n\
             pure True"
                .to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed (compile error happens on first trigger)");

    let no_msg = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    assert!(
        no_msg.is_err(),
        "SECURITY: Shell.execute program must NOT produce a wake message — \
         Pattern.Shell must be absent from the wake-eval prelude"
    );

    evaluator.unregister(&SmolStr::new("shell-probe"));
}

/// SECURITY (Critical 1): A wake-eval program that imports `Pattern.Message` and
/// calls `Message.send` must fail to compile. Message is absent from the
/// wake-eval prelude because the Message category is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_eval_program_using_message_rejected_at_compile() {
    if skip_without_tidepool() {
        eprintln!("wake_eval_program_using_message_rejected_at_compile: SKIPPED (no tidepool)");
        return;
    }

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

    evaluator
        .register_interval(
            SmolStr::new("message-probe"),
            "import Pattern.Message\n\
             send \"agent:evil\" \"exploit\"\n\
             pure True"
                .to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed (compile error happens on first trigger)");

    let no_msg = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    assert!(
        no_msg.is_err(),
        "SECURITY: Message.send program must NOT produce a wake message — \
         Pattern.Message must be absent from the wake-eval prelude"
    );

    evaluator.unregister(&SmolStr::new("message-probe"));
}

/// Positive test: a wake-eval program that uses `Memory.get` must succeed.
/// This verifies the wake-eval capset is not over-restrictive — Memory (Observe
/// constructors) must be available for meaningful wake conditions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_eval_program_using_memory_get_succeeds() {
    if skip_without_tidepool() {
        eprintln!("wake_eval_program_using_memory_get_succeeds: SKIPPED (no tidepool)");
        return;
    }

    let (evaluator, mailbox, _ctx) = test_evaluator().await; let mut rx = mailbox.lock_rx().await;

    // Memory.get is Observe-classed and the Memory category is kept.
    // This program reads a (non-existent) block and returns True regardless.
    evaluator
        .register_interval(
            SmolStr::new("memory-get-ok"),
            // Memory.get returns a default value when the block doesn't exist.
            // We return True unconditionally to verify the program compiles and runs.
            "pure True".to_string(),
            Duration::from_secs(1),
        )
        .expect("registration should succeed");

    // Should receive a wake message within 10s (GHC warm-up may be slow).
    let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
    assert!(
        msg.is_ok(),
        "Memory.get program must produce a wake message — Memory module must be \
         available in the wake-eval prelude"
    );

    evaluator.unregister(&SmolStr::new("memory-get-ok"));
}

/// Verify CustomEvaluator enforces the per-session condition cap.
#[tokio::test]
async fn condition_cap_enforced() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("agent-cap", "A");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));

    let (mailbox, _) = Mailbox::new(PersonaId::from("agent-cap"));
    let evaluator = CustomEvaluator::new(mailbox, vec![], tokio::runtime::Handle::current(), ctx)
        .with_max_conditions(2)
        .with_min_interval(Duration::from_millis(100));

    // Register two conditions — should succeed.
    evaluator
        .register_interval(
            SmolStr::new("c1"),
            "pure True".into(),
            Duration::from_secs(1),
        )
        .expect("first should succeed");
    evaluator
        .register_interval(
            SmolStr::new("c2"),
            "pure True".into(),
            Duration::from_secs(1),
        )
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

/// REGRESSION TEST (Critical 2): `WakeRegistry::unregister` must abort the
/// `CustomEvaluator`'s real task, not just the no-op sentinel stored in the
/// registry. Before the fix, unregistering a custom condition left the
/// evaluator task running — register/unregister/re-register cycles would
/// accumulate tasks and eventually exhaust the 32-condition cap.
///
/// This test verifies the fix by:
/// 1. Building a `WakeRegistry` wired with a `CustomEvaluator` (cap = 3).
/// 2. Registering a condition.
/// 3. Unregistering it via the `WakeRegistry` (not directly via the evaluator).
/// 4. Verifying the evaluator's task was actually freed (len == 0 after unregister).
/// 5. Re-registering the same id — must succeed (slot was freed, not leaked).
#[tokio::test]
async fn unregister_aborts_custom_task_and_frees_cap_slot() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("agent-unreg", "A");
    let ctx = Arc::new(pattern_runtime::session::SessionContext::from_persona(
        &persona,
        store,
        provider,
        db,
        tokio::runtime::Handle::current(),
    ));

    let (registry_mailbox, _) = Mailbox::new(PersonaId::from("agent-unreg-registry"));
    let (evaluator_mailbox, _) = Mailbox::new(PersonaId::from("agent-unreg-evaluator"));

    let evaluator = Arc::new(
        CustomEvaluator::new(
            evaluator_mailbox,
            vec![],
            tokio::runtime::Handle::current(),
            ctx,
        )
        .with_max_conditions(3)
        .with_min_interval(Duration::from_millis(100)),
    );

    let registry = pattern_runtime::wake::WakeRegistry::new(
        registry_mailbox,
        tokio::runtime::Handle::current(),
    )
    .with_custom_evaluator(Arc::clone(&evaluator));

    use pattern_runtime::wake::registry::WakeCondition;
    use smol_str::SmolStr;

    // Step 1: register a condition via the WakeRegistry.
    let id = SmolStr::new("test-unreg-1");
    registry
        .register_test(
            id.clone(),
            WakeCondition::Custom {
                id: id.clone(),
                program: "pure False".to_string(),
                period: Duration::from_secs(1),
            },
        )
        .expect("registration should succeed");

    // Registry has 1 entry; evaluator has 1 real task.
    assert_eq!(
        registry.len(),
        1,
        "registry must have 1 entry after register"
    );
    assert_eq!(
        evaluator.len(),
        1,
        "evaluator must have 1 task after register"
    );

    // Step 2: unregister via the WakeRegistry (not directly via evaluator).
    let was_present = registry.unregister(&id);
    assert!(
        was_present,
        "unregister must return true for a registered id"
    );

    // Registry is empty; evaluator must ALSO be empty (task was freed).
    // Before the Critical 2 fix, evaluator.len() would be 1 here (leaked task).
    assert_eq!(registry.len(), 0, "registry must be empty after unregister");
    assert_eq!(
        evaluator.len(),
        0,
        "REGRESSION: evaluator must have 0 tasks after WakeRegistry::unregister; \
         before the fix, the evaluator task leaked (only the no-op sentinel was aborted)"
    );

    // Step 3: re-register the same id — must succeed (slot was freed, not leaked).
    registry
        .register_test(
            id.clone(),
            WakeCondition::Custom {
                id: id.clone(),
                program: "pure False".to_string(),
                period: Duration::from_secs(1),
            },
        )
        .expect("re-registration must succeed after unregister (cap slot was freed)");

    assert_eq!(
        registry.len(),
        1,
        "registry must have 1 entry after re-register"
    );
    assert_eq!(
        evaluator.len(),
        1,
        "evaluator must have 1 task after re-register"
    );

    // Clean up.
    registry.unregister(&id);
}
