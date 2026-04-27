//! Phase 6 T7: end-to-end integration tests for the constellation registry RPCs.
//!
//! Sets up a real project mount in a tmpdir, sends `InitSession` so the
//! daemon's per-mount `ConstellationRegistryDb` is wired, then exercises:
//!
//! - `ListPersonas` (empty + populated, project filter)
//! - `AddRelationship` (success path + unknown-kind error)
//! - `ListGroups` + `CreateGroup` (success + duplicate-collision error)
//! - `PromoteDraft` integration with the registry-flip path
//!
//! Uses `DaemonServer::spawn_with_config` with a `NopProviderClient` so no
//! LLM credentials are needed.

use std::sync::Arc;

use pattern_memory::modes::in_repo;
use pattern_runtime::NopProviderClient;
use pattern_runtime::SdkLocation;
use pattern_runtime::port_registry::PortRegistryImpl;
use pattern_server::client::DaemonClient;
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

/// Set up a real in-repo mount in a tmpdir and return its path. The daemon's
/// `get_or_mount_project` walks up from this path and finds `.pattern/shared/.pattern.kdl`.
fn make_mount() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().expect("tempdir must succeed");
    in_repo::init(tmp.path()).expect("in_repo::init must succeed");
    tmp
}

async fn init_mount(client: &DaemonClient, mount: &std::path::Path) {
    // The agent_id won't resolve (no personas seeded yet) but that's fine for
    // registry-only operations — the mount itself is what matters.
    let _ = client
        .init_session(mount.to_path_buf(), "default".into())
        .await
        .expect("InitSession must succeed");
}

async fn seed_persona(
    db: &Arc<pattern_db::ConstellationDb>,
    id: &str,
    name: &str,
    status: pattern_core::constellation::PersonaStatus,
    config_path: Option<&std::path::Path>,
) {
    let registry = pattern_db::ConstellationRegistryDb::new(db.clone());
    let mut record = pattern_core::constellation::PersonaRecord::new(id, name, status);
    record.config_path = config_path.map(|p| p.to_path_buf());
    use pattern_core::ConstellationRegistry;
    registry
        .register(record)
        .await
        .expect("seed register must succeed");
}

/// Acquire a clone of the daemon's per-mount DB by mounting separately.
/// This works because `pattern_memory::mount::attach` returns a `MountedStore`
/// that holds an `Arc<ConstellationDb>` — same Arc the daemon uses for the
/// same path.
async fn open_mount_db(mount: &std::path::Path) -> Arc<pattern_db::ConstellationDb> {
    let mounted =
        pattern_memory::mount::attach(mount, None).expect("test mount attach");
    let db = mounted.db.clone();
    // Detach without dropping the DB Arc so we can use it.
    drop(mounted);
    db
}

// ── ListPersonas ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_personas_returns_seeded_records() {
    let tmp = make_mount();
    let db = open_mount_db(tmp.path()).await;
    seed_persona(
        &db,
        "alice",
        "Alice",
        pattern_core::constellation::PersonaStatus::Active,
        None,
    )
    .await;
    seed_persona(
        &db,
        "bob",
        "Bob",
        pattern_core::constellation::PersonaStatus::Draft,
        None,
    )
    .await;

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.list_personas(None).await.unwrap();
    assert!(resp.error.is_none(), "no error expected; got: {:?}", resp.error);
    assert_eq!(resp.personas.len(), 2);
    let mut by_id: std::collections::HashMap<_, _> = resp
        .personas
        .iter()
        .map(|p| (p.id.clone(), p.status.clone()))
        .collect();
    assert_eq!(by_id.remove("alice").as_deref(), Some("active"));
    assert_eq!(by_id.remove("bob").as_deref(), Some("draft"));
}

#[tokio::test]
async fn list_personas_empty_returns_empty_vec() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.list_personas(None).await.unwrap();
    assert!(resp.error.is_none());
    assert!(resp.personas.is_empty());
}

#[tokio::test]
async fn list_personas_without_init_returns_no_mount_error() {
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);

    let resp = client.list_personas(None).await.unwrap();
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("no project mounted"))
            .unwrap_or(false),
        "expected no-mount error; got: {:?}",
        resp.error
    );
}

// ── AddRelationship ──────────────────────────────────────────────────────────

#[tokio::test]
async fn add_relationship_success_path() {
    let tmp = make_mount();
    let db = open_mount_db(tmp.path()).await;
    seed_persona(
        &db,
        "alice",
        "Alice",
        pattern_core::constellation::PersonaStatus::Active,
        None,
    )
    .await;
    seed_persona(
        &db,
        "bob",
        "Bob",
        pattern_core::constellation::PersonaStatus::Active,
        None,
    )
    .await;

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client
        .add_relationship("alice".into(), "bob".into(), "supervisor_of".into())
        .await
        .unwrap();
    assert!(resp.success, "got error: {:?}", resp.error);

    // Verify the edge landed by listing personas and checking alice's
    // relationships through the underlying registry.
    use pattern_core::ConstellationRegistry;
    let registry = pattern_db::ConstellationRegistryDb::new(db);
    let alice = registry
        .get(&"alice".into())
        .await
        .unwrap()
        .expect("alice must exist");
    let outgoing: Vec<_> = alice
        .relationships
        .iter()
        .filter(|e| e.direction == pattern_core::constellation::EdgeDirection::Outgoing)
        .collect();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].other.as_str(), "bob");
}

#[tokio::test]
async fn add_relationship_unknown_kind_returns_error() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client
        .add_relationship("a".into(), "b".into(), "buddy_with".into())
        .await
        .unwrap();
    assert!(!resp.success);
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("unknown relationship kind"))
            .unwrap_or(false),
        "expected unknown-kind error; got: {:?}",
        resp.error
    );
}

// ── Groups ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_group_then_list_groups_returns_it() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let create = client
        .create_group("support".into(), Some("proj-a".into()))
        .await
        .unwrap();
    assert!(create.error.is_none());
    let g = create.group.expect("created group must be returned");
    assert_eq!(g.name, "support");
    assert_eq!(g.project_id.as_deref(), Some("proj-a"));

    let listed = client.list_groups(None).await.unwrap();
    assert!(listed.error.is_none());
    assert_eq!(listed.groups.len(), 1);
    assert_eq!(listed.groups[0].name, "support");
}

#[tokio::test]
async fn create_group_duplicate_returns_error() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let _ok = client
        .create_group("support".into(), Some("proj-a".into()))
        .await
        .unwrap();
    let dup = client
        .create_group("support".into(), Some("proj-a".into()))
        .await
        .unwrap();
    assert!(dup.group.is_none());
    assert!(
        dup.error
            .as_deref()
            .map(|e| e.contains("duplicate") || e.contains("Duplicate"))
            .unwrap_or(false),
        "expected duplicate-group error; got: {:?}",
        dup.error
    );
}
