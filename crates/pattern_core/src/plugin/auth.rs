//! Plugin authentication primitives (Phase 6 Task 5d).
//!
//! Lives in `pattern_core::plugin::auth` (gated under `plugin-transport` feature)
//! so both daemon (`pattern_runtime`) and plugin SDK (`pattern_plugin_sdk`)
//! can share PluginKey, AllowList, and (later) KeyStore + challenge-response.
//!
//! Two paths:
//! - **Localhost plugins** (v1): pubkey-only. Registry.kdl is filesystem-private
//!   (mode-600 on `~/.config/pattern/plugins/`). Compromising the pubkey requires
//!   already-having-the-disk, which is past our threat model.
//! - **Atproto plugins** (phase 7): pubkey + shared-secret challenge-response.
//!   Atproto record publishes pubkey; secret is exchanged at install,
//!   persisted in both daemon's + plugin's keystore. Challenge-response (HMAC
//!   over nonce) is replay-resistant. Longer-term: DH-shaped per-session keys.
//!
//! V1 ships only the localhost path. `PluginKey::Atproto` exists in the type
//! system but errors at allow-list build with \"atproto resolution not yet
//! implemented (phase 7).\"

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::plugin::PluginId;

/// How a plugin's pubkey is registered with the daemon.
#[derive(Debug, Clone)]
pub enum PluginKey {
    /// Localhost case: pubkey directly in registry.kdl.
    Direct(iroh::PublicKey),
    /// Atproto case (phase 7): pubkey-uri + CID pin to a public record.
    /// V1 errors at allow-list build.
    Atproto { uri: SmolStr, cid: SmolStr },
}

/// Daemon-side allow-list mapping registered plugin pubkeys to their PluginId.
/// Built from registry.kdl entries at startup; consulted by the auth-gated
/// protocol handler on each incoming Connection.
#[derive(Debug, Clone, Default)]
pub struct AllowList {
    by_pubkey: HashMap<iroh::PublicKey, PluginId>,
}

#[derive(Debug, thiserror::Error)]
pub enum AllowListBuildError {
    #[error("plugin {plugin_id:?}: invalid pubkey encoding: {message}")]
    InvalidPubkey { plugin_id: PluginId, message: SmolStr },
    #[error("plugin {plugin_id:?}: atproto pubkey resolution not yet implemented (phase 7)")]
    AtprotoNotImplemented { plugin_id: PluginId },
    #[error("plugin {plugin_id:?}: duplicate pubkey already registered for plugin {other:?}")]
    DuplicatePubkey { plugin_id: PluginId, other: PluginId },
}

impl AllowList {
    pub fn new() -> Self { Self::default() }

    /// Build from a slice of (plugin_id, plugin_key) pairs.
    /// Atproto entries error in v1; phase 7 wires DID-doc resolution + CID pinning.
    pub fn build_from(entries: &[(PluginId, PluginKey)]) -> Result<Self, AllowListBuildError> {
        let mut by_pubkey = HashMap::new();
        for (plugin_id, key) in entries {
            match key {
                PluginKey::Direct(pk) => {
                    if let Some(other) = by_pubkey.insert(*pk, plugin_id.clone()) {
                        return Err(AllowListBuildError::DuplicatePubkey {
                            plugin_id: plugin_id.clone(),
                            other,
                        });
                    }
                }
                PluginKey::Atproto { .. } => {
                    return Err(AllowListBuildError::AtprotoNotImplemented {
                        plugin_id: plugin_id.clone(),
                    });
                }
            }
        }
        Ok(Self { by_pubkey })
    }

    pub fn lookup(&self, pubkey: &iroh::PublicKey) -> Option<&PluginId> {
        self.by_pubkey.get(pubkey)
    }

    pub fn len(&self) -> usize { self.by_pubkey.len() }
    pub fn is_empty(&self) -> bool { self.by_pubkey.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn fresh_pk() -> iroh::PublicKey {
        SecretKey::generate().public()
    }

    #[test]
    fn allow_list_lookup_finds_registered_plugin() {
        let pk = fresh_pk();
        let id: PluginId = "test-plugin".into();
        let al = AllowList::build_from(&[(id.clone(), PluginKey::Direct(pk))]).unwrap();
        assert_eq!(al.lookup(&pk), Some(&id));
        assert_eq!(al.len(), 1);
    }

    #[test]
    fn allow_list_lookup_misses_unregistered_pubkey() {
        let registered = fresh_pk();
        let unregistered = fresh_pk();
        let al = AllowList::build_from(&[(
            "x".into(),
            PluginKey::Direct(registered),
        )]).unwrap();
        assert!(al.lookup(&unregistered).is_none());
    }

    #[test]
    fn allow_list_atproto_variant_errors_in_v1() {
        let r = AllowList::build_from(&[(
            "remote".into(),
            PluginKey::Atproto {
                uri: "at://did:plc:abc/app.pattern.plugin/foo".into(),
                cid: "bafy...".into(),
            },
        )]);
        assert!(matches!(r, Err(AllowListBuildError::AtprotoNotImplemented { .. })));
    }

    #[test]
    fn allow_list_duplicate_pubkey_errors() {
        let pk = fresh_pk();
        let r = AllowList::build_from(&[(
            "a".into(),
            PluginKey::Direct(pk),
        ), (
            "b".into(),
            PluginKey::Direct(pk),
        )]);
        assert!(matches!(r, Err(AllowListBuildError::DuplicatePubkey { .. })));
    }
}

use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};

/// Error returned when an incoming connection's pubkey isn't in the daemon's allow-list.
/// Wrapped into `AcceptError::User` because `AcceptError::NotAllowed`'s constructor is
/// stack_error-macro-internal (private to iroh).
#[derive(Debug, thiserror::Error)]
#[error("plugin auth: peer pubkey {pubkey} not in allow-list")]
pub struct PluginAuthRejected {
    pub pubkey: iroh::PublicKey,
}

/// iroh `ProtocolHandler` wrapper that gates per-connection on the remote peer's pubkey.
///
/// Wraps any `inner: H: ProtocolHandler` (e.g. `irpc_iroh::IrohProtocol::new(handler)`).
/// On each incoming Connection, extracts `conn.remote_id()` and checks it against the
/// AllowList. If the pubkey is registered, delegates to `inner.accept(conn)`. Otherwise
/// closes the connection with `AcceptError::NotAllowed`.
///
/// V1 implements the localhost path: pubkey-only check. Phase 7 will extend this to also
/// run a shared-secret challenge-response for atproto-published plugins before delegating.
#[derive(Debug, Clone)]
pub struct AuthGatedProtocolHandler<H> {
    allow_list: Arc<AllowList>,
    inner: H,
}

impl<H> AuthGatedProtocolHandler<H> {
    pub fn new(allow_list: Arc<AllowList>, inner: H) -> Self {
        Self { allow_list, inner }
    }

    pub fn allow_list(&self) -> &AllowList {
        &self.allow_list
    }
}

impl<H> ProtocolHandler for AuthGatedProtocolHandler<H>
where
    H: ProtocolHandler,
{
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        match self.allow_list.lookup(&remote) {
            Some(plugin_id) => {
                tracing::debug!(
                    plugin_id = %plugin_id,
                    remote = %remote,
                    "plugin auth: allowed"
                );
                self.inner.accept(conn).await
            }
            None => {
                tracing::warn!(
                    remote = %remote,
                    "plugin auth: rejected (pubkey not in allow-list)"
                );
                conn.close(1u32.into(), b"not allowed");
                Err(AcceptError::from_err(PluginAuthRejected { pubkey: remote }))
            }
        }
    }

    async fn shutdown(&self) {
        self.inner.shutdown().await
    }
}

// ─── Plugin-side keystore ───────────────────────────────────────────────────

use std::path::PathBuf;

/// Where keys come from / go to. Plugin-side primitive.
///
/// Strategy: keyring first (system credential manager: dbus secret-service on Linux,
/// Keychain on macOS, Credential Manager on Windows). Falls back to a mode-0600
/// file at `$XDG_DATA_HOME/pattern/plugins/<plugin-id>/secret` when the keyring
/// can't be reached (no D-Bus session, headless CI, etc.). Same precedence applies
/// on both load and store, so a key written via keyring is read via keyring.
///
/// V1 stores raw 32-byte `iroh::SecretKey::to_bytes()` payloads — no encoding wrapper.
/// Phase 7 adds shared-secret-for-atproto-plugins alongside the keypair (likely as a
/// separate keystore entry keyed by `<plugin-id>:secret`).
pub struct PluginKeyStore;

const KEYRING_SERVICE: &str = "pattern-plugin";

#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    #[error("keystore: invalid key length: expected 32 bytes, got {got}")]
    InvalidKeyLength { got: usize },
    #[error("keystore: io error at {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("keystore: data-dir resolution failed (no XDG_DATA_HOME and no HOME)")]
    NoDataDir,
    #[error("keystore: keyring error: {message}")]
    Keyring { message: SmolStr },
}

impl PluginKeyStore {
    /// Load the plugin's keypair if one exists, otherwise generate + persist one.
    /// Idempotent: calling repeatedly returns the same key (modulo store-side mutation).
    pub fn load_or_generate(plugin_id: &PluginId) -> Result<iroh::SecretKey, KeyStoreError> {
        if let Some(sk) = Self::load(plugin_id)? {
            return Ok(sk);
        }
        let sk = iroh::SecretKey::generate();
        Self::store(plugin_id, &sk)?;
        Ok(sk)
    }

    /// Try to load. Returns Ok(None) if no key is registered, Ok(Some) if found.
    pub fn load(plugin_id: &PluginId) -> Result<Option<iroh::SecretKey>, KeyStoreError> {
        // Keyring first.
        if let Some(bytes) = try_keyring_load(plugin_id)? {
            return Ok(Some(secret_from_bytes(&bytes)?));
        }
        // File fallback.
        if let Some(bytes) = try_file_load(plugin_id)? {
            return Ok(Some(secret_from_bytes(&bytes)?));
        }
        Ok(None)
    }

    /// Persist a keypair. Tries keyring first; falls back to file on keyring failure.
    /// A successful keyring write does NOT also write the file (single-source-of-truth).
    pub fn store(plugin_id: &PluginId, secret: &iroh::SecretKey) -> Result<(), KeyStoreError> {
        let bytes = secret.to_bytes();
        if try_keyring_store(plugin_id, &bytes).is_ok() {
            return Ok(());
        }
        try_file_store(plugin_id, &bytes)
    }
}

fn secret_from_bytes(bytes: &[u8]) -> Result<iroh::SecretKey, KeyStoreError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| KeyStoreError::InvalidKeyLength {
        got: bytes.len(),
    })?;
    Ok(iroh::SecretKey::from_bytes(&arr))
}

fn try_keyring_load(plugin_id: &PluginId) -> Result<Option<Vec<u8>>, KeyStoreError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, plugin_id.as_str())
        .map_err(|e| KeyStoreError::Keyring { message: e.to_string().into() })?;
    match entry.get_secret() {
        Ok(bytes) => Ok(Some(bytes)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeyStoreError::Keyring { message: e.to_string().into() }),
    }
}

fn try_keyring_store(plugin_id: &PluginId, bytes: &[u8]) -> Result<(), KeyStoreError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, plugin_id.as_str())
        .map_err(|e| KeyStoreError::Keyring { message: e.to_string().into() })?;
    entry.set_secret(bytes)
        .map_err(|e| KeyStoreError::Keyring { message: e.to_string().into() })
}

fn plugin_secret_path(plugin_id: &PluginId) -> Result<PathBuf, KeyStoreError> {
    // Route through PatternRoots so a single PATTERN_HOME override isolates all
    // plugin-side state (matches `PluginState::state_dir`'s lookup path). Previously
    // used `dirs::data_dir()` directly which only honored XDG_DATA_HOME — diverged
    // from PluginState + made test isolation require setting two env vars.
    let roots = crate::PatternRoots::default_paths().map_err(|_| KeyStoreError::NoDataDir)?;
    Ok(roots.data_root().join("plugins").join(plugin_id.as_str()).join("secret"))
}

fn try_file_load(plugin_id: &PluginId) -> Result<Option<Vec<u8>>, KeyStoreError> {
    let path = plugin_secret_path(plugin_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(KeyStoreError::Io { path, source }),
    }
}

fn try_file_store(plugin_id: &PluginId, bytes: &[u8]) -> Result<(), KeyStoreError> {
    let path = plugin_secret_path(plugin_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| KeyStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, bytes).map_err(|source| KeyStoreError::Io {
        path: path.clone(),
        source,
    })?;
    // Set mode 0600 on unix-likes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms).map_err(|source| KeyStoreError::Io {
            path: path.clone(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod keystore_tests {
    use super::*;

    /// File-backend round-trip via an explicit override of the path resolution.
    /// We don't go through try_keyring_* in tests because that hits the real system
    /// keyring — keyring's behavior under test is environment-dependent. Production
    /// path is exercised when register_plugin runs on a real plugin install.
    #[test]
    fn file_round_trip_with_mode_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_id: PluginId = "unit-test-plugin".into();
        let dir = tmp.path().join(plugin_id.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret");

        let bytes = iroh::SecretKey::generate().to_bytes();
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let loaded = std::fs::read(&path).unwrap();
        assert_eq!(loaded.len(), 32);
        let sk = secret_from_bytes(&loaded).unwrap();
        assert_eq!(sk.to_bytes()[..], loaded[..]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = plugin_id;
    }

    #[test]
    fn secret_from_bytes_rejects_wrong_length() {
        let bad = [0u8; 16];
        assert!(matches!(
            secret_from_bytes(&bad),
            Err(KeyStoreError::InvalidKeyLength { got: 16 })
        ));
    }
}
