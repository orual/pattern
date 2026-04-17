//! Credential resolution for every supported provider.
//!
//! Each provider has a **tier chain** — an ordered list of tiers tried in
//! order. Anthropic's chain: session-pickup → PKCE → API key (the first two
//! gated by the `subscription-oauth` feature). Gemini and OpenAI use API
//! key only. Tasks 9 and 10 populate PKCE and API-key + the composing
//! resolver; Task 8 lands session-pickup in isolation.
//!
//! The gateway asks the per-provider chain for credentials on each request;
//! the first tier that returns a [`pattern_core::types::provider::ProviderOAuthToken`]
//! (or other `ResolvedCredential` variant — that shape lands with Task 10)
//! wins. Absence of a credential from one tier is not an error; the chain
//! falls through. An explicit failure (e.g. stored token refresh failed)
//! short-circuits the chain with a hard
//! [`pattern_core::error::ProviderError`].

#[cfg(feature = "subscription-oauth")]
pub mod session_pickup;

#[cfg(feature = "subscription-oauth")]
pub use session_pickup::SessionPickupTier;

// api_key, resolver, pkce populate in Tasks 9-10 of phase_04.md.
