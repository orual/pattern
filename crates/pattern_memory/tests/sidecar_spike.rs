//! Sidecar mode validation spike — interleaved jj and git operations.
//!
//! This test creates a real git repository with a Sidecar mode pattern mount
//! (sidecar jj inside `.pattern/shared/`), then exercises ~38 interleaved
//! operations to verify that the two VCS tools coexist correctly.
//!
//! Operations covered:
//! - Phase A: basic pattern-jj ops (5 ops)
//! - Phase B: host git operations interleaved (8 ops)
//! - Phase C: pattern operations after host git ops (5 ops)
//! - Phase D: host git reset stress test (4 ops)
//! - Phase E: concurrent-ish operations (3 ops)
//! - Phase F: attach/detach cycles with MemoryStore writes (7 ops)
//! - Phase G: external .md edits through filesystem (3 ops)
//! - Phase H: re-attach after external edits (3 ops)
//!
//! Skipped automatically when either `jj` or `git` is not on PATH.
//!
//! To run:
//! ```sh
//! cargo nextest run -p pattern-memory --test sidecar_spike --nocapture
//! ```

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_memory::jj::JjAdapter;
use pattern_memory::modes::sidecar;
use pattern_memory::mount::attach;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn maybe_adapter() -> Option<JjAdapter> {
    JjAdapter::detect().unwrap_or(None)
}

/// Return a `JjAdapter` or skip the test.
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

/// Check that `git` is available on PATH. Return false to skip.
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a git command in `dir` and assert success. Returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {}: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed (exit {}): {}",
        args.join(" "),
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Initialize a git repo with an initial commit, returning the tempdir.
fn git_init_project() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir creation");
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    // Create an initial file so the first commit is non-empty.
    std::fs::write(dir.path().join("README.md"), "# test project\n").expect("write README");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "initial"]);
    dir
}

// ---------------------------------------------------------------------------
// Spike test
// ---------------------------------------------------------------------------

/// Sidecar mode validation spike: ~38 interleaved jj + git + attach/detach + MemoryStore operations.
///
/// Verifies that a sidecar jj repo inside `.pattern/shared/` coexists with
/// the host git repo without corruption or state interference. Also exercises
/// attach/detach cycles, MemoryStore-level writes, and external .md edits.
#[test]
fn sidecar_validation_spike() {
    let adapter = skip_if_no_jj!();
    if !git_available() {
        eprintln!("SKIP: git not available on PATH");
        return;
    }

    let project = git_init_project();
    let root = project.path();
    let mount_path = root.join(".pattern").join("shared");

    // -----------------------------------------------------------------------
    // Setup: initialize Sidecar mode
    // -----------------------------------------------------------------------

    let _mode = sidecar::init(root, &adapter).expect("sidecar::init failed");
    assert!(
        mount_path.join(".jj").is_dir(),
        ".jj/ should exist after init"
    );

    // Host git: track the pattern files, commit. We must add .gitignore first
    // so that git knows to ignore .pattern/shared/.jj/ (which contains a git
    // repo from `jj git init` and would otherwise be treated as a submodule).
    git(root, &["add", ".gitignore"]);
    git(root, &["add", ".pattern/"]);
    git(root, &["commit", "-m", "add pattern mount"]);

    // Verify the .gitignore was set up correctly.
    let gitignore = std::fs::read_to_string(root.join(".gitignore")).expect("read .gitignore");
    assert!(
        gitignore.contains(".pattern/shared/.jj/"),
        ".gitignore must contain .pattern/shared/.jj/"
    );

    // -----------------------------------------------------------------------
    // Phase A: basic pattern-jj ops (5 ops)
    // -----------------------------------------------------------------------

    // Op 1: write a block file.
    std::fs::write(
        mount_path.join("blocks/core/notes.md"),
        "# Notes\n\nFirst entry.\n",
    )
    .expect("write notes.md");

    // Op 2: jj commit.
    adapter
        .commit(&mount_path, "initial pattern commit")
        .expect("jj commit 1");

    // Op 3: jj log — verify the commit.
    let log = adapter
        .log(&mount_path, "@-::@")
        .expect("jj log after first commit");
    let descriptions: Vec<_> = log.iter().map(|e| e.description.trim()).collect();
    assert!(
        descriptions.contains(&"initial pattern commit"),
        "expected 'initial pattern commit' in {descriptions:?}"
    );

    // Op 4: write another file.
    std::fs::write(
        mount_path.join("blocks/working/scratch.md"),
        "# Scratch\n\nWorking memory.\n",
    )
    .expect("write scratch.md");

    // Op 5: jj commit.
    adapter
        .commit(&mount_path, "second pattern commit")
        .expect("jj commit 2");

    // -----------------------------------------------------------------------
    // Phase B: host git operations interleaved (8 ops)
    // -----------------------------------------------------------------------

    // Op 6: host git snapshots the pattern files.
    git(root, &["add", ".pattern/shared/"]);
    git(root, &["commit", "-m", "snapshot pattern files"]);

    // Op 7: host creates a feature branch.
    git(root, &["checkout", "-b", "feature-branch"]);

    // Op 8: modify a pattern file on the feature branch.
    std::fs::write(
        mount_path.join("blocks/core/notes.md"),
        "# Notes\n\nFirst entry.\nFeature branch edit.\n",
    )
    .expect("write notes.md on feature branch");

    // Op 9: commit on feature branch.
    git(root, &["add", ".pattern/shared/blocks/core/notes.md"]);
    git(root, &["commit", "-m", "feature branch edit"]);

    // Op 10: switch back to main — notes.md reverts to pre-feature state.
    // Try both "main" and "master" since git init may use either.
    let main_branch = {
        let branches = git(root, &["branch", "--list"]);
        if branches.contains("main") {
            "main"
        } else {
            "master"
        }
    };
    git(root, &["checkout", main_branch]);

    // Verify notes.md is back to the pre-feature state.
    let notes = std::fs::read_to_string(mount_path.join("blocks/core/notes.md"))
        .expect("read notes.md after checkout main");
    assert!(
        !notes.contains("Feature branch edit"),
        "notes.md should not have feature content after checkout main"
    );

    // Op 11: verify jj's .jj/ is untouched and still works.
    assert!(
        mount_path.join(".jj").is_dir(),
        ".jj/ must survive git checkout"
    );
    let log_after_checkout = adapter
        .log(&mount_path, "all()")
        .expect("jj log after git checkout");
    assert!(
        !log_after_checkout.is_empty(),
        "jj log should return commits after git checkout"
    );

    // Op 12: merge the feature branch back.
    git(root, &["merge", "feature-branch", "-m", "merge feature"]);

    // Op 13: verify notes.md now has the feature content.
    let notes_after_merge = std::fs::read_to_string(mount_path.join("blocks/core/notes.md"))
        .expect("read notes.md after merge");
    assert!(
        notes_after_merge.contains("Feature branch edit"),
        "notes.md should have feature content after merge"
    );

    // jj sees the merge as working-copy modifications — expected and benign.
    // Just verify jj still works.
    let log_after_merge = adapter
        .log(&mount_path, "@")
        .expect("jj log after git merge");
    assert!(
        !log_after_merge.is_empty(),
        "jj should have a working copy commit after merge"
    );

    // -----------------------------------------------------------------------
    // Phase C: pattern operations after host git ops (5 ops)
    // -----------------------------------------------------------------------

    // Op 14: jj captures the git-merged state.
    adapter
        .commit(&mount_path, "post-merge commit")
        .expect("jj commit post-merge");

    // Op 15: write a new block file.
    std::fs::write(
        mount_path.join("blocks/core/context.md"),
        "# Context\n\nAdded after merge.\n",
    )
    .expect("write context.md");

    // Op 16: jj commit.
    adapter
        .commit(&mount_path, "new block after merge")
        .expect("jj commit new block");

    // Op 17: set a bookmark.
    adapter
        .bookmark_set(&mount_path, "stable", "@-")
        .expect("bookmark_set stable");

    // Op 18: list bookmarks — verify it exists.
    let bookmarks = adapter.bookmark_list(&mount_path).expect("bookmark_list");
    assert!(
        bookmarks.iter().any(|b| b.name == "stable"),
        "bookmark 'stable' should exist, got: {bookmarks:?}"
    );

    // -----------------------------------------------------------------------
    // Phase D: host git reset stress test (4 ops)
    // -----------------------------------------------------------------------

    // Op 19: capture current git state.
    let git_log_before = git(root, &["log", "--oneline"]);
    assert!(
        git_log_before.lines().count() >= 3,
        "git should have multiple commits"
    );

    // Op 20: hard reset host git by 2 commits.
    git(root, &["reset", "--hard", "HEAD~2"]);

    // Op 21: jj should still work fine — .jj/ is untouched by git reset.
    let log_after_reset = adapter
        .log(&mount_path, "@")
        .expect("jj log after git reset");
    assert!(
        !log_after_reset.is_empty(),
        "jj should still have a working copy after git reset"
    );

    // Op 22: jj captures the post-reset state.
    adapter
        .commit(&mount_path, "jj captures post-reset state")
        .expect("jj commit after reset");

    // -----------------------------------------------------------------------
    // Phase E: concurrent-ish operations (3 ops)
    // -----------------------------------------------------------------------

    // Op 23: write to a pattern file. The directory may have been removed by
    // git reset, so recreate it if needed.
    std::fs::create_dir_all(mount_path.join("blocks/working")).expect("ensure blocks/working");
    std::fs::write(
        mount_path.join("blocks/working/scratch.md"),
        "# Scratch\n\nUpdated concurrently.\n",
    )
    .expect("write scratch.md update");

    // Op 24: git add and commit the current state.
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "concurrent snapshot"]);

    // Op 25: jj commit after git commit — should work fine.
    adapter
        .commit(&mount_path, "pattern commit after git commit")
        .expect("jj commit after concurrent git commit");

    // -----------------------------------------------------------------------
    // Phase F: attach/detach cycles with MemoryStore-level writes (7 ops)
    //
    // Exercises the subscriber-aware path: create_block + set_text +
    // persist_block through the actual MemoryStore trait.
    // -----------------------------------------------------------------------

    // Op 26: first attach cycle — attach, create blocks, detach.
    {
        let store = attach(root).expect("attach cycle 1 failed");
        let cache = Arc::clone(&store.cache);

        // Op 27: create 3 blocks through MemoryStore.
        // Pattern: create_block → set_text → mark_dirty → persist_block.
        // `create_block` stores the doc with dirty=false. `set_text` mutates
        // the LoroDoc in-place but does not set the dirty flag. `mark_dirty`
        // sets the flag, allowing `persist_block` to flush to the database.
        let doc1 = cache
            .create_block(
                "agent-spike",
                BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text()),
            )
            .expect("create_block persona");
        doc1.set_text("Pattern agent persona.", true)
            .expect("set_text persona");
        cache.mark_dirty("agent-spike", "persona");
        cache
            .persist_block("agent-spike", "persona")
            .expect("persist persona");

        let doc2 = cache
            .create_block(
                "agent-spike",
                BlockCreate::new("task_list", MemoryBlockType::Working, BlockSchema::text()),
            )
            .expect("create_block task_list");
        doc2.set_text("- Task one\n- Task two\n", true)
            .expect("set_text task_list");
        cache.mark_dirty("agent-spike", "task_list");
        cache
            .persist_block("agent-spike", "task_list")
            .expect("persist task_list");

        let doc3 = cache
            .create_block(
                "agent-spike",
                BlockCreate::new("notes", MemoryBlockType::Core, BlockSchema::text()),
            )
            .expect("create_block notes");
        doc3.set_text("Core notes block.", true)
            .expect("set_text notes");
        cache.mark_dirty("agent-spike", "notes");
        cache
            .persist_block("agent-spike", "notes")
            .expect("persist notes");

        // Verify blocks are readable through the store before detach.
        let meta_list = cache
            .list_blocks(pattern_core::types::memory_types::BlockFilter::by_agent(
                "agent-spike",
            ))
            .expect("list_blocks after create");
        assert_eq!(
            meta_list.len(),
            3,
            "expected 3 blocks after create, got {}",
            meta_list.len()
        );

        store.detach();
    }

    // Op 28: second attach — re-attach and verify blocks survived detach.
    {
        let store = attach(root).expect("attach cycle 2 failed");
        let cache = Arc::clone(&store.cache);

        let doc = cache
            .get_block("agent-spike", "persona")
            .expect("get_block persona on re-attach")
            .expect("persona block should exist after re-attach");
        let content = doc.render();
        assert!(
            content.contains("Pattern agent persona"),
            "persona content should survive detach/re-attach, got: {content}"
        );

        // Op 29: add two more blocks on the second attach.
        let doc4 = cache
            .create_block(
                "agent-spike",
                BlockCreate::new("context", MemoryBlockType::Core, BlockSchema::text()),
            )
            .expect("create_block context");
        doc4.set_text("Additional context block.", true)
            .expect("set_text context");
        cache.mark_dirty("agent-spike", "context");
        cache
            .persist_block("agent-spike", "context")
            .expect("persist context");

        let doc5 = cache
            .create_block(
                "agent-spike",
                BlockCreate::new("scratch", MemoryBlockType::Working, BlockSchema::text()),
            )
            .expect("create_block scratch");
        doc5.set_text("Scratch working memory.", true)
            .expect("set_text scratch");
        cache.mark_dirty("agent-spike", "scratch");
        cache
            .persist_block("agent-spike", "scratch")
            .expect("persist scratch");

        let meta_list = cache
            .list_blocks(pattern_core::types::memory_types::BlockFilter::by_agent(
                "agent-spike",
            ))
            .expect("list_blocks after second create");
        assert_eq!(
            meta_list.len(),
            5,
            "expected 5 blocks on second attach, got {}",
            meta_list.len()
        );

        store.detach();
    }

    // Op 30: jj commit captures the DB state alongside file changes.
    adapter
        .commit(&mount_path, "after MemoryStore writes")
        .expect("jj commit after MemoryStore writes");

    // Op 31: third attach cycle — verify all 5 blocks still accessible.
    {
        let store = attach(root).expect("attach cycle 3 failed");
        let cache = Arc::clone(&store.cache);

        let meta_list = cache
            .list_blocks(pattern_core::types::memory_types::BlockFilter::by_agent(
                "agent-spike",
            ))
            .expect("list_blocks on third attach");
        assert_eq!(
            meta_list.len(),
            5,
            "expected 5 blocks on third attach, got {}",
            meta_list.len()
        );

        store.detach();
    }

    // -----------------------------------------------------------------------
    // Phase G: external .md edits through the filesystem (3 ops)
    //
    // Simulates a human editor writing directly to a block .md file while
    // the mount is detached. Verifies the file is readable after re-attach
    // (the subscriber/watcher would pick it up in a running session; here we
    // verify the filesystem state is consistent regardless).
    // -----------------------------------------------------------------------

    // Op 32: direct write to blocks/core/notes.md (human-style external edit).
    let notes_path = mount_path.join("blocks/core/notes.md");
    std::fs::write(
        &notes_path,
        "# Notes\n\nExternal edit 1: human added this line.\n",
    )
    .expect("external edit 1");

    // Op 33: direct write to a second file (simulating concurrent editor).
    let context_path = mount_path.join("blocks/core/context.md");
    std::fs::write(
        &context_path,
        "# Context\n\nExternal edit 2: context updated externally.\n",
    )
    .expect("external edit 2");

    // Op 34: direct write to a working block file.
    let scratch_path = mount_path.join("blocks/working/scratch.md");
    std::fs::write(
        &scratch_path,
        "# Scratch\n\nExternal edit 3: scratch updated externally.\n",
    )
    .expect("external edit 3");

    // Verify all three files are on disk with the external content.
    assert!(
        std::fs::read_to_string(&notes_path)
            .expect("read notes_path")
            .contains("External edit 1"),
        "external edit 1 not on disk"
    );
    assert!(
        std::fs::read_to_string(&context_path)
            .expect("read context_path")
            .contains("External edit 2"),
        "external edit 2 not on disk"
    );
    assert!(
        std::fs::read_to_string(&scratch_path)
            .expect("read scratch_path")
            .contains("External edit 3"),
        "external edit 3 not on disk"
    );

    // -----------------------------------------------------------------------
    // Phase H: re-attach after external edits + jj captures final state (3 ops)
    // -----------------------------------------------------------------------

    // Op 35: jj sees the external edits as working-copy modifications.
    let log_after_external = adapter
        .log(&mount_path, "@")
        .expect("jj log after external edits");
    assert!(
        !log_after_external.is_empty(),
        "jj should see a working copy after external edits"
    );

    // Op 36: jj commit captures the externally-edited files.
    adapter
        .commit(&mount_path, "capture external edits")
        .expect("jj commit after external edits");

    // Op 37: re-attach and verify the external file content is accessible.
    // The subscriber/watcher path is what keeps memory_doc in sync in a live
    // session; here we verify the DB attach/detach round-trip still works
    // cleanly after filesystem changes.
    {
        let store = attach(root).expect("attach after external edits failed");

        // The memory.db has the pre-external-edit block content (it was
        // persisted via MemoryStore before the external edit). The on-disk
        // files have the external content. This is the normal split-brain
        // state that the subscriber reconciles in a live session.
        // Verify the mount is healthy and the DB is readable.
        store
            .db
            .health_check()
            .expect("db health after external edits");

        let meta_list = store
            .cache
            .list_blocks(pattern_core::types::memory_types::BlockFilter::by_agent(
                "agent-spike",
            ))
            .expect("list_blocks after external edits");
        assert_eq!(
            meta_list.len(),
            5,
            "DB should still have 5 blocks after external edits"
        );

        store.detach();
    }

    // Op 38: final git snapshot — host git picks up all changes.
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "final snapshot after all ops"]);

    // -----------------------------------------------------------------------
    // Final verification
    // -----------------------------------------------------------------------

    // jj log returns commits (no corruption).
    let final_jj_log = adapter
        .log(&mount_path, "all()")
        .expect("final jj log all()");
    assert!(
        final_jj_log.len() >= 5,
        "jj should have at least 5 commits, got {}",
        final_jj_log.len()
    );

    // git log returns commits.
    let final_git_log = git(root, &["log", "--oneline"]);
    assert!(
        final_git_log.lines().count() >= 2,
        "git should have commits after the spike"
    );

    // .jj/ still exists.
    assert!(
        mount_path.join(".jj").is_dir(),
        ".pattern/shared/.jj/ must exist at end"
    );

    // .gitignore still has the entry.
    let final_gitignore =
        std::fs::read_to_string(root.join(".gitignore")).expect("read .gitignore");
    assert!(
        final_gitignore.contains(".pattern/shared/.jj/"),
        ".gitignore must still contain .pattern/shared/.jj/"
    );

    // Report success.
    eprintln!("--- Sidecar mode validation spike: PASS ---");
    eprintln!("  total ops: 38");
    eprintln!("  jj commits: {}", final_jj_log.len());
    eprintln!("  git commits: {}", final_git_log.lines().count());
    eprintln!("  attach/detach cycles: 3");
    eprintln!("  MemoryStore writes: 5 blocks created");
    eprintln!("  external .md edits: 3");
    eprintln!("  .jj/ intact: true");
    eprintln!("  .gitignore correct: true");
}
