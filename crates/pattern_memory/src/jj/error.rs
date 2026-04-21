//! Error types for the jj CLI adapter.
//!
//! [`JjError`] covers all failure modes: missing binary, unsupported version,
//! subprocess failures, output parse failures, and not-found conditions for
//! workspaces and bookmarks. Mode A tolerates `Ok(None)` from
//! [`super::adapter::JjAdapter::detect`]; Modes B/C surface these errors loudly
//! at attach time.

use miette::Diagnostic;
use thiserror::Error;

/// All errors produced by the jj CLI adapter.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum JjError {
    /// The `jj` binary was not found on PATH. Not an error in Mode A (it just
    /// returns `Ok(None)` from detect); surfaced as an error only when
    /// explicitly required.
    #[error("jj binary not found on PATH")]
    #[diagnostic(
        code(pattern_memory::jj::binary_not_found),
        help(
            "install jj via your package manager or https://jj-vcs.github.io/jj/install-and-setup/. Pattern requires jj >= {min}"
        )
    )]
    BinaryNotFound {
        /// The minimum supported version.
        min: String,
    },

    /// jj was found but its version is below the minimum Pattern supports.
    #[error("jj version {installed} is below minimum supported {min}")]
    #[diagnostic(
        code(pattern_memory::jj::unsupported_version),
        help("upgrade jj to {min} or later")
    )]
    UnsupportedVersion {
        /// The installed version string.
        installed: String,
        /// The minimum required version string.
        min: String,
    },

    /// The `jj --version` output could not be parsed as a semver version.
    #[error("could not parse jj --version output: {raw}")]
    #[diagnostic(code(pattern_memory::jj::version_parse))]
    VersionParse {
        /// The raw output that failed to parse.
        raw: String,
    },

    /// A jj subprocess exited with a non-zero status code.
    #[error("jj subprocess failed (exit {status}): {stderr}")]
    #[diagnostic(code(pattern_memory::jj::subprocess_failed))]
    SubprocessFailed {
        /// The jj command that was invoked (for diagnostics).
        command: String,
        /// The exit code, or -1 if unavailable.
        status: i32,
        /// The stderr output from jj.
        stderr: String,
    },

    /// A jj subprocess succeeded but its output could not be parsed.
    #[error("jj output parse failed for command {command}: {reason}")]
    #[diagnostic(code(pattern_memory::jj::output_parse))]
    OutputParseFailed {
        /// The jj command whose output failed to parse.
        command: String,
        /// Human-readable reason for the parse failure.
        reason: String,
    },

    /// A workspace lookup by name found no match.
    #[error("workspace not found: {name}")]
    #[diagnostic(code(pattern_memory::jj::workspace_not_found))]
    WorkspaceNotFound {
        /// The workspace name that was not found.
        name: String,
    },

    /// A bookmark lookup by name found no match.
    #[error("bookmark not found: {name}")]
    #[diagnostic(code(pattern_memory::jj::bookmark_not_found))]
    BookmarkNotFound {
        /// The bookmark name that was not found.
        name: String,
    },

    /// An I/O error occurred while invoking jj.
    #[error("io error invoking jj: {source}")]
    #[diagnostic(code(pattern_memory::jj::io))]
    Io {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
        /// Human-readable context describing what operation triggered the error.
        context: String,
    },
}

/// Convenience alias for [`Result`] with [`JjError`].
pub type JjResult<T> = Result<T, JjError>;
