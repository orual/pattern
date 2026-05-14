//! Phase 6 Task 7 (real): out-of-process plugin loop integration test.
//!
//! Lives in pattern-server because it consumes daemon-side primitives (Endpoint,
//! Router, AuthGatedProtocolHandler, host_handler) AND runtime-side primitives
//! (OutOfProcessPluginConnection). Isolates state via `PATTERN_HOME` tempdir so
//! concurrent runs + a real running daemon don't collide.
//!
//! Test classes:
//! - `regression_*` — pinned behavior. Currently passes; expected to stay green
//!   as the loop unstubs (real wire round-trips for declare_ports + library).
//! - `progress_*` — eventual-correct behavior. Currently FAILS. Each failure
//!   marks a specific stubbed path that needs unstubbing. As stubs land, these
//!   flip to green. Serves as a granular to-do list.
//!
//! Run all: `cargo nextest run -p pattern-server --test plugin_loop`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, PublicKey, SecretKey};
use irpc::rpc::RemoteService;
use irpc_iroh::IrohProtocol;
use pattern_core::daemon_state::DaemonState;
use pattern_core::plugin::PluginId;
use pattern_core::plugin::auth::{PluginKeyStore, PluginRouteTable, SessionRoutingProtocolHandler};
use pattern_core::plugin::protocol::{PLUGIN_HOST_ALPN, PluginHostProtocol};
use pattern_runtime::plugin::host_handler;
use pattern_runtime::plugin::transport::{OutOfProcessPluginConnection, PluginConnection, PluginHealth};
use smol_str::SmolStr;
use tempfile::TempDir;

const FIXTURE_PLUGIN_ID: &str = "minimal_plugin";

/// Wires a fake daemon (iroh endpoint + Router accepting PLUGIN_HOST_ALPN with
/// AuthGatedProtocolHandler) and resolves the fixture plugin binary.
struct Harness {
    _tmp: TempDir,
    plugin_id: SmolStr,
    plugin_pubkey: PublicKey,
    daemon_endpoint: Endpoint,
    _daemon_router: Router,
    fixture_binary: PathBuf,
}

impl Harness {
    async fn new() -> Result<Self> {
        let tmp = TempDir::new().context("tempdir")?;

        // SAFETY: nextest runs each test in its own subprocess by default, so env
        // mutation here doesn't race with other tests.
        unsafe {
            std::env::set_var("PATTERN_HOME", tmp.path());
            // Clear in case the user's env has it set — would override PATTERN_HOME
            // for DaemonState specifically and break isolation.
            std::env::remove_var("PATTERN_STATE_DIR");
            // Force file-only keystore so test parent + spawned fixture share the
            // same PATTERN_HOME-scoped file path deterministically. Without this,
            // keyring-vs-file split path produces inconsistent keys across processes.
            std::env::set_var("PATTERN_KEYSTORE_FILE_ONLY", "1");
        }

        // Pre-generate the plugin's keypair. Both the test (here) and the spawned
        // fixture process will hit `PluginKeyStore::load_or_generate` and get the
        // SAME key because both see the same PATTERN_HOME.
        let plugin_id: PluginId = FIXTURE_PLUGIN_ID.into();
        let plugin_sk = PluginKeyStore::load_or_generate(&plugin_id)
            .context("load_or_generate plugin keypair")?;
        let plugin_pubkey = plugin_sk.public();

        // Daemon endpoint + DaemonState.save so the fixture can dial back.
        let daemon_sk = SecretKey::generate();
        let daemon_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(daemon_sk.clone())
            .bind()
            .await
            .map_err(|e| anyhow::anyhow!("daemon endpoint bind: {e}"))?;
        let daemon_addr = daemon_endpoint
            .bound_sockets()
            .into_iter()
            .next()
            .context("daemon has no bound socket")?;
        let daemon_state = DaemonState {
            pid: std::process::id(),
            addr: daemon_addr,
            node_id: daemon_sk.public().to_string(),
        };
        daemon_state.save(&daemon_sk.to_bytes())?;

        // Session-aware route table + gated host handler — matches main.rs.
        // Register the fixture's pubkey under a test session id so the iroh accept
        // gate permits the incoming connection. Without this, accept would reject.
        let plugin_routes = Arc::new(PluginRouteTable::new());
        plugin_routes.register(
            plugin_pubkey,
            plugin_id.clone(),
            "test-session".into(),
        ).expect("register fixture route");
        let host_client = host_handler::spawn();
        let host_local = host_client
            .as_local()
            .expect("freshly-spawned host client is local");
        let host_handler_proto = PluginHostProtocol::remote_handler(host_local);
        let gated_host = SessionRoutingProtocolHandler::new(
            Arc::clone(&plugin_routes),
            IrohProtocol::new(host_handler_proto),
        );
        let daemon_router = Router::builder(daemon_endpoint.clone())
            .accept(PLUGIN_HOST_ALPN, gated_host)
            .spawn();

        let fixture_binary = build_fixture()?;

        Ok(Self {
            _tmp: tmp,
            plugin_id,
            plugin_pubkey,
            daemon_endpoint,
            _daemon_router: daemon_router,
            fixture_binary,
        })
    }

    async fn spawn_plugin(&self) -> Result<OutOfProcessPluginConnection> {
        Ok(OutOfProcessPluginConnection::spawn(
            self.plugin_id.clone(),
            self.fixture_binary.clone(),
            self.plugin_pubkey,
            self.daemon_endpoint.clone(),
            std::env::temp_dir(), // plugin_root — fixture doesn't care
            serde_json::Value::Null, // empty user_config
            pattern_core::CapabilitySet::all(), // permissive for tests
        )
        .await?)
    }
}

fn build_fixture() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_manifest = manifest_dir
        .join("..")
        .join("pattern_plugin_sdk")
        .join("tests")
        .join("fixtures")
        .join("minimal_plugin")
        .join("Cargo.toml");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--manifest-path"])
        .arg(&fixture_manifest)
        .status()
        .context("cargo build minimal_plugin")?;
    anyhow::ensure!(status.success(), "minimal_plugin failed to build");
    let fixture_dir = fixture_manifest.parent().unwrap();
    let binary = fixture_dir.join("target/debug/minimal_plugin");
    anyhow::ensure!(binary.exists(), "binary not found at {}", binary.display());
    Ok(binary)
}

// ── regression: pinned behavior ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regression_declare_ports_round_trips_to_fixture() -> Result<()> {
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    let ports = conn.declare_ports().await?;
    assert!(ports.is_empty(), "minimal_plugin declares no ports");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regression_library_round_trips_to_fixture() -> Result<()> {
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    let lib = conn.library().await?;
    assert!(lib.is_none(), "minimal_plugin ships no library");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regression_connection_reports_healthy() -> Result<()> {
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    assert!(matches!(conn.health(), PluginHealth::Healthy));
    Ok(())
}

// ── progress: currently FAIL, each marks a stubbed path ───────────────────
//
// These need a real-shaped PluginContext to call lifecycle methods. Building
// that requires wiring up a real session (ConstellationDb, agent_id, scope,
// MemoryStore). That wiring is the next chunk after this lands.

fn make_real_plugin_context() -> pattern_core::traits::plugin::PluginContext {
    // Unit-shape PluginContext for OOP wire-conversion tests. NOT a full
    // SessionContext — the production session-open path is exercised by
    // separate integration-shape tests (queued: build via
    // TidepoolSession::open_with_agent_loop + assert route-table population +
    // Drop clears routes + plugin lifecycle runs).
    //
    // Earlier framing claimed this needed ConstellationDb + scope + full
    // MemoryStore session setup. That conflated PluginContext with
    // SessionContext — PluginContext is 5 trivial fields. The actual blocker
    // for progress tests is the PluginContext→WirePluginContext conversion
    // in OutOfProcessPluginConnection, not the ctx itself.
    use std::sync::Arc;
    use pattern_runtime::testing::InMemoryMemoryStore;
    pattern_core::traits::plugin::PluginContext {
        plugin_id: "minimal-plugin-fixture".into(),
        hook_bus: Arc::new(pattern_core::hooks::HookBus::new()),
        plugin_root: std::env::temp_dir(),
        mount_path: None,
        memory_store: Some(Arc::new(InMemoryMemoryStore::new())),
        scope: Some(pattern_core::types::memory_types::Scope::global("test-persona")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_on_install_reaches_plugin() -> Result<()> {
    // Today: returns Err(Lifecycle("oop on_install: PluginContext->wire conversion not yet wired (v1)")).
    // Eventual: invokes MinimalPlugin::on_install on the fixture side, returns Ok(()).
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    let ctx = make_real_plugin_context();
    conn.on_install(&ctx).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_on_enable_reaches_plugin() -> Result<()> {
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    let ctx = make_real_plugin_context();
    conn.on_enable(&ctx).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_on_event_reaches_plugin() -> Result<()> {
    use pattern_core::hooks::{HookEvent, tags};
    let harness = Harness::new().await?;
    let conn = harness.spawn_plugin().await?;
    let event = HookEvent::notification(tags::TURN_BEFORE, serde_json::Value::Null);
    // Today: returns Err(Lifecycle("...not yet wired (v1)")). Eventual: fires the
    // plugin's on_event subscriber path.
    let _ = conn.on_event(event).await?;
    Ok(())
}
