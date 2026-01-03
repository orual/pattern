//! Tests for process execution backends.
//!
//! These tests require a real PTY and shell, so they may behave differently
//! in CI environments. Tests that require PTY functionality are skipped in
//! environments where PTY is not available.

use std::time::Duration;

use super::*;

/// Helper to check if we're in a CI environment where PTY tests may not work.
fn should_skip_pty_tests() -> bool {
    std::env::var("CI").is_ok()
}

// =============================================================================
// Simple execute tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_execute_simple() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("echo hello", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert!(result.output.contains("hello"));
    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn test_local_pty_execute_multiline() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("echo line1; echo line2", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert!(result.output.contains("line1"));
    assert!(result.output.contains("line2"));
}

// =============================================================================
// Exit code tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_exit_code_success() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("true", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn test_local_pty_exit_code_failure() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("false", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert_eq!(result.exit_code, Some(1));
}

#[tokio::test]
async fn test_local_pty_exit_code_custom() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    // Using a subshell to exit with a custom code without killing our session.
    let result = backend
        .execute("(exit 42)", Duration::from_secs(5))
        .await
        .expect("execute should succeed");

    assert_eq!(result.exit_code, Some(42));
}

#[tokio::test]
async fn test_local_pty_exit_kills_session() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    // `exit 42` kills the session.
    let result = backend.execute("exit 42", Duration::from_secs(5)).await;

    // This kills the session, so we expect SessionDied.
    assert!(
        matches!(result, Err(ShellError::SessionDied)),
        "expected SessionDied, got {:?}",
        result
    );
}

// =============================================================================
// CWD persistence tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_cwd_persistence() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::temp_dir());

    // Create a uniquely named temp dir.
    let test_dir = format!("test_cwd_{}", std::process::id());

    // Create a temp dir and cd into it.
    backend
        .execute(&format!("mkdir -p {test_dir}"), Duration::from_secs(5))
        .await
        .expect("mkdir should succeed");

    backend
        .execute(&format!("cd {test_dir}"), Duration::from_secs(5))
        .await
        .expect("cd should succeed");

    // pwd should show we're in the new directory.
    let result = backend
        .execute("pwd", Duration::from_secs(5))
        .await
        .expect("pwd should succeed");

    assert!(result.output.contains(&test_dir));

    // Cleanup.
    backend
        .execute(
            &format!("cd .. && rmdir {test_dir}"),
            Duration::from_secs(5),
        )
        .await
        .ok();
}

#[tokio::test]
async fn test_local_pty_cwd_cached_after_cd() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::temp_dir());

    // Initial cwd should be temp_dir (before session init, returns initial_cwd).
    let initial_cwd = backend.cwd().await.expect("cwd should be set");
    // On macOS /tmp is a symlink to /private/tmp, on Linux it may be /tmp or /var/tmp.
    assert!(
        initial_cwd.starts_with("/tmp")
            || initial_cwd.starts_with("/var")
            || initial_cwd.starts_with("/private/tmp"),
        "expected temp dir path, got {:?}",
        initial_cwd
    );

    // Run a command to ensure session is initialized and cwd is cached.
    backend
        .execute("echo init", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    // After first command, cached cwd should match initial.
    let cached_cwd = backend.cwd().await.expect("cwd should be set");
    assert!(
        cached_cwd.starts_with("/tmp")
            || cached_cwd.starts_with("/var")
            || cached_cwd.starts_with("/private/tmp"),
        "expected temp dir path, got {:?}",
        cached_cwd
    );

    // cd to a subdirectory.
    let test_dir = format!("cwd_test_{}", std::process::id());
    backend
        .execute(
            &format!("mkdir -p {test_dir} && cd {test_dir}"),
            Duration::from_secs(5),
        )
        .await
        .expect("cd should succeed");

    // Cached cwd should now reflect the new directory.
    let new_cwd = backend.cwd().await.expect("cwd should be set");
    assert!(
        new_cwd.to_string_lossy().contains(&test_dir),
        "expected cwd to contain '{test_dir}', got {:?}",
        new_cwd
    );

    // Cleanup.
    backend
        .execute(
            &format!("cd .. && rmdir {test_dir}"),
            Duration::from_secs(5),
        )
        .await
        .ok();
}

// =============================================================================
// Environment persistence tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_env_persistence() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    // Set an env var.
    backend
        .execute("export TEST_VAR=pattern_test", Duration::from_secs(5))
        .await
        .expect("export should succeed");

    // Should persist.
    let result = backend
        .execute("echo $TEST_VAR", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    assert!(result.output.contains("pattern_test"));
}

// =============================================================================
// Streaming tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_spawn_streaming() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let (task_id, mut rx) = backend
        .spawn_streaming("echo streaming && sleep 0.1 && echo done")
        .await
        .expect("spawn should succeed");

    let mut outputs = Vec::new();
    let mut has_output = false;
    while let Ok(chunk) = rx.recv().await {
        match &chunk {
            OutputChunk::Output(_) => {
                has_output = true;
                outputs.push(chunk);
            }
            OutputChunk::Exit { .. } => {
                outputs.push(chunk);
                break;
            }
        }
    }

    // Should have received output chunks.
    assert!(has_output, "should have received output");

    // Should have exit event.
    assert!(matches!(outputs.last(), Some(OutputChunk::Exit { .. })));

    // Task should be cleaned up.
    // Give it a moment to clean up.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        backend.running_tasks().is_empty(),
        "task should be cleaned up"
    );

    // Avoid unused variable warning.
    let _ = task_id;
}

// =============================================================================
// Kill tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_kill() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let (task_id, _rx) = backend
        .spawn_streaming("sleep 60")
        .await
        .expect("spawn should succeed");

    // Give it a moment to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(backend.running_tasks().len(), 1);

    backend.kill(&task_id).await.expect("kill should succeed");

    // Give tokio a moment to clean up.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        backend.running_tasks().is_empty(),
        "task should be cleaned up after kill"
    );
}

#[tokio::test]
async fn test_local_pty_kill_unknown_task() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let unknown_task = TaskId("unknown123".to_string());
    let result = backend.kill(&unknown_task).await;

    assert!(matches!(result, Err(ShellError::UnknownTask(_))));
}

// =============================================================================
// Timeout tests
// =============================================================================

#[tokio::test]
async fn test_local_pty_timeout() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap());

    let result = backend
        .execute("sleep 10", Duration::from_millis(100))
        .await;

    assert!(matches!(result, Err(ShellError::Timeout(_))));
}

// =============================================================================
// Exit code parsing unit tests (no PTY needed)
// =============================================================================

#[test]
fn test_parse_exit_code_basic() {
    let marker = "__PATTERN_EXIT_abc12345__";
    let output = "hello world\n__PATTERN_EXIT_abc12345__:0\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "hello world");
    assert_eq!(code, 0);
}

#[test]
fn test_parse_exit_code_nonzero() {
    let marker = "__PATTERN_EXIT_xyz98765__";
    let output = "error message\n__PATTERN_EXIT_xyz98765__:127\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "error message");
    assert_eq!(code, 127);
}

#[test]
fn test_parse_exit_code_output_contains_fake_marker() {
    // The command output contains something that looks like our marker, but with wrong nonce.
    let marker = "__PATTERN_EXIT_real1234__";
    let output =
        "user typed __PATTERN_EXIT_fake0000__:999\nactual output\n__PATTERN_EXIT_real1234__:0\n";

    // Should use the LAST occurrence with the correct marker.
    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(code, 0);
    // The fake marker is part of the cleaned output since it doesn't match our nonce.
    assert!(cleaned.contains("__PATTERN_EXIT_fake0000__:999"));
}

#[test]
fn test_parse_exit_code_negative() {
    // Test negative exit codes (signals are often reported as negative).
    let marker = "__PATTERN_EXIT_neg12345__";
    let output = "killed\n__PATTERN_EXIT_neg12345__:-9\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "killed");
    assert_eq!(code, -9);
}

#[test]
fn test_parse_exit_code_missing_marker() {
    let marker = "__PATTERN_EXIT_missing1__";
    let output = "no marker here\n";

    let result = LocalPtyBackend::parse_exit_code(output, marker);
    assert!(matches!(result, Err(ShellError::ExitCodeParseFailed)));
}

#[test]
fn test_parse_exit_code_empty_output() {
    let marker = "__PATTERN_EXIT_empty123__";
    let output = "__PATTERN_EXIT_empty123__:0\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "");
    assert_eq!(code, 0);
}

#[test]
fn test_parse_exit_code_multiline_output() {
    let marker = "__PATTERN_EXIT_multi123__";
    let output = "line1\nline2\nline3\n__PATTERN_EXIT_multi123__:42\n";

    let (cleaned, code) = LocalPtyBackend::parse_exit_code(output, marker).unwrap();
    assert_eq!(cleaned, "line1\nline2\nline3");
    assert_eq!(code, 42);
}

// =============================================================================
// Exit marker generation unit tests (no PTY needed)
// =============================================================================

#[test]
fn test_generate_exit_marker_uniqueness() {
    let marker1 = LocalPtyBackend::generate_exit_marker();
    let marker2 = LocalPtyBackend::generate_exit_marker();

    assert_ne!(marker1, marker2);
    assert!(marker1.starts_with("__PATTERN_EXIT_"));
    assert!(marker1.ends_with("__"));
}

#[test]
fn test_generate_exit_marker_format() {
    let marker = LocalPtyBackend::generate_exit_marker();

    // Should have format: __PATTERN_EXIT_<8chars>__
    assert!(marker.starts_with("__PATTERN_EXIT_"));
    assert!(marker.ends_with("__"));
    // Total length: 15 (prefix) + 8 (nonce) + 2 (suffix) = 25.
    assert_eq!(marker.len(), 25);
}

// =============================================================================
// Builder pattern tests
// =============================================================================

#[test]
fn test_backend_builder_with_shell() {
    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap()).with_shell("/bin/sh");

    // We can't easily inspect the shell field since it's private, but we can
    // at least verify the builder returns the right type.
    let _ = backend;
}

#[test]
fn test_backend_builder_with_env() {
    use std::collections::HashMap;

    let mut env = HashMap::new();
    env.insert("MY_VAR".to_string(), "my_value".to_string());

    let backend = LocalPtyBackend::new(std::env::current_dir().unwrap()).with_env(env);

    let _ = backend;
}

// =============================================================================
// Multiple backends isolation tests
// =============================================================================

#[tokio::test]
async fn test_multiple_backends_isolated() {
    if should_skip_pty_tests() {
        eprintln!("Skipping PTY test in CI environment");
        return;
    }

    // Create two separate backends.
    let backend1 = LocalPtyBackend::new(std::env::temp_dir());
    let backend2 = LocalPtyBackend::new(std::env::temp_dir());

    // Set env var in backend1.
    backend1
        .execute("export ISOLATED_VAR=backend1", Duration::from_secs(5))
        .await
        .expect("export should succeed");

    // backend2 should NOT have this var (different session).
    let result = backend2
        .execute("echo $ISOLATED_VAR", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    // Should be empty (or just newline) since ISOLATED_VAR doesn't exist in backend2.
    assert!(
        !result.output.contains("backend1"),
        "backends should be isolated"
    );

    // Verify backend1 still has it.
    let result = backend1
        .execute("echo $ISOLATED_VAR", Duration::from_secs(5))
        .await
        .expect("echo should succeed");

    assert!(result.output.contains("backend1"));
}
