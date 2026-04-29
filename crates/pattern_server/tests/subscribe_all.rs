//! Phase 6 T8: integration tests for `SubscribeAll` mount-scoped subscription
//! and `EventEmittingRegistry` event emission.
//!
//! Uses a real in-repo project mount (via `pattern_memory::modes::in_repo::init`)
//! so the daemon's per-mount registry + fronting state are wired through the
//! real path. Drives the registry RPCs and asserts that:
//!
//! 1. `SubscribeAll` receives `ConstellationChanged` events on registry
//!    mutations (`AddRelationship`, `CreateGroup`, etc.).
//! 2. `SubscribeAll` receives `FrontingChanged` events on `SetFronting`.
//! 3. `SessionInfo.fronting_snapshot` is populated after `InitSession`.
//! 4. The `partner_display_name` field is populated when `.pattern.kdl` has a
//!    `partner { display-name "..." }` block.

use std::sync::Arc;

use pattern_memory::modes::in_repo;
use pattern_runtime::NopProviderClient;
use pattern_runtime::SdkLocation;
use pattern_runtime::port_registry::PortRegistryImpl;
use pattern_server::client::DaemonClient;
use pattern_server::protocol::WireTurnEvent;
use pattern_server::server::{DaemonServer, SessionConfig};

fn make_config() -> SessionConfig {
    let port_registry = Arc::new(PortRegistryImpl::with_runtime_ports(
        &tokio::runtime::Handle::current(),
    ));
    SessionConfig {
        sdk: SdkLocation::default(),
        provider: Arc::new(NopProviderClient),
        port_registry,
    }
}

fn make_mount() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    in_repo::init(tmp.path(), "test").expect("in_repo::init");
    tmp
}

/// Append a `partner { display-name "<name>" }` block to the mount's
/// `.pattern.kdl` so the daemon picks up the display name.
fn add_partner_block(mount_path: &std::path::Path, display_name: &str) {
    let kdl_path = mount_path
        .join(".pattern")
        .join("shared")
        .join(".pattern.kdl");
    let mut content = std::fs::read_to_string(&kdl_path).expect("read .pattern.kdl");
    content.push_str(&format!(
        "\npartner {{\n    display-name \"{}\"\n}}\n",
        display_name
    ));
    std::fs::write(&kdl_path, content).expect("write .pattern.kdl");
}

async fn drain_until<F>(
    rx: &mut irpc::channel::mpsc::Receiver<pattern_server::protocol::TaggedTurnEvent>,
    mut predicate: F,
    timeout: std::time::Duration,
) -> Option<pattern_server::protocol::TaggedTurnEvent>
where
    F: FnMut(&pattern_server::protocol::TaggedTurnEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = match deadline.checked_duration_since(tokio::time::Instant::now()) {
            Some(r) => r,
            None => return None,
        };
        let next = tokio::time::timeout(remaining, rx.recv()).await;
        match next {
            Ok(Ok(Some(event))) => {
                if predicate(&event) {
                    return Some(event);
                }
            }
            Ok(_) => return None,  // channel closed
            Err(_) => return None, // timeout
        }
    }
}

// ── ConstellationChanged emission ────────────────────────────────────────────

#[tokio::test]
async fn subscribe_all_receives_constellation_changed_on_create_group() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);

    // InitSession to mount.
    let info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");
    assert!(info.error.is_none() || info.error.as_deref() == Some(""));

    // SubscribeAll on the canonical mount path so the daemon's canonicalize
    // matches our subscription key.
    let canonical = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut rx = client
        .subscribe_all(canonical.clone())
        .await
        .expect("SubscribeAll");

    // Mutate the registry — should emit ConstellationChanged.
    let _ = client
        .create_group("alpha".into(), Some("proj-x".into()))
        .await
        .expect("CreateGroup");

    let event = drain_until(
        &mut rx,
        |e| matches!(e.event, WireTurnEvent::ConstellationChanged { .. }),
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("ConstellationChanged event must arrive within 2s");

    match event.event {
        WireTurnEvent::ConstellationChanged { kind } => {
            assert_eq!(kind, "group_created");
        }
        _ => unreachable!(),
    }
    assert_eq!(event.agent_id, "daemon");
    assert!(event.mount_path.is_some(), "event must carry mount_path");
}

#[tokio::test]
async fn subscribe_all_receives_constellation_changed_on_add_relationship() {
    use pattern_core::ConstellationRegistry;
    use pattern_core::constellation::{PersonaRecord, PersonaStatus};

    let tmp = make_mount();
    // Seed two personas via a separate registry handle so AddRelationship
    // has valid endpoints.
    let mounted = pattern_memory::mount::attach(tmp.path(), None).expect("attach");
    let raw = pattern_db::ConstellationRegistryDb::new(mounted.db.clone());
    raw.register(PersonaRecord::new("alice", "Alice", PersonaStatus::Active))
        .await
        .unwrap();
    raw.register(PersonaRecord::new("bob", "Bob", PersonaStatus::Active))
        .await
        .unwrap();
    drop(mounted);

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    let _info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");

    let canonical = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut rx = client.subscribe_all(canonical).await.expect("SubscribeAll");

    let resp = client
        .add_relationship("alice".into(), "bob".into(), "supervisor_of".into())
        .await
        .expect("AddRelationship");
    assert!(resp.success, "got error: {:?}", resp.error);

    let event = drain_until(
        &mut rx,
        |e| matches!(e.event, WireTurnEvent::ConstellationChanged { .. }),
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("ConstellationChanged event must arrive");
    match event.event {
        WireTurnEvent::ConstellationChanged { kind } => {
            assert_eq!(kind, "relationship_added");
        }
        _ => unreachable!(),
    }
}

/// Verify that calling `AddRelationship` twice with the same `(from, to, kind)`
/// triple emits exactly ONE `ConstellationChanged { kind: "relationship_added" }`
/// event. The second call must be a no-op at the DB level (`ON CONFLICT DO
/// NOTHING`) so `EventEmittingRegistry::add_relationship` skips the emit on
/// `Ok(false)`.
#[tokio::test]
async fn add_relationship_duplicate_emits_exactly_one_event() {
    use pattern_core::ConstellationRegistry;
    use pattern_core::constellation::{PersonaRecord, PersonaStatus};

    let tmp = make_mount();
    let mounted = pattern_memory::mount::attach(tmp.path(), None).expect("attach");
    let raw = pattern_db::ConstellationRegistryDb::new(mounted.db.clone());
    raw.register(PersonaRecord::new("alice", "Alice", PersonaStatus::Active))
        .await
        .unwrap();
    raw.register(PersonaRecord::new("bob", "Bob", PersonaStatus::Active))
        .await
        .unwrap();
    drop(mounted);

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    let _info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");

    let canonical = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut rx = client.subscribe_all(canonical).await.expect("SubscribeAll");

    // First call: must succeed and emit an event.
    let first = client
        .add_relationship("alice".into(), "bob".into(), "supervisor_of".into())
        .await
        .expect("first AddRelationship");
    assert!(first.success, "first add_relationship must succeed");

    let event = drain_until(
        &mut rx,
        |e| matches!(e.event, WireTurnEvent::ConstellationChanged { .. }),
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("first ConstellationChanged event must arrive");
    match &event.event {
        WireTurnEvent::ConstellationChanged { kind } => {
            assert_eq!(kind, "relationship_added");
        }
        _ => unreachable!(),
    }

    // Second call: same edge — must succeed (no error) but NOT emit a second event.
    let second = client
        .add_relationship("alice".into(), "bob".into(), "supervisor_of".into())
        .await
        .expect("second AddRelationship");
    assert!(
        second.success,
        "duplicate add_relationship must not return an error; got: {:?}",
        second.error
    );

    // Assert no second event arrives within the timeout.
    let spurious = drain_until(
        &mut rx,
        |e| matches!(e.event, WireTurnEvent::ConstellationChanged { .. }),
        std::time::Duration::from_millis(300),
    )
    .await;
    assert!(
        spurious.is_none(),
        "duplicate add_relationship must not emit a second ConstellationChanged event; got: {spurious:?}"
    );
}

// ── FrontingChanged via SetFronting ──────────────────────────────────────────

#[tokio::test]
async fn subscribe_all_receives_fronting_changed_on_set_fronting() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    let _info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");

    let canonical = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut rx = client.subscribe_all(canonical).await.expect("SubscribeAll");

    let resp = client
        .set_fronting(vec!["alice".into()], Some("alice".into()))
        .await
        .expect("SetFronting");
    assert!(resp.success);

    let event = drain_until(
        &mut rx,
        |e| matches!(e.event, WireTurnEvent::FrontingChanged { .. }),
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("FrontingChanged event must arrive");
    match event.event {
        WireTurnEvent::FrontingChanged { active, .. } => {
            assert_eq!(active, vec!["alice".to_string()]);
        }
        _ => unreachable!(),
    }
    assert!(event.mount_path.is_some(), "event must carry mount_path");
}

// ── SessionInfo populates fronting_snapshot ─────────────────────────────────

#[tokio::test]
async fn init_session_populates_fronting_snapshot_when_real_mount() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);

    // First call: empty fronting set, snapshot should be Some(default-shape).
    let info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");
    let snap = info
        .fronting_snapshot
        .expect("fronting_snapshot must be Some(_) for a real mount");
    assert!(snap.active.is_empty(), "default fronting set is empty");
    assert!(snap.fallback.is_none());
    assert!(snap.rules.is_empty());

    // Set fronting and re-init: snapshot should reflect the new state.
    let _ = client
        .set_fronting(vec!["alice".into(), "bob".into()], Some("alice".into()))
        .await
        .expect("SetFronting");

    let info2 = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");
    let snap2 = info2.fronting_snapshot.expect("snapshot present");
    assert_eq!(snap2.active.len(), 2);
    assert_eq!(snap2.fallback.as_deref(), Some("alice"));
}

// ── SessionInfo populates partner_display_name ──────────────────────────────

#[tokio::test]
async fn init_session_populates_partner_display_name_from_kdl() {
    let tmp = make_mount();
    add_partner_block(tmp.path(), "orual");

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);

    let info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");

    assert_eq!(
        info.partner_display_name.as_deref(),
        Some("orual"),
        "partner display name from .pattern.kdl must surface in SessionInfo"
    );
}

#[tokio::test]
async fn init_session_partner_display_name_none_when_block_absent() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    let info = client
        .init_session(tmp.path().to_path_buf(), "default".into())
        .await
        .expect("InitSession");
    assert!(
        info.partner_display_name.is_none(),
        "without partner block in .pattern.kdl, display name must be None"
    );
}
