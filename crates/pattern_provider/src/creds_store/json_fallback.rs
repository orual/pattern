// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! JSON-file credential fallback when the keyring is unavailable.
//!
//! Stores per-provider tokens as `<root>/<provider>.json` with restrictive
//! Unix permissions (`0700` on the directory, `0600` on files). Writes are
//! atomic: serialize → temp file → rename.
//!
//! The default root is `$XDG_CONFIG_HOME/pattern/creds/` (falling back to
//! `~/.config/pattern/creds/` on platforms without XDG). Callers can
//! override via [`JsonFallbackStore::with_root`] for tests or
//! portable-install scenarios.
//!
//! # Windows
//!
//! Unix permission bits don't apply; we just `create_dir_all` and trust the
//! user's %APPDATA% ACL. A harder posture would require
//! `windows-acl`-style tightening — out of scope for now.

use std::path::{Path, PathBuf};

use pattern_core::error::ProviderError;
use pattern_core::types::provider::ProviderCredential;
use rand::RngCore;

use super::CredsStore;
use crate::auth::file_lock::{FileLockError, acquire_file_lock};

/// JSON-file credential store.
pub struct JsonFallbackStore {
    root: PathBuf,
}

impl JsonFallbackStore {
    /// Default root: `$XDG_CONFIG_HOME/pattern/creds/` (falling back to
    /// `~/.config/pattern/creds/`). Creates the directory if absent; sets
    /// `0700` on Unix.
    pub fn new() -> Result<Self, ProviderError> {
        let root = default_root()?;
        Self::with_root(root)
    }

    /// Construct with an explicit root directory. Primarily for tests.
    pub fn with_root(root: PathBuf) -> Result<Self, ProviderError> {
        std::fs::create_dir_all(&root).map_err(|e| io_to_provider(&root, "create_dir_all", e))?;
        tighten_dir_perms(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, provider: &str) -> Result<PathBuf, ProviderError> {
        // Reject path traversal in the provider name at runtime, not just in
        // debug builds. Provider names are normally internal constants
        // (AdapterKind → &str), but a misconfigured chain or a future
        // user-supplied provider string could slip something through. The
        // check is cheap; the consequence of skipping it is arbitrary file
        // reads/writes under the creds directory.
        if provider.contains('/') || provider.contains('\\') || provider.contains("..") {
            return Err(ProviderError::CredentialStorage {
                reason: format!(
                    "provider name '{provider}' contains path separators or traversal sequences"
                ),
            });
        }
        Ok(self.root.join(format!("{provider}.json")))
    }
}

#[async_trait::async_trait]
impl CredsStore for JsonFallbackStore {
    async fn get(&self, provider: &str) -> Result<Option<ProviderCredential>, ProviderError> {
        let path = self.path_for(provider)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => {
                let tok: ProviderCredential =
                    serde_json::from_str(&json).map_err(|e| ProviderError::CredentialStorage {
                        reason: format!("json_fallback parse failed for {path:?}: {e}"),
                    })?;
                Ok(Some(tok))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_to_provider(&path, "read_to_string", e)),
        }
    }

    async fn put(&self, token: &ProviderCredential) -> Result<(), ProviderError> {
        let path = self.path_for(&token.provider)?;

        // Per-call random nonce — the previous `{provider}.json.tmp` was
        // shared across concurrent in-process callers, which races: A's
        // rename consumed the path before B finished writing. Nonce
        // makes each writer's temp file unique.
        let mut nonce_bytes = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = u64::from_le_bytes(nonce_bytes);
        let tmp = path.with_extension(format!("json.tmp.{nonce:x}"));

        // Cross-process advisory flock. Mirrors the same pattern codex
        // storage uses; protects against Pattern-vs-Pattern races on the
        // shared keyring-fallback path.
        let lock_path = path.with_extension("json.lock");
        let _guard = acquire_file_lock(&lock_path)
            .await
            .map_err(|e| file_lock_to_provider(&lock_path, e))?;

        let json =
            serde_json::to_string_pretty(token).map_err(|e| ProviderError::CredentialStorage {
                reason: format!("json_fallback serialize failed: {e}"),
            })?;

        tokio::fs::write(&tmp, &json)
            .await
            .map_err(|e| io_to_provider(&tmp, "write temp", e))?;

        tighten_file_perms(&tmp).await?;

        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| io_to_provider(&path, "atomic rename", e))?;

        Ok(())
    }

    async fn delete(&self, provider: &str) -> Result<(), ProviderError> {
        let path = self.path_for(provider)?;
        // Acquire the same lock as put() so a delete can't race a
        // concurrent put on the same provider.
        let lock_path = path.with_extension("json.lock");
        let _guard = acquire_file_lock(&lock_path)
            .await
            .map_err(|e| file_lock_to_provider(&lock_path, e))?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // idempotent
            Err(e) => Err(io_to_provider(&path, "remove_file", e)),
        }
    }
}

fn file_lock_to_provider(lock_path: &Path, e: FileLockError) -> ProviderError {
    tracing::warn!(?lock_path, error = %e, "json_fallback file_lock error");
    ProviderError::CredentialStoreUnavailable
}

// ---- helpers ----

fn default_root() -> Result<PathBuf, ProviderError> {
    let roots = pattern_core::PatternRoots::default_paths()
        .map_err(|_| ProviderError::CredentialStoreUnavailable)?;
    Ok(roots.config_root().join("creds"))
}

/// Classify an I/O error as "backend unreachable" vs "storage layer".
///
/// We lean toward `CredentialStoreUnavailable` for permission / filesystem
/// layout issues because the usual fallback-chain semantics apply — another
/// tier (keyring) may succeed where the file tier cannot. Parse failures
/// are classified as `CredentialStorage` at the call site instead.
fn io_to_provider(path: &Path, op: &str, e: std::io::Error) -> ProviderError {
    use std::io::ErrorKind::*;
    tracing::warn!(?path, op, error = %e, "json_fallback io error");
    match e.kind() {
        PermissionDenied | NotFound | AlreadyExists | InvalidInput => {
            ProviderError::CredentialStoreUnavailable
        }
        _ => ProviderError::CredentialStorage {
            reason: format!("{op} failed for {path:?}: {e}"),
        },
    }
}

#[cfg(unix)]
fn tighten_dir_perms(path: &Path) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| io_to_provider(path, "metadata", e))?
        .permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
        .map_err(|e| io_to_provider(path, "set_permissions 0700", e))?;
    Ok(())
}

#[cfg(not(unix))]
fn tighten_dir_perms(_path: &Path) -> Result<(), ProviderError> {
    Ok(())
}

#[cfg(unix)]
async fn tighten_file_perms(path: &Path) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = tokio::fs::metadata(path)
        .await
        .map_err(|e| io_to_provider(path, "metadata", e))?
        .permissions();
    perms.set_mode(0o600);
    tokio::fs::set_permissions(path, perms)
        .await
        .map_err(|e| io_to_provider(path, "set_permissions 0600", e))?;
    Ok(())
}

#[cfg(not(unix))]
async fn tighten_file_perms(_path: &Path) -> Result<(), ProviderError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use secrecy::{ExposeSecret, SecretString};
    use tempfile::tempdir;

    fn sample_token(provider: &str) -> ProviderCredential {
        let now = Timestamp::now();
        ProviderCredential {
            provider: provider.into(),
            access_token: SecretString::from(format!("at-{provider}")),
            refresh_token: Some(SecretString::from(format!("rt-{provider}"))),
            expires_at: None,
            scope: Some("user:inference".into()),
            session_id: Some("sess-123".into()),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn round_trip_put_get_delete() {
        let dir = tempdir().expect("tempdir");
        let store =
            JsonFallbackStore::with_root(dir.path().join("creds")).expect("construct store");

        let tok = sample_token("anthropic");
        store.put(&tok).await.expect("put");

        let fetched = store
            .get("anthropic")
            .await
            .expect("get")
            .expect("token present");

        assert_eq!(fetched.provider, "anthropic");
        assert_eq!(fetched.access_token.expose_secret(), "at-anthropic");
        assert_eq!(
            fetched.refresh_token.as_ref().map(|s| s.expose_secret()),
            Some("rt-anthropic")
        );
        assert_eq!(fetched.scope.as_deref(), Some("user:inference"));
        assert_eq!(fetched.session_id.as_deref(), Some("sess-123"));

        store.delete("anthropic").await.expect("delete");
        let after = store.get("anthropic").await.expect("get after delete");
        assert!(after.is_none(), "token should be absent after delete");
    }

    #[tokio::test]
    async fn delete_absent_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let store =
            JsonFallbackStore::with_root(dir.path().join("creds")).expect("construct store");
        store.delete("never-stored").await.expect("no-op delete");
    }

    #[tokio::test]
    async fn get_absent_returns_none_not_error() {
        let dir = tempdir().expect("tempdir");
        let store =
            JsonFallbackStore::with_root(dir.path().join("creds")).expect("construct store");
        let result = store.get("anthropic").await.expect("absent key is ok");
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stored_file_has_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().expect("tempdir");
        let store =
            JsonFallbackStore::with_root(dir.path().join("creds")).expect("construct store");
        store.put(&sample_token("anthropic")).await.expect("put");

        let path = dir.path().join("creds").join("anthropic.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "stored credential file must be 0600");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn creds_dir_has_0700_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().expect("tempdir");
        let creds_dir = dir.path().join("creds");
        let _store = JsonFallbackStore::with_root(creds_dir.clone()).expect("construct store");

        let mode = std::fs::metadata(&creds_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "creds dir must be 0700");
    }

    #[tokio::test]
    async fn path_traversal_in_provider_name_is_rejected() {
        let dir = tempdir().expect("tempdir");
        let store =
            JsonFallbackStore::with_root(dir.path().join("creds")).expect("construct store");

        // All three traversal forms must be rejected at runtime.
        let err = store
            .get("../etc/passwd")
            .await
            .expect_err("traversal must fail");
        assert!(
            matches!(err, ProviderError::CredentialStorage { .. }),
            "expected CredentialStorage, got {err:?}"
        );

        let err = store
            .get("..\\windows\\system32")
            .await
            .expect_err("backslash traversal must fail");
        assert!(matches!(err, ProviderError::CredentialStorage { .. }));
    }

    #[tokio::test]
    async fn corrupt_stored_json_surfaces_as_credential_storage_error() {
        let dir = tempdir().expect("tempdir");
        let creds_dir = dir.path().join("creds");
        let store = JsonFallbackStore::with_root(creds_dir.clone()).expect("construct store");

        // Write garbage directly to the expected path.
        tokio::fs::write(creds_dir.join("anthropic.json"), "{not valid json")
            .await
            .expect("write garbage");

        let err = store
            .get("anthropic")
            .await
            .expect_err("corrupt json should surface as error");
        assert!(
            matches!(err, ProviderError::CredentialStorage { .. }),
            "got: {err:?}"
        );
    }
}
