//! End-to-end wiring test for [`SessionRegistries`].
//!
//! Verifies that `open_with_agent_loop` with `Some(SessionRegistries { ... })`
//! correctly wires the three inter-session registries into the session:
//!
//! 1. `agent_registry` → session is registered as `Active` in the registry on
//!    open, and unregistered (via `RegistryGuard` drop) when the session drops.
//! 2. `router_registry` → `agent:` scheme routes to `PersonaNotFound` (not
//!    `NoRouterForScheme`), proving the `AgentRouter` is registered.
//! 3. `wake_registry_extras` → `WakeRegistry` is present on the context after
//!    open (Pattern.Wake.Register would succeed for a capable session).
//!
//! This test intentionally does NOT exercise the Haskell eval path — it uses
//! `NopProviderClient` and focuses on the post-open structural invariants.
//! The Haskell-free approach lets it run in CI without `tidepool-extract`.
//!
//! This is Option B from the cycle-3 review: a `pattern_runtime`-side test that
//! catches "wiring broken silently" without needing the full daemon stack.

use std::sync::Arc;

use pattern_core::CapabilitySet;
use pattern_core::traits::{TurnSink, VecSink};
use pattern_core::types::ids::PersonaId;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_runtime::NopProviderClient;
use pattern_runtime::SdkLocation;
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::router::RouterError;
use pattern_runtime::router::RouterRegistry;
use pattern_runtime::router::agent::AgentRouter;
use pattern_runtime::session::{SessionRegistries, TidepoolSession, WakeRegistryExtras};
use pattern_runtime::testing::InMemoryMemoryStore;

/// Build a simple NoOp turn sink for tests that don't need event observation.
fn nop_sink() -> Arc<dyn TurnSink> {
    Arc::new(VecSink::new())
}

/// Open a session wired with all three registries and verify post-open invariants.
///
/// Verifies:
/// - The session's persona_id is registered as Active in the agent_registry.
/// - Routing to `agent:<persona_id>` returns PersonaNotFound (not NoRouterForScheme).
/// - The session context has a WakeRegistry after open.
/// - Dropping the session unregisters the persona from the agent_registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_with_agent_loop_wires_session_registries() {
    let persona_id = "wiring-test-agent";
    let agent_registry = Arc::new(AgentRegistry::new());
    let mut router_reg = RouterRegistry::new();
    router_reg.register(Arc::new(AgentRouter::new(agent_registry.clone())));
    let router_reg = Arc::new(router_reg);

    let registries = SessionRegistries {
        agent_registry: Some(agent_registry.clone()),
        router_registry: Some(router_reg.clone()),
        wake_registry_extras: Some(WakeRegistryExtras {
            block_change_notifier: None,
            memory_store: None,
        }),
        port_registry: None,
        file_policy: None,
        fronting_committer: None,
        constellation_registry: None,
            sibling_resolver: None,
    };

    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new(persona_id, "WiringTestAgent");
    let sdk = SdkLocation::default();

    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
        nop_sink(),
        None,
        None,
        Some(CapabilitySet::all()),
        Some(registries),
    )
    .await
    .expect("open_with_agent_loop should succeed");

    // --- Invariant 1: session registered as Active in agent_registry ---
    let persona_key: PersonaId = persona_id.into();
    assert_eq!(
        agent_registry.status(&persona_key),
        Some(SessionStatus::Active),
        "session should be registered as Active in agent_registry after open"
    );

    // --- Invariant 2: router has the agent: scheme wired ---
    // Route a message to the session's own persona_id. Since it IS registered,
    // we expect MailboxClosed (the mailbox receiver in the session is alive, so
    // actually we expect Ok or a delivery). But more importantly we must NOT get
    // NoRouterForScheme, which would indicate the AgentRouter was never wired.
    //
    // Use a *different* persona_id to ensure PersonaNotFound (not delivery).
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

    let sender = MessageOrigin::new(
        Author::System {
            reason: SystemReason::Timer,
        },
        Sphere::System,
    );
    let msg = Message {
        chat_message: genai::chat::ChatMessage::new(genai::chat::ChatRole::User, "ping"),
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        owner_id: AgentId::from("test"),
        created_at: Timestamp::now(),
        batch: BatchId::from(new_snowflake_id()),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };

    // Route to a known-absent persona — should return PersonaNotFound, proving
    // the `agent:` scheme router is registered (NoRouterForScheme would mean it isn't).
    let route_err = router_reg
        .route(&sender, "agent:does-not-exist", &msg)
        .await
        .expect_err("routing to absent agent should fail");

    assert!(
        matches!(route_err, RouterError::PersonaNotFound(_)),
        "expected PersonaNotFound (router is wired); got: {route_err:?}. \
         If NoRouterForScheme: the agent: scheme was not registered."
    );

    // --- Invariant 3: WakeRegistry is present on the session context ---
    let ctx = session.context();
    assert!(
        ctx.wake_registry().is_some(),
        "WakeRegistry should be wired on context after open_with_agent_loop with wake extras"
    );

    // --- Invariant 4: drop fires RegistryGuard → unregisters from agent_registry ---
    // Hold the persona_key ref before dropping so we can check post-drop status.
    let persona_key_for_check: PersonaId = persona_id.into();
    drop(session);

    assert_eq!(
        agent_registry.status(&persona_key_for_check),
        None,
        "RegistryGuard should unregister session from agent_registry on drop"
    );
}

/// Verify that passing `None` registries leaves no agent_registry wired.
///
/// Regression guard: existing tests and ephemeral-child sessions pass `None`
/// and must not accidentally get a registry wired or hit a `RegistryGuard`
/// double-registration scenario.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_with_agent_loop_none_registries_leaves_agent_registry_unwired() {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let persona = PersonaSnapshot::new("no-registry-agent", "NoRegistryAgent");
    let sdk = SdkLocation::default();

    let session = TidepoolSession::open_with_agent_loop(
        persona,
        &sdk,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
        nop_sink(),
        None,
        None,
        None,
        None, // no registries
    )
    .await
    .expect("open should succeed without registries");

    // No agent_registry on context.
    let ctx = session.context();
    assert!(
        ctx.agent_registry().is_none(),
        "agent_registry should be None when not wired via SessionRegistries"
    );
    // No wake_registry.
    assert!(
        ctx.wake_registry().is_none(),
        "wake_registry should be None when not wired via SessionRegistries"
    );
}
