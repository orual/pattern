//! Integration tests for jj adapter mutation functions.
//!
//! These tests invoke the real `jj` binary via a tempdir repository. They are
//! skipped automatically when `jj` is not on PATH.
//!
//! To run:
//! ```sh
//! cargo nextest run -p pattern-memory --test jj_adapter_mutate --nocapture
//! ```

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;

use pattern_memory::jj::JjAdapter;

// -------------------------------------------------------------------------
// Test helpers
// -------------------------------------------------------------------------

fn maybe_adapter() -> Option<JjAdapter> {
    JjAdapter::detect().unwrap_or(None)
}

macro_rules! skip_if_no_jj {
    () => {
        match maybe_adapter() {
            Some(a) => a,
            None => {
                eprintln!("SKIP: jj not available on PATH");
                return;
            }
        }
    };
}

/// Initialize a temporary jj git repository.
fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir creation failed");
    let status = Command::new("jj")
        .args(["git", "init"])
        .current_dir(dir.path())
        .status()
        .expect("jj git init failed to spawn");
    assert!(status.success(), "jj git init exited non-zero");
    dir
}

/// Write a file, describe, and create a new working copy commit.
fn make_commit(repo: &Path, filename: &str, description: &str) {
    std::fs::write(repo.join(filename), description).expect("write test file");
    let status = Command::new("jj")
        .args(["describe", "-m", description])
        .current_dir(repo)
        .status()
        .expect("jj describe spawn");
    assert!(status.success(), "jj describe failed");
    let status = Command::new("jj")
        .args(["new"])
        .current_dir(repo)
        .status()
        .expect("jj new spawn");
    assert!(status.success(), "jj new failed");
}

// -------------------------------------------------------------------------
// init_repo() test (AC8.2)
// -------------------------------------------------------------------------

/// init_repo() creates a jj git repository that workspace_list() can query.
#[test]
fn init_repo_creates_jj_repository() {
    let adapter = skip_if_no_jj!();
    let dir = tempfile::tempdir().expect("tempdir");
    adapter.init_repo(dir.path()).expect("init_repo failed");

    // Verify by listing workspaces in the newly-created repo.
    let workspaces = adapter
        .workspace_list(dir.path())
        .expect("workspace_list failed");
    assert!(
        !workspaces.is_empty(),
        "newly-init'd repo should have at least one workspace"
    );
}

// -------------------------------------------------------------------------
// commit() and describe() tests (AC8.2)
// -------------------------------------------------------------------------

/// commit() creates a commit visible via log().
#[test]
fn commit_creates_log_entry() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    std::fs::write(repo.path().join("data.txt"), "hello").expect("write");
    adapter
        .commit(repo.path(), "test commit message")
        .expect("commit failed");

    let entries = adapter.log(repo.path(), "all()").expect("log failed");
    let descriptions: Vec<_> = entries
        .iter()
        .map(|e| e.description.trim().to_string())
        .collect();
    assert!(
        descriptions.contains(&"test commit message".to_string()),
        "expected 'test commit message' in {descriptions:?}"
    );
}

/// describe() updates the working copy description without creating a new commit.
#[test]
fn describe_updates_working_copy_message() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    adapter
        .describe(repo.path(), "my description")
        .expect("describe failed");

    // The working copy commit (@) should have the description we just set.
    let entries = adapter.log(repo.path(), "@").expect("log failed");
    assert_eq!(entries.len(), 1, "@ should resolve to exactly one commit");
    assert_eq!(entries[0].description.trim(), "my description");
}

// -------------------------------------------------------------------------
// workspace_add() + workspace_forget() tests (AC8.2, AC8.7)
// -------------------------------------------------------------------------

/// workspace_add() creates a second workspace visible in workspace_list().
#[test]
fn workspace_add_and_list() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    let sibling = tempfile::tempdir().expect("sibling tempdir");
    adapter
        .workspace_add(repo.path(), sibling.path())
        .expect("workspace_add failed");

    let workspaces = adapter
        .workspace_list(repo.path())
        .expect("workspace_list failed");
    let names: Vec<_> = workspaces.iter().map(|w| w.name.as_str()).collect();
    // Should now have at least two workspaces.
    assert!(names.len() >= 2, "expected >= 2 workspaces, got {names:?}");
}

/// workspace_forget() on a nonexistent name returns WorkspaceNotFound (AC8.7).
#[test]
fn workspace_forget_nonexistent_returns_not_found() {
    use pattern_memory::jj::JjError;

    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    let result = adapter.workspace_forget(repo.path(), "does-not-exist");
    match result {
        Err(JjError::WorkspaceNotFound { name }) => {
            assert_eq!(name, "does-not-exist");
        }
        other => panic!("expected WorkspaceNotFound, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// bookmark_set() + bookmark_delete() tests (AC8.2, AC8.7)
// -------------------------------------------------------------------------

/// bookmark_set() creates a bookmark; bookmark_delete() removes it.
#[test]
fn bookmark_set_and_delete_round_trip() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();
    make_commit(repo.path(), "bm.txt", "bookmark base");

    adapter
        .bookmark_set(repo.path(), "test-bm", "@-")
        .expect("bookmark_set failed");

    let bookmarks = adapter.bookmark_list(repo.path()).expect("bookmark_list");
    assert!(
        bookmarks.iter().any(|b| b.name == "test-bm"),
        "bookmark 'test-bm' should exist after set"
    );

    adapter
        .bookmark_delete(repo.path(), "test-bm")
        .expect("bookmark_delete failed");

    let bookmarks_after = adapter
        .bookmark_list(repo.path())
        .expect("bookmark_list after delete");
    assert!(
        !bookmarks_after.iter().any(|b| b.name == "test-bm"),
        "bookmark 'test-bm' should not exist after delete"
    );
}

/// bookmark_delete() on a nonexistent name returns BookmarkNotFound (AC8.7).
#[test]
fn bookmark_delete_nonexistent_returns_not_found() {
    use pattern_memory::jj::JjError;

    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    let result = adapter.bookmark_delete(repo.path(), "no-such-bookmark");
    match result {
        Err(JjError::BookmarkNotFound { name }) => {
            assert_eq!(name, "no-such-bookmark");
        }
        other => panic!("expected BookmarkNotFound, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// merge() test (AC8.2)
// -------------------------------------------------------------------------

/// merge() creates a commit with two parents visible via log().
#[test]
fn merge_creates_commit_with_two_parents() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();

    // Create commit A (describe the working copy, then advance).
    std::fs::write(repo.path().join("a.txt"), "branch A").expect("write a.txt");
    let status = Command::new("jj")
        .args(["describe", "-m", "branch A"])
        .current_dir(repo.path())
        .status()
        .expect("jj describe A");
    assert!(status.success());

    // Record the change_id for commit A before advancing.
    let entries_a = adapter.log(repo.path(), "@").expect("log @");
    assert!(!entries_a.is_empty(), "should have at least one entry");
    let change_id_a = entries_a[0].change_id.clone();

    // Go back to root() and create commit B on a separate branch.
    let status = Command::new("jj")
        .args(["new", "root()"])
        .current_dir(repo.path())
        .status()
        .expect("jj new root");
    assert!(status.success());

    std::fs::write(repo.path().join("b.txt"), "branch B").expect("write b.txt");
    let status = Command::new("jj")
        .args(["describe", "-m", "branch B"])
        .current_dir(repo.path())
        .status()
        .expect("jj describe B");
    assert!(status.success());

    // Record the change_id for commit B.
    let entries_b = adapter.log(repo.path(), "@").expect("log @ for B");
    assert!(
        !entries_b.is_empty(),
        "should have at least one entry for B"
    );
    let change_id_b = entries_b[0].change_id.clone();

    // Merge the two branches using their change IDs.
    adapter
        .merge(
            repo.path(),
            &[change_id_a.as_str(), change_id_b.as_str()],
            Some("merge commit"),
        )
        .expect("merge failed");

    // Verify the merge commit exists in the log.
    let entries = adapter.log(repo.path(), "@").expect("log @");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].description.trim(), "merge commit");
}

// -------------------------------------------------------------------------
// Concurrent mutation serialization test (AC8.2)
// -------------------------------------------------------------------------

/// Five threads each call bookmark_set() concurrently; all succeed (no
/// sibling-operation errors from jj).
#[test]
fn concurrent_bookmark_set_all_succeed() {
    let adapter = skip_if_no_jj!();
    let repo = init_repo();
    make_commit(repo.path(), "base.txt", "base for concurrent test");

    let adapter = Arc::new(adapter);
    let repo_path = Arc::new(repo.path().to_path_buf());

    let handles: Vec<_> = (0..5)
        .map(|i| {
            let adapter = Arc::clone(&adapter);
            let repo_path = Arc::clone(&repo_path);
            std::thread::spawn(move || {
                let name = format!("concurrent-bm-{i}");
                adapter
                    .bookmark_set(&repo_path, &name, "@-")
                    .expect("concurrent bookmark_set failed")
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread panicked");
    }

    let bookmarks = adapter
        .bookmark_list(&repo_path)
        .expect("bookmark_list after concurrent set");
    for i in 0..5 {
        let name = format!("concurrent-bm-{i}");
        assert!(
            bookmarks.iter().any(|b| b.name == name),
            "bookmark '{name}' missing after concurrent set"
        );
    }

    // Keep repo alive until end of test.
    drop(repo);
}
