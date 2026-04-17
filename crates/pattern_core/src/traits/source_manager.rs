//! Registry of [`crate::traits::DataStream`]s.
//!
//! A [`SourceManager`] owns the set of external data streams registered
//! with a running agent. Tools and context composers consult the manager
//! via `&dyn SourceManager` to locate a stream by name (or by concrete
//! type, via [`crate::traits::DataStream::as_any`]).
//!
//! # Interior mutability
//!
//! Methods take `&self`, not `&mut self`. Concrete implementations use
//! interior mutability (e.g. `DashMap`, `RwLock`) so that the manager can
//! be shared by reference across many tool contexts without threading a
//! mutable borrow through every call site. This matches the pre-v3
//! `MockSourceManager` test utility and the production-side expectation.

use std::sync::Arc;

use async_trait::async_trait;
use smol_str::SmolStr;

use crate::error::CoreError;
use crate::traits::data_stream::DataStream;

/// Human-readable name for a registered data stream.
///
/// Aliased as `SmolStr` because source names are short, frequently cloned,
/// and compared by value across every routing decision.
pub type SourceName = SmolStr;

/// Registry of active data streams.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use pattern_core::error::CoreError;
/// use pattern_core::traits::{DataStream, SourceManager};
/// use pattern_core::traits::source_manager::SourceName;
///
/// struct Dummy;
///
/// #[async_trait]
/// impl SourceManager for Dummy {
///     async fn register(
///         &self,
///         _name: SourceName,
///         _stream: Arc<dyn DataStream>,
///     ) -> Result<(), CoreError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn list_streams(&self) -> Vec<SourceName> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn get_stream_source(&self, _name: &SourceName) -> Option<Arc<dyn DataStream>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait SourceManager: Send + Sync {
    /// Register a stream under the given name.
    ///
    /// Uses `&self` so callers need not thread a mutable borrow; implement
    /// with interior mutability.
    async fn register(
        &self,
        name: SourceName,
        stream: Arc<dyn DataStream>,
    ) -> Result<(), CoreError>;

    /// List the names of all currently-registered streams.
    fn list_streams(&self) -> Vec<SourceName>;

    /// Fetch a stream by name.
    fn get_stream_source(&self, name: &SourceName) -> Option<Arc<dyn DataStream>>;
}
