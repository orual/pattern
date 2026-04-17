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

    /// The credential store (pattern-auth database) is not reachable.
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
        help("check that the pattern-auth database exists and is not locked")
    )]
    CredentialStoreUnavailable,

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
}
