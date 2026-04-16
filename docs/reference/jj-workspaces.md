# Jujutsu Workspaces: Isolation and Sharing

A focused reference on Jujutsu (jj) workspace semantics for informing Pattern's design decision between ephemeral subagent forks as workspaces versus isolated repos.

**Last updated**: 2026-04-16  
**Scope**: jj 0.15+  
**Official docs**: [jj-vcs.dev](https://docs.jj-vcs.dev/latest/) and [jj-vcs.github.io](https://jj-vcs.github.io/jj/latest/)

## 1. Workspace Fundamentals

### What is a jj workspace?

In Jujutsu, a **workspace** is a working copy paired with metadata that links it to a shared repository. The working copy contains the actual files you edit; the `.jj/` metadata directory stores pointers to the repo store.

Multiple workspaces backed by a single repository allow different working copies to coexist, each checking out a different commit, with independent uncommitted state. This is jj's native answer to Git's `git-worktree`.

### Core commands

**`jj workspace add <name> [--sparse-patterns ...]`**  
Create a new working copy at the specified path. The new workspace inherits the sparse patterns of the current workspace unless overridden. The new workspace gets a fresh working-copy commit pointing at the same revision as the parent workspace's `@` (current working-copy commit).

**`jj workspace list`**  
List all workspaces in the repository. Output shows the workspace name and path.

**`jj workspace forget <name>`**  
Remove the repository's knowledge of a workspace. The files on disk are not automatically deleted; you must delete them separately (before or after).

**`jj workspace update-stale`**  
Update the working copy to match the repository's current state when the working copy has become stale. "Stale" means the working-copy metadata (stored in `.jj/working_copy/`) indicates the working copy was last updated at an older operation than the current operation. If the operation itself was lost (e.g., by `jj op abandon`), `update-stale` creates a recovery commit.

### Directory layout

Assume the main workspace root is `/repo`:

```
/repo/
  .jj/
    repo/              # Symlinked or hard-linked to shared store
      store/           # Commit backend, operation log, etc.
        git            # (Git backend: points to or contains .git)
      working_copy/    # Metadata: which operation this workspace matched last
    working_copy.toml  # (Deprecated but may exist)

/repo-workspace-2/
  .jj/
    repo/              # Symlink to /repo/.jj/repo
    working_copy/      # Independent metadata for this workspace
```

Each workspace has its own `.jj/working_copy/` state file; the `.jj/repo/` is shared (typically via symlink or hard link) across all workspaces.

### What workspaces share

- **Commit store**: All commits ever created.
- **Operation log**: The complete history of all operations, including divergent forks.
- **Bookmarks/branches**: All named references (though they can conflict across workspaces—see section 3).
- **Tags**: All tags.
- **Git refs** (if backed by Git): All remote tracking branches, fetch refspecs, etc.
- **Configuration**: `.jj/repo/config/` (global repo config).

### What workspaces do NOT share

- **Working copy**: The actual files on disk are independent per workspace.
- **`@` (working-copy commit pointer)**: Each workspace has its own current commit.
- **Working-copy metadata** (`.jj/working_copy/`): Tracks which operation last updated that workspace's files.
- **Uncommitted state**: Changes made in one workspace do not appear in another's working copy.

---

## 2. Workspaces vs Git Worktrees: Precise Differences

### Similarities

Both jj workspaces and Git worktrees allow multiple working copies of a single repository without full cloning.

### Key differences

| Aspect | Git worktrees | jj workspaces |
|--------|---------------|---------------|
| **Branch constraint** | One branch per worktree; git enforces this. | No branch constraint. Multiple workspaces can have the same commit checked out. |
| **Create workflow** | Must create a new branch first, then `git worktree add <path> <branch>`. | Just `jj workspace add <path>`; workspace gets a fresh working-copy commit at current revision. |
| **Visibility of changes** | Changes in one worktree are not visible to another until committed. | Same: working copy is isolated. |
| **Concurrent commits** | Possible (each branch can diverge independently). | Possible; divergences are recorded in operation log. |
| **Cross-workspace reference updates** | If worktree A checks out a commit that worktree B's branch pointed to, worktree B sees no change. | If workspace A modifies a commit that workspace B depends on, workspace B's working copy becomes stale (see **stale working copy** below). |
| **Lock semantics** | Uses file locks; checkout is serialized per ref. | Lock-free: uses operation log to detect and merge divergent ops. |
| **Distributed filesystem support** | Lock-file approach breaks on NFS/Dropbox. | Lock-free design survives NFS/Dropbox sync (with caveats; see section 2.5). |

### Operation log visibility

When workspace A runs a command, it loads the repo at the latest operation *at that moment*. It does not see concurrent changes written by workspace B *during* that command's execution. However, once workspace A completes and writes its own operation, workspace B's next command will load at the true latest operation and see both chains.

**Can workspace A see workspace B's operations immediately?** Not during its own execution, but yes as of its next command start.

### Concurrent editing: working copy isolation

Two workspaces can have completely different uncommitted changes simultaneously. There are no locks preventing this. Each workspace's `.jj/working_copy/` file records which operation it last matched, so `jj status` in workspace A will not affect workspace B's working copy.

### Stale working copy

**When is it needed?**

The most common case: workspace B modifies the working-copy commit of workspace A. This happens because jj allows modifying a commit from any workspace, not just the workspace that checked it out.

Steps:
1. Workspace A checks out commit `abc123`.
2. Workspace B runs `jj describe --revision @-A` (modify A's working-copy commit).
3. Workspace A's `.jj/working_copy/` still says "last updated at operation X," but the current operation is now Y.
4. A's working copy becomes stale.

**What does `jj workspace update-stale` fix?**

It re-snapshots the working copy and checks it out to match the current operation's view. If the operation that created the staleness is still in the log, the files are simply re-written. If the operation was lost (e.g., `jj op abandon`), a recovery commit is created, preserving the working-copy contents even though the original operation is gone.

---

## 3. Levels of Isolation/Sharing: The Trade-Off Space

For Pattern's "parallel subagent fork" use case, here are the concrete isolation levels:

### (a) Single workspace (default, not viable)

All agents share one `.jj/working_copy/`. The working copy becomes a thrashing, conflicted mess. Agents step on each other's files.

**Verdict**: Wrong for parallel agents.

### (b) Multiple workspaces, same repo

**Setup**: Run `jj workspace add agent-1 /tmp/agent-1` and `jj workspace add agent-2 /tmp/agent-2` from the root repo.

**Sharing**:
- All commits are visible to all workspaces.
- All bookmarks/branches are visible to all workspaces (and can conflict).
- Operation log is shared; divergent operations are detected and merged.

**Isolation**:
- Working copies are separate.
- Uncommitted changes do not cross workspaces.
- Each workspace has independent `@` (working-copy commit).

**Concurrency semantics**:
- Two workspaces can run commands in parallel without locks.
- If workspace A commits and workspace B was reading at the moment A committed, B does not see the commit until B runs its next command (due to lock-free snapshotting).
- If workspace A modifies workspace B's working-copy commit, B becomes stale and must run `jj workspace update-stale` to recover.

**Bookmark conflicts**:
- If agent-1 creates a bookmark `agent-1/task-1` and agent-2 creates `agent-1/task-1` concurrently pointing to different commits, the bookmark becomes "conflicted" (shown with `??` in logs). Neither agent can use `jj new agent-1/task-1` without error.
- **Solution**: Use namespaced bookmark names (`agent-1/my-feature` vs `agent-2/my-feature`) to avoid collisions.

**Verdict**: Cheap (no duplication), transparent cross-visibility (agents can see each other's commits), but requires careful naming discipline and stale-workspace handling.

### (c) Forked/cloned repos

**Setup**: `jj git clone <url> agent-1` and `jj git clone <url> agent-2`.

**Isolation**:
- Complete: separate commit stores, separate operation logs, separate bookmarks.
- Agents cannot see each other's commits until they push/pull.
- Crashes in one repo do not affect the other.

**Cost**:
- Disk overhead: full clone per agent.
- Push/pull overhead: must explicitly `jj git push` and `jj git pull` to sync.
- Merge complexity: if agent-1 and agent-2 both modify the same file on the same branch, merging back requires manual conflict resolution.

**Verdict**: Maximum isolation, maximum overhead. Suitable for long-lived, independent branches but overkill for ephemeral task forks.

### (d) Middle ground: shared store, isolated operation logs (theoretical)

Jujutsu does not officially support this. The operation log is per-repository; there is no "shared operation log, isolated working copies" mode.

---

## 4. Colocated jj (jj over an existing Git repo)

### What `--colocate` does

`jj git init --colocate` (or `jj git clone --colocate`) creates a workspace where both `.jj/` and `.git/` directories exist at the root, sharing the same working copy.

**Automatic import/export**: On every `jj` command, Jujutsu imports the latest Git refs and exports jj changes back to Git refs. This keeps `.git` synchronized with `.jj`'s view.

**Mixed commands**: You can run `git` and `jj` commands in any order:
```bash
jj new -m "add feature"
git diff
jj squash
git log --oneline
```

### Can git commands still work?

**Yes, with caveats:**
- `git log`, `git show`, `git diff` work as expected.
- `git status` usually shows a clean working copy (because jj snapshots eagerly) or shows conflicts if jj left conflict markers.
- `git commit` will commit on top of the current Git HEAD. If jj has modified the HEAD (likely to a detached state), `git commit` may create unexpected forks.
- `git switch` or `git checkout` can put Git into a different state than jj, potentially creating a "diverged change ID" conflict or confusing branch pointers.

**Best practice**: Use `jj` for mutations; use `git` only for reads or if you explicitly run `git switch <branch>` first.

### Stability: experimental or production-ready?

Colocation is the **default for `jj git init` and `jj git clone`**, so it is expected to be stable. However, the documentation lists several caveats:

1. **IDE interference**: IDEs that auto-run `git fetch` in the background can cause interleaving and branch conflicts.
2. **Large branch counts**: With hundreds of branches, the automatic import on each command becomes slow.
3. **Conflict representation**: Git tools see jj's internal conflict markers (`.jjconflict-*` files), not human-readable conflict markers.
4. **Change ID loss**: Git tools may not preserve jj's custom `jj:change-id` header, leading to "diverged change IDs."
5. **NFS/Dropbox**: Colocated repos are less resilient to concurrent access via shared filesystems.

**Known bugs**: The documentation warns that "there may still be bugs when interleaving mutating `jj` and `git` commands." These are typically minor (branch pointers ending up in wrong places), but should be kept in mind for automation.

### Data loss risk with concurrent operations?

**No**. The lock-free operation log prevents data loss. However, bookmark pointers *may* end up in unexpected states, and committed changes could be orphaned if not properly tracked.

---

## 5. Nested Scenarios: jj Workspace in a Git Subdirectory

### Can jj initialize in a subdirectory of a git-tracked repo?

**Short answer**: Yes, jj can be initialized anywhere. There is no inherent block.

### Mode C: Pattern use case

**Scenario**: The host repo (Pattern's monorepo) is Git. Pattern wants to initialize a jj workspace at `<host-repo>/.pattern/shared/` for subagent forks.

**Design**:
```
pattern/                           # Git root
  .git/
  .pattern/
    shared/                        # jj workspace root
      .jj/
        repo/
          store/
            git                    # jj's internal Git backend store
      agent-1/                     # subagent fork workspace
        .jj/
          repo/                    # symlink to shared/repo
```

### Safety and feasibility

**No official docs or blockers** for this configuration, but it is not explicitly documented or tested by the jj team.

**Considerations**:

1. **Git tracking of `.jj/`**: The host Git repo will track the `.jj/` directory unless you explicitly `.gitignore` it. Simple solution: add `/.pattern/shared/.jj/` to the host `.gitignore`.

2. **File locking**: Both Git and jj write to the working copy. Because jj uses eager snapshotting (writing a commit whenever you run any command), and Git also writes during checkout, there is a *potential* for writes to race if a hook or background process in Git modifies files while jj is running. However, jj's operation log is lock-free, so even if a race occurs, no data is lost; the operation log simply records divergent operations.

3. **jj's workspace root discovery**: jj searches up the directory tree for the closest `.jj/` directory. If you are in `/pattern/.pattern/shared/agent-1/`, jj will find `/pattern/.pattern/shared/.jj/` and use that. This is correct behavior.

4. **Concurrent writes on NFS/Dropbox**: If the host repo is on a network filesystem, colocated jj + git concurrency issues are amplified. See section 4 caveats.

5. **Git submodule interactions**: If the Pattern monorepo uses submodules, and submodules themselves are jj repos, no official guidance exists. Jujutsu does not yet support submodules.

**Verdict**: Mode C is **likely safe** (no data loss risk thanks to lock-free ops), but not officially tested. Best practices:
- Add `.pattern/shared/.jj/` to host `.gitignore`.
- Keep jj and Git mutations separate; don't interleave in the same directory.
- Use colocate=false if using NFS/Dropbox.
- Run `jj workspace update-stale` after any Git operations that may have modified commits.

---

## 6. Library/Embedding: jj-lib

### Is jj-lib stable and production-ready?

**jj-lib** is the Rust library crate used by the jj CLI. It is designed to be embeddable in GUIs, TUIs, and servers. However:

- **API stability**: The documentation notes "not much has gone into details such as which collection types are used, or which symbols are exposed in the API." Expect API churn.
- **No formal stability guarantee**: No semantic versioning policy is documented.
- **Crates.io presence**: [jj-lib is published](https://crates.io/crates/jj-lib), but with no stability tags.

### Production use

A few projects embed jj-lib:

- **agentjj** ([GitHub](https://github.com/2389-research/agentjj)): Explicitly designed for AI agents. Embeds jj-lib as the VCS engine. This is a strong signal that jj-lib's API is usable for agent workloads.
- **jujutsu-skill**: Agent skill for Claude Code using jj. Calls jj as a subprocess (not embedding jj-lib directly).

No major mainstream projects (e.g., IDEs, large code hosts) yet embed jj-lib, so it is still in an early adoption phase.

### Relevant jj-lib modules for Pattern

Based on the [jj-lib architecture doc](https://jj-vcs.github.io/jj/latest/technical/architecture/):

- **`jj_lib::workspace`**: Types for managing workspaces (`Workspace`, `WorkingCopy`).
- **`jj_lib::repo_loader`**: `RepoLoader` for opening repos at specified operations.
- **`jj_lib::transaction`**: `Transaction` for atomic batches of operations (commits, branch moves, etc.).
- **`jj_lib::commit`**: Commit creation and inspection.
- **`jj_lib::bookmark`**: Bookmark manipulation (equivalent to branches in Git).
- **`jj_lib::revsets`**: Querying commits via revset expressions.
- **`jj_lib::backend`**: Backend trait for swappable commit stores (Git, Simple, etc.).

### Workspace API availability?

Yes. The architecture doc specifically describes `Workspace` as "a pointer to `.jj/repo/` and working copy state." From a `Workspace`, you can obtain a `WorkingCopy` or `RepoLoader`. The `jj_lib::workspace` module is part of the public API.

**However**, the workspace lifecycle (add, forget, list) may be CLI-only. Verify by checking the `jj-lib` crate source to confirm if `WorkspaceFactory` or similar exists.

---

## 7. Practical Recommendations for Pattern's Design

### Ephemeral subagent forks (minutes to hours)

**Recommendation: Use option (b)—multiple workspaces, same repo.**

**Rationale**:
- **Cost**: Negligible disk overhead (only working-copy files, not full commit store).
- **Speed**: Instant creation (`jj workspace add`).
- **Visibility**: Agents can see each other's commits and bookmarks, enabling knowledge sharing (useful for multi-agent coordination).
- **Convergence**: If agent-1 writes a commit that agent-2 depends on, agent-2's `jj workspace update-stale` picks it up automatically.

**Implementation**:
```bash
# Pattern main service
jj workspace add agent-1 /tmp/agent-1
jj workspace add agent-2 /tmp/agent-2
jj workspace add agent-3 /tmp/agent-3

# Each agent subprocess initializes in its workspace:
# agent-1 runs `jj -R /tmp/agent-1 new -m "task-1"` and works there.
```

**Gotchas**:
- Use namespaced bookmarks: `agent-1/task-1`, not just `task-1`.
- After cross-workspace modifications, call `jj workspace update-stale` in the affected workspace.
- Be aware that concurrent `jj` commands load at the snapshot when they start, so there can be temporary divergence.

### Long-lived persona sibling branches (days to months)

**Recommendation: Use option (b) for shared history, but with strong naming isolation.**

Alternatively, if you want zero cross-visibility, use option (c) and accept the disk/sync overhead.

### Mode C (Pattern jj workspace in git subdirectory)

**Recommendation: Safe to use, but follow best practices.**

- Add `.pattern/shared/.jj/` to Pattern's `.gitignore`.
- Do not colocate within the Pattern monorepo root; keep the jj workspace in a subdirectory.
- Test on your actual filesystem (local disk, NFS, etc.) before deploying.
- Document the setup clearly for team members.

### Should you use jj-lib directly, or call jj CLI?

**For Pattern's scope**:
- **Minimal wrapper over CLI** is safest today: spawn `jj` subprocess, parse output.
- **agentjj's approach** shows jj-lib is viable for agents, but API may churn.
- **Recommendation**: Start with CLI wrapping. If performance becomes critical (many `jj` invocations), evaluate jj-lib migration.

---

## 8. Known Unknowns and Caveats

### What is definitively NOT documented

1. **Nested jj repos**: Can you have a jj workspace whose working copy is itself a jj repo? No official guidance.
2. **Concurrent bookmark creation**: Two agents create the same bookmark simultaneously. jj handles this (records conflict), but best practices for agent automation are unclear.
3. **jj-lib API stability**: No SemVer guarantee. Breaking changes may occur.
4. **Performance at scale**: How many workspaces before performance degrades? No benchmarks published.

### Known issues relevant to Pattern

- **[#2193](https://github.com/jj-vcs/jj/issues/2193)**: Git backend is not entirely lock-free; repository corruption possible with colocated repos under concurrent NFS access. Workaround: `jj debug reindex`.
- **Concurrency caveat**: With NFS/Dropbox, colocated jj+git can lose bookmark pointers (though commits are safe).
- **IDE interference**: Tools that auto-run `git fetch` can corrupt state in colocated repos.

---

## 9. References and Links

### Official documentation
- [Jujutsu main docs](https://docs.jj-vcs.dev/latest/)
- [Working copy (includes workspace section)](https://docs.jj-vcs.dev/latest/working-copy/)
- [Git compatibility](https://docs.jj-vcs.dev/latest/git-compatibility/)
- [Operation log](https://docs.jj-vcs.dev/latest/operation-log/)
- [Concurrency (lock-free design)](https://docs.jj-vcs.dev/latest/technical/concurrency/)
- [Architecture](https://docs.jj-vcs.dev/latest/technical/architecture/)
- [jj-lib API docs](https://docs.rs/jj-lib/latest/jj_lib/)

### Community/adjacent resources
- [agentjj: VCS for AI agents](https://github.com/2389-research/agentjj)
- [Jujutsu skill for Claude Code](https://github.com/danverbraganza/jujutsu-skill)
- [Comparison of jj workspaces vs git worktrees](https://gist.github.com/ruvnet/60e5749c934077c7040ab32b542539d0)
- [Using jj in colocated git repos](https://cuffaro.com/2025-03-15-using-jujutsu-in-a-colocated-git-repository/)
- [Avoid losing work with jj for AI coding agents](https://www.panozzaj.com/blog/2025/11/22/avoid-losing-work-with-jujutsu-jj-for-ai-coding-agents/)

### GitHub repository
- [jj-vcs/jj](https://github.com/jj-vcs/jj)

---

## Summary

For Pattern's ephemeral subagent parallel forks, **option (b)—multiple workspaces in the same repo—is the best fit**. It provides cheap parallelism, transparency, and automatic divergence detection. Use namespaced bookmark names to avoid collisions. For nested jj in a git subdirectory (Mode C), follow the best practices in section 5; the approach is safe but not officially blessed. If you embed jj-lib, be prepared for API churn and start with CLI wrapping as a safer interim solution.
