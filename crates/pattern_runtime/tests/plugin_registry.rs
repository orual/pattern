//! Integration tests for plugin registry: discovery, install, uninstall.

use std::path::PathBuf;
use std::sync::Arc;

use pattern_memory::paths::PatternPaths;
use pattern_runtime::plugin::registry::{InstallSource, LoadedPlugin, PluginRegistry};
use pattern_runtime::plugin::PluginScope;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
        .join(name)
}

/// Set up a test environment with temp dirs for global and project paths.
fn test_env() -> (tempfile::TempDir, Arc<PatternPaths>, tempfile::TempDir) {
    let global_base = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let paths = Arc::new(PatternPaths::with_base(global_base.path()));
    // Create the plugins directory so discovery doesn't fail.
    std::fs::create_dir_all(paths.plugins_global_root()).unwrap();
    (global_base, paths, project)
}

#[test]
fn empty_registry_loads() {
    let (_base, paths, project) = test_env();
    let reg = PluginRegistry::load(paths, Some(project.path().to_path_buf())).unwrap();
    assert!(reg.is_empty());
}

#[test]
fn install_local_path_adds_to_registry() {
    let (_base, paths, _project) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cache-source");
    let result = reg.install(InstallSource::LocalPath(&source), PluginScope::Global);
    assert!(result.is_ok(), "install should succeed: {result:?}");
    let lp = result.unwrap();
    assert_eq!(lp.id.as_str(), "test-cache-plugin");
    assert!(matches!(lp.scope, PluginScope::Global));
    assert_eq!(reg.len(), 1);
}

#[test]
fn get_returns_installed_plugin() {
    let (_base, paths, _project) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cache-source");
    reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    let lp = reg.get("test-cache-plugin");
    assert!(lp.is_some());
    assert_eq!(lp.unwrap().manifest.version.as_deref(), Some("1.0.0"));
}

#[test]
fn list_returns_all_plugins() {
    let (_base, paths, _project) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cache-source");
    reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    let all = reg.list();
    assert_eq!(all.len(), 1);
}

#[test]
fn uninstall_removes_from_registry() {
    let (_base, paths, _project) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cache-source");
    reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    assert_eq!(reg.len(), 1);
    reg.uninstall("test-cache-plugin", false).unwrap();
    assert_eq!(reg.len(), 0);
    assert!(reg.get("test-cache-plugin").is_none());
}

#[test]
fn uninstall_unknown_plugin_errors() {
    let (_base, paths, _project) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let result = reg.uninstall("nonexistent", false);
    assert!(result.is_err());
}

#[test]
fn ambient_discovery_finds_plugins() {
    let (_base, paths, _project) = test_env();
    // Copy a plugin into the global plugins directory.
    let source = fixture("cache-source");
    let dest = paths.plugins_global_root().join("test-ambient");
    copy_dir(&source, &dest);
    // Write a manifest with a different name so we can identify it.
    std::fs::write(
        dest.join("manifest.kdl"),
        "name \"ambient-discovered\"\nversion \"1.0.0\"\n",
    )
    .unwrap();

    let reg = PluginRegistry::load(paths, None).unwrap();
    let lp = reg.get("ambient-discovered");
    assert!(lp.is_some(), "ambient plugin should be discovered");
    assert!(matches!(lp.unwrap().scope, PluginScope::Ambient));
}

/// Helper: recursively copy a directory.
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

/// Helper: create a minimal plugin dir with a manifest.
fn create_plugin_dir(dir: &std::path::Path, name: &str, version: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("manifest.kdl"),
        format!("name \"{}\"\nversion \"{}\"\n", name, version),
    )
    .unwrap();
}

/// Helper: write a registry KDL file.
fn write_registry_kdl(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

#[test]
fn global_pin_overrides_ambient() {
    let (_base, paths, _project) = test_env();

    // Put a plugin in the ambient directory.
    create_plugin_dir(
        &paths.plugins_global_root().join("override-test"),
        "override-test",
        "1.0.0",
    );

    // Pin the same plugin globally via registry KDL (higher precedence).
    create_plugin_dir(
        &paths.plugin_cache_dir("override-test"),
        "override-test",
        "2.0.0",
    );
    write_registry_kdl(
        &paths.plugins_global_registry(),
        "plugin \"override-test\" {\n    source \"/tmp/test\"\n}\n",
    );

    let reg = PluginRegistry::load(paths, None).unwrap();
    let lp = reg.get("override-test").expect("should find plugin");
    // Global pin should win over ambient.
    assert!(
        matches!(lp.scope, PluginScope::Global),
        "expected Global scope, got {:?}",
        lp.scope
    );
    // Should have the cached version (from the global pin), not the ambient one.
    assert_eq!(lp.manifest.version.as_deref(), Some("2.0.0"));
}

#[test]
fn project_pin_overrides_global() {
    let (_base, paths, project) = test_env();

    // Put a plugin in global cache.
    create_plugin_dir(
        &paths.plugin_cache_dir("proj-override"),
        "proj-override",
        "1.0.0",
    );
    write_registry_kdl(
        &paths.plugins_global_registry(),
        "plugin \"proj-override\" {\n    source \"/tmp/test\"\n}\n",
    );

    // Pin the same plugin at project scope.
    let project_plugin_dir = project.path().join("plugins").join("proj-override");
    create_plugin_dir(&project_plugin_dir, "proj-override", "3.0.0");
    let project_registry = pattern_memory::paths::project_plugin_registry(project.path(), false);
    write_registry_kdl(
        &project_registry,
        &format!(
            "plugin \"proj-override\" {{\n    source \"{}\"\n}}\n",
            project_plugin_dir.display()
        ),
    );

    let reg = PluginRegistry::load(paths, Some(project.path().to_path_buf())).unwrap();
    let lp = reg.get("proj-override").expect("should find plugin");
    assert!(
        matches!(lp.scope, PluginScope::Project { .. }),
        "expected Project scope, got {:?}",
        lp.scope
    );
    assert_eq!(lp.manifest.version.as_deref(), Some("3.0.0"));
}

#[test]
fn restart_survival_reload_preserves_plugins() {
    // AC2.2: install() persists to registry KDL, so plugins survive restart.
    let (_base, paths, _project) = test_env();

    // Install a plugin via the real install() path.
    let source = fixture("cache-source");
    let reg = PluginRegistry::new(paths.clone(), None);
    reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    assert_eq!(reg.len(), 1);
    let original = reg.get("test-cache-plugin").unwrap();

    // Drop the registry (simulates daemon restart).
    drop(reg);

    // The registry KDL file should have been written by install().
    assert!(
        paths.plugins_global_registry().exists(),
        "install() should persist to registry KDL file"
    );

    // Rebuild from disk — the plugin should survive via the persisted entry.
    let reg2 = PluginRegistry::load(paths, None).unwrap();
    let reloaded = reg2.get("test-cache-plugin");
    assert!(
        reloaded.is_some(),
        "plugin should survive registry reload"
    );
    assert_eq!(
        reloaded.unwrap().manifest.version,
        original.manifest.version,
    );
}

#[test]
fn install_duplicate_uses_cached() {
    let (_base, paths, _project) = test_env();
    let source = fixture("cache-source");
    let reg = PluginRegistry::new(paths, None);

    // First install.
    let lp1 = reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    assert_eq!(reg.len(), 1);

    // Second install of the same source — should succeed (cached exists).
    let lp2 = reg.install(InstallSource::LocalPath(&source), PluginScope::Global).unwrap();
    assert_eq!(reg.len(), 1, "should still be 1 plugin");
    assert_eq!(lp1.id, lp2.id);
}

#[test]
fn load_reads_user_config_from_registry_kdl() {
    let (_base, paths, _project) = test_env();

    // Create a plugin in the cache.
    create_plugin_dir(
        &paths.plugin_cache_dir("config-test"),
        "config-test",
        "1.0.0",
    );

    // Write a registry KDL with user-config block.
    write_registry_kdl(
        &paths.plugins_global_registry(),
        concat!(
            "plugin \"config-test\" {\n",
            "    source \"/tmp/test\"\n",
            "    user-config {\n",
            "        threshold \"8\"\n",
            "        api-key \"secret123\"\n",
            "    }\n",
            "}\n",
        ),
    );

    let reg = PluginRegistry::load(paths, None).unwrap();
    let lp = reg.get("config-test").expect("should find plugin");
    assert!(
        matches!(lp.scope, PluginScope::Global),
        "expected Global scope"
    );

    // User config should be populated.
    let config = &lp.user_config;
    assert!(config.is_object(), "user_config should be an object, got {config}");
    assert_eq!(
        config.get("threshold").and_then(|v| v.as_str()),
        Some("8"),
        "threshold should be '8'"
    );
    assert_eq!(
        config.get("api-key").and_then(|v| v.as_str()),
        Some("secret123"),
        "api-key should be 'secret123'"
    );
}

#[test]
fn load_user_config_tunable_edit_reflected_on_reload() {
    // AC2.6: edit user_config value in registry KDL, reload, verify change.
    let (_base, paths, _project) = test_env();

    create_plugin_dir(
        &paths.plugin_cache_dir("tunable-test"),
        "tunable-test",
        "1.0.0",
    );

    // Initial registry with threshold=8.
    write_registry_kdl(
        &paths.plugins_global_registry(),
        concat!(
            "plugin \"tunable-test\" {\n",
            "    source \"/tmp/test\"\n",
            "    user-config {\n",
            "        threshold \"8\"\n",
            "    }\n",
            "}\n",
        ),
    );

    let reg = PluginRegistry::load(paths.clone(), None).unwrap();
    let lp = reg.get("tunable-test").unwrap();
    assert_eq!(
        lp.user_config.get("threshold").and_then(|v| v.as_str()),
        Some("8"),
    );
    drop(reg);

    // "Edit" the registry KDL: change threshold to 12.
    write_registry_kdl(
        &paths.plugins_global_registry(),
        concat!(
            "plugin \"tunable-test\" {\n",
            "    source \"/tmp/test\"\n",
            "    user-config {\n",
            "        threshold \"12\"\n",
            "    }\n",
            "}\n",
        ),
    );

    // Reload — should see the new value.
    let reg2 = PluginRegistry::load(paths, None).unwrap();
    let lp2 = reg2.get("tunable-test").unwrap();
    assert_eq!(
        lp2.user_config.get("threshold").and_then(|v| v.as_str()),
        Some("12"),
        "threshold should be updated to '12' after reload"
    );
}
