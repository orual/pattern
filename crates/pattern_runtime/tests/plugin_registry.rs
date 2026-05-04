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
