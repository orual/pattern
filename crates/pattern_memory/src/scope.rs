//! Memory scope isolation layer.
//!
//! [`MemoryScope`] wraps any [`MemoryStore`] and routes reads/writes based on
//! an [`IsolatePolicy`]. This enables persona isolation in project contexts:
//! the caller sees a unified memory surface while the scope layer enforces
//! read-only or invisible semantics on persona blocks depending on policy.
//!
//! # Module layout
//!
//! - `scope.rs` — this file; re-exports public API.
//! - `scope/policy.rs` — [`ScopeBinding`] configuration struct.
//! - `scope/wrapper.rs` — [`MemoryScope<S>`] wrapper and `MemoryStore` impl.

mod policy;
mod wrapper;

pub use policy::ScopeBinding;
pub use wrapper::MemoryScope;
