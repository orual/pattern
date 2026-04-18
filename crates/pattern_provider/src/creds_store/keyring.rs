//! Keyring-backed [`CredsStore`] — platform native secure storage.
//!
//! Linux: Secret Service API (gnome-keyring, KWallet, etc. via DBus).
//! macOS: Keychain. Windows: Credential Manager.
//!
//! The `keyring` crate calls are synchronous (no async trait equivalent
//! available). For interactive session checks the direct call is fine;
//! if this becomes a bottleneck in hot paths, wrap in
//! `tokio::task::spawn_blocking`.
//!
//! # Service naming
//!
//! Entries are stored under the service name `pattern-<provider>` (e.g.
//! `pattern-anthropic`, `pattern-gemini`). The account name is the
//! platform user's login name via [`whoami::username`]. This matches the
//! pre-v3 `pattern_auth` convention (that crate has been retired).

use keyring::Entry;
use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;

use super::CredsStore;

/// Keyring-backed credential store.
pub struct KeyringStore {
    /// Service-name prefix; default `"pattern"`. Entries end up stored as
    /// `<service_prefix>-<provider>` in the keyring.
    service_prefix: String,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self {
            service_prefix: "pattern".into(),
        }
    }
}

impl KeyringStore {
    /// Construct with the default `"pattern"` service prefix.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with a custom service prefix. Useful for tests that want
    /// to avoid colliding with a developer's real keyring entries.
    pub fn with_service_prefix(prefix: impl Into<String>) -> Self {
        Self {
            service_prefix: prefix.into(),
        }
    }

    fn service_name(&self, provider: &str) -> String {
        format!("{}-{}", self.service_prefix, provider)
    }

    /// Produce a keyring entry for `provider`, mapping any construction
    /// failure to `CredentialStoreUnavailable` (likely means "no keyring
    /// backend accessible" — DBus down, no Secret Service daemon, etc.).
    fn entry(&self, provider: &str) -> Result<Entry, ProviderError> {
        Entry::new(&self.service_name(provider), &whoami::username()).map_err(|e| {
            tracing::warn!(provider, error = %e, "keyring entry construction failed");
            ProviderError::CredentialStoreUnavailable
        })
    }
}

/// Classify a `keyring::Error` as either backend-unreachable (retry via
/// fallback) or stored-data-corrupt (propagate).
fn classify_keyring_error(e: keyring::Error) -> ProviderError {
    use keyring::Error;
    match e {
        // Backend-unreachable variants → fallback tier gets a chance.
        Error::PlatformFailure(_) | Error::NoStorageAccess(_) => {
            ProviderError::CredentialStoreUnavailable
        }
        // Stored data is unreadable — this is corruption, not unavailability.
        Error::BadEncoding(_) | Error::Ambiguous(_) => ProviderError::CredentialStorage {
            reason: format!("keyring stored data unusable: {e}"),
        },
        // Rare shape errors — conservatively treat as storage errors.
        Error::TooLong(_, _) | Error::Invalid(_, _) => ProviderError::CredentialStorage {
            reason: format!("keyring API misuse: {e}"),
        },
        // NoEntry is caller-level absence, not an error from this function's POV.
        // The callers map it to Ok(None) before reaching here.
        Error::NoEntry => ProviderError::CredentialStorage {
            reason: "NoEntry reached classify_keyring_error — this is a bug in KeyringStore".into(),
        },
        // Future-proofing against new variants we don't recognise.
        other => ProviderError::CredentialStoreUnavailable
            .tap_log(format!("unknown keyring error: {other}")),
    }
}

/// Tiny extension trait so we can log-and-return in one line.
trait TapLog: Sized {
    fn tap_log(self, msg: String) -> Self;
}

impl TapLog for ProviderError {
    fn tap_log(self, msg: String) -> Self {
        tracing::warn!(message = %msg);
        self
    }
}

#[async_trait::async_trait]
impl CredsStore for KeyringStore {
    async fn get(&self, provider: &str) -> Result<Option<ProviderCredential>, ProviderError> {
        let entry = self.entry(provider)?;
        match entry.get_password() {
            Ok(json) => {
                let tok: ProviderCredential =
                    serde_json::from_str(&json).map_err(|e| ProviderError::CredentialStorage {
                        reason: format!("keyring JSON parse failed for provider '{provider}': {e}"),
                    })?;
                Ok(Some(tok))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(classify_keyring_error(e)),
        }
    }

    async fn put(&self, token: &ProviderCredential) -> Result<(), ProviderError> {
        let entry = self.entry(&token.provider)?;
        let json = serde_json::to_string(token).map_err(|e| ProviderError::CredentialStorage {
            reason: format!("keyring JSON serialize failed: {e}"),
        })?;
        entry.set_password(&json).map_err(classify_keyring_error)
    }

    async fn delete(&self, provider: &str) -> Result<(), ProviderError> {
        let entry = self.entry(provider)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()), // idempotent — nothing to delete
            Err(e) => Err(classify_keyring_error(e)),
        }
    }
}
