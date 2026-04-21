//! CLI integration tests for `pattern mount` subcommands.
//!
//! Spawns the `pattern` binary via `std::process::Command` and verifies exit
//! codes, stdout, and stderr. These tests are skipped automatically if the
//! binary has not been built (e.g. in CI that runs `cargo check` only).
//!
//! To run:
//! ```sh
//! cargo build -p pattern-cli && cargo nextest run -p pattern-cli --test cli_mount
//! ```

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Binary path helpers
// ---------------------------------------------------------------------------

/// Locate the `pattern` binary produced by `cargo build`.
///
/// Returns `None` if the binary is not present, which causes tests to be
/// skipped gracefully rather than failing.
fn pattern_bin() -> Option<PathBuf> {
    // CARGO_BIN_EXE_pattern is set by cargo when running integration tests
    // for a crate that declares a [[bin]] target. Since this is an integration
    // test in pattern_cli, cargo sets this automatically.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_pattern") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    // Fallback: look in the workspace target/debug directory.
    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2) // crates/pattern_cli → crates → workspace root
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("target")
        .join("debug")
        .join("pattern");

    if fallback.exists() {
        Some(fallback)
    } else {
        None
    }
}

/// Skip macro: if the binary is not built, print a message and return.
macro_rules! skip_if_no_binary {
    () => {
        match pattern_bin() {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP: pattern binary not found — run `cargo build -p pattern-cli` first"
                );
                return;
            }
        }
    };
}

/// Check that `jj` is available on PATH.
fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `pattern mount init --mode a --path <tempdir>` should exit 0 and create
/// the expected directory layout.
#[test]
fn mount_init_mode_a_exits_zero() {
    let bin = skip_if_no_binary!();
    let tmp = TempDir::new().expect("tempdir");

    let output = Command::new(&bin)
        .args(["mount", "init", "--mode", "a", "--path"])
        .arg(tmp.path())
        .output()
        .expect("failed to spawn pattern");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("stdout: {stdout}");
    eprintln!("stderr: {stderr}");

    assert!(
        output.status.success(),
        "pattern mount init --mode a should exit 0, got {:?}",
        output.status.code()
    );

    // The mount layout should exist.
    let mount_path = tmp.path().join(".pattern").join("shared");
    assert!(
        mount_path.is_dir(),
        ".pattern/shared/ should exist after Mode A init"
    );
    assert!(
        mount_path.join(".pattern.kdl").is_file(),
        ".pattern/shared/.pattern.kdl should exist after Mode A init"
    );
    assert!(
        mount_path.join("blocks").join("core").is_dir(),
        ".pattern/shared/blocks/core/ should exist after Mode A init"
    );
}

/// `pattern mount init --mode b --project-id <id>` should exit 0 if jj is
/// available, or exit non-zero with a useful error message if jj is absent.
/// The test is skipped entirely on the positive path if jj is not available.
#[test]
fn mount_init_mode_b_requires_jj() {
    let bin = skip_if_no_binary!();

    if !jj_available() {
        // Without jj, the command should fail with a clear message.
        // Use a unique project ID to avoid touching any real data.
        let project_id = format!(
            "cli-test-mode-b-no-jj-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        );
        let output = Command::new(&bin)
            .args(["mount", "init", "--mode", "b", "--project-id", &project_id])
            .env(
                "PATTERN_HOME",
                TempDir::new().expect("tempdir").path().to_str().unwrap(),
            )
            .output()
            .expect("failed to spawn pattern");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "mode b without jj should fail, but it exited 0"
        );
        assert!(
            stderr.contains("jj") || String::from_utf8_lossy(&output.stdout).contains("jj"),
            "error output should mention jj; stderr={stderr}"
        );
        return;
    }

    // jj is available — run the happy path.
    let home_dir = TempDir::new().expect("tempdir for PATTERN_HOME");
    // Use a unique project ID to avoid touching any real ~/.pattern/.
    let project_id = format!(
        "cli-test-mode-b-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );

    let output = Command::new(&bin)
        .args(["mount", "init", "--mode", "b", "--project-id", &project_id])
        .env("PATTERN_HOME", home_dir.path())
        .output()
        .expect("failed to spawn pattern");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("stdout: {stdout}");
    eprintln!("stderr: {stderr}");

    assert!(
        output.status.success(),
        "pattern mount init --mode b should exit 0 when jj is available, got {:?}: {stderr}",
        output.status.code()
    );

    // The mount layout should exist under the PATTERN_HOME override.
    let mount_path = home_dir
        .path()
        .join("projects")
        .join(&project_id)
        .join("shared");
    assert!(
        mount_path.is_dir(),
        "projects/<id>/shared/ should exist under PATTERN_HOME"
    );
    assert!(
        mount_path.join(".pattern.kdl").is_file(),
        ".pattern.kdl should exist after Mode B init"
    );
}

/// `pattern mount attach <path-with-no-mount>` should exit non-zero and
/// print a useful error message to stderr.
#[test]
fn mount_attach_no_mount_exits_nonzero() {
    let bin = skip_if_no_binary!();
    let tmp = TempDir::new().expect("tempdir");

    let output = Command::new(&bin)
        .args(["mount", "attach"])
        .arg(tmp.path())
        .output()
        .expect("failed to spawn pattern");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("stdout: {stdout}");
    eprintln!("stderr: {stderr}");

    assert!(
        !output.status.success(),
        "pattern mount attach on a path with no mount should fail, but exited 0"
    );

    // The error output should contain something useful — not just an exit code.
    // We check both stdout and stderr since miette may write to either.
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("pattern mount init")
            || combined.contains("mount")
            || combined.contains("no mount")
            || combined.contains("not found"),
        "error output should mention mount or suggest pattern mount init; combined={combined}"
    );
}

/// `pattern mount attach <path>` on a valid Mode A mount should exit 0.
///
/// This test creates a Mode A mount via `mount init` first, then attaches.
/// It verifies the round-trip works end-to-end through the CLI.
#[test]
fn mount_attach_mode_a_exits_zero() {
    let bin = skip_if_no_binary!();
    let tmp = TempDir::new().expect("tempdir");

    // First, initialize a Mode A mount.
    let init_output = Command::new(&bin)
        .args(["mount", "init", "--mode", "a", "--path"])
        .arg(tmp.path())
        .output()
        .expect("failed to spawn pattern for init");

    assert!(
        init_output.status.success(),
        "mount init should succeed before attach test"
    );

    // Now attach.
    let attach_output = Command::new(&bin)
        .args(["mount", "attach"])
        .arg(tmp.path())
        .output()
        .expect("failed to spawn pattern for attach");

    let stdout = String::from_utf8_lossy(&attach_output.stdout);
    let stderr = String::from_utf8_lossy(&attach_output.stderr);
    eprintln!("stdout: {stdout}");
    eprintln!("stderr: {stderr}");

    assert!(
        attach_output.status.success(),
        "pattern mount attach on a valid Mode A mount should exit 0, got {:?}: {stderr}",
        attach_output.status.code()
    );
    assert!(
        stdout.contains("Attached") || stdout.contains("mode"),
        "stdout should mention attachment; got: {stdout}"
    );
}
