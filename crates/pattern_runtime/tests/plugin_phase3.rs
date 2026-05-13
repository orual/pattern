//! Phase 3 integration tests: plugin trait boundary + CC adapter.

use std::path::PathBuf;
use std::sync::Arc;

use pattern_core::traits::plugin::PluginExtension;
use pattern_memory::paths::PatternPaths;
use pattern_runtime::plugin::cc_adapter::CcPluginAdapter;
use pattern_runtime::plugin::manifest;
use pattern_runtime::plugin::registry::{InstallSource, PluginRegistry};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
        .join(name)
}

fn test_env() -> (tempfile::TempDir, Arc<PatternPaths>) {
    let base = tempfile::tempdir().unwrap();
    let paths = Arc::new(PatternPaths::with_base(base.path()));
    std::fs::create_dir_all(paths.plugins_global_root()).unwrap();
    (base, paths)
}

#[test]
fn cc_plugin_manifest_parses_with_cc_block() {
    let cc_dir = fixture("cc-test-plugin");
    let manifest = manifest::from_cc_json_file(&cc_dir.join(".claude-plugin").join("plugin.json"))
        .expect("CC manifest should parse");

    assert_eq!(manifest.name.as_str(), "cc-test-plugin");
    assert!(manifest.cc.is_some(), "CC plugin should have cc block");
    assert!(!manifest.skills.is_empty(), "should have skills declared");
    assert!(!manifest.hooks.is_empty(), "should have hooks declared");
}

#[test]
fn cc_adapter_wraps_cc_manifest() {
    let cc_dir = fixture("cc-test-plugin");
    let manifest =
        manifest::from_cc_json_file(&cc_dir.join(".claude-plugin").join("plugin.json")).unwrap();

    let adapter = CcPluginAdapter::wrap("cc-test-plugin".into(), cc_dir, manifest);

    // CC adapter provides no ports (Phase 4 adds monitor→port translation).
    assert!(adapter.ports().is_empty());
    // CC adapter returns None for on_event (uses subscription receivers instead).
    let event = pattern_core::hooks::HookEvent::notification("test.event", serde_json::json!({}));
    assert!(adapter.on_event(&event).is_none());
}

#[test]
fn install_cc_plugin_creates_extension() {
    let (_base, paths) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cc-test-plugin");

    let result = reg.install(
        InstallSource::LocalPath(&source),
        pattern_core::plugin::PluginScope::Global,
    );
    assert!(
        result.is_ok(),
        "CC plugin install should succeed: {result:?}"
    );

    let lp = reg
        .get("cc-test-plugin")
        .expect("should find installed plugin");
    assert!(
        lp.connection.is_some(),
        "CC plugin should have extension trait object"
    );
    assert!(
        lp.host.is_none(),
        "CC plugins should have no host (no callbacks)"
    );
}

#[test]
fn install_native_plugin_has_no_extension_yet() {
    // Native (non-CC) plugins don't get an extension until Phase 6.
    let (_base, paths) = test_env();
    let reg = PluginRegistry::new(paths, None);
    let source = fixture("cache-source"); // Pattern-native manifest (no cc block)

    let result = reg.install(
        InstallSource::LocalPath(&source),
        pattern_core::plugin::PluginScope::Global,
    );
    assert!(result.is_ok());

    let lp = reg.get("test-cache-plugin").expect("should find plugin");
    assert!(
        lp.connection.is_none(),
        "Native plugin should have no extension yet (Phase 6)"
    );
}

#[test]
fn cc_plugin_skill_directory_exists() {
    // Verify the test fixture has a skills directory with SKILL.md.
    let cc_dir = fixture("cc-test-plugin");
    let skill_md = cc_dir.join("skills").join("summarize").join("SKILL.md");
    assert!(skill_md.exists(), "SKILL.md fixture should exist");

    let raw = std::fs::read(&skill_md).unwrap();
    let parsed =
        pattern_memory::fs::markdown_skill::parse::parse(&raw).expect("SKILL.md should parse");
    assert_eq!(parsed.metadata.name, "summarize");
    assert_eq!(
        parsed.metadata.description.as_deref(),
        Some("Summarize text concisely")
    );
}
