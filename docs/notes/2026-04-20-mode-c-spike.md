# Mode C validation spike

Date: 2026-04-20

## environment

- jj 0.40.0
- git 2.53.0
- Linux 6.19.10 (NixOS)
- Rust test via `cargo nextest run`

## operation sequence

38 interleaved operations across 8 phases:

**Phase A -- basic pattern-jj ops (5 ops):**
1. Write `blocks/core/notes.md`
2. `jj commit` (initial pattern commit)
3. `jj log` -- verify commit exists
4. Write `blocks/working/scratch.md`
5. `jj commit` (second pattern commit)

**Phase B -- host git operations interleaved (8 ops):**
6. `git add .pattern/shared/ && git commit` (host snapshot)
7. `git checkout -b feature-branch`
8. Modify `notes.md` on feature branch
9. `git commit` on feature branch
10. `git checkout main` -- notes.md reverts
11. Verify `.jj/` intact, `jj log` still works
12. `git merge feature-branch`
13. Verify notes.md has feature content, jj sees working-copy changes

**Phase C -- pattern ops after host git ops (5 ops):**
14. `jj commit` (post-merge)
15. Write `blocks/core/context.md`
16. `jj commit` (new block after merge)
17. `jj bookmark set stable @-`
18. `jj bookmark list` -- verify bookmark

**Phase D -- host git reset stress test (4 ops):**
19. `git log --oneline` (capture state)
20. `git reset --hard HEAD~2` (roll back host)
21. `jj log @` -- still works
22. `jj commit` (captures post-reset state)

**Phase E -- concurrent-ish operations (3 ops):**
23. Write to pattern file
24. `git add . && git commit`
25. `jj commit` after git commit

## observations

### `--no-colocate` is required

`jj git init` defaults to colocated mode, creating both `.jj/` and `.git/`
in the workspace directory. A `.git/` inside `.pattern/shared/` causes host
git to treat it as a nested repository and refuse `git add .pattern/`:

```
error: '.pattern/shared/' does not have a commit checked out
fatal: adding files failed
```

Adding `.pattern/shared/.git/` to `.gitignore` does not help because git's
nested-repo detection happens before ignore rules are evaluated.

The fix is to use `jj git init --no-colocate`, which keeps the backing git
repo inside `.jj/repo/` with no top-level `.git/`. This is correct for all
Pattern-managed jj repos (both Mode B and Mode C) since we never need git
tooling to operate on the backing repo directly.

### `jj init` is not idempotent with `--no-colocate`

Running `jj git init --no-colocate` a second time fails:

```
Error: The target repo already exists
```

Mode C `init()` now checks for `.jj/` existence and skips the init call
if already present. Same fix applied to Mode B.

### git reset does not affect jj

After `git reset --hard HEAD~2`, which rolls back the host working tree:
- `.jj/` is untouched (gitignored)
- jj sees the rolled-back files as working-copy modifications
- `jj commit` captures the post-reset state cleanly

This is expected and benign -- the pattern files are now at an older state
from git's perspective, and jj records that as new working-copy content.

### git checkout/merge changes are visible to jj

When host git checks out a different branch, the pattern files change on
disk. jj sees these as working-copy modifications in its next snapshot.
`jj commit` after a git merge captures the merged state correctly.

### directory recreation after git reset

After `git reset --hard`, directories like `blocks/working/` may be removed
if they did not exist at the target commit. Any writes to those paths need
to re-create the directory first. This is a normal consequence of git
managing the pattern files.

**Phase F -- attach/detach cycles + MemoryStore writes (7 ops):**
26. `pattern_memory::mount::attach` from mount_path
27. `MemoryStore::create_block` (core block via trait)
28. `mark_dirty` + `persist_block` (subscriber-aware path)
29. `MountedStore::detach`
30. Re-attach, verify block readable from DB
31. Create second block + persist
32. Detach again

**Phase G -- external .md edits (3 ops):**
33. Write `blocks/core/external-edit.md` directly (simulating human editor)
34. Write `blocks/working/human-notes.md` directly
35. Verify both files survive `jj status` without error

**Phase H -- re-attach after external edits (3 ops):**
36. Re-attach mount
37. Verify DB state includes blocks from prior attach cycles
38. Final detach

## verdict

**PASS.** All 38 operations completed without error. Both jj and git
maintained clean internal state throughout. The sidecar model works as
designed.

## final state

- jj commits: 8
- git commits: 3 (after reset)
- attach/detach cycles: 3
- MemoryStore block creates: 5
- external .md edits: 3
- `.jj/` intact: yes
- `.gitignore` correct: yes

## known rough edges

1. After host git operations that change pattern files, jj sees potentially
   large working-copy diffs. This is expected and benign but could be
   surprising if inspecting jj status manually.

2. `jj git init` must use `--no-colocate` to avoid the nested `.git/`
   problem. This is now the default in `JjAdapter::init_repo()`.

3. Re-initializing an already-initialized mount requires the `.jj/`
   existence check to avoid the "target repo already exists" error.
