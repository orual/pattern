//! Hook event lifecycle system.
//!
//! Open string-tag dispatch — adding a hook point is non-breaking.
//! See `tags` for the catalog of well-known events emitted by Pattern.

pub mod cc_aliases;
pub mod event;
pub mod filter;
pub mod payloads;
pub mod tags;

pub use event::{HookEvent, HookEventMetadata, HookResponse, HookSemantics};
pub use filter::{HookFilter, HookFilterError};
