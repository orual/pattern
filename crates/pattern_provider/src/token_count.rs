//! Provider-reported token counting.
//!
//! Two complementary paths:
//!
//! - **Pre-request sizing**: async `count_tokens` against the provider's
//!   dedicated endpoint (Anthropic's `/v1/messages/count_tokens`). Expensive
//!   because it's a separate HTTP round trip, so callers use it sparingly
//!   (e.g. before committing to an expensive request or to populate a cache).
//! - **Post-response capture**: the `usage` field on the chat response is
//!   exposed verbatim through the `ProviderClient` return shape. Subsequent
//!   compaction / context-length decisions can use these counts directly
//!   without a separate network call.
//!
//! Phase 5 migrates the existing compaction call sites (in
//! `rewrite-staging/context/compression.rs`) from heuristic token estimates
//! to these provider-reported counts; Phase 4 just lands the API.
//!
//! Phase 4 Tasks 16 (`count_tokens` wrapper) and 17 (usage capture) populate
//! this module. See phase_04.md AC5b.1, AC5b.2, AC5b.4.
