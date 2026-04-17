//! Pattern provider: LLM authentication, request shaping, rate limiting, token counting.
//!
//! Owns the three-tier auth resolver (session-pickup → PKCE → API key), the
//! rebased `rust-genai` fork, and the request composer that emits the
//! three-segment cache layout defined in the v3 foundation design.
//!
//! Populated incrementally across v3 foundation phase 4 (auth/shaping/rate
//! limiting/token counting) and phase 5 (request composer with segmented
//! `cache_control` markers).
