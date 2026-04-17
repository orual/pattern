//! Credential storage — keyring primary, JSON fallback.
//!
//! Used only for pattern's own stored credentials (OAuth tokens, refresh
//! tokens, API keys saved to local config). Never touches claude-code's
//! own `~/.claude/.credentials.json` — session-pickup reads that file
//! directly without going through this store.
//!
//! This module is compiled only with the `subscription-oauth` feature
//! because keyring + whoami are subscription-OAuth-only dependencies.
//!
//! Phase 4 Task 6 populates the keyring and JSON fallback implementations.
//! See phase_04.md for the full layout and behaviour contract (AC3.6,
//! AC4.6 are driven by this module's error behaviour).
