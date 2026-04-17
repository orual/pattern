//! Registry of outbound [`crate::traits::Endpoint`]s.
//!
//! An [`EndpointRegistry`] is the lookup surface the agent runtime consults
//! when deciding where to send an outbound message. It replaces the pre-v3
//! hard-coded endpoint matching inside `AgentMessageRouter`, making the set
//! of endpoints extensible and testable.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::CoreError;
use crate::traits::endpoint::Endpoint;

/// Lookup surface for registered [`Endpoint`]s.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use pattern_core::error::CoreError;
/// use pattern_core::traits::{Endpoint, EndpointRegistry};
///
/// struct Dummy;
///
/// #[async_trait]
/// impl EndpointRegistry for Dummy {
///     async fn register(&self, _ep: Arc<dyn Endpoint>) -> Result<(), CoreError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn endpoint(&self, _name: &str) -> Option<Arc<dyn Endpoint>> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
///     fn list(&self) -> Vec<String> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait EndpointRegistry: Send + Sync {
    /// Register an endpoint. Implementations decide how to handle
    /// duplicate-name registrations (typical choice: replace).
    async fn register(&self, endpoint: Arc<dyn Endpoint>) -> Result<(), CoreError>;

    /// Fetch a registered endpoint by name.
    fn endpoint(&self, name: &str) -> Option<Arc<dyn Endpoint>>;

    /// List the names of all registered endpoints.
    fn list(&self) -> Vec<String>;
}
