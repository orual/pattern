//! ATProto authentication module.
//!
//! This module contains implementations of Jacquard's auth traits for SQLite storage.
//!
//! The `oauth_store` module implements `jacquard::oauth::authstore::ClientAuthStore`
//! for `AuthDb`, enabling persistent OAuth session storage.
//!
//! The `session_store` module implements `jacquard::session::SessionStore` for
//! app-password sessions, enabling simple JWT-based authentication.
//!
//! The `models` module provides database row types with proper `FromRow` derives
//! for compile-time query verification.

pub mod models;
mod oauth_store;
mod session_store;

// Re-export summary types for external use
pub use models::{AppPasswordSessionRow, AtprotoAuthType, AtprotoIdentitySummary};
pub use oauth_store::OAuthSessionSummaryRow;
