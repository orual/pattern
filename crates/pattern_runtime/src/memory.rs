//! Memory subsystem: adapter, turn history, and supporting types.
//!
//! - [`MemoryStoreAdapter`] — thin delegating wrapper over `Arc<dyn MemoryStore>`
//!   with a pending `BlockWrite` buffer, drained at turn close.
//! - [`TurnHistory`] — in-memory active turn history + cached archive-summary
//!   head, with running estimated-token count.

pub mod adapter;
pub mod turn_history;

pub use adapter::MemoryStoreAdapter;
pub use turn_history::TurnHistory;
