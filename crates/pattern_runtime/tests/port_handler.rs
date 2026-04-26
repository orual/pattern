//! Integration tests for the Phase 4 Port subsystem — AC4.2 through AC4.9.
//!
//! # AC coverage
//!
//! | AC  | Test                                            | Mechanism                             |
//! |-----|-------------------------------------------------|---------------------------------------|
//! | 4.1 | doctest on `Port` trait                         | Verified at compile/doc time           |
//! | 4.2 | `port_list_returns_registered_metadatas`        | 3 MockPorts → List → 3 entries         |
//! | 4.3 | `port_call_dispatches_to_registered_port`       | MockPort call_response → Call → match  |
//! | 4.4 | `port_subscribe_delivers_events_via_attachments`| Subscribe + push → next turn has PortEvent attachments |
//! | 4.5 | `port_unsubscribe_stops_event_delivery`         | Subscribe → Unsubscribe → no new events|
//! | 4.6 | `port_library_appended_to_preamble_when_capable`| MockPort library → preamble has source |
//! | 4.7 | `port_call_capability_denied_blocks_dispatch`   | Cap set without port → CapabilityDenied|
//! | 4.8 | `port_call_unknown_port_returns_not_found`      | Unregistered id → NotFound             |
//! | 4.9 | `port_library_excluded_when_not_capable`        | Port not in caps → preamble has no lib |
//!
//! # Architecture note: why `spawn_blocking`
//!
//! `PortHandler::handle` calls `tokio::sync::mpsc::Sender::blocking_send` to
//! dispatch ops to the dispatcher actor task. `blocking_send` panics when
//! invoked from within an async tokio context. In production this is fine
//! because the handler runs on the Tidepool eval-worker thread (no ambient
//! runtime). In tests, which run inside `#[tokio::test]`, we must wrap each
//! handler dispatch in `tokio::task::spawn_blocking` to get a non-runtime
//! thread.
//!
//! Tests use `Arc<SessionContext>` so the context can be shared across
//! multiple `spawn_blocking` closures without move-conflicts.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::CapabilitySet;
use pattern_core::ProviderClient;
use pattern_core::capability::EffectCategory;
use pattern_core::traits::MemoryStore;
use pattern_core::traits::PortRegistry;
use pattern_core::types::message::MessageAttachment;
use pattern_core::types::port::{PortEvent, PortId};
use pattern_core::types::snapshot::PersonaSnapshot;
use smol_str::SmolStr;
use tidepool_bridge::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_repr::DataConTable;
use tidepool_testing::r#gen::standard_datacon_table;

use pattern_runtime::port_registry::PortRegistryImpl;
use pattern_runtime::sdk::handlers::port::PortHandler;
use pattern_runtime::sdk::preamble;
use pattern_runtime::sdk::requests::PortReq;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, MockPort, NopProviderClient, test_db};

// ── helpers ────────────────────────────────────────────────────────��─────────

/// Build a `DataConTable` with unit constructor `()` (needed by `cx.respond(())`).
fn handler_table() -> DataConTable {
    use tidepool_repr::{DataCon, DataConId};
    let mut table = standard_datacon_table();
    table.insert(DataCon {
        id: DataConId(100),
        name: "()".to_string(),
        tag: 1,
        rep_arity: 0,
        field_bangs: vec![],
        qualified_name: Some("GHC.Tuple.()".to_string()),
    });
    table
}

/// Build an `Arc<SessionContext>` with a fresh `PortRegistryImpl` injected.
///
/// Returns the context (Arc so it can be shared across spawn_blocking
/// closures) and the registry so callers can register ports.
async fn make_ctx_with_registry() -> (Arc<SessionContext>, Arc<PortRegistryImpl>) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-port-test", "A");

    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));
    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db)
            .with_port_registry(Arc::clone(&registry)),
    );
    (ctx, registry)
}

/// Build a context with restricted `CapabilitySet` (no Port category at all).
async fn make_ctx_no_port_cap() -> (Arc<SessionContext>, Arc<PortRegistryImpl>) {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-port-test-no-cap", "A");

    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));
    // Memory + Message only — no Port category in the set.
    let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]);
    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db)
            .with_port_registry(Arc::clone(&registry))
            .with_capabilities(Some(caps)),
    );
    (ctx, registry)
}

/// Dispatch a `PortReq` via `spawn_blocking`.
///
/// `PortHandler::handle` calls `blocking_send` internally (simulating the
/// eval-worker thread's calling convention). `blocking_send` panics when
/// called from within an async tokio context, so we dispatch on a
/// non-runtime thread via `spawn_blocking`. The `Arc<SessionContext>` is
/// cloned into the closure and dereferenced there.
async fn dispatch(
    ctx: Arc<SessionContext>,
    req: PortReq,
) -> Result<tidepool_eval::Value, EffectError> {
    let table = Arc::new(handler_table());
    tokio::task::spawn_blocking(move || {
        let mut handler = PortHandler;
        let cx = EffectContext::with_user(&table, &*ctx);
        handler.handle(req, &cx)
    })
    .await
    .expect("spawn_blocking task must not panic")
}

/// Extract a `String` from a handler response (a Haskell `Text` value).
fn extract_string(val: &tidepool_eval::Value, table: &DataConTable) -> String {
    <String as FromCore>::from_value(val, table).expect("expected Text string in handler response")
}

// ── AC4.2: List returns all registered port metadatas ────────────────────────

/// AC4.2: `Port.List` returns metadata for all registered ports visible to the
/// agent. Full-power session (no capability restriction) sees all three.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_list_returns_registered_metadatas() {
    let (ctx, registry) = make_ctx_with_registry().await;

    // Register three ports.
    for id in &["alpha", "beta", "gamma"] {
        let port = MockPort::new(id);
        registry
            .register_sync(port as Arc<dyn pattern_core::traits::Port>)
            .unwrap();
    }

    let val = dispatch(Arc::clone(&ctx), PortReq::List)
        .await
        .expect("List must succeed");

    // The response is a Haskell `[Text]` — decode as Vec<String>.
    let table = handler_table();
    let list: Vec<String> = <Vec<String> as FromCore>::from_value(&val, &table)
        .expect("List must decode as Vec<String>");

    assert_eq!(list.len(), 3, "expected 3 port entries, got: {list:?}");

    // Each entry is JSON-serialized PortMetadata — verify the ids appear.
    for id in ["alpha", "beta", "gamma"] {
        assert!(
            list.iter().any(|s| s.contains(id)),
            "List must contain port id {id:?}; got: {list:?}"
        );
    }
}

// ── AC4.3: Call dispatches to the correct port ───────────────────────────────

/// AC4.3: `Port.Call(id, method, payload)` dispatches to the registered port
/// and returns its JSON response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_call_dispatches_to_registered_port() {
    let (ctx, registry) = make_ctx_with_registry().await;

    let port = MockPort::new("ping-port");
    port.set_call_response(Ok(serde_json::json!({"pong": true, "value": 42})));
    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    let val = dispatch(
        Arc::clone(&ctx),
        PortReq::Call("ping-port".into(), "ping".into(), "{}".into()),
    )
    .await
    .expect("Call must succeed");

    let table = handler_table();
    let response_str = extract_string(&val, &table);
    let response: serde_json::Value =
        serde_json::from_str(&response_str).expect("response must be JSON");

    assert_eq!(
        response["pong"].as_bool(),
        Some(true),
        "response must contain pong=true; got: {response}"
    );
    assert_eq!(
        response["value"].as_i64(),
        Some(42),
        "response must contain value=42; got: {response}"
    );
}

// ── AC4.4: Subscribe delivers events as PortEvent attachments ────────────────

/// AC4.4: `Port.Subscribe` causes the dispatcher to push
/// `MessageAttachment::PortEvent` entries into the session's
/// `async_reminder_queue` as events arrive on the subscription stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_subscribe_delivers_events_via_attachments() {
    let (ctx, registry) = make_ctx_with_registry().await;

    let port = MockPort::new("evt-port");
    let now = jiff::Timestamp::now();
    port.push_event(PortEvent::new(
        PortId::new("evt-port"),
        serde_json::json!({"n": 1}),
        now,
    ));
    port.push_event(PortEvent::new(
        PortId::new("evt-port"),
        serde_json::json!({"n": 2}),
        now,
    ));
    port.push_event(PortEvent::new(
        PortId::new("evt-port"),
        serde_json::json!({"n": 3}),
        now,
    ));

    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // Subscribe — the dispatcher actor spawns a drain task. The subscribe()
    // call returns a snapshot of the 3 pre-queued events; the drain task
    // pushes them into the async-reminder queue.
    let val = dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("evt-port".into(), "{}".into()),
    )
    .await
    .expect("Subscribe must succeed");

    // Subscribe returns `()` on success.
    assert!(
        matches!(val, tidepool_eval::Value::Con(..)),
        "Subscribe must return ()"
    );

    // Wait for the drain task to push events into the async-reminder queue.
    // Condition-based polling (no arbitrary sleep): check until all 3 events
    // arrive or the deadline passes.
    let queue = ctx.async_reminder_queue();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let len = queue.lock().unwrap().len();
        if len >= 3 {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("drain task did not push 3 events within 5s; got {len}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let attachments = queue.lock().unwrap();
    assert_eq!(attachments.len(), 3, "expected 3 PortEvent attachments");

    for (i, attachment) in attachments.iter().enumerate() {
        match attachment {
            MessageAttachment::PortEvent {
                port_id, payload, ..
            } => {
                assert_eq!(
                    port_id, "evt-port",
                    "attachment {i} must have port_id='evt-port'"
                );
                let n = payload["n"].as_i64().expect("payload must have 'n'");
                assert_eq!(
                    n,
                    (i + 1) as i64,
                    "event {i} must have n={}, got n={n}",
                    i + 1
                );
            }
            other => panic!("expected PortEvent attachment, got {other:?}"),
        }
    }
}

// ── AC4.5: Unsubscribe stops event delivery ──────────────────────────────────

/// AC4.5: `Port.Unsubscribe` aborts the drain task so no further events
/// arrive after the call returns.
///
/// Strategy: subscribe to a port with one pre-queued event, wait for it,
/// then unsubscribe. Verify the queue count stays stable (no new events).
/// Then re-subscribe to confirm the handler path remains usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_unsubscribe_stops_event_delivery() {
    let (ctx, registry) = make_ctx_with_registry().await;

    let port = MockPort::new("unsub-port");
    // Push 1 event for the initial subscribe.
    let now = jiff::Timestamp::now();
    port.push_event(PortEvent::new(
        PortId::new("unsub-port"),
        serde_json::json!({"phase": "before-unsub"}),
        now,
    ));

    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // Subscribe — drain task will push the 1 pre-queued event.
    dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("unsub-port".into(), "{}".into()),
    )
    .await
    .expect("Subscribe must succeed");

    // Wait for the 1 pre-queued event.
    let queue = ctx.async_reminder_queue();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !queue.lock().unwrap().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("expected 1 event before unsubscribe");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Unsubscribe — abort the drain task.
    let unsub_val = dispatch(Arc::clone(&ctx), PortReq::Unsubscribe("unsub-port".into()))
        .await
        .expect("Unsubscribe must succeed");
    assert!(
        matches!(unsub_val, tidepool_eval::Value::Con(..)),
        "Unsubscribe must return ()"
    );

    // Count events in the queue right after unsubscribe.
    let count_after_unsub = queue.lock().unwrap().len();
    assert_eq!(
        count_after_unsub, 1,
        "exactly 1 event should have arrived before unsubscribe"
    );

    // Re-subscribe (confirms the handler path works cleanly after unsubscribe).
    dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("unsub-port".into(), "{}".into()),
    )
    .await
    .expect("Re-subscribe after Unsubscribe must succeed");

    // Give the new drain task a moment. Since we didn't push new events,
    // the queue should stay at 1.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let count_after_resub = queue.lock().unwrap().len();
    assert_eq!(
        count_after_resub, 1,
        "no new events; count should stay at 1 after re-subscribe with empty stream"
    );
}

// ── AC4.6: Port library appended to preamble when capable ────────────────────

/// AC4.6: When a port has a `library()` and the agent's `CapabilitySet`
/// permits that port, the library source is spliced into the preamble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_library_appended_to_preamble_when_capable() {
    let (_ctx, registry) = make_ctx_with_registry().await;

    const LIB_SRC: &str = "-- Mock helpers\nmockHelper :: Int -> Int\nmockHelper x = x + 1\n";
    let port = MockPort::new_with_library("lib-port", LIB_SRC);
    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // Full-power CapabilitySet — all ports visible (None = full power in the
    // context, has_port always returns true).
    // Use PortRegistry trait methods directly (trait is in scope via `use`).
    let registry_ref: &dyn PortRegistry = registry.as_ref();
    let metadatas = registry_ref.list();
    assert_eq!(metadatas.len(), 1);

    // Build the port_libraries list the way code_tool.rs would:
    let libraries: Vec<(PortId, &str)> = metadatas
        .iter()
        .filter_map(|m| {
            let port = registry_ref.get(&m.id)?;
            port.library().map(|src| (m.id.clone(), src))
        })
        .collect();

    let decls = pattern_runtime::sdk::bundle::canonical_effect_decls();
    let preamble_str = preamble::build_with_libraries(&decls, &libraries);

    assert!(
        preamble_str.contains("-- Port library: lib-port"),
        "preamble must contain port library header; got:\n{preamble_str}"
    );
    assert!(
        preamble_str.contains("mockHelper"),
        "preamble must contain mockHelper from library source"
    );
    assert!(
        preamble_str.contains("mockHelper x = x + 1"),
        "preamble must contain library function body"
    );

    // Library must appear before `type M`.
    let lib_pos = preamble_str
        .find("-- Port library: lib-port")
        .expect("library header must be present");
    let type_m_pos = preamble_str.find("type M = '").expect("type M alias");
    assert!(
        lib_pos < type_m_pos,
        "library must appear before type M (lib_pos={lib_pos}, type_m_pos={type_m_pos})"
    );
}

// ── AC4.7: Call with no capability returns CapabilityDenied ──────────────────

/// AC4.7: `Port.Call` to a port not in the agent's `CapabilitySet` returns
/// a capability-denied error before reaching the dispatcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_call_capability_denied_blocks_dispatch() {
    let (ctx, registry) = make_ctx_no_port_cap().await;

    let port = MockPort::new("denied-port");
    port.set_call_response(Ok(serde_json::json!({"should": "not reach here"})));
    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Call("denied-port".into(), "get".into(), "{}".into()),
    )
    .await
    .expect_err("Call to denied port must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("capability denied") || msg.contains("CapabilityDenied"),
                "error message must mention capability denied; got: {msg}"
            );
        }
        other => panic!("expected EffectError::Handler with capability denial, got: {other:?}"),
    }
}

/// AC4.7 variant: Subscribe to a port not in the CapabilitySet also returns
/// a capability denial error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_subscribe_capability_denied_blocks_dispatch() {
    let (ctx, registry) = make_ctx_no_port_cap().await;

    let port = MockPort::new("denied-sub");
    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("denied-sub".into(), "{}".into()),
    )
    .await
    .expect_err("Subscribe to denied port must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("capability denied") || msg.contains("CapabilityDenied"),
                "error message must mention capability denied; got: {msg}"
            );
        }
        other => panic!("expected capability denial error, got: {other:?}"),
    }
}

// ── AC4.8: Call to unregistered port returns NotFound ────────────────────────

/// AC4.8: `Port.Call` with a `PortId` not registered in the registry returns
/// a not-found error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_call_unknown_port_returns_not_found() {
    let (ctx, _registry) = make_ctx_with_registry().await;
    // No ports registered — any port id is unknown.

    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Call("does-not-exist".into(), "ping".into(), "{}".into()),
    )
    .await
    .expect_err("Call to unknown port must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("not found") || msg.contains("NotFound"),
                "error must mention port-not-found; got: {msg}"
            );
        }
        other => panic!("expected Handler error with not-found, got: {other:?}"),
    }
}

/// AC4.8 variant: Subscribe to an unregistered port also returns NotFound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_subscribe_unknown_port_returns_not_found() {
    let (ctx, _registry) = make_ctx_with_registry().await;

    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("ghost-port".into(), "{}".into()),
    )
    .await
    .expect_err("Subscribe to unregistered port must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("not found") || msg.contains("NotFound"),
                "error must mention port-not-found; got: {msg}"
            );
        }
        other => panic!("expected Handler error with not-found, got: {other:?}"),
    }
}

// ── AC4.9: Library excluded when port not in CapabilitySet ───────────────────

/// AC4.9: When a port has a `library()` but is NOT in the agent's
/// `CapabilitySet`, the library source must NOT appear in the preamble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_library_excluded_when_not_capable() {
    let (ctx, registry) = make_ctx_no_port_cap().await;

    const LIB_SRC: &str = "excludedHelper = pure ()\n";
    let port = MockPort::new_with_library("excluded-port", LIB_SRC);
    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // The agent's capability set doesn't include Port (no EffectCategory::Port
    // and no port-specific allowlist entry). Simulate the capability filtering
    // the way code_tool.rs does it: only include ports the agent can see.
    let caps = ctx.capabilities().cloned();
    let registry_ref: &dyn PortRegistry = registry.as_ref();
    let metadatas = registry_ref.list();

    let libraries: Vec<(PortId, &str)> = metadatas
        .iter()
        .filter(|m| {
            // Full power (None caps) → all ports included.
            // Restricted caps → only ports in allowlist.
            caps.as_ref()
                .map(|c| c.has_port(m.id.as_str()))
                .unwrap_or(true)
        })
        .filter_map(|m| {
            let port = registry_ref.get(&m.id)?;
            port.library().map(|src| (m.id.clone(), src))
        })
        .collect();

    // With no Port category and no allowlist entry, the filtered list should
    // be empty (port is not visible to this agent).
    assert!(
        libraries.is_empty(),
        "capability-restricted agent must see no port libraries; got: {libraries:?}"
    );

    let decls = pattern_runtime::sdk::bundle::canonical_effect_decls();
    let preamble_str = preamble::build_with_libraries(&decls, &libraries);

    assert!(
        !preamble_str.contains("excludedHelper"),
        "preamble must NOT contain excluded library source"
    );
    assert!(
        !preamble_str.contains("-- Port library: excluded-port"),
        "preamble must NOT contain excluded library header"
    );
}

// ── Additional edge cases ─────────────────────────────────────────────────────

/// Port.List with a capability-restricted agent only shows permitted ports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_list_filters_by_capability() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-list-cap-test", "A");

    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));

    // Register two ports.
    let allowed = MockPort::new("allowed-port");
    let denied = MockPort::new("denied-port");
    registry
        .register_sync(allowed as Arc<dyn pattern_core::traits::Port>)
        .unwrap();
    registry
        .register_sync(denied as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // Build a CapabilitySet with Port category but only "allowed-port" in
    // the per-port allowlist.
    let caps = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Port])
        .with_resources(EffectCategory::Port, [SmolStr::from("allowed-port")]);

    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db)
            .with_port_registry(Arc::clone(&registry))
            .with_capabilities(Some(caps)),
    );

    let val = dispatch(Arc::clone(&ctx), PortReq::List)
        .await
        .expect("List must succeed");

    let table = handler_table();
    let list: Vec<String> = <Vec<String> as FromCore>::from_value(&val, &table)
        .expect("List must decode as Vec<String>");

    assert_eq!(
        list.len(),
        1,
        "only 1 port should be visible; got: {list:?}"
    );
    assert!(
        list[0].contains("allowed-port"),
        "visible port must be 'allowed-port'; got: {:?}",
        list[0]
    );
    assert!(
        !list.iter().any(|s| s.contains("denied-port")),
        "denied-port must not appear in filtered list"
    );
}

/// Port.List when no registry is wired returns DispatcherClosed error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_handler_no_registry_returns_dispatcher_closed() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-no-registry", "A");

    // Do NOT call with_port_registry — ctx.port_registry() returns None.
    let ctx = Arc::new(SessionContext::from_persona(&persona, store, provider, db));

    let err = dispatch(Arc::clone(&ctx), PortReq::List)
        .await
        .expect_err("List without registry must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("closed") || msg.contains("DispatcherClosed"),
                "error must mention dispatcher closed; got: {msg}"
            );
        }
        other => panic!("expected Handler error with DispatcherClosed, got: {other:?}"),
    }
}

/// Unsubscribe is idempotent — calling it when there's no active subscription
/// returns Ok without error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_unsubscribe_idempotent_when_no_subscription() {
    let (ctx, _registry) = make_ctx_with_registry().await;

    // Unsubscribe from a port we never subscribed to — must succeed silently.
    let val = dispatch(
        Arc::clone(&ctx),
        PortReq::Unsubscribe("never-subscribed".into()),
    )
    .await
    .expect("Unsubscribe from non-existent subscription must succeed");

    assert!(
        matches!(val, tidepool_eval::Value::Con(..)),
        "Unsubscribe must return () even with no prior subscription"
    );
}

// ── AC4.5 (live-stream): Unsubscribe actually aborts the drain task ──────────

/// AC4.5 (live-stream coverage): `Port.Unsubscribe` MUST abort the drain task
/// and stop future event delivery.
///
/// This test distinguishes "abort actually stopped delivery" from "stream
/// finished naturally before abort ran". It uses `MockPort::new_live` so the
/// subscription stream stays open indefinitely — a snapshot-mode port would
/// let the drain task finish naturally before `Unsubscribe` runs, making
/// `handle.abort()` a no-op and masking a missing-abort bug.
///
/// Protocol:
///   1. Subscribe to a live `MockPort`.
///   2. Push event 1 via `push_event_live`; poll until queue len == 1.
///   3. Call `Port.Unsubscribe`.
///   4. Push event 2 via `push_event_live`.
///   5. Wait 100ms (bounded delay — drain task would deliver event 2 within
///      a few milliseconds if abort didn't fire).
///   6. Assert queue len is still 1: event 2 must NOT have arrived.
///
/// Mutation-test property: replacing the `Op::Unsubscribe` match arm with a
/// no-op causes this test to FAIL because event 2 arrives on the live stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_unsubscribe_actually_aborts_drain_task() {
    let (ctx, registry) = make_ctx_with_registry().await;

    // Live-mode port: the drain task stays alive until aborted.
    let port = MockPort::new_live("live-unsub-port");
    // Hold a clone of the Arc to push events after subscribe.
    let port_for_push = Arc::clone(&port);

    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // Subscribe — dispatcher spawns drain task that reads from live stream.
    dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("live-unsub-port".into(), "{}".into()),
    )
    .await
    .expect("Subscribe must succeed");

    // Push event 1 and wait for it to land in the queue.
    let now = jiff::Timestamp::now();
    port_for_push.push_event_live(PortEvent::new(
        PortId::new("live-unsub-port"),
        serde_json::json!({"seq": 1}),
        now,
    ));

    let queue = ctx.async_reminder_queue();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !queue.lock().unwrap().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("event 1 never arrived in queue before deadline");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        queue.lock().unwrap().len(),
        1,
        "exactly 1 event should have arrived before Unsubscribe"
    );

    // Unsubscribe — this MUST abort the drain task.
    dispatch(
        Arc::clone(&ctx),
        PortReq::Unsubscribe("live-unsub-port".into()),
    )
    .await
    .expect("Unsubscribe must succeed");

    // Push event 2 AFTER unsubscribe. If the drain task was correctly aborted,
    // this event will never be consumed from the live stream.
    port_for_push.push_event_live(PortEvent::new(
        PortId::new("live-unsub-port"),
        serde_json::json!({"seq": 2}),
        now,
    ));

    // Wait a bounded delay. The drain task, if still running, would deliver
    // event 2 within a few milliseconds. 100ms is a generous margin.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let final_len = queue.lock().unwrap().len();
    assert_eq!(
        final_len, 1,
        "event 2 must NOT arrive after Unsubscribe aborted the drain task; \
         queue len should still be 1 but got {final_len}"
    );
}

/// Phase 4 review (cycle 2) Important — multiplex behaviour regression guard.
///
/// `drain_subscription` was changed in cycle-1 to use `event.port_id` (not the
/// registered `PortId`), enabling the plugin-as-multiplexer pattern: one
/// registered port may emit events tagged with logical sub-ids (e.g.
/// `slack:channel-alice`, `slack:channel-bob`).
///
/// This test pins that behaviour: register a port as `"slack"`, push an event
/// with `event.port_id = "slack:channel-alice"`, and assert that the resulting
/// `MessageAttachment::PortEvent.port_id` is `"slack:channel-alice"` — NOT the
/// registered handle `"slack"`.
///
/// Mutation-test property: reverting `drain_subscription` to use the
/// registered `port_id` (e.g., `port_id: port_id.to_string()` instead of
/// `port_id: event.port_id.to_string()`) makes this test FAIL because the
/// attachment would carry `"slack"` instead of the multiplex tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_subscribe_uses_event_port_id_for_multiplex() {
    let (ctx, registry) = make_ctx_with_registry().await;

    // Live-mode port registered as "slack". Events the test pushes carry a
    // distinct `event.port_id` — that's the multiplex tag.
    let port = MockPort::new_live("slack");
    let port_for_push = Arc::clone(&port);

    registry
        .register_sync(port as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("slack".into(), "{}".into()),
    )
    .await
    .expect("Subscribe must succeed");

    // Push an event with a multiplex tag distinct from the registered handle.
    let now = jiff::Timestamp::now();
    port_for_push.push_event_live(PortEvent::new(
        PortId::new("slack:channel-alice"),
        serde_json::json!({"text": "hi"}),
        now,
    ));

    // Wait for the drain task to deliver.
    let queue = ctx.async_reminder_queue();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !queue.lock().unwrap().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("event never arrived in queue before deadline");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let q = queue.lock().unwrap();
    assert_eq!(q.len(), 1, "expected exactly 1 attachment");
    match &q[0] {
        pattern_core::types::message::MessageAttachment::PortEvent { port_id, .. } => {
            assert_eq!(
                port_id, "slack:channel-alice",
                "attachment must carry event.port_id (the multiplex tag), \
                 NOT the registered port handle 'slack'; got: {port_id}"
            );
        }
        other => panic!("expected MessageAttachment::PortEvent, got: {other:?}"),
    }
}

// ── Important #3: per-port allowlist denial test for Call ────────────────────

/// AC4.7 variant: `Port.Call` is denied when the capability set includes the
/// Port category but the specific port_id is NOT in the per-port allowlist.
///
/// This covers the case "category present but allowlist excludes this port_id"
/// — distinct from the AC4.7 tests that use a session with no Port category
/// at all. Both denial paths exercise different branches of `has_port`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_call_denied_when_port_id_not_in_allowlist() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-allowlist-test", "A");

    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));

    // Register two ports: "alpha" is in the allowlist; "beta" is not.
    let alpha = MockPort::new("alpha");
    alpha.set_call_response(Ok(serde_json::json!({"from": "alpha"})));
    let beta = MockPort::new("beta");
    beta.set_call_response(Ok(serde_json::json!({"from": "beta"})));
    registry
        .register_sync(alpha as Arc<dyn pattern_core::traits::Port>)
        .unwrap();
    registry
        .register_sync(beta as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    // CapabilitySet with Port category present but only "alpha" in the
    // per-port resource allowlist.
    let caps = CapabilitySet::from_iter([EffectCategory::Port])
        .with_resources(EffectCategory::Port, [SmolStr::from("alpha")]);

    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db)
            .with_port_registry(Arc::clone(&registry))
            .with_capabilities(Some(caps)),
    );

    // Call to "beta" (not in allowlist) must be denied.
    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Call("beta".into(), "get".into(), "{}".into()),
    )
    .await
    .expect_err("Call to beta must fail: not in allowlist");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("capability denied") || msg.contains("CapabilityDenied"),
                "error message must mention capability denied; got: {msg}"
            );
        }
        other => panic!("expected EffectError::Handler with capability denial, got: {other:?}"),
    }

    // Sanity: Call to "alpha" (in allowlist) must succeed.
    let ok = dispatch(
        Arc::clone(&ctx),
        PortReq::Call("alpha".into(), "get".into(), "{}".into()),
    )
    .await
    .expect("Call to alpha (in allowlist) must succeed");

    let table = handler_table();
    let response_str = extract_string(&ok, &table);
    let response: serde_json::Value =
        serde_json::from_str(&response_str).expect("response must be JSON");
    assert_eq!(
        response["from"].as_str(),
        Some("alpha"),
        "alpha response must return {{from: alpha}}; got: {response}"
    );
}

/// AC4.7 variant: `Port.Subscribe` is denied when the port_id is not in the
/// per-port allowlist (category present, specific port absent).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_subscribe_denied_when_port_id_not_in_allowlist() {
    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
    let db = test_db().await;
    let persona = PersonaSnapshot::new("agent-allowlist-sub-test", "A");

    let registry = Arc::new(PortRegistryImpl::new(&tokio::runtime::Handle::current()));

    let allowed = MockPort::new("allowed-sub");
    let denied = MockPort::new("denied-sub-port");
    registry
        .register_sync(allowed as Arc<dyn pattern_core::traits::Port>)
        .unwrap();
    registry
        .register_sync(denied as Arc<dyn pattern_core::traits::Port>)
        .unwrap();

    let caps = CapabilitySet::from_iter([EffectCategory::Port])
        .with_resources(EffectCategory::Port, [SmolStr::from("allowed-sub")]);

    let ctx = Arc::new(
        SessionContext::from_persona(&persona, store, provider, db)
            .with_port_registry(Arc::clone(&registry))
            .with_capabilities(Some(caps)),
    );

    let err = dispatch(
        Arc::clone(&ctx),
        PortReq::Subscribe("denied-sub-port".into(), "{}".into()),
    )
    .await
    .expect_err("Subscribe to denied-sub-port must fail");

    match err {
        EffectError::Handler(msg) => {
            assert!(
                msg.contains("capability denied") || msg.contains("CapabilityDenied"),
                "error message must mention capability denied; got: {msg}"
            );
        }
        other => panic!("expected capability denial for Subscribe, got: {other:?}"),
    }
}
