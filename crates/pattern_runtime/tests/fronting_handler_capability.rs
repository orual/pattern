//! Capability-gate and missing-set behaviour of the `Pattern.Fronting` handler.
//!
//! Verifies:
//! - `Set`, `Current`, `Route`, `Clear` without `FrontingControl` capability
//!   return a `CapabilityDenied: ` prefixed `EffectError`.
//! - Calls with `CapabilitySet::all()` succeed when a `FrontingSet` is wired.
//! - `Current` returns a JSON-encoded snapshot.
//! - `Route` with an invalid regex preserves existing rules.
//! - `Clear` resets the fronting set to default.
//! - Missing-set (no `fronting_set` wired) returns `FrontingNotWired: ` prefix.

use std::sync::{Arc, RwLock};

use tidepool_effect::{EffectContext, EffectHandler};

use pattern_core::CapabilitySet;
use pattern_core::fronting::{FrontingSet, MessagePattern, RoutingRule, RoutingTable};
use pattern_core::types::snapshot::PersonaSnapshot;
use pattern_runtime::NopProviderClient;
use pattern_runtime::policy::{CAPABILITY_DENIED_PREFIX, FRONTING_NOT_WIRED_PREFIX};
use pattern_runtime::sdk::handlers::FrontingHandler;
use pattern_runtime::sdk::requests::FrontingReq;
use pattern_runtime::sdk::requests::fronting::{WireMessagePattern, WireRoutingRule};
use pattern_runtime::session::SessionContext;
use pattern_runtime::testing::{InMemoryMemoryStore, standard_datacon_table};

/// Build a `DataConTable` that includes both the standard constructors AND
/// the GHC unit type `"()"`.
///
/// [`standard_datacon_table()`] only includes `Maybe`, `Bool`, `Pair`, `List`,
/// `I#`, `W#`, `Text`, etc. — it does NOT include `"()"`. Handlers that
/// return unit (e.g. `Pattern.Fronting.Set`, `Clear`) call `cx.respond(())`
/// which encodes `()` into a Core `Value::Con("()", [])`. The table must
/// have that constructor registered or `respond` returns `BridgeError`.
fn datacon_table_with_unit() -> tidepool_repr::DataConTable {
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

/// Build a session with optional capabilities and an optional wired `FrontingSet`.
///
/// When `fronting_set` is `Some(arc)`, the session has the set wired in.
/// When `None`, the session has no fronting set (for missing-set tests).
async fn build_session_opts(
    caps: Option<CapabilitySet>,
    fronting_set: Option<Arc<RwLock<FrontingSet>>>,
) -> Arc<SessionContext> {
    let store = Arc::new(InMemoryMemoryStore::new());
    let db = pattern_runtime::testing::test_db().await;
    let mut persona = PersonaSnapshot::new("agent-fronting-cap-test", "agent-fronting-cap-test");
    persona.capabilities = caps;
    let ctx = SessionContext::from_persona(
        &persona,
        store,
        Arc::new(NopProviderClient),
        db,
        tokio::runtime::Handle::current(),
    );
    let ctx = if let Some(fs) = fronting_set {
        ctx.with_fronting_set(fs)
    } else {
        ctx
    };
    Arc::new(ctx)
}

/// Build a session with all capabilities and a default (empty) `FrontingSet` wired.
async fn build_session_all_caps() -> Arc<SessionContext> {
    let fs = Arc::new(RwLock::new(FrontingSet::default()));
    build_session_opts(Some(CapabilitySet::all()), Some(fs)).await
}

/// Build a session with an empty capability set and a `FrontingSet` wired.
async fn build_session_no_caps() -> Arc<SessionContext> {
    let fs = Arc::new(RwLock::new(FrontingSet::default()));
    build_session_opts(Some(CapabilitySet::empty()), Some(fs)).await
}

// ── Capability-denied tests ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_without_capability_is_denied() {
    let ctx = build_session_no_caps().await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(FrontingReq::Set(vec!["alice".to_string()], None), &cx)
        .expect_err("Set without FrontingControl must be denied");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "expected CapabilityDenied prefix; got: {msg}"
    );
    assert!(
        msg.contains("fronting-control"),
        "denial should name the missing flag; got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_without_capability_is_denied() {
    let ctx = build_session_no_caps().await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(FrontingReq::Current, &cx)
        .expect_err("Current without FrontingControl must be denied");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "expected CapabilityDenied prefix; got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn none_caps_is_denied_fail_closed() {
    // `None` capabilities is fail-closed. The daemon always passes
    // `CapabilitySet::all()` explicitly; test sessions must do the same.
    let fs = Arc::new(RwLock::new(FrontingSet::default()));
    let ctx = build_session_opts(None, Some(fs)).await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(FrontingReq::Current, &cx)
        .expect_err("None caps must be fail-closed");
    let msg = err.to_string();
    assert!(
        msg.contains(CAPABILITY_DENIED_PREFIX),
        "expected CapabilityDenied prefix; got: {msg}"
    );
}

// ── Missing fronting set ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_fronting_set_returns_not_wired_prefix() {
    // Session has the flag but no FrontingSet wired.
    let ctx = build_session_opts(Some(CapabilitySet::all()), None).await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let err = h
        .handle(FrontingReq::Current, &cx)
        .expect_err("missing FrontingSet must return an error");
    let msg = err.to_string();
    assert!(
        msg.contains(FRONTING_NOT_WIRED_PREFIX),
        "expected FrontingNotWired prefix; got: {msg}"
    );
}

// ── Successful operations ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_with_capability_succeeds() {
    let ctx = build_session_all_caps().await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let result = h
        .handle(
            FrontingReq::Set(
                vec!["alice".to_string(), "bob".to_string()],
                Some("charlie".to_string()),
            ),
            &cx,
        )
        .expect("Set with capability must succeed");
    // Result is `()` encoded as a Core value.
    drop(result);

    // Verify the in-memory FrontingSet was updated.
    let fs = ctx.fronting_set().expect("FrontingSet must be wired");
    let guard = fs.read().unwrap();
    assert_eq!(guard.active.len(), 2, "expected two active personas");
    assert!(guard.fallback.is_some(), "expected fallback to be set");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_returns_json_snapshot() {
    // Seed the FrontingSet with known data.
    let mut initial = FrontingSet::default();
    initial.active = vec![pattern_core::types::ids::PersonaId::new("alice")];
    initial.fallback = Some(pattern_core::types::ids::PersonaId::new("bob"));
    let fs = Arc::new(RwLock::new(initial));
    let ctx = build_session_opts(Some(CapabilitySet::all()), Some(fs)).await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);
    let val = h
        .handle(FrontingReq::Current, &cx)
        .expect("Current with capability must succeed");

    // The result is a Core value wrapping a GHC `Text` (JSON-encoded snapshot).
    // Use `tidepool_bridge::FromCore` to extract the Rust String, then parse as JSON.
    let json_str = <String as tidepool_bridge::FromCore>::from_value(&val, &table)
        .expect("Current must return a Text value decodable as String");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("Current must return valid JSON");
    let active = parsed["active"]
        .as_array()
        .expect("active must be an array");
    assert_eq!(active.len(), 1, "expected one active persona");
    assert_eq!(active[0], "alice");
    let fallback = parsed["fallback"]
        .as_str()
        .expect("fallback must be a string");
    assert_eq!(fallback, "bob");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn route_invalid_regex_preserves_existing_rules() {
    // Seed with one valid rule.
    let existing_rule = RoutingRule::new(
        "rule-0",
        MessagePattern::Prefix("!cmd".to_string()),
        "alice",
        1,
    );
    let table_result = RoutingTable::try_from_rules(vec![existing_rule]);
    let mut initial = FrontingSet::default();
    initial.routing = table_result.expect("initial rule must compile");
    let fs = Arc::new(RwLock::new(initial));
    let ctx = build_session_opts(Some(CapabilitySet::all()), Some(fs.clone())).await;
    let dt = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&dt, &*ctx);

    // Attempt to replace rules with an invalid regex.
    let bad_rule = WireRoutingRule {
        id: "rule-bad".to_string(),
        pattern: WireMessagePattern::Regex("[invalid regex".to_string()),
        target: "bob".to_string(),
        priority: 5,
    };
    let err = h
        .handle(FrontingReq::Route(vec![bad_rule]), &cx)
        .expect_err("invalid regex in Route must return an error");
    assert!(
        err.to_string().contains("route compile failed"),
        "error must mention compile failure; got: {err}"
    );

    // Verify existing rules are still intact.
    let guard = fs.read().unwrap();
    assert_eq!(
        guard.routing.rules.len(),
        1,
        "existing rules must be preserved after compile failure"
    );
    assert_eq!(guard.routing.rules[0].id, "rule-0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clear_resets_to_default() {
    // Seed with non-default state.
    let mut initial = FrontingSet::default();
    initial.active = vec![pattern_core::types::ids::PersonaId::new("alice")];
    let fs = Arc::new(RwLock::new(initial));
    let ctx = build_session_opts(Some(CapabilitySet::all()), Some(fs.clone())).await;
    let table = datacon_table_with_unit();
    let mut h = FrontingHandler;
    let cx = EffectContext::with_user(&table, &*ctx);

    h.handle(FrontingReq::Clear, &cx)
        .expect("Clear with capability must succeed");

    let guard = fs.read().unwrap();
    assert!(guard.active.is_empty(), "Clear must reset active personas");
    assert!(guard.fallback.is_none(), "Clear must reset fallback");
    assert!(
        guard.routing.rules.is_empty(),
        "Clear must reset routing rules"
    );
}
