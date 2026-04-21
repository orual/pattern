//! End-to-end integration tests for the Pattern IRPC service.
//!
//! These tests exercise the full IRPC contract in-process using local channels
//! (no QUIC transport). The same [`DaemonServer`] actor and [`DaemonClient`]
//! wrapper used by the daemon binary are exercised here, verifying the protocol
//! contract defined in [`pattern_server::protocol`].
//!
//! Tests run in the same tokio runtime as the server actor, so async message
//! passing is exercised without mocking.

use pattern_core::types::ids::new_snowflake_id;
use pattern_core::types::provider::ContentPart;
use pattern_server::client::DaemonClient;
use pattern_server::protocol::WireTurnEvent;
use pattern_server::server::DaemonServer;
use smol_str::SmolStr;
use tokio::time::{Duration, timeout};

/// Maximum time to wait for a single event before failing the test.
///
/// This is generous enough to avoid false failures on slow CI machines while
/// still catching hangs (e.g. if filtering is broken and no event ever arrives).
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Subscribe to an agent's output, send a message, collect events until Stop,
/// and verify all events are correctly tagged with the expected batch_id and
/// agent_id.
///
/// Verifies: v3-tui.AC1.2 (IRPC test client connects and receives events)
#[tokio::test]
async fn full_send_subscribe_flow() {
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);

    // Subscribe before sending — this ensures the subscriber is registered in
    // the actor before any events can be emitted, so no events are missed.
    let mut events = client.subscribe_output("agent-1".into()).await.unwrap();

    // Mint a client-side batch_id (snowflake), as the TUI would do.
    let batch_id: SmolStr = new_snowflake_id();
    client
        .send_message(
            batch_id.clone(),
            "agent-1".into(),
            vec![ContentPart::Text("what is 2+2?".into())],
        )
        .await
        .unwrap();

    // Collect all events until we receive a Stop event.
    let mut received = vec![];
    loop {
        let ev = timeout(EVENT_TIMEOUT, events.recv())
            .await
            .expect("timed out waiting for event")
            .expect("recv returned error")
            .expect("channel closed before Stop event");

        let is_stop = matches!(ev.event, WireTurnEvent::Stop(_));
        received.push(ev);
        if is_stop {
            break;
        }
    }

    // Every event must carry the correct batch_id and agent_id.
    assert!(
        received.iter().all(|e| e.batch_id == batch_id),
        "all events must have batch_id = {batch_id}; got: {:?}",
        received.iter().map(|e| &e.batch_id).collect::<Vec<_>>()
    );
    assert!(
        received.iter().all(|e| e.agent_id == "agent-1"),
        "all events must have agent_id = agent-1; got: {:?}",
        received.iter().map(|e| &e.agent_id).collect::<Vec<_>>()
    );

    // Echo handler emits at least one Text event and exactly one Stop event.
    assert!(
        received
            .iter()
            .any(|e| matches!(e.event, WireTurnEvent::Text(_))),
        "expected at least one Text event"
    );
    assert!(
        matches!(received.last().unwrap().event, WireTurnEvent::Stop(_)),
        "last event must be Stop"
    );
}

/// InitSession in echo mode returns synthetic session info with the requested
/// agent_id and empty persona name.
#[tokio::test]
async fn init_session_echo_mode() {
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);

    let info = client
        .init_session(
            std::path::PathBuf::from("/tmp/test-project"),
            "pattern-default".into(),
        )
        .await
        .unwrap();

    assert_eq!(info.agent_id, "pattern-default");
    assert_eq!(info.persona_name, "echo");
    assert!(info.available_agents.is_empty());
}

/// A subscriber registered for agent-1 must not receive events emitted for
/// agent-2, and must receive events emitted for agent-1.
///
/// Verifies: v3-tui.AC1.5 (agent-level event filtering for concurrent batches)
#[tokio::test]
async fn subscriber_filtering_by_agent() {
    let handle = DaemonServer::spawn();
    let client = DaemonClient::from_local(handle.client);

    // Subscribe only to agent-1.
    let mut rx = client.subscribe_output("agent-1".into()).await.unwrap();

    // Send to agent-2 first. The subscriber must not receive these events:
    // the fan_out logic checks agent_id before forwarding to any subscriber.
    client
        .send_message(
            new_snowflake_id(),
            "agent-2".into(),
            vec![ContentPart::Text("hello from agent-2".into())],
        )
        .await
        .unwrap();

    // Send to agent-1. The subscriber must receive this event.
    let batch_id_1: SmolStr = new_snowflake_id();
    client
        .send_message(
            batch_id_1.clone(),
            "agent-1".into(),
            vec![ContentPart::Text("hello from agent-1".into())],
        )
        .await
        .unwrap();

    // The first event we receive must be from agent-1, not agent-2.
    // If filtering were broken and agent-2's events leaked into this subscriber,
    // the agent_id assertion below would catch it.
    let ev = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for agent-1 event")
        .expect("recv returned error")
        .expect("channel closed unexpectedly");

    assert_eq!(
        ev.agent_id, "agent-1",
        "subscriber must only receive events for the subscribed agent"
    );
    assert_eq!(
        ev.batch_id, batch_id_1,
        "event must belong to the agent-1 batch"
    );
}
