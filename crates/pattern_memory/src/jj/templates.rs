//! Template string constants for jj CLI commands.
//!
//! All templates use `json(self) ++ "\n"` which outputs the full self object
//! as JSON followed by a newline. This produces newline-delimited JSON (NDJSON)
//! that [`super::adapter`] parses with [`super::adapter::parse_jsonl`].
//!
//! Template verification (jj 0.40.0, 2026-04-20):
//! - `LOG_TEMPLATE`: confirmed produces `{"commit_id":..., "change_id":...,
//!   "description":..., "parents":..., "author":..., "committer":...}` per
//!   commit. We deserialize only the fields we need.
//! - `WORKSPACE_LIST_TEMPLATE`: confirmed produces
//!   `{"name":"default","target":{"commit_id":...,...}}` per workspace.
//!   `target` is a full commit object; [`super::types::JjWorkspaceTarget`]
//!   captures only `commit_id`.
//! - `BOOKMARK_LIST_TEMPLATE`: confirmed produces
//!   `{"name":"...", "target":["<commit_id>", ...]}` per bookmark. `target`
//!   is an array of commit ID strings (conflict-aware representation).

/// Template for `jj log -T '<this>' --no-graph`.
///
/// Produces one JSON line per commit. Serde deserialization into
/// [`super::types::JjLogEntry`] is forgiving of extra fields.
pub const LOG_TEMPLATE: &str = r#"json(self) ++ "\n""#;

/// Template for `jj workspace list -T '<this>'`.
///
/// Produces one JSON line per workspace. The `target` field is a full commit
/// object; deserialized into [`super::types::JjWorkspace`] which extracts
/// only `target.commit_id`.
pub const WORKSPACE_LIST_TEMPLATE: &str = r#"json(self) ++ "\n""#;

/// Template for `jj bookmark list -T '<this>'`.
///
/// Produces one JSON line per bookmark. The `target` field is an array of
/// commit IDs (normally length 1; length > 1 means a conflicted bookmark).
pub const BOOKMARK_LIST_TEMPLATE: &str = r#"json(self) ++ "\n""#;
