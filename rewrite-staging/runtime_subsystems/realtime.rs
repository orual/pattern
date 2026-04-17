// MOVING TO: pattern_runtime/src/realtime.rs
// ORIGIN: crates/pattern_core/src/realtime.rs
// PHASE: 3 + future-rework
// RESHAPE: Sink traits reshape against Phase 3 event model; tap helpers likely retained with new event types
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Real-time helpers: event sinks and stream tap (tee)
//!
//! This module defines lightweight sink traits for forwarding live
//! agent and group events to multiple consumers (e.g., CLI printer,
//! file logger). It also exposes `tap_*_stream` helpers that tee an
//! existing event stream to one or more sinks without altering the
//! original consumer behavior.

use std::sync::Arc;

use tokio_stream::StreamExt;

use crate::{agent::ResponseEvent, coordination::groups::GroupResponseEvent};

/// Optional context for agent event sinks
#[derive(Debug, Clone, Default)]
pub struct AgentEventContext {
    /// Human-readable source tag (e.g., "CLI", "Discord", "Jetstream")
    pub source_tag: Option<String>,
    /// Optional agent display name
    pub agent_name: Option<String>,
}

/// Optional context for group event sinks
#[derive(Debug, Clone, Default)]
pub struct GroupEventContext {
    /// Human-readable source tag (e.g., "CLI", "Discord", "Jetstream")
    pub source_tag: Option<String>,
    /// Optional group name
    pub group_name: Option<String>,
}

/// Sink for agent `ResponseEvent` items
#[async_trait::async_trait]
pub trait AgentEventSink: Send + Sync {
    async fn on_event(&self, event: ResponseEvent, ctx: AgentEventContext);
}

/// Sink for group `GroupResponseEvent` items
#[async_trait::async_trait]
pub trait GroupEventSink: Send + Sync {
    async fn on_event(&self, event: GroupResponseEvent, ctx: GroupEventContext);
}

/// Tee an agent stream to the provided sinks and return a new stream with the
/// original events. Best-effort forwarding: sink errors do not affect the stream.
pub fn tap_agent_stream(
    mut stream: Box<dyn tokio_stream::Stream<Item = ResponseEvent> + Send + Unpin>,
    sinks: Vec<Arc<dyn AgentEventSink>>,
    ctx: AgentEventContext,
) -> Box<dyn tokio_stream::Stream<Item = ResponseEvent> + Send + Unpin> {
    use tokio::sync::mpsc;
    let (tx, rx) = mpsc::channel::<ResponseEvent>(100);

    let ctx_arc = Arc::new(ctx);
    tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            // Forward to sinks (best-effort, non-blocking)
            let cloned = event.clone();
            for sink in &sinks {
                let sink = sink.clone();
                let ctx = (*ctx_arc).clone();
                let evt = cloned.clone();
                tokio::spawn(async move {
                    let _ = sink.on_event(evt, ctx).await;
                });
            }
            // Send original event downstream
            if tx.send(event).await.is_err() {
                break;
            }
        }
        // Dropping tx closes the receiver
    });

    Box::new(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Tee a group stream to the provided sinks and return a new stream with the
/// original events. Best-effort forwarding: sink errors do not affect the stream.
pub fn tap_group_stream(
    mut stream: Box<dyn tokio_stream::Stream<Item = GroupResponseEvent> + Send + Unpin>,
    sinks: Vec<Arc<dyn GroupEventSink>>,
    ctx: GroupEventContext,
) -> Box<dyn tokio_stream::Stream<Item = GroupResponseEvent> + Send + Unpin> {
    use tokio::sync::mpsc;
    let (tx, rx) = mpsc::channel::<GroupResponseEvent>(100);

    let ctx_arc = Arc::new(ctx);
    tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            // Forward to sinks (best-effort, non-blocking)
            let cloned = event.clone();
            for sink in &sinks {
                let sink = sink.clone();
                let ctx = (*ctx_arc).clone();
                let evt = cloned.clone();
                tokio::spawn(async move {
                    let _ = sink.on_event(evt, ctx).await;
                });
            }
            // Send original event downstream
            if tx.send(event).await.is_err() {
                break;
            }
        }
        // Dropping tx closes the receiver
    });

    Box::new(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[async_trait::async_trait]
impl GroupEventSink for Arc<dyn GroupEventSink> {
    async fn on_event(&self, event: GroupResponseEvent, ctx: GroupEventContext) {
        (**self).on_event(event, ctx).await;
    }
}

#[async_trait::async_trait]
impl AgentEventSink for Arc<dyn AgentEventSink> {
    async fn on_event(&self, event: ResponseEvent, ctx: AgentEventContext) {
        (**self).on_event(event, ctx).await;
    }
}
