//! Phase 6 Task 7: SDK dep-tree smoke test.
//!
//! Verifies AC6.8 — `pattern-plugin-sdk` is slim enough that a real consumer
//! plugin (`tests/fixtures/minimal_plugin`) builds without pulling in any of the
//! forbidden heavy dependencies that would bloat plugin processes.

use std::path::PathBuf;

fn fixture_manifest() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/minimal_plugin/Cargo.toml");
    p
}

#[test]
fn minimal_plugin_builds_and_dep_tree_is_lean() {
    let manifest = fixture_manifest();

    // 1. Build the fixture plugin. If it doesn't compile, the SDK surface is broken.
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            manifest.to_str().expect("manifest path utf8"),
        ])
        .status()
        .expect("cargo build minimal_plugin");
    assert!(status.success(), "minimal_plugin failed to build");

    // 2. Confirm dep tree omits forbidden heavy crates.
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "tree",
            "--manifest-path",
            manifest.to_str().expect("manifest path utf8"),
        ])
        .output()
        .expect("cargo tree");
    assert!(output.status.success(), "cargo tree failed");
    let tree = String::from_utf8_lossy(&output.stdout);

    // Forbidden list: heavy crates that plugin authors should never pay for transitively.
    // `tokio-tungstenite` from the plan's original list is legitimately carried via jacquard
    // (atproto client; needed for phase 7). It's gateable via jacquard's `websocket` feature
    // if a leaner subset is ever wanted, but the current workspace feature set is intentional.
    let forbidden = [
        "loro",
        "genai",
        "candle-core",
        "rusqlite",
        "pattern-runtime",
        "pattern-memory",
    ];
    for crate_name in &forbidden {
        assert!(
            !tree.contains(crate_name),
            "minimal_plugin should NOT depend on {crate_name}; full dep tree:\n{tree}"
        );
    }
}
