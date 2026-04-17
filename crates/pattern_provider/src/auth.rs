//! Credential resolution for every supported provider.
//!
//! Each provider has a **tier chain** — an ordered list of [`CredentialTier`]
//! implementations tried in order. Anthropic uses session-pickup → PKCE →
//! API key (all three gated by the `subscription-oauth` feature for the first
//! two). Gemini and OpenAI use API key only.
//!
//! The gateway asks the per-provider chain for credentials on each request;
//! the first tier that returns a [`ResolvedCredential`] wins. Absence of a
//! credential from one tier is not an error — the chain falls through. An
//! explicit failure (e.g. stored token refresh failed) short-circuits the
//! chain with a hard [`pattern_core::error::ProviderError`].
//!
//! Phase 4 populates: `api_key.rs` (always-present), `session_pickup.rs` and
//! `pkce.rs` (feature-gated), and the top-level `resolver.rs` that composes
//! per-provider chains. See phase_04.md Tasks 8, 9, 10.

// Phase 4 Task 10: populate this module tree with per-provider tier chains.
// Subcomponents (`session_pickup`, `pkce`, `api_key`) land in their own tasks
// per phase_04.md Subcomponent C.
