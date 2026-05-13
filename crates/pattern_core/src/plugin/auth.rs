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

// ─── Session-aware routing (replaces AllowList for live daemon use) ─────

/// Identity of a session that owns a plugin route entry. Opaque to the auth layer;
/// surfaced in logs + diagnostics so cross-session leakage is visible.
pub type RouteSessionId = SmolStr;

/// A single entry in [`PluginRouteTable`]: which session has this plugin enabled.
#[derive(Debug, Clone)]
pub struct PluginRouteEntry {
    pub plugin_id: PluginId,
    pub session_id: RouteSessionId,
}

/// Live, mutable map from plugin pubkey → owning session. Daemon holds one shared
/// `Arc<PluginRouteTable>`; sessions register their plugins on open + unregister on close.
/// The session-routing protocol handler consults this on every incoming connection.
///
/// Why per-session instead of daemon-wide AllowList: plugin trust is project-scoped.
/// A plugin enabled in session A shouldn't be reachable from session B if B doesn't
/// enable it. The canonical case: discord plugin enabled in one project but not another.
#[derive(Debug, Default)]
pub struct PluginRouteTable {
    /// Pubkey can be claimed by multiple sessions (e.g. two project mounts both
    /// enabling the same plugin). Per-session entries keyed under one pubkey.
    /// SessionRoutingProtocolHandler dispatches to the first match — all entries
    /// for a given pubkey trust the same plugin, so any session can handle the
    /// incoming connection from the auth perspective.
    routes: dashmap::DashMap<iroh::PublicKey, Vec<PluginRouteEntry>>,
}

/// Error registering a plugin route. Mismatched plugin_id under the same pubkey
/// indicates two sessions disagree about which plugin this pubkey represents — that's
/// a real bug (the pubkey IS the plugin's identity), not a multi-session-routing case.
#[derive(Debug, thiserror::Error)]
pub enum PluginRouteError {
    #[error(
        "pubkey already claimed under plugin {existing_plugin} by session {existing_session}; \
         cannot register under different plugin {new_plugin} for session {new_session}"
    )]
    PluginIdMismatch {
        existing_plugin: PluginId,
        existing_session: RouteSessionId,
        new_plugin: PluginId,
        new_session: RouteSessionId,
    },
}

impl PluginRouteTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin's pubkey under the given session. Multiple sessions may
    /// claim the same pubkey — they're all valid routing targets for incoming
    /// connections (any one of them can handle the plugin). Idempotent for the
    /// same (pubkey, plugin_id, session_id) triple. Errors only on plugin_id
    /// mismatch (which is a real bug: same pubkey, different plugin identity).
    pub fn register(
        &self,
        pubkey: iroh::PublicKey,
        plugin_id: PluginId,
        session_id: RouteSessionId,
    ) -> Result<(), PluginRouteError> {
        let mut entries = self.routes.entry(pubkey).or_default();
        // Same-session idempotent re-register.
        if entries.iter().any(|e| e.session_id == session_id && e.plugin_id == plugin_id) {
            return Ok(());
        }
        // Sanity check: all entries under this pubkey must claim the same plugin_id.
        if let Some(other) = entries.iter().find(|e| e.plugin_id != plugin_id) {
            return Err(PluginRouteError::PluginIdMismatch {
                existing_plugin: other.plugin_id.clone(),
                existing_session: other.session_id.clone(),
                new_plugin: plugin_id,
                new_session: session_id,
            });
        }
        entries.push(PluginRouteEntry { plugin_id, session_id });
        Ok(())
    }

    /// Remove all entries for `pubkey` regardless of session. Returns the prior
    /// entries if any. Use with care — usually you want `unregister_session` or
    /// per-(pubkey, session) removal instead.
    pub fn unregister_all(&self, pubkey: &iroh::PublicKey) -> Vec<PluginRouteEntry> {
        self.routes.remove(pubkey).map(|(_, v)| v).unwrap_or_default()
    }

    /// Remove all routes belonging to `session_id` across all pubkeys. Used at
    /// session close. Returns the count of removed entries (for logging).
    pub fn unregister_session(&self, session_id: &str) -> usize {
        let mut removed = 0;
        // Walk each pubkey's Vec, retaining entries that don't belong to this session.
        // Drop the whole entry if its Vec becomes empty.
        self.routes.retain(|_, entries| {
            let before = entries.len();
            entries.retain(|e| e.session_id != session_id);
            removed += before - entries.len();
            !entries.is_empty()
        });
        removed
    }

    /// Look up the first session that owns this pubkey. SessionRoutingProtocolHandler
    /// dispatches to this one. Returning None means no session has registered this
    /// pubkey — reject the connection.
    pub fn lookup(&self, pubkey: &iroh::PublicKey) -> Option<PluginRouteEntry> {
        self.routes.get(pubkey).and_then(|r| r.first().cloned())
    }

    /// All sessions claiming this pubkey. Useful for diagnostics or future
    /// load-balancing across sessions.
    pub fn lookup_all(&self, pubkey: &iroh::PublicKey) -> Vec<PluginRouteEntry> {
        self.routes.get(pubkey).map(|r| r.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// iroh `ProtocolHandler` wrapper that consults [`PluginRouteTable`] at accept-time.
/// Replacement for [`AuthGatedProtocolHandler`] when the allow-list is session-scoped
/// rather than static. V1 dispatches all allowed connections to the same inner handler;
/// later phases will route to per-session host handlers via the route entry's session_id.
#[derive(Debug, Clone)]
pub struct SessionRoutingProtocolHandler<H> {
    routes: Arc<PluginRouteTable>,
    inner: H,
}

impl<H> SessionRoutingProtocolHandler<H> {
    pub fn new(routes: Arc<PluginRouteTable>, inner: H) -> Self {
        Self { routes, inner }
    }

    pub fn routes(&self) -> &PluginRouteTable {
        &self.routes
    }
}

impl<H> ProtocolHandler for SessionRoutingProtocolHandler<H>
where
    H: ProtocolHandler,
{
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        match self.routes.lookup(&remote) {
            Some(entry) => {
                tracing::debug!(
                    plugin_id = %entry.plugin_id,
                    session_id = %entry.session_id,
                    remote = %remote,
                    "plugin route: allowed"
                );
                self.inner.accept(conn).await
            }
            None => {
                tracing::warn!(
                    remote = %remote,
                    "plugin route: rejected (no session has this pubkey registered)"
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
        // Keyring first, unless PATTERN_KEYSTORE_FILE_ONLY is set (test isolation:
        // keyring access can differ between parent test process + spawned plugin
        // subprocess, producing different keys for the same plugin_id; forcing file-only
        // makes both processes share the same PATTERN_HOME-scoped path deterministically).
        if !file_only_mode() {
            if let Some(bytes) = try_keyring_load(plugin_id)? {
                return Ok(Some(secret_from_bytes(&bytes)?));
            }
        }
        // File fallback (or primary path when file-only).
        if let Some(bytes) = try_file_load(plugin_id)? {
            return Ok(Some(secret_from_bytes(&bytes)?));
        }
        Ok(None)
    }

    /// Persist a keypair. Tries keyring first; falls back to file on keyring failure.
    /// A successful keyring write does NOT also write the file (single-source-of-truth).
    /// `PATTERN_KEYSTORE_FILE_ONLY` env var forces file-only (test isolation).
    pub fn store(plugin_id: &PluginId, secret: &iroh::SecretKey) -> Result<(), KeyStoreError> {
        let bytes = secret.to_bytes();
        if !file_only_mode() && try_keyring_store(plugin_id, &bytes).is_ok() {
            return Ok(());
        }
        try_file_store(plugin_id, &bytes)
    }

    /// Test-only file path inspection. Returns the resolved keystore file path
    /// for the given plugin id; doesn't read or write.
    pub fn file_path_for_testing(plugin_id: &PluginId) -> Result<PathBuf, KeyStoreError> {
        plugin_secret_path(plugin_id)
    }
}

fn file_only_mode() -> bool {
    std::env::var_os("PATTERN_KEYSTORE_FILE_ONLY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
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

#[cfg(test)]
mod route_table_tests {
    use super::*;

    fn mkpk() -> iroh::PublicKey {
        iroh::SecretKey::generate().public()
    }

    #[test]
    fn register_and_lookup_single() {
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        let entry = table.lookup(&pk).unwrap();
        assert_eq!(entry.plugin_id.as_str(), "plug-a");
        assert_eq!(entry.session_id.as_str(), "sess-1");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn same_session_same_plugin_is_idempotent() {
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        assert_eq!(table.lookup_all(&pk).len(), 1);
    }

    #[test]
    fn multi_session_same_plugin_allowed() {
        // Two sessions both enabling the same plugin (e.g. two project mounts).
        // Both routes are kept; lookup returns the first; lookup_all returns both.
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        table.register(pk, "plug-a".into(), "sess-2".into()).unwrap();
        let all = table.lookup_all(&pk);
        assert_eq!(all.len(), 2);
        let sessions: std::collections::HashSet<_> =
            all.iter().map(|e| e.session_id.as_str()).collect();
        assert!(sessions.contains("sess-1"));
        assert!(sessions.contains("sess-2"));
    }

    #[test]
    fn plugin_id_mismatch_rejected() {
        // Same pubkey, two different plugin_ids = real bug (pubkey IS plugin identity).
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        let err = table.register(pk, "plug-b".into(), "sess-2".into()).unwrap_err();
        assert!(matches!(err, PluginRouteError::PluginIdMismatch { .. }));
    }

    #[test]
    fn unregister_session_removes_only_that_sessions_entries() {
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        table.register(pk, "plug-a".into(), "sess-2".into()).unwrap();
        let removed = table.unregister_session("sess-1");
        assert_eq!(removed, 1);
        let remaining = table.lookup_all(&pk);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].session_id.as_str(), "sess-2");
    }

    #[test]
    fn unregister_session_drops_empty_pubkey_entry() {
        // Last session for a pubkey unregisters → the pubkey entry should be
        // removed entirely so the next registrant doesn't see a stale empty Vec.
        let table = PluginRouteTable::new();
        let pk = mkpk();
        table.register(pk, "plug-a".into(), "sess-1".into()).unwrap();
        assert_eq!(table.len(), 1);
        table.unregister_session("sess-1");
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
    }

    #[test]
    fn lookup_miss_returns_none() {
        let table = PluginRouteTable::new();
        assert!(table.lookup(&mkpk()).is_none());
    }
}

