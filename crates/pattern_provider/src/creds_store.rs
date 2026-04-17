//! Credential storage — keyring primary, JSON fallback.
//!
//! Used only for pattern's own stored credentials (OAuth tokens, refresh
//! tokens, saved API keys). Never touches claude-code's own
//! `~/.claude/.credentials.json` — session-pickup reads that file directly
//! without going through this store.
//!
//! This module is compiled only with the `subscription-oauth` feature
//! because `keyring` + `whoami` are subscription-OAuth-only dependencies.
//! Builds without that feature skip the whole module (Anthropic chain
//! collapses to API-key only, other providers unchanged).
//!
//! # Error semantics
//!
//! - [`pattern_core::error::ProviderError::CredentialStoreUnavailable`] —
//!   the backend is unreachable (no keyring daemon, DBus down, filesystem
//!   path refused). Callers with a fallback tier try it next.
//! - [`pattern_core::error::ProviderError::CredentialStorage`] — the
//!   backend is reachable but the stored data is corrupt or failed to
//!   persist. Callers should NOT fall back; the problem is the data.
//! - `Ok(None)` — the backend is reachable and has no entry for this
//!   provider. Not an error; the caller's tier chain falls through.

pub mod json_fallback;
pub mod keyring;

use std::sync::Arc;

use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;

pub use json_fallback::JsonFallbackStore;
pub use keyring::KeyringStore;

/// A persistent store for per-provider OAuth credentials.
///
/// Implementations decide how to serialise + persist. The only shape
/// guarantee is that a `put(tok)` followed by a `get(tok.provider)`
/// round-trips the token unchanged (modulo `SecretString` identity —
/// implementations serialise the inner string verbatim).
#[async_trait::async_trait]
pub trait CredsStore: Send + Sync {
    /// Fetch the stored token for `provider`, if any.
    async fn get(&self, provider: &str) -> Result<Option<ProviderCredential>, ProviderError>;

    /// Insert or replace the token for `token.provider`.
    async fn put(&self, token: &ProviderCredential) -> Result<(), ProviderError>;

    /// Remove the token for `provider`, if any. Absence is not an error.
    async fn delete(&self, provider: &str) -> Result<(), ProviderError>;
}

/// A two-tier [`CredsStore`] — primary + fallback — that transparently
/// routes through the first-available backend.
///
/// Reads: try `primary`; on [`ProviderError::CredentialStoreUnavailable`]
/// fall through to `fallback`. Any other error propagates unchanged.
///
/// Writes: try `primary`; on `CredentialStoreUnavailable` fall through to
/// `fallback`. We don't mirror writes — whichever store is live takes the
/// authoritative copy. When primary comes back online later, the stale
/// fallback record is benign (gets overwritten on next refresh).
///
/// AC4.6: when BOTH backends are unavailable, the caller sees
/// [`ProviderError::CredentialStoreUnavailable`] with no silent success.
pub struct CredsStoreResolver {
    primary: Arc<dyn CredsStore>,
    fallback: Arc<dyn CredsStore>,
}

impl CredsStoreResolver {
    /// Compose two stores. Order matters: `primary` is tried first on every
    /// operation. Typical shape: `KeyringStore` primary, `JsonFallbackStore`
    /// fallback.
    pub fn new(primary: Arc<dyn CredsStore>, fallback: Arc<dyn CredsStore>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait::async_trait]
impl CredsStore for CredsStoreResolver {
    async fn get(&self, provider: &str) -> Result<Option<ProviderCredential>, ProviderError> {
        match self.primary.get(provider).await {
            Ok(result) => Ok(result),
            Err(ProviderError::CredentialStoreUnavailable) => {
                tracing::warn!(
                    provider,
                    "primary creds store unavailable; falling back to secondary"
                );
                self.fallback.get(provider).await
            }
            Err(e) => Err(e),
        }
    }

    async fn put(&self, token: &ProviderCredential) -> Result<(), ProviderError> {
        match self.primary.put(token).await {
            Ok(()) => Ok(()),
            Err(ProviderError::CredentialStoreUnavailable) => {
                tracing::warn!(
                    provider = %token.provider,
                    "primary creds store unavailable; writing to fallback"
                );
                self.fallback.put(token).await
            }
            Err(e) => Err(e),
        }
    }

    async fn delete(&self, provider: &str) -> Result<(), ProviderError> {
        // Delete from both — callers expect "forget this token" to be total.
        // If either backend is unavailable, we still propagate the failure
        // so the caller knows the forget wasn't complete.
        let primary_res = self.primary.delete(provider).await;
        let fallback_res = self.fallback.delete(provider).await;
        match (primary_res, fallback_res) {
            (Ok(()), Ok(())) => Ok(()),
            // Both unavailable → genuine error
            (
                Err(ProviderError::CredentialStoreUnavailable),
                Err(ProviderError::CredentialStoreUnavailable),
            ) => Err(ProviderError::CredentialStoreUnavailable),
            // One unavailable, other succeeded → log + succeed (forget is best-effort)
            (Err(ProviderError::CredentialStoreUnavailable), Ok(()))
            | (Ok(()), Err(ProviderError::CredentialStoreUnavailable)) => {
                tracing::warn!(provider, "one creds-store backend unavailable during delete");
                Ok(())
            }
            // Any non-Unavailable error propagates
            (Err(e), _) | (_, Err(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use secrecy::SecretString;
    use std::sync::Mutex;

    /// Test double: configurable CredsStore behaviour per-call.
    struct MockStore {
        get_fn: Mutex<Box<dyn FnMut(&str) -> Result<Option<ProviderCredential>, ProviderError> + Send>>,
        put_fn: Mutex<Box<dyn FnMut(&ProviderCredential) -> Result<(), ProviderError> + Send>>,
        delete_fn: Mutex<Box<dyn FnMut(&str) -> Result<(), ProviderError> + Send>>,
    }

    impl MockStore {
        fn new<G, P, D>(get: G, put: P, del: D) -> Arc<Self>
        where
            G: FnMut(&str) -> Result<Option<ProviderCredential>, ProviderError> + Send + 'static,
            P: FnMut(&ProviderCredential) -> Result<(), ProviderError> + Send + 'static,
            D: FnMut(&str) -> Result<(), ProviderError> + Send + 'static,
        {
            Arc::new(Self {
                get_fn: Mutex::new(Box::new(get)),
                put_fn: Mutex::new(Box::new(put)),
                delete_fn: Mutex::new(Box::new(del)),
            })
        }
    }

    #[async_trait::async_trait]
    impl CredsStore for MockStore {
        async fn get(&self, provider: &str) -> Result<Option<ProviderCredential>, ProviderError> {
            (self.get_fn.lock().unwrap())(provider)
        }
        async fn put(&self, token: &ProviderCredential) -> Result<(), ProviderError> {
            (self.put_fn.lock().unwrap())(token)
        }
        async fn delete(&self, provider: &str) -> Result<(), ProviderError> {
            (self.delete_fn.lock().unwrap())(provider)
        }
    }

    fn sample_token() -> ProviderCredential {
        let now = Timestamp::now();
        ProviderCredential {
            provider: "anthropic".into(),
            access_token: SecretString::from("at".to_string()),
            refresh_token: None,
            expires_at: None,
            scope: None,
            session_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn resolver_falls_through_when_primary_unavailable() {
        let primary = MockStore::new(
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
        );
        let tok = sample_token();
        let fallback = {
            let stored = tok.clone();
            MockStore::new(move |_| Ok(Some(stored.clone())), |_| Ok(()), |_| Ok(()))
        };

        let resolver = CredsStoreResolver::new(primary, fallback);
        let result = resolver.get("anthropic").await.expect("fallback should succeed");
        let fetched = result.expect("token should be present");
        assert_eq!(fetched.provider, "anthropic");
    }

    #[tokio::test]
    async fn resolver_reports_unavailable_when_both_fail() {
        let primary = MockStore::new(
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
        );
        let fallback = MockStore::new(
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Err(ProviderError::CredentialStoreUnavailable),
        );

        let resolver = CredsStoreResolver::new(primary, fallback);
        let err = resolver.get("anthropic").await.expect_err("both unavailable");
        assert!(matches!(err, ProviderError::CredentialStoreUnavailable));
    }

    #[tokio::test]
    async fn resolver_propagates_non_unavailable_errors() {
        let primary = MockStore::new(
            |_| {
                Err(ProviderError::CredentialStorage {
                    reason: "corrupt json".into(),
                })
            },
            |_| Ok(()),
            |_| Ok(()),
        );
        let fallback = MockStore::new(
            |_| unreachable!("should not fall through on non-Unavailable error"),
            |_| unreachable!(),
            |_| Ok(()),
        );

        let resolver = CredsStoreResolver::new(primary, fallback);
        let err = resolver.get("anthropic").await.expect_err("corruption propagates");
        assert!(matches!(
            err,
            ProviderError::CredentialStorage { reason } if reason.contains("corrupt")
        ));
    }

    #[tokio::test]
    async fn resolver_write_falls_through_when_primary_unavailable() {
        let primary = MockStore::new(
            |_| Ok(None),
            |_| Err(ProviderError::CredentialStoreUnavailable),
            |_| Ok(()),
        );
        let fallback_calls = Arc::new(Mutex::new(0u32));
        let fallback = {
            let calls = fallback_calls.clone();
            MockStore::new(
                |_| Ok(None),
                move |_| {
                    *calls.lock().unwrap() += 1;
                    Ok(())
                },
                |_| Ok(()),
            )
        };

        let resolver = CredsStoreResolver::new(primary, fallback);
        resolver.put(&sample_token()).await.expect("fallback write should succeed");
        assert_eq!(*fallback_calls.lock().unwrap(), 1);
    }
}
