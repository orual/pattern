//! Data-stream trait: async subscription to an external event source.
//!
//! A [`DataStream`] is any source that surfaces events over time — the
//! ATProto firehose, a Discord gateway, a shell `ProcessSource`, an RSS
//! feed, a filesystem watcher, etc. Concrete sources live in
//! `pattern_runtime` (Phase 3) or in plugin crates; this trait is the
//! contract they implement so the runtime can register and observe them
//! uniformly.
//!
//! # Downcasting via `as_any`
//!
//! Tools that need typed access to a specific stream implementation
//! downcast via [`DataStream::as_any`]. This preserves the guide pattern
//! documented in `docs/data-sources-guide.md`: the `SourceManager` returns
//! trait objects, and the consumer downcasts to the concrete type at the
//! point of use.

use std::any::Any;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// An event emitted by a [`DataStream`].
///
/// Phase 2 lands an opaque payload; Phase 3 tightens this to a typed
/// event enum per concrete source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Opaque event payload. Interpretation is source-specific.
    pub payload: serde_json::Value,
}

/// Async subscription to an external event source.
///
/// # Example
///
/// ```no_run
/// use std::any::Any;
/// use async_trait::async_trait;
/// use futures::stream::BoxStream;
/// use pattern_core::error::CoreError;
/// use pattern_core::traits::data_stream::{DataStream, StreamEvent};
///
/// struct Dummy;
///
/// #[async_trait]
/// impl DataStream for Dummy {
///     async fn subscribe(&self) -> Result<BoxStream<'static, StreamEvent>, CoreError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn as_any(&self) -> &dyn Any { self }
/// }
/// ```
#[async_trait]
pub trait DataStream: Send + Sync {
    /// Subscribe to this stream, returning an async event stream.
    async fn subscribe(&self) -> Result<BoxStream<'static, StreamEvent>, CoreError>;

    /// Downcast accessor for tools that need typed access to the concrete
    /// stream implementation. See module docs and `docs/data-sources-guide.md`.
    fn as_any(&self) -> &dyn Any;
}
