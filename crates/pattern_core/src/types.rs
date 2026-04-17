//! Core value types used across the pattern_core trait surface.
pub mod batch;
pub mod block_ref;
pub mod message;

pub use batch::{BatchType, MessageBatch};
pub use block_ref::BlockRef;
pub use message::{Message, ResponseMeta};
