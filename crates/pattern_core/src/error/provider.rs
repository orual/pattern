//! Provider errors for LLM and credential interactions.
//!
//! This file defines errors that occur when communicating with an external LLM
//! provider (Anthropic, OpenAI, etc.) or credential store. Surfaced through
//! [`super::core::CoreError::Provider`].
//!
//! # Pre-v3 CoreError variants replaced by this file
//!
//! - `ModelProviderError` → [`ProviderError::RequestFailed`] (partial; the
//!   old variant also held a `genai::Error` source which is preserved here via
//!   the `source` field of `RequestFailed` where applicable).
//! - `ProviderHttpError` → [`ProviderError::RequestFailed`] (the structured
//!   HTTP status/body fields map directly).
//! - `OAuthError` (operation == "flow_timeout") → [`ProviderError::AuthFlowTimeout`].
//! - `OAuthError` (other operations) → [`ProviderError::RefreshFailed`] /
//!   [`ProviderError::CredentialStoreUnavailable`] as appropriate.
//! - `RateLimited` (provider-side) → [`ProviderError::RateLimited`].

use std::time::Duration;

use miette::Diagnostic;
use thiserror::Error;

/// Errors from external LLM providers and the credential/token store.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum ProviderError {
    /// The OAuth interactive flow did not complete within the allowed window.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::AuthFlowTimeout;
    /// assert!(err.to_string().contains("timed out"));
    /// ```
    #[error("OAuth authentication flow timed out")]
    #[diagnostic(
        code(pattern_core::provider::auth_flow_timeout),
        help("complete the browser authentication within the allowed time window")
    )]
    AuthFlowTimeout,

    /// A token refresh attempt failed.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::RefreshFailed {
    ///     reason: "invalid_grant".to_string(),
    /// };
    /// assert!(err.to_string().contains("invalid_grant"));
    /// ```
    #[error("token refresh failed: {reason}")]
    #[diagnostic(
        code(pattern_core::provider::refresh_failed),
        help("re-authenticate via `pattern auth login`")
    )]
    RefreshFailed {
        /// Reason from the provider (e.g. `"invalid_grant"`).
        reason: String,
    },

    /// The initial authorization-code exchange failed (code → access token).
    ///
    /// Distinguished from [`ProviderError::RefreshFailed`] because the
    /// remediation is different: refresh failure typically means "re-auth
    /// from scratch"; exchange failure means "the auth flow itself didn't
    /// complete" (bad state, invalid code, rejected by provider, network
    /// failure).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::AuthExchangeFailed {
    ///     reason: "state parameter mismatch (CSRF guard)".into(),
    /// };
    /// assert!(err.to_string().contains("state parameter"));
    /// ```
    #[error("auth code exchange failed: {reason}")]
    #[diagnostic(
        code(pattern_core::provider::auth_exchange_failed),
        help("restart the auth flow; check the browser copied the entire code#state string")
    )]
    AuthExchangeFailed {
        /// Description of the exchange failure.
        reason: String,
    },

    /// The credential store backend is not reachable (keyring daemon down,
    /// DBus unavailable, filesystem path refused, etc.).
    ///
    /// Callers with a fallback store try the next tier on this error;
    /// distinguished from [`ProviderError::CredentialStorage`] which
    /// indicates corruption or a hard persistence failure that should NOT
    /// trigger fallback.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::CredentialStoreUnavailable;
    /// assert!(err.to_string().contains("credential store"));
    /// ```
    #[error("credential store unavailable")]
    #[diagnostic(
        code(pattern_core::provider::credential_store_unavailable),
        help("check that the credential store backend is running and reachable")
    )]
    CredentialStoreUnavailable,

    /// The credential store returned a value that could not be processed —
    /// corrupt JSON, wrong shape, I/O error during write, etc.
    ///
    /// Distinguished from [`ProviderError::CredentialStoreUnavailable`] by
    /// the fact that the backend IS available but the stored credential is
    /// unusable. Callers should NOT fall back to a different tier on this
    /// error — the problem is the data itself.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::CredentialStorage {
    ///     reason: "malformed JSON in ~/.config/pattern/creds/anthropic.json".into(),
    /// };
    /// assert!(err.to_string().contains("malformed"));
    /// ```
    #[error("credential storage error: {reason}")]
    #[diagnostic(
        code(pattern_core::provider::credential_storage),
        help("inspect the credential store manually or re-authenticate")
    )]
    CredentialStorage {
        /// Human-readable description of the persistence failure.
        reason: String,
    },

    /// Token counting failed before the request was sent.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::TokenCountFailed {
    ///     reason: "tokenizer not initialised".to_string(),
    /// };
    /// assert!(err.to_string().contains("token count"));
    /// ```
    #[error("token count failed: {reason}")]
    #[diagnostic(
        code(pattern_core::provider::token_count_failed),
        help("ensure the tokenizer is initialised before calling token_count")
    )]
    TokenCountFailed {
        /// Description of why counting failed.
        reason: String,
    },

    /// The provider returned a rate-limit response.
    ///
    /// `retry_after` is a stopwatch duration (relative, not wall-clock) that
    /// the caller should wait before retrying. Use `std::time::Duration` here
    /// because it is the conventional type for "wait this long", independent of
    /// the current time.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::RateLimited { retry_after: Duration::from_secs(60) };
    /// assert!(err.to_string().contains("rate limited"));
    /// ```
    #[error("rate limited by provider; retry after {retry_after:?}")]
    #[diagnostic(
        code(pattern_core::provider::rate_limited),
        help("wait for the retry_after duration before sending another request")
    )]
    RateLimited {
        /// How long to wait before retrying.
        retry_after: Duration,
    },

    /// Request shaper is missing required configuration (e.g. empty `x_app`,
    /// banned beta header set, etc.). Raised at shaper construction time
    /// rather than at request time — AC5.5.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::ShaperMisconfigured {
    ///     reason: "x_app cannot be empty".into(),
    /// };
    /// assert!(err.to_string().contains("x_app"));
    /// ```
    #[error("request shaper misconfigured: {reason}")]
    #[diagnostic(
        code(pattern_core::provider::shaper_misconfigured),
        help("check the shaper config passed to the gateway construction")
    )]
    ShaperMisconfigured {
        /// Human-readable description of the misconfiguration.
        reason: String,
    },

    /// No credential tier could resolve a usable credential for the provider.
    ///
    /// Surfaced by `pattern_provider::auth` when every tier in a provider's
    /// chain (session-pickup, PKCE, API key for Anthropic; API key only for
    /// Gemini) has fallen through without producing a credential.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::NoAuthAvailable {
    ///     provider: "anthropic".into(),
    /// };
    /// assert!(err.to_string().contains("no auth"));
    /// ```
    #[error("no auth available for provider '{provider}'")]
    #[diagnostic(
        code(pattern_core::provider::no_auth_available),
        help("run `pattern auth login` or set the provider's API key env var")
    )]
    NoAuthAvailable {
        /// Provider name (matches `AdapterKind` string form).
        provider: String,
    },

    /// The provider returned an HTTP error response.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::RequestFailed {
    ///     status: 500,
    ///     body: Some("internal server error".to_string()),
    /// };
    /// assert!(err.to_string().contains("500"));
    /// ```
    #[error("provider request failed with HTTP {status}")]
    #[diagnostic(
        code(pattern_core::provider::request_failed),
        help("inspect the response body for provider-specific error details")
    )]
    RequestFailed {
        /// HTTP status code returned by the provider.
        status: u16,
        /// Response body, if one was received.
        body: Option<String>,
    },

    // ---- Composer pipeline errors (Phase 5) ----
    //
    // Produced by `pattern_provider::compose` passes and the finalization
    // step. Surfaced when a composer pass fails, a cache-breakpoint budget
    // is exceeded, a placement targets an out-of-bounds index, or the
    // required beta header is missing when extended-TTL markers are in use.
    /// A composer pass returned an error. The pass name + inner error are
    /// preserved for diagnosis; pass names are internal string literals
    /// (`"segment_1"`, `"segment_2"`, …).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let inner = ProviderError::ShaperMisconfigured {
    ///     reason: "x_app empty".into(),
    /// };
    /// let err = ProviderError::ComposerPassFailed {
    ///     pass: "segment_1".into(),
    ///     source: Box::new(inner),
    /// };
    /// assert!(err.to_string().contains("segment_1"));
    /// ```
    #[error("composer pass '{pass}' failed: {source}")]
    #[diagnostic(
        code(pattern_core::provider::composer_pass_failed),
        help("check the source error for the pass-specific failure reason")
    )]
    ComposerPassFailed {
        /// Name of the pass that failed (e.g., `"segment_1"`).
        pass: String,
        /// Underlying error that caused the failure.
        #[source]
        source: Box<ProviderError>,
    },

    /// A composer pass attempted to place a cache_control marker when the
    /// breakpoint budget (Anthropic: 4 per request) was already exhausted.
    /// The `placed_by` list identifies which passes already consumed
    /// breakpoints; `attempted_by` names the pass that would have placed
    /// the fifth marker.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::CacheBreakpointBudgetExceeded {
    ///     budget: 4,
    ///     placed_by: vec!["segment_1".into(), "segment_2".into(),
    ///                      "segment_3".into(), "cache_reference".into()],
    ///     attempted_by: "cache_edits".into(),
    /// };
    /// assert!(err.to_string().contains("4"));
    /// ```
    #[error(
        "cache breakpoint budget of {budget} exceeded (placed by {placed_by:?}; \
         '{attempted_by}' attempted to exceed it)"
    )]
    #[diagnostic(
        code(pattern_core::provider::breakpoint_budget_exceeded),
        help(
            "anthropic allows at most 4 cache_control markers per request; \
             review the pipeline pass set and drop a marker placement"
        )
    )]
    CacheBreakpointBudgetExceeded {
        /// Maximum number of breakpoints allowed (Anthropic: 4).
        budget: usize,
        /// Names of passes that had already placed breakpoints when the
        /// budget-exceeding attempt fired.
        placed_by: Vec<String>,
        /// Name of the pass that attempted to exceed the budget.
        attempted_by: String,
    },

    /// A breakpoint placement targets an out-of-bounds index into its
    /// location collection (system_blocks / messages / tools). Usually
    /// indicates a composer pass running before the block it placed a
    /// marker on was populated — order-of-operations bug.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::InvalidBreakpointLocation {
    ///     location: "system".into(),
    ///     idx: 42,
    /// };
    /// assert!(err.to_string().contains("42"));
    /// ```
    #[error("breakpoint location '{location}' index {idx} is out of bounds")]
    #[diagnostic(
        code(pattern_core::provider::invalid_breakpoint_location),
        help(
            "a composer pass placed a marker at an index that doesn't \
             exist in the final request — check pass ordering and any \
             conditional message/block emission"
        )
    )]
    InvalidBreakpointLocation {
        /// Which collection the breakpoint targeted
        /// (`"system"`, `"message"`, `"tool"`).
        location: String,
        /// The out-of-bounds index.
        idx: usize,
    },

    /// A cache_control marker with extended-TTL semantics (`Ephemeral1h`
    /// or `Ephemeral24h`) was placed but the outbound request lacks the
    /// required `anthropic-beta: extended-cache-ttl-2025-04-11` header.
    /// The shaper normally ensures the header is present when the
    /// session's `CacheProfile::requires_extended_ttl_beta()` is true;
    /// this variant surfaces when that invariant breaks.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::ProviderError;
    ///
    /// let err = ProviderError::MissingExtendedCacheTtlBeta;
    /// assert!(err.to_string().contains("extended-cache-ttl"));
    /// ```
    #[error(
        "composer placed an extended-TTL cache marker but the outbound \
         request lacks the `extended-cache-ttl-2025-04-11` beta header"
    )]
    #[diagnostic(
        code(pattern_core::provider::missing_extended_cache_ttl_beta),
        help(
            "ensure the shaper emits the extended-cache-ttl-2025-04-11 \
             anthropic-beta marker when CacheProfile::requires_extended_ttl_beta() \
             is true"
        )
    )]
    MissingExtendedCacheTtlBeta,
}
