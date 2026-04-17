//! Outbound-message endpoint trait.
//!
//! An [`Endpoint`] is a destination for messages the agent produces — a CLI
//! terminal, a Discord channel, a Bluesky post, a group-coordination router,
//! a database queue, etc. Pre-v3 Pattern wired endpoint kinds in an ad-hoc
//! enum; v3 makes the set extensible through this trait and the companion
//! [`crate::traits::EndpointRegistry`].
//!
//! # Why a trait (and not the pre-v3 `MessageRouter`)
//!
//! The pre-v3 design bundled "where to send" and "how to decide where to
//! send" into a single `MessageRouter`. Those are separate concerns: one is
//! plumbing (this trait), the other is policy (which now lives on the agent
//! runtime itself, informed by the [`crate::types::MessageOrigin`] of the
//! inbound message). Collapsing the router into the runtime avoids a layer
//! that was only ever one call deep.

use async_trait::async_trait;

use crate::error::CoreError;
use crate::types::message::Message;

/// An outbound-message destination.
///
/// # Example
///
/// ```no_run
/// use async_trait::async_trait;
/// use pattern_core::error::CoreError;
/// use pattern_core::traits::Endpoint;
/// use pattern_core::types::message::Message;
///
/// struct Dummy;
///
/// #[async_trait]
/// impl Endpoint for Dummy {
///     fn name(&self) -> &str { "dummy" }
///     async fn deliver(&self, _msg: Message) -> Result<(), CoreError> {
///         unimplemented!("dummy: satisfaction-only example; AC1.3")
///     }
/// }
/// ```
#[async_trait]
pub trait Endpoint: Send + Sync {
    /// Stable, human-readable name used by [`crate::traits::EndpointRegistry`]
    /// for lookup and logging.
    fn name(&self) -> &str;

    /// Deliver a single message to this endpoint.
    ///
    /// Errors are surfaced as [`CoreError`]; the endpoint is responsible for
    /// classifying transport failures into appropriate variants (e.g.
    /// `NoEndpointConfigured`, `RateLimited`).
    async fn deliver(&self, message: Message) -> Result<(), CoreError>;
}
