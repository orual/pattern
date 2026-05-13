//! Plugin registry: discovery, pin state, install/uninstall.
//!
//! Uses `PatternPaths` for directory resolution and `knus` for KDL
//! registry file persistence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use pattern_core::CapabilitySet;
use pattern_core::plugin::PluginId;
use pattern_core::plugin::manifest::PluginManifest;
use pattern_core::plugin::scope::PluginScope;

use pattern_core::plugin::RegistryError;

/// A plugin loaded into the registry with its resolved scope and config.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// The plugin's identifier.
    pub id: PluginId,
    /// Where this plugin was discovered/pinned.
    pub scope: PluginScope,
    /// Path to the plugin's directory on disk.
    pub source_path: PathBuf,
    /// The parsed manifest.
    pub manifest: PluginManifest,
    /// User-level configuration values.
    pub user_config: serde_json::Value,
    /// Capability overrides from the registry.
    pub capability_overrides: Option<CapabilitySet>,
    /// The plugin's transport-agnostic connection. In-process variants
    /// wrap a `PluginExtension`; out-of-process (Task 5) use IRPC/QUIC.
    pub connection: Option<std::sync::Arc<dyn crate::plugin::transport::PluginConnection>>,
    /// Plugin → runtime callback host. None for CC plugins (no callbacks).
    pub host: Option<std::sync::Arc<dyn pattern_core::traits::plugin::HostApi>>,
    /// Parsed plugin key from registry.kdl. `None` for ambient/legacy entries
    /// (no pubkey field) and freshly-installed plugins (pubkey gets written
    /// after plugin's first run). Drives session-open route-table population.
    pub plugin_key: Option<pattern_core::plugin::auth::PluginKey>,
}

// ---- KDL persistence types (knus::Decode) -----------------------------------

/// A single plugin installation entry in a registry KDL file.
#[derive(Debug, Clone, knus::Decode)]
pub struct PluginInstallation {
    #[knus(argument)]
    pub id: String,
    #[knus(child, unwrap(argument), default)]
    pub source: Option<String>,
    #[knus(child, unwrap(argument), default)]
    pub installed_at: Option<String>,
    /// Localhost-case: iroh public key in base32 (`PublicKey::to_string()` shape).
    /// Daemon allow-lists the plugin by this pubkey at startup.
    #[knus(child, unwrap(argument), default)]
    pub pubkey: Option<String>,
    /// Atproto-case (phase 7): AT-URI pointing at an atproto record that publishes the pubkey.
    /// Resolved at daemon startup via DID-doc lookup. V1 errors at allow-list build.
    #[knus(child, unwrap(argument), default)]
    pub pubkey_uri: Option<String>,
    /// Atproto-case (phase 7): CID pinning the specific version of the pubkey record.
    /// Required alongside `pubkey_uri`.
    #[knus(child, unwrap(argument), default)]
    pub pubkey_cid: Option<String>,
    #[knus(child)]
    pub user_config: Option<UserConfigBlock>,
    #[knus(child)]
    pub capability_override: Option<CapabilitiesBlock>,
}

impl PluginInstallation {
    /// Resolve the plugin's registered key, if any.
    ///
    /// Returns:
    /// - `Ok(None)` if no auth fields are set (legacy entries, will be rejected at connection time)
    /// - `Ok(Some(PluginKey::Direct(_)))` if `pubkey` parses as a valid iroh pubkey
    /// - `Ok(Some(PluginKey::Atproto { .. }))` if `pubkey_uri` + `pubkey_cid` both set
    /// - `Err` if a field is malformed (e.g. invalid pubkey encoding, uri without cid)
    pub fn plugin_key(
        &self,
    ) -> Result<Option<pattern_core::plugin::auth::PluginKey>, PluginKeyParseError> {
        use pattern_core::plugin::auth::PluginKey;
        match (&self.pubkey, &self.pubkey_uri, &self.pubkey_cid) {
            (Some(s), None, None) => {
                let pk = s.parse::<iroh::PublicKey>().map_err(|e| {
                    PluginKeyParseError::InvalidPubkey {
                        plugin_id: self.id.clone().into(),
                        message: e.to_string().into(),
                    }
                })?;
                Ok(Some(PluginKey::Direct(pk)))
            }
            (None, Some(uri), Some(cid)) => Ok(Some(PluginKey::Atproto {
                uri: uri.clone().into(),
                cid: cid.clone().into(),
            })),
            (None, None, None) => Ok(None),
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                Err(PluginKeyParseError::ConflictingAuth {
                    plugin_id: self.id.clone().into(),
                })
            }
            (None, Some(_), None) | (None, None, Some(_)) => {
                Err(PluginKeyParseError::AtprotoIncomplete {
                    plugin_id: self.id.clone().into(),
                })
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginKeyParseError {
    #[error("plugin {plugin_id}: invalid pubkey encoding: {message}")]
    InvalidPubkey { plugin_id: smol_str::SmolStr, message: smol_str::SmolStr },
    #[error("plugin {plugin_id}: conflicting auth fields (both `pubkey` and atproto form set)")]
    ConflictingAuth { plugin_id: smol_str::SmolStr },
    #[error("plugin {plugin_id}: atproto auth requires both `pubkey-uri` and `pubkey-cid`")]
    AtprotoIncomplete { plugin_id: smol_str::SmolStr },
}

/// User-configurable values from the registry KDL.
#[derive(Debug, Clone, knus::Decode)]
pub struct UserConfigBlock {
    #[knus(children)]
    pub entries: Vec<UserConfigEntry>,
}

/// A single user config key-value entry.
#[derive(Debug, Clone, knus::Decode)]
pub struct UserConfigEntry {
    #[knus(node_name)]
    pub key: String,
    #[knus(argument)]
    pub value: String,
}

/// Capability override block from the registry KDL.
#[derive(Debug, Clone, knus::Decode)]
pub struct CapabilitiesBlock {
    #[knus(children)]
    pub effects: Vec<EffectEntry>,
}

/// A single effect category entry.
#[derive(Debug, Clone, knus::Decode)]
pub struct EffectEntry {
    #[knus(node_name)]
    pub name: String,
}

/// Top-level registry file structure.
#[derive(Debug, Clone, knus::Decode)]
pub struct RegistryFile {
    #[knus(children(name = "plugin"))]
    pub plugins: Vec<PluginInstallation>,
}

/// Hook emission callback seam. Phase 1: no-op. Phase 2: wired to real bus.
pub type HookEmitter = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Registry of all discovered and pinned plugins.
pub struct PluginRegistry {
    paths: Arc<pattern_memory::paths::PatternPaths>,
    mount_path: Option<PathBuf>,
    inner: RwLock<HashMap<PluginId, LoadedPlugin>>,
    hook_emit: HookEmitter,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("mount_path", &self.mount_path)
            .field("plugin_count", &self.inner.read().len())
            .finish_non_exhaustive()
    }
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new(
        paths: Arc<pattern_memory::paths::PatternPaths>,
        mount_path: Option<PathBuf>,
    ) -> Self {
        Self {
            paths,
            mount_path,
            inner: RwLock::new(HashMap::new()),
            hook_emit: Box::new(|_, _| {}),
        }
    }

    /// Swap the hook emitter (Phase 2 integration point).
    pub fn with_hook_emitter(mut self, emitter: HookEmitter) -> Self {
        self.hook_emit = emitter;
        self
    }

    /// Number of loaded plugins.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a loaded plugin by id.
    pub fn get(&self, id: &str) -> Option<LoadedPlugin> {
        self.inner.read().get(id).cloned()
    }

    /// List all loaded plugins.
    pub fn list(&self) -> Vec<LoadedPlugin> {
        self.inner.read().values().cloned().collect()
    }

    /// Pubkey-routable plugins in this registry: returns `(plugin_id, pubkey)` for
    /// each loaded plugin whose registry entry has a parsed `PluginKey::Direct(_)`.
    /// Atproto-keyed plugins (phase 7) are skipped — their resolution path isn't
    /// wired yet. Plugins with no `plugin_key` (ambient / legacy / freshly-installed)
    /// are also skipped; only OOP plugins with a pinned pubkey go through the
    /// session-aware route table.
    pub fn routable_pubkeys(&self) -> Vec<(PluginId, iroh::PublicKey)> {
        self.inner
            .read()
            .values()
            .filter_map(|lp| match lp.plugin_key.as_ref()? {
                pattern_core::plugin::auth::PluginKey::Direct(pk) => {
                    Some((lp.id.clone(), *pk))
                }
                pattern_core::plugin::auth::PluginKey::Atproto { .. } => None,
            })
            .collect()
    }

    /// Read and parse a registry KDL file.
    pub fn read_registry_file(path: &Path) -> Result<Option<RegistryFile>, RegistryError> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path).map_err(|source| RegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file: RegistryFile =
            knus::parse("<registry>", &raw).map_err(|e| RegistryError::Kdl {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(Some(file))
    }

    /// Insert a plugin into the in-memory registry.
    pub fn insert(&self, plugin: LoadedPlugin) {
        let id = plugin.id.clone();
        self.inner.write().insert(id.clone(), plugin);
        (self.hook_emit)("plugin.registered", serde_json::json!({ "id": id }));
    }

    /// Remove a plugin from the in-memory registry.
    pub fn remove(&self, id: &str) -> Option<LoadedPlugin> {
        let removed = self.inner.write().remove(id);
        if removed.is_some() {
            (self.hook_emit)("plugin.unregistered", serde_json::json!({ "id": id }));
        }
        removed
    }

    /// Build the registry by discovering plugins across all three scopes.
    /// Precedence: Project > Global > Ambient (last write wins).
    pub fn load(
        paths: Arc<pattern_memory::paths::PatternPaths>,
        mount_path: Option<PathBuf>,
    ) -> Result<Self, RegistryError> {
        let mut combined: HashMap<PluginId, LoadedPlugin> = HashMap::new();

        // 1. Ambient (lowest precedence): directories under <base>/plugins/
        let global_root = paths.plugins_global_root();
        if global_root.is_dir() {
            for entry in scan_plugin_dirs(&global_root)? {
                if let Ok(manifest) = load_manifest_from_dir(&entry) {
                    let conn = build_connection(&manifest, &entry);
                    let lp = LoadedPlugin {
                        id: manifest.name.clone(),
                        scope: PluginScope::Ambient,
                        source_path: entry,
                        manifest,
                        user_config: serde_json::Value::Null,
                        capability_overrides: None,
                        connection: conn,
                        host: None,
                        plugin_key: None, // ambient = unauthed by design
                    };
                    combined.insert(lp.id.clone(), lp);
                }
            }
        }

        // 2. Global pins: ~/.pattern/plugins/registry.kdl
        if let Some(file) = Self::read_registry_file(&paths.plugins_global_registry())? {
            for inst in file.plugins {
                let plugin_dir = paths.plugin_cache_dir(&inst.id);
                if let Ok(manifest) = load_manifest_from_dir(&plugin_dir) {
                    let lp = build_loaded_from_installation(
                        inst,
                        manifest,
                        PluginScope::Global,
                        &plugin_dir,
                    );
                    if let Some(prev) = combined.insert(lp.id.clone(), lp) {
                        tracing::warn!(
                            plugin_id = %prev.id,
                            prev_scope = ?prev.scope,
                            new_scope = ?PluginScope::Global,
                            "plugin override: global pin shadows ambient"
                        );
                    }
                }
            }
        }

        // 3. Project pins: shared then private.
        if let Some(mp) = &mount_path {
            for private in [false, true] {
                let reg_path = pattern_memory::paths::project_plugin_registry(mp, private);
                if let Some(file) = Self::read_registry_file(&reg_path)? {
                    for inst in file.plugins {
                        // Project plugins may live in the project dir or the cache.
                        let plugin_dir = if let Some(ref src) = inst.source {
                            PathBuf::from(src)
                        } else {
                            paths.plugin_cache_dir(&inst.id)
                        };
                        if let Ok(manifest) = load_manifest_from_dir(&plugin_dir) {
                            let scope = PluginScope::Project { private };
                            let lp =
                                build_loaded_from_installation(inst, manifest, scope, &plugin_dir);
                            if let Some(prev) = combined.insert(lp.id.clone(), lp) {
                                tracing::warn!(
                                    plugin_id = %prev.id,
                                    prev_scope = ?prev.scope,
                                    new_scope = ?scope,
                                    "plugin override: project pin shadows lower-precedence"
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(Self {
            paths,
            mount_path,
            inner: RwLock::new(combined),
            hook_emit: Box::new(|_, _| {}),
        })
    }

    /// Install a plugin from a local path or git URL into the given scope.
    pub fn install(
        &self,
        source: InstallSource<'_>,
        scope: PluginScope,
    ) -> Result<LoadedPlugin, RegistryError> {
        let dest = match &source {
            InstallSource::LocalPath(path) => {
                // Read the manifest from the source path directly.
                let manifest = load_manifest_from_dir(path).map_err(|e| RegistryError::Io {
                    path: path.to_path_buf(),
                    source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                })?;
                let cache_dir = self.paths.plugin_cache_dir(&manifest.name);
                if !cache_dir.exists() {
                    // Copy the plugin directory to the cache.
                    copy_dir_recursive(path, &cache_dir)?;
                }
                cache_dir
            }
            InstallSource::JjGitUrl(url) => {
                // For now, use a simple git clone to the cache dir.
                // TODO: wire through JjAdapter when available.
                let temp_id = url.rsplit('/').next().unwrap_or("plugin");
                let temp_id = temp_id.trim_end_matches(".git");
                let cache_dir = self.paths.plugin_cache_dir(temp_id);
                if cache_dir.exists() {
                    return Err(RegistryError::DestinationExists(cache_dir));
                }
                // Shell out to git clone as a fallback.
                let status = std::process::Command::new("git")
                    .args(["clone", url, &cache_dir.to_string_lossy()])
                    .status()
                    .map_err(|e| RegistryError::Io {
                        path: cache_dir.clone(),
                        source: e,
                    })?;
                if !status.success() {
                    return Err(RegistryError::Io {
                        path: cache_dir.clone(),
                        source: std::io::Error::new(std::io::ErrorKind::Other, "git clone failed"),
                    });
                }
                cache_dir
            }
        };

        let manifest = load_manifest_from_dir(&dest).map_err(|e| RegistryError::Io {
            path: dest.clone(),
            source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
        })?;

        let lp = LoadedPlugin {
            id: manifest.name.clone(),
            scope,
            source_path: dest.clone(),
            manifest: manifest.clone(),
            user_config: serde_json::Value::Null,
            capability_overrides: None,
            connection: build_connection(&manifest, &dest),
            host: None,
            plugin_key: None, // newly installed; pubkey lands after plugin's first run
        };

        self.insert(lp.clone());

        // Native plugin steps: cargo build (or validate prebuilt), then run
        // --pattern-plugin-init to extract pubkey. Updates the LoadedPlugin
        // with plugin_key set so persist_installation writes the pubkey.
        let lp = match self.install_native_steps(&lp) {
            Ok(updated) => {
                if updated.plugin_key.is_some() {
                    // Replace cached LoadedPlugin with the updated one carrying the pubkey.
                    self.insert(updated.clone());
                }
                updated
            }
            Err(e) => {
                tracing::warn!(plugin = %lp.id, error = %e, "native install steps failed — registering as non-routable");
                lp
            }
        };

        // Persist the installation to the registry KDL file for the scope.
        self.persist_installation(&lp)?;
        Ok(lp)
    }

    /// Native-plugin post-install steps. Cargo build (if `manifest.build`),
    /// invoke `--pattern-plugin-init` to extract pubkey via PluginKeyStore.
    /// Returns the LoadedPlugin updated with `plugin_key` set (or unchanged if
    /// the source isn't a native Rust plugin).
    fn install_native_steps(&self, lp: &LoadedPlugin) -> Result<LoadedPlugin, RegistryError> {
        // Not a native rust plugin if there's no Cargo.toml at the source root.
        if !lp.source_path.join("Cargo.toml").exists() {
            return Ok(lp.clone());
        }

        let bin_name = if cfg!(target_os = "windows") {
            format!("{}.exe", lp.id)
        } else {
            lp.id.to_string()
        };
        let bin_dir = lp.source_path.join("bin");
        let bin_path = bin_dir.join(&bin_name);

        // build = true (default): cargo build --release in source_path; copy
        //     `target/release/<crate-or-bin>` into `bin/<plugin-id>[.exe]`.
        // build = false: expect prebuilt at `bin/<plugin-id>[.exe]`, error if missing.
        if lp.manifest.build {
            tracing::info!(plugin = %lp.id, "running cargo build --release");
            let output = std::process::Command::new("cargo")
                .args(["build", "--release"])
                .current_dir(&lp.source_path)
                .output()
                .map_err(|source| RegistryError::Io {
                    path: lp.source_path.clone(),
                    source,
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(RegistryError::Io {
                    path: lp.source_path.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("cargo build failed: {stderr}"),
                    ),
                });
            }
            // Find binary: try target/release/<plugin-id> first, then target/release/<plugin-id>[.exe].
            // crate name = manifest name for the simple case; complex workspace TBD.
            let target_bin = lp.source_path.join("target").join("release").join(&bin_name);
            let target_bin_no_ext = lp.source_path.join("target").join("release").join(lp.id.as_str());
            let src_bin = if target_bin.exists() { target_bin }
                else if target_bin_no_ext.exists() { target_bin_no_ext }
                else {
                    return Err(RegistryError::Io {
                        path: lp.source_path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("built binary not found — expected target/release/{} (no manifest binary-name override yet)", bin_name),
                        ),
                    });
                };
            std::fs::create_dir_all(&bin_dir).map_err(|source| RegistryError::Io { path: bin_dir.clone(), source })?;
            std::fs::copy(&src_bin, &bin_path).map_err(|source| RegistryError::Io { path: bin_path.clone(), source })?;
        } else if !bin_path.exists() {
            return Err(RegistryError::Io {
                path: bin_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("build = false but no prebuilt binary at {}", bin_path.display()),
                ),
            });
        }

        // Invoke `<bin> --pattern-plugin-init` to generate-or-load keypair + print JSON.
        tracing::info!(plugin = %lp.id, "running --pattern-plugin-init to extract pubkey");
        let output = std::process::Command::new(&bin_path)
            .arg("--pattern-plugin-init")
            .output()
            .map_err(|source| RegistryError::Io { path: bin_path.clone(), source })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RegistryError::Io {
                path: bin_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("--pattern-plugin-init failed: {stderr}"),
                ),
            });
        }
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| RegistryError::Io {
                path: bin_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("--pattern-plugin-init stdout not valid JSON: {e}"),
                ),
            })?;
        let pubkey_str = json["pubkey"].as_str().ok_or_else(|| RegistryError::Io {
            path: bin_path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::Other,
                "--pattern-plugin-init JSON missing pubkey field",
            ),
        })?;
        let pubkey: iroh::PublicKey = pubkey_str.parse().map_err(|e: <iroh::PublicKey as std::str::FromStr>::Err| RegistryError::Io {
            path: bin_path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("--pattern-plugin-init returned invalid pubkey: {e}"),
            ),
        })?;

        let mut updated = lp.clone();
        updated.plugin_key = Some(pattern_core::plugin::auth::PluginKey::Direct(pubkey));
        Ok(updated)
    }

    /// Persist a plugin installation to the appropriate registry KDL file.
    fn persist_installation(&self, plugin: &LoadedPlugin) -> Result<(), RegistryError> {
        let reg_path = match plugin.scope {
            PluginScope::Ambient => return Ok(()), // Ambient is discovery-only.
            PluginScope::Global => self.paths.plugins_global_registry(),
            PluginScope::Project { private } => {
                let mp = self.mount_path.as_ref().ok_or(RegistryError::NoCacheDir)?;
                pattern_memory::paths::project_plugin_registry(mp, private)
            }
            _ => return Ok(()), // Future scope variants: no-op for now.
        };

        // Read existing content (if any) and append the new entry.
        let mut content = if reg_path.exists() {
            std::fs::read_to_string(&reg_path).unwrap_or_default()
        } else {
            if let Some(parent) = reg_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| RegistryError::Io {
                    path: reg_path.clone(),
                    source,
                })?;
            }
            String::new()
        };

        // Append a plugin entry. Includes pubkey line for native (OOP)
        // plugins so SessionRoutingProtocolHandler can dispatch incoming
        // connections to the right plugin at iroh accept time.
        let ts = jiff::Timestamp::now();
        let pubkey_line = match &plugin.plugin_key {
            Some(pattern_core::plugin::auth::PluginKey::Direct(pk)) => {
                format!("    pubkey \"{}\"\n", pk)
            }
            _ => String::new(),
        };
        content.push_str(&format!(
            "\nplugin \"{}\" {{\n    source \"{}\"\n    installed-at \"{}\"\n{}}}\n",
            plugin.id,
            plugin.source_path.display(),
            ts,
            pubkey_line,
        ));

        std::fs::write(&reg_path, &content).map_err(|source| RegistryError::Io {
            path: reg_path,
            source,
        })?;

        Ok(())
    }

    /// Uninstall a plugin by id. Removes from registry, removes from
    /// persisted KDL, and optionally cleans the cache directory.
    pub fn uninstall(&self, id: &str, clean_cache: bool) -> Result<(), RegistryError> {
        let removed = self
            .remove(id)
            .ok_or_else(|| RegistryError::NotFound { id: id.into() })?;
        // Remove from persisted registry KDL.
        self.remove_from_persisted_registry(id, removed.scope)?;
        if clean_cache && removed.source_path.exists() {
            let _ = std::fs::remove_dir_all(&removed.source_path);
        }
        Ok(())
    }

    /// Remove a plugin entry from the persisted registry KDL file.
    fn remove_from_persisted_registry(
        &self,
        id: &str,
        scope: PluginScope,
    ) -> Result<(), RegistryError> {
        let reg_path = match scope {
            PluginScope::Ambient => return Ok(()),
            PluginScope::Global => self.paths.plugins_global_registry(),
            PluginScope::Project { private } => {
                let mp = self.mount_path.as_ref().ok_or(RegistryError::NoCacheDir)?;
                pattern_memory::paths::project_plugin_registry(mp, private)
            }
            _ => return Ok(()),
        };
        if !reg_path.exists() {
            return Ok(());
        }
        // Read, filter out the plugin's entry, rewrite.
        // Simple approach: parse with knus, filter, re-serialize.
        // For now, use string-based removal (find the plugin block and remove it).
        let content = std::fs::read_to_string(&reg_path).map_err(|source| RegistryError::Io {
            path: reg_path.clone(),
            source,
        })?;
        // Remove the block `plugin "<id>" { ... }`
        let pattern = format!("plugin \"{}\" {{", id);
        if let Some(start) = content.find(&pattern) {
            // Find the matching closing brace.
            let rest = &content[start..];
            if let Some(end_offset) = rest.find("\n}\n") {
                let end = start + end_offset + 3; // include the closing }\n
                let mut new_content = String::new();
                new_content.push_str(&content[..start]);
                new_content.push_str(&content[end..]);
                std::fs::write(&reg_path, new_content.trim()).map_err(|source| {
                    RegistryError::Io {
                        path: reg_path,
                        source,
                    }
                })?;
            }
        }
        Ok(())
    }
}

/// Build the appropriate PluginConnection based on manifest source format.
/// In-process variants wrap a PluginExtension via InProcessPluginConnection.
/// Out-of-process native plugins get OutOfProcessPluginConnection (Task 5).
fn build_connection(
    manifest: &pattern_core::plugin::manifest::PluginManifest,
    source_path: &std::path::Path,
) -> Option<std::sync::Arc<dyn crate::plugin::transport::PluginConnection>> {
    if manifest.cc.is_some() {
        let ext = super::cc_adapter::CcPluginAdapter::wrap(
            manifest.name.clone(),
            source_path.to_path_buf(),
            manifest.clone(),
        );
        Some(std::sync::Arc::new(
            crate::plugin::transport::InProcessPluginConnection::new(ext, manifest.name.clone()),
        ))
    } else {
        // Native IRPC plugins get their connection in Phase 6 Task 5.
        None
    }
}

/// Source for plugin installation.
pub enum InstallSource<'a> {
    /// Install from a local directory path.
    LocalPath(&'a Path),
    /// Clone from a git URL (via jj or plain git).
    JjGitUrl(&'a str),
}

// ---- Helper functions -------------------------------------------------------

/// Scan a directory for plugin subdirectories that contain a manifest.
fn scan_plugin_dirs(root: &Path) -> Result<Vec<PathBuf>, RegistryError> {
    let mut dirs = Vec::new();
    if !root.is_dir() {
        return Ok(dirs);
    }
    let entries = std::fs::read_dir(root).map_err(|source| RegistryError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| RegistryError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() && has_manifest(&path) {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

/// Check if a directory contains a plugin manifest.
fn has_manifest(dir: &Path) -> bool {
    dir.join("manifest.kdl").exists() || dir.join(".claude-plugin").join("plugin.json").exists()
}

/// Load a manifest from a plugin directory.
fn load_manifest_from_dir(
    dir: &Path,
) -> Result<PluginManifest, pattern_core::plugin::ManifestError> {
    let kdl_path = dir.join("manifest.kdl");
    if kdl_path.exists() {
        return super::manifest::from_kdl_file(&kdl_path);
    }
    let cc_path = dir.join(".claude-plugin").join("plugin.json");
    if cc_path.exists() {
        return super::manifest::from_cc_json_file(&cc_path);
    }
    Err(pattern_core::plugin::ManifestError::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no manifest.kdl or .claude-plugin/plugin.json found",
        ),
    })
}

/// Build a LoadedPlugin from a registry installation entry.
fn build_loaded_from_installation(
    inst: PluginInstallation,
    manifest: PluginManifest,
    scope: PluginScope,
    source_path: &Path,
) -> LoadedPlugin {
    // Parse plugin key from registry entry. Errors here are logged + the plugin
    // loads as if it had no key (None) — better than failing the whole registry
    // load on one malformed pubkey. Real malformed entries surface at session-open
    // when routable_pubkeys() walks the loaded set.
    let plugin_key = match inst.plugin_key() {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(plugin_id = %inst.id, error = %e, "failed to parse plugin key; loading as unauthed");
            None
        }
    };
    // Convert user_config entries to a JSON object.
    let user_config = inst
        .user_config
        .map(|uc| {
            let map: serde_json::Map<String, serde_json::Value> = uc
                .entries
                .into_iter()
                .map(|e| {
                    (
                        e.key.to_string(),
                        serde_json::Value::String(e.value.to_string()),
                    )
                })
                .collect();
            serde_json::Value::Object(map)
        })
        .unwrap_or(serde_json::Value::Null);

    let conn = build_connection(&manifest, source_path);
    LoadedPlugin {
        id: manifest.name.clone(),
        scope,
        source_path: source_path.to_path_buf(),
        manifest,
        user_config,
        capability_overrides: None,
        connection: conn,
        host: None,
        plugin_key,
    }
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), RegistryError> {
    std::fs::create_dir_all(dst).map_err(|source| RegistryError::Io {
        path: dst.to_path_buf(),
        source,
    })?;
    for entry in std::fs::read_dir(src).map_err(|source| RegistryError::Io {
        path: src.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| RegistryError::Io {
            path: src.to_path_buf(),
            source,
        })?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|source| RegistryError::Io {
                path: src_path,
                source,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod plugin_key_tests {
    use super::*;
    use pattern_core::plugin::auth::PluginKey;

    fn inst(id: &str) -> PluginInstallation {
        PluginInstallation {
            id: id.into(),
            source: None,
            installed_at: None,
            pubkey: None,
            pubkey_uri: None,
            pubkey_cid: None,
            user_config: None,
            capability_override: None,
        }
    }

    #[test]
    fn no_auth_fields_returns_none() {
        assert!(inst("x").plugin_key().unwrap().is_none());
    }

    #[test]
    fn valid_direct_pubkey_parses() {
        let pk = iroh::SecretKey::generate().public();
        let mut i = inst("x");
        i.pubkey = Some(pk.to_string());
        match i.plugin_key().unwrap() {
            Some(PluginKey::Direct(parsed)) => assert_eq!(parsed, pk),
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn invalid_pubkey_errors() {
        let mut i = inst("x");
        i.pubkey = Some("not-a-real-pubkey".into());
        assert!(matches!(
            i.plugin_key(),
            Err(PluginKeyParseError::InvalidPubkey { .. })
        ));
    }

    #[test]
    fn atproto_pair_parses() {
        let mut i = inst("remote");
        i.pubkey_uri = Some("at://did:plc:abc/app.pattern.plugin/foo".into());
        i.pubkey_cid = Some("bafyabc".into());
        assert!(matches!(
            i.plugin_key().unwrap(),
            Some(PluginKey::Atproto { .. })
        ));
    }

    #[test]
    fn atproto_uri_without_cid_errors() {
        let mut i = inst("x");
        i.pubkey_uri = Some("at://did:plc:abc/app.pattern.plugin/foo".into());
        assert!(matches!(
            i.plugin_key(),
            Err(PluginKeyParseError::AtprotoIncomplete { .. })
        ));
    }

    #[test]
    fn direct_and_atproto_conflict_errors() {
        let pk = iroh::SecretKey::generate().public();
        let mut i = inst("x");
        i.pubkey = Some(pk.to_string());
        i.pubkey_uri = Some("at://did:plc:abc/app.pattern.plugin/foo".into());
        i.pubkey_cid = Some("bafyabc".into());
        assert!(matches!(
            i.plugin_key(),
            Err(PluginKeyParseError::ConflictingAuth { .. })
        ));
    }
}
