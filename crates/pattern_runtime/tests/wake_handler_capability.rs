// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Capability-gate behaviour of `Pattern.Wake` handler.
//!
//! Verifies AC7.5: registering / unregistering a wake condition
//! without [`pattern_core::CapabilityFlag::WakeConditionRegistration`]
//! returns an [`tidepool_effect::EffectError::Handler`] whose message
//! starts with [`pattern_runtime::policy::CAPABILITY_DENIED_PREFIX`].

use std::sync::Arc;

use tidepool_effect::{EffectContext, EffectHandler};

use pattern_core::CapabilitySet;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_runtime::NopProviderClient;
use pattern_runtime::mailbox::Mailbox;
use pattern_runtime::policy::CAPABILITY_DENIED_PREFIX;
use pattern_runtime::sdk::handlers::WakeHandler;
use pattern_runtime::sdk::handlers::wake::WAKE_REGISTRY_MISSING_PREFIX;
use pattern_runtime::sdk::requests::WakeReq;
use pattern_runtime::sdk::requests::wake::WireWakeCondition;
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, standard_datacon_table};
use pattern_runtime::wake::WakeRegistry;

/// Build a test session with an optional capability set and an optional
/// wake registry. Passing `wire_registry = true` attaches a live
/// `WakeRegistry`; `false` leaves it unset (for missing-registry tests).
async fn build_session_opts(
    caps: Option<CapabilitySet>,
    wire_registry: bool,
) -> Arc<SessionContext> {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("agent-wake-cap-test", "agent-wake-cap-test");
    persona.capabilities = caps;
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    let ctx = if wire_registry {
        let (mailbox, _) = Mailbox::new(PersonaId::from("wake-cap-test"));
        let registry = Arc::new(WakeRegistry::new(
            mailbox,
            tokio::runtime::Handle::current(),
        ));
        ctx.with_wake_registry(registry)
    } else {
        ctx
    };
    Arc::new(ctx)
}

async fn build_session(caps: Option<CapabilitySet>) -> Arc<SessionContext> {
    build_session_opts(caps, true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_without_capability_is_denied() {
    // Empty capability set: no flags, no categories.
    let ctx = build_session(Some(CapabilitySet::empty())).await;
    let table = standard_datacon_table();
    let mut h = WakeHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let res = h.handle(WakeReq::Register(None, WireWakeCondition::Interval(60_000)), &cx);
    let err = res.expect_err("registration without flag must be denied");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "expected CapabilityDenied prefix; got: {msg}"
    );
    assert!(
        msg.contains("wake-condition-registration"),
        "denial should name the missing flag; got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregister_without_capability_is_denied() {
    let ctx = build_session(Some(CapabilitySet::empty())).await;
    let table = standard_datacon_table();
    let mut h = WakeHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(WakeReq::Unregister("nonexistent".into()), &cx)
        .expect_err("unregister without flag must be denied");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "unregister denial should also use CapabilityDenied prefix; got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_with_capability_succeeds_for_interval() {
    // Caps with the flag — registration should succeed.
    let caps = CapabilitySet::all();
    let ctx = build_session(Some(caps)).await;
    let table = standard_datacon_table();
    let mut h = WakeHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let _ = h
        .handle(WakeReq::Register(None, WireWakeCondition::Interval(60_000)), &cx)
        .expect("interval registration should succeed");
    assert_eq!(ctx.wake_registry().expect("registry").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_without_capabilities_set_is_denied() {
    // `None` capabilities is fail-closed: the handler cannot distinguish
    // "this session deliberately has full power" from "nobody configured
    // capabilities yet". The daemon explicitly passes `CapabilitySet::all()`
    // so production sessions are unaffected; test sessions and pre-capability
    // code must now pass `CapabilitySet::all()` explicitly.
    let ctx = build_session(None).await;
    let table = standard_datacon_table();
    let mut h = WakeHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(WakeReq::Register(None, WireWakeCondition::Interval(60_000)), &cx)
        .expect_err("None caps must be denied (fail-closed)");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "expected CapabilityDenied prefix; got: {msg}"
    );
}

/// Minor 4: missing-registry path surfaces the WAKE_REGISTRY_MISSING_PREFIX.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_without_wake_registry_returns_registry_missing_prefix() {
    // Session has the flag (CapabilitySet::all) but no WakeRegistry wired.
    let ctx = build_session_opts(Some(CapabilitySet::all()), false).await;
    let table = standard_datacon_table();
    let mut h = WakeHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(WakeReq::Register(None, WireWakeCondition::Interval(60_000)), &cx)
        .expect_err("missing registry must return an error");
    let msg = err.to_string();
    assert!(
        msg.contains(WAKE_REGISTRY_MISSING_PREFIX),
        "expected WakeRegistryMissing prefix; got: {msg}"
    );
}
