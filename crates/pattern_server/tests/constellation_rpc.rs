//! Phase 6 T7: end-to-end integration tests for the constellation registry RPCs.
//!
//! Sets up a real project mount in a tmpdir, sends `InitSession` so the
//! daemon's per-mount `ConstellationRegistryDb` is wired, then exercises:
//!
//! - `ListPersonas` (empty + populated, project filter)
//! - `AddRelationship` (success path + unknown-kind error)
//! - `ListGroups` + `CreateGroup` (success + duplicate-collision error)
//! - `PromoteDraft` (happy path, no-session-config failure, version-mismatch,
//!    partial-failure retry)
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
    let mounted = pattern_memory::mount::attach(mount, None).expect("test mount attach");
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
    assert!(
        resp.error.is_none(),
        "no error expected; got: {:?}",
        resp.error
    );
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

// ── PromoteDraft ──────────────────────────────────────────────────────────────

/// Minimal persona KDL sufficient for `persona_loader::load_persona` to
/// produce a valid `PersonaSnapshot`. Uses `nop-test-agent` as the agent-id so
/// tests do not collide with real personas.
fn minimal_persona_kdl(agent_id: &str) -> String {
    format!(
        r#"name "test-promote-{agent_id}"
agent-id "{agent_id}"
system-prompt "Minimal test persona for PromoteDraft integration tests."
model provider="anthropic" model-id="claude-sonnet-4-6" {{
    temperature 0.0
    max-tokens 256
}}
context {{
    compress-check-message-floor 100
}}
"#
    )
}

/// Write a draft persona KDL under `drafts_dir/<persona_id>.kdl` and seed the
/// registry with a `Draft` record pointing at it. Returns the path of the
/// written KDL file.
async fn make_draft_persona(
    drafts_dir: &std::path::Path,
    db: &Arc<pattern_db::ConstellationDb>,
    persona_id: &str,
    agent_id: &str,
) -> std::path::PathBuf {
    std::fs::create_dir_all(drafts_dir).expect("create drafts_dir");
    let kdl_path = drafts_dir.join(format!("{persona_id}.kdl"));
    std::fs::write(&kdl_path, minimal_persona_kdl(agent_id)).expect("write draft persona KDL");

    let registry = pattern_db::ConstellationRegistryDb::new(db.clone());
    let mut record = pattern_core::constellation::PersonaRecord::new(
        persona_id,
        &format!("test-promote-{persona_id}"),
        pattern_core::constellation::PersonaStatus::Draft,
    );
    record.config_path = Some(kdl_path.clone());
    use pattern_core::ConstellationRegistry;
    registry
        .register(record)
        .await
        .expect("seed draft register must succeed");
    kdl_path
}

/// Verify that PromoteDraft fails cleanly when no SessionConfig is wired
/// (echo-mode daemon). This tests the "no session infrastructure" error path
/// without requiring tidepool-extract.
#[tokio::test]
async fn promote_draft_fails_without_session_config() {
    let tmp = make_mount();
    let db = open_mount_db(tmp.path()).await;
    let drafts_dir = tmp.path().join("drafts");
    make_draft_persona(&drafts_dir, &db, "test-persona", "test-nop-agent").await;

    // spawn() = echo mode, no SessionConfig.
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.promote_draft("test-persona".into()).await.unwrap();
    assert!(!resp.success, "expected failure without session config");
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("no SessionConfig") || e.contains("session infrastructure"))
            .unwrap_or(false),
        "expected no-session-config error; got: {:?}",
        resp.error
    );
}

/// Verify that PromoteDraft fails with a clear error when the persona id is
/// not in the registry.
#[tokio::test]
async fn promote_draft_fails_when_persona_not_found() {
    let tmp = make_mount();
    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.promote_draft("does-not-exist".into()).await.unwrap();
    assert!(!resp.success);
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("not found") || e.contains("registry lookup"))
            .unwrap_or(false),
        "expected not-found error; got: {:?}",
        resp.error
    );
}

/// Verify that PromoteDraft fails cleanly when the persona is not in Draft
/// status (e.g. already Active).
#[tokio::test]
async fn promote_draft_fails_for_non_draft_persona() {
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

    let handle = DaemonServer::spawn_with_config(make_config());
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.promote_draft("alice".into()).await.unwrap();
    assert!(!resp.success);
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("not Draft") || e.contains("status"))
            .unwrap_or(false),
        "expected non-Draft status error; got: {:?}",
        resp.error
    );
}

/// Verify that `migrate_seed_cache` returns a clear error when the manifest
/// version does not match `SEED_CACHE_MANIFEST_VERSION`. The promote call
/// should proceed with empty memory (seed migration is best-effort) but the
/// session would fail to open without tidepool-extract. We test the version
/// mismatch path via a daemon with no SessionConfig so we can assert the
/// migration path ran (the error comes from the session-open step, not the
/// version check, since migration is best-effort).
///
/// What we *can* verify without tidepool-extract: when a seed cache directory
/// exists with an incompatible manifest, the promote still proceeds to the
/// session-open step (it doesn't short-circuit on seed cache errors).
#[tokio::test]
async fn promote_draft_seed_cache_version_mismatch_is_best_effort() {
    let tmp = make_mount();
    let db = open_mount_db(tmp.path()).await;
    let drafts_dir = tmp.path().join("drafts");
    let kdl_path = make_draft_persona(&drafts_dir, &db, "seed-test", "seed-test-agent").await;

    // Write a seed cache directory with an incompatible manifest version.
    let cache_dir = drafts_dir.join("seed-test.cache");
    std::fs::create_dir_all(&cache_dir).expect("create cache dir");
    let bad_manifest = serde_json::json!({
        "version": 9999,
        "persona_id": "seed-test",
        "entries": []
    });
    std::fs::write(
        cache_dir.join("manifest.json"),
        serde_json::to_string(&bad_manifest).unwrap(),
    )
    .expect("write bad manifest");

    // Echo-mode daemon: promote will fail at session-open (no SessionConfig),
    // but we can verify the error is from that step, not from the version check
    // (which is best-effort / warn-only).
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let resp = client.promote_draft("seed-test".into()).await.unwrap();
    assert!(!resp.success);
    // The error must come from the session-open step, not the seed cache step.
    // If the version check were fatal it would say "manifest version mismatch";
    // since it's best-effort the promote reaches session-open and fails there.
    assert!(
        resp.error
            .as_deref()
            .map(|e| e.contains("no SessionConfig") || e.contains("session infrastructure"))
            .unwrap_or(false),
        "seed cache version mismatch should be best-effort; expected session-open error; \
         got: {:?}",
        resp.error
    );

    // The promote design moves the KDL file as step 3a before the session-open
    // step (5). On step-5 failure the file is at the promoted path, not the
    // original draft path. The important invariant is that the KDL is not
    // lost — it should exist at the promoted location so a retry can succeed.
    //
    // NOTE: `mount_path` = `<tmp>/.pattern/shared/` (find_mount returns the
    // shared/ directory, not the project root). Promoted personas land at
    // `<mount_path>/personas/@<id>/persona.kdl`.
    let mount_path = tmp.path().join(".pattern").join("shared");
    let promoted_path = mount_path
        .join("personas")
        .join("@seed-test")
        .join("persona.kdl");
    assert!(
        promoted_path.exists() || kdl_path.exists(),
        "KDL should be accessible at promoted or draft path after failed promote; \
         promoted={promoted_path:?} exists={}, draft={kdl_path:?} exists={}",
        promoted_path.exists(),
        kdl_path.exists()
    );
}

/// Verify that a failed promote leaves the persona in a state where a second
/// promote attempt can succeed. This exercises the partial-failure-recovery
/// scenario:
///
/// 1. First promote: file moves, session-open fails (no SessionConfig) →
///    persona remains Draft.
/// 2. Wire up a real SessionConfig.
/// 3. Second promote: file is already at the promoted path (idempotent move);
///    session opens; status flips to Active.
///
/// Requires tidepool-extract; skips when not available.
#[tokio::test]
async fn promote_draft_retry_after_session_open_failure_succeeds() {
    // Tidepool-extract is required to open a real session.
    if pattern_runtime::preflight::check().is_err() {
        return;
    }

    let tmp = make_mount();
    let db = open_mount_db(tmp.path()).await;
    let drafts_dir = tmp.path().join("drafts");
    // Use a unique agent-id to avoid colliding with other test runs.
    let agent_id = format!("retry-test-{}", pattern_core::types::ids::new_id());
    make_draft_persona(&drafts_dir, &db, "retry-persona", &agent_id).await;

    // Step 1: promote with no SessionConfig (echo mode) → fails at session-open.
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);
    init_mount(&client, tmp.path()).await;

    let first = client.promote_draft("retry-persona".into()).await.unwrap();
    assert!(
        !first.success,
        "first promote should fail (no SessionConfig); got: {:?}",
        first.error
    );

    // After the first failed attempt, the file may or may not have moved.
    // The registry status must still be Draft.
    let registry = pattern_db::ConstellationRegistryDb::new(db.clone());
    use pattern_core::ConstellationRegistry;
    let record = registry
        .get(&"retry-persona".into())
        .await
        .unwrap()
        .expect("persona must still exist");
    assert_eq!(
        record.status,
        pattern_core::constellation::PersonaStatus::Draft,
        "persona must remain Draft after failed promote"
    );

    // Step 2: new daemon with real SessionConfig.
    let handle2 = DaemonServer::spawn_with_config(make_config());
    let client2 = DaemonClient::from_local(handle2.client);
    init_mount(&client2, tmp.path()).await;

    let second = client2.promote_draft("retry-persona".into()).await.unwrap();
    assert!(
        second.success,
        "second promote should succeed; got error: {:?}",
        second.error
    );

    // Verify the persona is now Active.
    let after = registry
        .get(&"retry-persona".into())
        .await
        .unwrap()
        .expect("persona must exist after successful promote");
    assert_eq!(
        after.status,
        pattern_core::constellation::PersonaStatus::Active,
        "persona must be Active after successful promote"
    );

    // Verify the session is actually open, not just that the registry status
    // was updated. `list_agents` returns the daemon's live sessions map —
    // the promoted agent's id must appear in it. This makes the test fail
    // if the session-open step was silently skipped while the registry
    // update succeeded (an impossible scenario today, but a regression
    // guard for future refactors).
    let agents = client2
        .list_agents()
        .await
        .expect("list_agents must succeed");
    assert!(
        agents.iter().any(|a| a.agent_id == agent_id),
        "promoted agent must appear in live sessions after successful promote; \
         agent_id={agent_id:?}, live={:?}",
        agents.iter().map(|a| &a.agent_id).collect::<Vec<_>>()
    );
}
