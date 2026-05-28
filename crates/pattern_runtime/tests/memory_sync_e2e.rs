// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! In-process MemorySync integration test.
//!
//! Drives the production-shape path:
//!
//!   PluginMemoryStore  <-- trait calls --   test code
//!         |
//!         |  irpc::Client::local (in-process tokio channels)
//!         v
//!   memory_sync_handler (daemon side)  +  host_handler (daemon side)
//!         |
//!         v
//!   MemoryCache (host) + observer broadcast
//!
//! Uses irpc's supported in-process mode (Client::local) so no iroh
//! endpoint / Router / network. Both halves of the protocol run through
//! tokio channels. Covers completion criterion #4 (round-trips through
//! BOTH sides) without the network-layer cost.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::AgentId;
use pattern_core::plugin::protocol::{MemorySyncProtocol, PluginHostProtocol};
use pattern_core::traits::memory_store::MemoryStore;
use pattern_core::traits::plugin::wire::{BlockAddr, SyncRequest};
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockFilter, BlockSchema, MemoryBlockType, Scope};
use pattern_db::ConstellationDb;
use pattern_memory::cache::MemoryCache;
use pattern_plugin_sdk::memory_sync_client::MemorySyncClient;
use pattern_plugin_sdk::plugin_memory_store::PluginMemoryStore;
use pattern_runtime::agent_registry::AgentRegistry;
use pattern_runtime::plugin::host_handler::{self, HostApiContext};
use pattern_runtime::plugin::memory_sync_handler::{self, MemorySyncApiContext};

/// Host-side fixture: real MemoryCache + DB + AgentRegistry + both
/// per-session handlers spawned with Client::local.
struct HostFixture {
    cache: Arc<MemoryCache>,
    _db: Arc<ConstellationDb>,
    host_client: irpc::Client<PluginHostProtocol>,
    sync_client: irpc::Client<MemorySyncProtocol>,
    default_scope: Scope,
}

fn build_host_fixture() -> HostFixture {
    let db = Arc::new(ConstellationDb::open_in_memory().expect("in-memory db"));
    let cache = Arc::new(MemoryCache::new(Arc::clone(&db)));
    let agent_registry = Arc::new(AgentRegistry::new());
    let session_agent_id = AgentId::from("test-agent");
    let default_scope = Scope::Global("test-agent".into());

    let host_ctx = HostApiContext {
        memory_store: Arc::clone(&cache) as Arc<dyn MemoryStore>,
        agent_registry,
        session_agent_id: session_agent_id.clone(),
        default_scope: default_scope.clone(),
        db: Arc::clone(&db),
    };
    let host_client = host_handler::spawn(host_ctx);

    let sync_ctx = MemorySyncApiContext {
        memory_store: Arc::clone(&cache) as Arc<dyn MemoryStore>,
        observer: cache.memory_observer().clone(),
        session_agent_id,
        default_scope: default_scope.clone(),
    };
    let sync_client = memory_sync_handler::spawn(sync_ctx);

    HostFixture {
        cache,
        _db: db,
        host_client,
        sync_client,
        default_scope,
    }
}

/// Build a plugin-side MemorySyncClient + PluginMemoryStore pair driving
/// against the host fixture via in-process irpc.
async fn build_plugin_side(
    fixture: &HostFixture,
    request: SyncRequest,
) -> (Arc<MemorySyncClient>, PluginMemoryStore) {
    let sync = Arc::new(
        MemorySyncClient::open_with_client(fixture.sync_client.clone(), request)
            .await
            .expect("open memory sync client"),
    );
    let store = PluginMemoryStore::new(
        Arc::clone(&sync),
        fixture.host_client.clone(),
        tokio::runtime::Handle::current(),
    );
    (sync, store)
}

/// Poll the plugin's local cache until `addr` materialises or timeout.
async fn wait_for_block(client: &MemorySyncClient, addr: &BlockAddr, max_ms: u64) {
    let start = std::time::Instant::now();
    while !client.has_block(addr) {
        if start.elapsed() > Duration::from_millis(max_ms) {
            panic!("block did not arrive within {}ms: {:?}", max_ms, addr);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll a predicate against the host cache's rendered text until true or timeout.
async fn wait_for_host_text<F: Fn(&str) -> bool>(
    cache: &MemoryCache,
    scope: &Scope,
    label: &str,
    predicate: F,
    max_ms: u64,
) -> String {
    let start = std::time::Instant::now();
    loop {
        let rendered = cache
            .get_rendered_content(scope, label)
            .expect("get_rendered_content")
            .unwrap_or_default();
        if predicate(&rendered) {
            return rendered;
        }
        if start.elapsed() > Duration::from_millis(max_ms) {
            panic!("host text predicate not satisfied within {}ms; saw: {:?}", max_ms, rendered);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll a predicate against a plugin-cached doc's rendered text.
async fn wait_for_plugin_text<F: Fn(&str) -> bool>(
    client: &MemorySyncClient,
    addr: &BlockAddr,
    predicate: F,
    max_ms: u64,
) -> String {
    let start = std::time::Instant::now();
    loop {
        let doc = client.get_block(addr).expect("plugin doc present");
        let rendered = doc.render();
        if predicate(&rendered) {
            return rendered;
        }
        if start.elapsed() > Duration::from_millis(max_ms) {
            panic!("plugin text predicate not satisfied within {}ms; saw: {:?}", max_ms, rendered);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn make_text_block(label: &str) -> BlockCreate {
    BlockCreate::new(
        label,
        MemoryBlockType::Working,
        BlockSchema::Text { viewport: None },
    )
    .with_description(format!("test block {label}"))
}

// ── Test 1: host-side edit propagates to plugin ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_edit_propagates_to_plugin() {
    let fixture = build_host_fixture();
    let label = "host-to-plugin";
    fixture
        .cache
        .create_block(&fixture.default_scope, make_text_block(label))
        .expect("seed block");
    // Seed initial content via the cached doc + persist to wire subscriber.
    // The subscriber that bridges subscribe_local_update → observer is lazy-
    // spawned in persist_block; without an explicit persist, host edits never
    // fire the broadcast.
    {
        let doc = fixture
            .cache
            .get_block(&fixture.default_scope, label)
            .expect("get_block")
            .expect("block exists");
        doc.append_text("hello", true).expect("append seed");
    }
    fixture
        .cache
        .persist_block(&fixture.default_scope, label)
        .expect("persist seed");

    let addr = BlockAddr {
        scope: fixture.default_scope.clone(),
        label: label.into(),
    };

    // Plugin subscribes to all Working blocks.
    let req = SyncRequest::Filter {
        filter: BlockFilter::default(),
        known: Vec::new(),
    };
    let (sync, _store) = build_plugin_side(&fixture, req).await;

    // Initial snapshot arrives via BlockAvailable.
    wait_for_block(&sync, &addr, 2000).await;
    let final_text = wait_for_plugin_text(&sync, &addr, |s| s.contains("hello"), 2000).await;
    assert!(final_text.contains("hello"), "initial snapshot present: {final_text:?}");

    // Host appends + persists; subscribe_local_update fires → observer
    // broadcast → handler emits Delta → plugin applies.
    {
        let doc = fixture
            .cache
            .get_block(&fixture.default_scope, label)
            .expect("get_block")
            .expect("block exists");
        doc.append_text(" world", true).expect("host append");
    }
    fixture
        .cache
        .persist_block(&fixture.default_scope, label)
        .expect("persist host edit");

    let after = wait_for_plugin_text(&sync, &addr, |s| s.contains("hello world"), 2000).await;
    assert!(after.contains("hello world"), "plugin saw host delta: {after:?}");
}

// ── Test 2: plugin-side edit propagates to host + persists ──────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_edit_propagates_and_persists() {
    let fixture = build_host_fixture();
    let label = "plugin-to-host";
    fixture
        .cache
        .create_block(&fixture.default_scope, make_text_block(label))
        .expect("seed block");
    {
        let doc = fixture
            .cache
            .get_block(&fixture.default_scope, label)
            .expect("get_block")
            .expect("block exists");
        doc.append_text("seed", true).expect("append seed");
    }
    fixture
        .cache
        .persist_block(&fixture.default_scope, label)
        .expect("persist seed");

    let addr = BlockAddr {
        scope: fixture.default_scope.clone(),
        label: label.into(),
    };
    let req = SyncRequest::Addrs { addrs: vec![addr.clone()], known: Vec::new() };
    let (sync, _store) = build_plugin_side(&fixture, req).await;
    wait_for_block(&sync, &addr, 2000).await;

    // Plugin mutates the local synced doc — subscribe_local_update bridge
    // (from wky stage 7b.1) pushes a Delta upstream.
    {
        let doc = sync.get_block(&addr).expect("plugin doc present");
        doc.append_text("-plugin", true).expect("plugin append");
    }

    // Host should see the imported content via push_external_commit →
    // per-block crossbeam → existing persistence pipeline.
    let host_text = wait_for_host_text(
        &fixture.cache,
        &fixture.default_scope,
        label,
        |s| s.contains("seed-plugin"),
        2000,
    )
    .await;
    assert!(host_text.contains("seed-plugin"), "host saw plugin delta: {host_text:?}");
}

// ── Test 3: echo suppression — plugin's own delta doesn't bounce back ─

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_delta_not_echoed_back() {
    let fixture = build_host_fixture();
    let label = "echo-test";
    fixture
        .cache
        .create_block(&fixture.default_scope, make_text_block(label))
        .expect("seed block");
    {
        let doc = fixture
            .cache
            .get_block(&fixture.default_scope, label)
            .expect("get_block")
            .expect("block exists");
        doc.append_text("x", true).expect("seed");
    }
    fixture
        .cache
        .persist_block(&fixture.default_scope, label)
        .expect("persist seed");

    let addr = BlockAddr {
        scope: fixture.default_scope.clone(),
        label: label.into(),
    };
    let req = SyncRequest::Addrs { addrs: vec![addr.clone()], known: Vec::new() };
    let (sync, _store) = build_plugin_side(&fixture, req).await;
    wait_for_block(&sync, &addr, 2000).await;

    // Plugin appends — pushes Delta to host. Host re-broadcasts on observer
    // with origin=self-session, which the SAME session's tokio::select! filter
    // skips (echo suppression). So the plugin's local doc should NOT receive
    // its own delta back as a separate Delta event.
    //
    // Test shape: after the plugin append, the plugin's local doc text should
    // be "x-once" (the local edit applied directly via loro). If echo suppression
    // failed, the plugin would re-apply the same delta and we'd see "x-once-once".
    {
        let doc = sync.get_block(&addr).expect("plugin doc present");
        doc.append_text("-once", true).expect("plugin append");
    }

    // Give it time to round-trip through the broadcast loop.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let doc = sync.get_block(&addr).expect("plugin doc present");
    let plugin_text = doc.render();
    assert!(plugin_text.contains("x-once"), "plugin has the edit: {plugin_text:?}");
    assert!(
        !plugin_text.contains("x-once-once"),
        "echo suppression failed; plugin re-applied its own delta: {plugin_text:?}"
    );

    // Host also has the edit (proves the wire round-tripped).
    let host_text = wait_for_host_text(
        &fixture.cache,
        &fixture.default_scope,
        label,
        |s| s.contains("x-once"),
        2000,
    )
    .await;
    assert!(host_text.contains("x-once"), "host received plugin delta: {host_text:?}");
}
