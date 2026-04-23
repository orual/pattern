//! The `JjAdapter` — a thin wrapper over the `jj` CLI.
//!
//! All jj invocations go through [`JjAdapter::cmd`], which universally applies
//! `--color=never` to suppress ANSI codes in parsed output (AC8.8).
//!
//! Workspace-mutation functions acquire [`JjAdapter::mutation_lock`] before
//! spawning jj to serialize against the concurrent-workspace-add hazard
//! documented in jj-vcs/jj#9314.
//!
//! # Detection
//!
//! [`JjAdapter::detect`] probes for `jj` on PATH and validates the version.
//! It returns `Ok(None)` when jj is absent (InRepo mode is fine without it) and
//! `Err(JjError::UnsupportedVersion)` when jj is present but too old.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use super::error::{JjError, JjResult};
use super::types::{JjBookmark, JjLogEntry, JjWorkspace};
use super::{templates, version};

/// Thin wrapper over the `jj` CLI.
///
/// Constructed via [`JjAdapter::detect`]. Holds the absolute path to the
/// `jj` binary and the detected version. Serializes workspace mutations via
/// an internal [`Mutex`] to avoid jj's documented concurrent-workspace-add
/// hazard (jj-vcs/jj#9314).
pub struct JjAdapter {
    binary: std::path::PathBuf,
    version: semver::Version,
    mutation_lock: Mutex<()>,
}

impl std::fmt::Debug for JjAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JjAdapter")
            .field("binary", &self.binary)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl JjAdapter {
    /// Probe for `jj` on PATH and validate its version.
    ///
    /// Returns:
    /// - `Ok(Some(_))` — a supported jj was found (AC8.1).
    /// - `Ok(None)` — jj is not on PATH; InRepo mode continues normally (AC8.5).
    /// - `Err(JjError::UnsupportedVersion)` — jj found but below the minimum
    ///   supported version (AC8.6).
    /// - `Err(_)` — other probe failures (I/O errors, subprocess failures).
    pub fn detect() -> JjResult<Option<Self>> {
        let binary = match which::which("jj") {
            Ok(path) => path,
            // Missing binary is not an error — InRepo mode works without jj.
            Err(_) => return Ok(None),
        };

        let output = Command::new(&binary)
            .args(["--color", "never", "--version"])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "invoking jj --version".into(),
            })?;

        if !output.status.success() {
            return Err(JjError::SubprocessFailed {
                command: "jj --version".into(),
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let raw = String::from_utf8_lossy(&output.stdout).to_string();
        let parsed_version = version::parse_jj_version(raw.trim())?;

        if !version::is_supported(&parsed_version) {
            return Err(JjError::UnsupportedVersion {
                installed: parsed_version.to_string(),
                min: version::MIN_SUPPORTED_VERSION.into(),
            });
        }

        if !version::is_tested(&parsed_version) {
            tracing::warn!(
                installed = %parsed_version,
                tested_max = version::MAX_TESTED_VERSION,
                "jj version exceeds Pattern's regression-tested maximum; \
                 proceed with caution if behavior drift is observed"
            );
        }

        Ok(Some(Self {
            binary,
            version: parsed_version,
            mutation_lock: Mutex::new(()),
        }))
    }

    /// The detected jj version.
    pub fn version(&self) -> &semver::Version {
        &self.version
    }

    /// Build a base [`Command`] with `--color=never` universally applied.
    ///
    /// All adapter functions call this rather than `Command::new` directly,
    /// ensuring AC8.8: no ANSI escape codes appear in parsed output.
    pub(crate) fn cmd(&self) -> Command {
        let mut c = Command::new(&self.binary);
        c.args(["--color", "never"]);
        c
    }

    // -------------------------------------------------------------------------
    // Read-only functions
    // -------------------------------------------------------------------------

    /// List commits matching `revset` in the given workspace.
    ///
    /// Invokes `jj log -r <revset> --no-graph -T 'json(self) ++ "\n"'` and
    /// returns one [`JjLogEntry`] per commit.
    ///
    /// # Errors
    ///
    /// Returns [`JjError::SubprocessFailed`] for an invalid revset or other
    /// jj errors (AC8.7).
    pub fn log(&self, workspace_root: &Path, revset: &str) -> JjResult<Vec<JjLogEntry>> {
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args([
                "log",
                "-r",
                revset,
                "--no-graph",
                "-T",
                templates::LOG_TEMPLATE,
            ])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj log".into(),
            })?;
        check_success(&output, "jj log")?;
        parse_jsonl::<JjLogEntry>(&output.stdout, "jj log")
    }

    /// List all workspaces in the repository.
    ///
    /// Invokes `jj workspace list -T 'json(self) ++ "\n"'` and returns one
    /// [`JjWorkspace`] per workspace.
    pub fn workspace_list(&self, repo_root: &Path) -> JjResult<Vec<JjWorkspace>> {
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args([
                "workspace",
                "list",
                "-T",
                templates::WORKSPACE_LIST_TEMPLATE,
            ])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj workspace list".into(),
            })?;
        check_success(&output, "jj workspace list")?;
        parse_jsonl::<JjWorkspace>(&output.stdout, "jj workspace list")
    }

    /// List all bookmarks in the repository.
    ///
    /// Invokes `jj bookmark list -T 'json(self) ++ "\n"'` and returns one
    /// [`JjBookmark`] per bookmark.
    pub fn bookmark_list(&self, repo_root: &Path) -> JjResult<Vec<JjBookmark>> {
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args(["bookmark", "list", "-T", templates::BOOKMARK_LIST_TEMPLATE])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj bookmark list".into(),
            })?;
        check_success(&output, "jj bookmark list")?;
        parse_jsonl::<JjBookmark>(&output.stdout, "jj bookmark list")
    }

    // -------------------------------------------------------------------------
    // Mutation functions (all acquire mutation_lock)
    // -------------------------------------------------------------------------

    /// Initialise a new jj git repository at the given path.
    ///
    /// Invokes `jj git init --no-colocate` at `path`. The `--no-colocate`
    /// flag keeps the backing git repository inside `.jj/repo/` rather than
    /// creating a top-level `.git/` directory. This is important for Sidecar mode
    /// where a top-level `.git/` would cause the host git to treat the mount
    /// directory as a nested repository, and harmless for Standalone mode (which has
    /// no host VCS to conflict with).
    pub fn init_repo(&self, path: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(path)
            .args(["git", "init", "--no-colocate"])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj git init --no-colocate".into(),
            })?;
        check_success(&output, "jj git init --no-colocate")
    }

    /// Add a new workspace at `new_workspace_path` linked to the repo at
    /// `repo_root`.
    ///
    /// Acquires the mutation lock to avoid the concurrent-workspace-add hazard
    /// documented in jj-vcs/jj#9314.
    pub fn workspace_add(&self, repo_root: &Path, new_workspace_path: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let ws_str = new_workspace_path.to_string_lossy();
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args(["workspace", "add", ws_str.as_ref()])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj workspace add".into(),
            })?;
        check_success(&output, "jj workspace add")
    }

    /// Forget (unregister) a workspace by name.
    ///
    /// jj prints a warning and exits 0 when the workspace does not exist, so
    /// we detect the not-found case by inspecting stderr. Returns
    /// [`JjError::WorkspaceNotFound`] when the warning indicates no such
    /// workspace was known.
    pub fn workspace_forget(&self, repo_root: &Path, name: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args(["workspace", "forget", name])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj workspace forget".into(),
            })?;
        // jj exits 0 even for unknown workspaces, writing "Warning: No such
        // workspace: <name>" to stderr and "Nothing changed." to stderr.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such workspace") {
            return Err(JjError::WorkspaceNotFound { name: name.into() });
        }
        check_success(&output, "jj workspace forget")
    }

    /// Update a stale workspace to the current operation state.
    ///
    /// Required when the working copy has been left behind by an operation
    /// run in another workspace. Invokes `jj workspace update-stale`.
    pub fn workspace_update_stale(&self, workspace_root: &Path) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args(["workspace", "update-stale"])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj workspace update-stale".into(),
            })?;
        check_success(&output, "jj workspace update-stale")
    }

    /// Commit the working copy changes with the given message.
    ///
    /// Invokes `jj commit -m <message>`. If the working copy is empty (no
    /// changes), jj still creates an empty commit and proceeds normally.
    pub fn commit(&self, workspace_root: &Path, message: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args(["commit", "-m", message])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj commit".into(),
            })?;
        check_success(&output, "jj commit")
    }

    /// Update the working-copy commit's description without creating a new commit.
    ///
    /// Invokes `jj describe -m <message>`.
    pub fn describe(&self, workspace_root: &Path, message: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args(["describe", "-m", message])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj describe".into(),
            })?;
        check_success(&output, "jj describe")
    }

    /// Set a bookmark to point at a revset.
    ///
    /// Invokes `jj bookmark set <name> -r <revset>`.
    pub fn bookmark_set(&self, repo_root: &Path, name: &str, revset: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args(["bookmark", "set", name, "-r", revset])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj bookmark set".into(),
            })?;
        check_success(&output, "jj bookmark set")
    }

    /// Delete a bookmark by name.
    ///
    /// jj exits 0 even when the bookmark does not exist, printing a warning to
    /// stderr. We detect the not-found case by inspecting stderr and return
    /// [`JjError::BookmarkNotFound`] in that case.
    pub fn bookmark_delete(&self, repo_root: &Path, name: &str) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let output = self
            .cmd()
            .current_dir(repo_root)
            .args(["bookmark", "delete", name])
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj bookmark delete".into(),
            })?;
        // jj exits 0 for missing bookmarks, writing "Warning: No matching
        // bookmarks for names: <name>" to stderr.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No matching bookmarks") {
            return Err(JjError::BookmarkNotFound { name: name.into() });
        }
        check_success(&output, "jj bookmark delete")
    }

    /// Create a new commit with multiple parents (merge).
    ///
    /// Uses `jj new <parents...>` — `jj merge` was deprecated in jj 0.14.0.
    /// If `message` is given, immediately describes the new commit within the
    /// same lock guard to prevent another thread from interposing a mutation
    /// between the `jj new` and `jj describe` calls.
    pub fn merge(
        &self,
        workspace_root: &Path,
        parent_revs: &[&str],
        message: Option<&str>,
    ) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let mut args: Vec<&str> = vec!["new"];
        args.extend_from_slice(parent_revs);
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj new (merge)".into(),
            })?;
        check_success(&output, "jj new (merge)")?;
        if let Some(msg) = message {
            let output = self
                .cmd()
                .current_dir(workspace_root)
                .args(["describe", "-m", msg])
                .output()
                .map_err(|e| JjError::Io {
                    source: e,
                    context: "jj describe (merge)".into(),
                })?;
            check_success(&output, "jj describe (merge)")?;
        }
        Ok(())
    }

    /// Restore paths in the working copy from a source revision.
    ///
    /// Invokes `jj restore --from <from_rev> [paths...]`. If `paths` is empty,
    /// restores all tracked paths.
    pub fn restore(&self, workspace_root: &Path, from_rev: &str, paths: &[&Path]) -> JjResult<()> {
        let _guard = self.mutation_lock.lock().map_err(|_| poisoned())?;
        let mut args: Vec<String> = vec!["restore".into(), "--from".into(), from_rev.into()];
        for p in paths {
            args.push(p.to_string_lossy().into_owned());
        }
        let output = self
            .cmd()
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .map_err(|e| JjError::Io {
                source: e,
                context: "jj restore".into(),
            })?;
        check_success(&output, "jj restore")
    }
}

// -------------------------------------------------------------------------
// Free functions used by adapter methods
// -------------------------------------------------------------------------

/// Return an error when the mutation lock was poisoned by a prior panic.
fn poisoned() -> JjError {
    JjError::SubprocessFailed {
        command: "internal mutex".into(),
        status: -2,
        stderr: "mutation lock was poisoned by a prior panic".into(),
    }
}

/// Check that a subprocess exited successfully; map failure to
/// [`JjError::SubprocessFailed`].
pub(super) fn check_success(output: &std::process::Output, cmd: &str) -> JjResult<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(JjError::SubprocessFailed {
        command: cmd.into(),
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Parse newline-delimited JSON (NDJSON) bytes into a `Vec<T>`.
///
/// Blank lines are skipped. Returns [`JjError::OutputParseFailed`] if any
/// non-blank line fails to deserialize.
pub(super) fn parse_jsonl<T: for<'de> serde::Deserialize<'de>>(
    bytes: &[u8],
    cmd: &str,
) -> JjResult<Vec<T>> {
    let text = std::str::from_utf8(bytes).map_err(|e| JjError::OutputParseFailed {
        command: cmd.into(),
        reason: format!("invalid utf-8: {e}"),
    })?;
    let mut out = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: T = serde_json::from_str(line).map_err(|e| JjError::OutputParseFailed {
            command: cmd.into(),
            reason: format!("line {}: {e}", line_no + 1),
        })?;
        out.push(v);
    }
    Ok(out)
}
