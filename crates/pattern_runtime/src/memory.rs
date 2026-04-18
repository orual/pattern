//! Memory subsystem: adapter, turn history, and supporting types.
//!
//! - [`MemoryStoreAdapter`] — thin delegating wrapper over `Arc<dyn MemoryStore>`
//!   with a pending `BlockWrite` buffer, drained at turn close.

pub mod adapter;

pub use adapter::MemoryStoreAdapter;
