// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use crate::auth::keyring_util::{classify_keyring_error, open_entry};

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

    /// Produce a keyring entry for `provider`. Naming convention:
    /// service = `"{service_prefix}-{provider}"`, account = local username.
    /// Error mapping shared with codex_storage via
    /// [`crate::auth::keyring_util::open_entry`].
    fn entry(&self, provider: &str) -> Result<Entry, ProviderError> {
        open_entry(&self.service_name(provider), &whoami::username())
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
