//! Shared keyring-handling primitives used by `creds_store::keyring` (the
//! Pattern-side `pattern-<provider>` store) and `auth::codex_storage` (the
//! codex-compatible `"Codex Auth"` store).
//!
//! Two helpers:
//! - `open_entry(service, account)` — construct a `keyring::Entry`,
//!   mapping construction failures to `CredentialStoreUnavailable` so
//!   fallback tiers get a chance.
//! - `classify_keyring_error` — single source of truth for which keyring
//!   errors are "backend unreachable" (retry via fallback) vs "stored
//!   data corrupt" (propagate).

#![cfg(feature = "subscription-oauth")]

use keyring::{Entry, Error as KeyringError};
use pattern_core::error::ProviderError;

/// Construct a keyring entry for `(service, account)`. Construction
/// failures map to [`ProviderError::CredentialStoreUnavailable`] —
/// usually "no keyring backend reachable" (DBus down, no Secret Service
/// daemon, etc.) — so callers can fall back to file storage.
pub(crate) fn open_entry(service: &str, account: &str) -> Result<Entry, ProviderError> {
    Entry::new(service, account).map_err(|e| {
        tracing::warn!(service, account, error = %e, "keyring entry construction failed");
        ProviderError::CredentialStoreUnavailable
    })
}

/// Classify a `keyring::Error` as either backend-unreachable (caller
/// should fall back to file storage) or stored-data-corrupt (propagate
/// as `CredentialStorage`).
///
/// `NoEntry` is intentionally NOT handled here — callers should map that
/// to `Ok(None)` at the call site before reaching this function (the
/// "no credential stored" case is not an error).
pub(crate) fn classify_keyring_error(e: KeyringError) -> ProviderError {
    use KeyringError as E;
    match e {
        // Backend-unreachable variants → fallback tier gets a chance.
        E::PlatformFailure(_) | E::NoStorageAccess(_) => ProviderError::CredentialStoreUnavailable,
        // Stored data is unreadable — this is corruption, not unavailability.
        E::BadEncoding(_) | E::Ambiguous(_) => ProviderError::CredentialStorage {
            reason: format!("keyring stored data unusable: {e}"),
        },
        // Rare shape errors — conservatively treat as storage errors.
        E::TooLong(_, _) | E::Invalid(_, _) => ProviderError::CredentialStorage {
            reason: format!("keyring API misuse: {e}"),
        },
        // NoEntry should never reach here — callers handle it as Ok(None).
        E::NoEntry => ProviderError::CredentialStorage {
            reason: "NoEntry reached classify_keyring_error — caller bug".into(),
        },
        // Future-proofing against new variants we don't recognise.
        other => {
            tracing::warn!(error = %other, "unknown keyring error variant; treating as unavailable");
            ProviderError::CredentialStoreUnavailable
        }
    }
}
