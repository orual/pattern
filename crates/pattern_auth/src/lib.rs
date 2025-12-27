//! Pattern Auth - Credential and token storage for Pattern constellations.
//!
//! This crate provides constellation-scoped authentication storage:
//! - ATProto OAuth sessions (implements Jacquard's `ClientAuthStore`)
//! - ATProto app-password sessions (implements Jacquard's `SessionStore`)
//! - Discord bot configuration
//! - Model provider OAuth tokens
//!
//! # Architecture
//!
//! Each constellation has its own `auth.db` alongside `constellation.db`.
//! This separation keeps sensitive credentials out of the main database,
//! making constellation backups safer to share.

pub mod atproto;
pub mod db;
pub mod discord;
pub mod error;
pub mod providers;

pub use db::AuthDb;
pub use discord::DiscordBotConfig;
pub use error::{AuthError, AuthResult};
pub use providers::ProviderOAuthToken;
