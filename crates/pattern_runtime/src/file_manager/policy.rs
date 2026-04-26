//! `FilePolicy` — KDL-backed ordered rules with last-match-wins evaluation.
//!
//! Rules are evaluated in declaration order; the last matching rule decides.
//! No rule matches → default deny. This mirrors gitignore / rsync `--filter`
//! semantics and is explicitly predictable: reading top-to-bottom is the debug
//! surface.
//!
//! # Examples
//!
//! ```text
//! // .pattern.kdl
//! file-policy {
//!     allow "/project/**"
//!     deny  "/project/.env"
//! }
//! ```
//! → `.env` denied (last match wins), everything else under `/project/` allowed.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use pattern_memory::config::FilePolicyMode;

use crate::file_manager::error::FileError;

/// Direction of a single policy rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    /// Allow access to the matched path.
    Allow,
    /// Deny access to the matched path.
    Deny,
}

impl From<FilePolicyMode> for RuleMode {
    fn from(mode: FilePolicyMode) -> Self {
        match mode {
            FilePolicyMode::Allow => RuleMode::Allow,
            FilePolicyMode::Deny => RuleMode::Deny,
        }
    }
}

/// A compiled rule ready for O(1) matching.
#[derive(Debug, Clone)]
struct Rule {
    mode: RuleMode,
    matcher: GlobMatcher,
    /// Original pattern string, kept for human-readable denial messages.
    pattern: String,
}

/// Ordered file-access policy evaluated using last-match-wins semantics.
///
/// An empty policy denies all paths (default-deny). This should be surfaced
/// to the operator via [`tracing::warn!`] at session open (see `is_empty`).
#[derive(Debug, Clone, Default)]
pub struct FilePolicy {
    rules: Vec<Rule>,
}

impl FilePolicy {
    /// Build from a KDL-decoded [`pattern_memory::config::FilePolicySection`].
    ///
    /// Converts [`FilePolicyMode`] → [`RuleMode`] and delegates to
    /// [`Self::from_rules`]. Rule order is preserved exactly as decoded.
    pub fn from_section(
        section: pattern_memory::config::FilePolicySection,
    ) -> Result<Self, FileError> {
        let rules = section
            .rules
            .into_iter()
            .map(|(mode, pat)| (RuleMode::from(mode), pat))
            .collect();
        Self::from_rules(rules)
    }

    /// Build from an ordered list of `(mode, glob_pattern)` pairs.
    ///
    /// Rules are compiled left-to-right in declaration order.
    /// Returns [`FileError::BadGlob`] if any pattern is malformed.
    pub fn from_rules(rules: Vec<(RuleMode, String)>) -> Result<Self, FileError> {
        let compiled = rules
            .into_iter()
            .map(|(mode, pattern)| {
                let matcher = Glob::new(&pattern)
                    .map_err(|e| FileError::BadGlob(format!("{pattern}: {e}")))?
                    .compile_matcher();
                Ok(Rule {
                    mode,
                    matcher,
                    pattern,
                })
            })
            .collect::<Result<Vec<_>, FileError>>()?;
        Ok(Self { rules: compiled })
    }

    /// Returns `true` if no rules have been added (session-open warning path).
    ///
    /// An empty policy is a valid state — every operation will be denied by
    /// default. Callers are expected to emit a `tracing::warn!` at session open
    /// when this returns `true`.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluate the policy for the given path.
    ///
    /// Paths are canonicalized via [`std::fs::canonicalize`] before matching
    /// so that `..`-escapes like `/project/../etc/passwd` cannot bypass rules
    /// that cover `/project/**`. For paths that do not yet exist on disk
    /// (e.g., write-new), canonicalization is applied to the parent directory
    /// and the filename is appended separately (see implementation).
    ///
    /// Returns `Ok(())` if the last matching rule is `Allow`, or
    /// `Err(FileError::PermissionDenied)` if the last match is `Deny` or there
    /// is no match (default-deny).
    pub fn check_access(&self, path: &Path) -> Result<(), FileError> {
        // Canonicalize so path-traversal attacks cannot escape policy scope.
        // For non-existent files (write-new), try the parent directory first
        // then append the file name; fall back to the path as-given if the
        // parent also doesn't exist.
        let check = canonicalize_best_effort(path);

        let mut decision: Option<(usize, &Rule)> = None;
        for (idx, rule) in self.rules.iter().enumerate() {
            if rule.matcher.is_match(&check) {
                decision = Some((idx, rule));
            }
        }

        match decision {
            Some((_, r)) if r.mode == RuleMode::Allow => Ok(()),
            Some((idx, r)) => Err(FileError::PermissionDenied {
                path: check,
                reason: format!("denied by rule {idx}: {}", r.pattern),
            }),
            None => Err(FileError::PermissionDenied {
                path: check,
                reason: "no matching rule (default deny)".to_string(),
            }),
        }
    }

    /// Convenience constructor for a policy that denies everything (no rules).
    pub fn default_deny_all() -> Self {
        Self::default()
    }
}

/// Delegates to the shared `path_util::canonicalize_best` which lexically
/// normalizes `.`/`..` components then attempts `std::fs::canonicalize` for
/// symlink resolution. Falls back to the lexically normalized path when the
/// file does not exist on disk (write-new case).
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    crate::file_manager::path_util::canonicalize_best(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(pat: &str) -> (RuleMode, String) {
        (RuleMode::Allow, pat.to_string())
    }

    fn deny(pat: &str) -> (RuleMode, String) {
        (RuleMode::Deny, pat.to_string())
    }

    /// AC2.10 example 1: allow-list with a carve-out.
    /// `allow /project/**` then `deny /project/.env` → `.env` denied,
    /// other files under `/project/` allowed.
    #[test]
    fn last_match_wins_allow_then_deny() {
        let policy =
            FilePolicy::from_rules(vec![allow("/project/**"), deny("/project/.env")]).unwrap();

        // .env is denied by the later deny rule.
        let err = policy
            .check_access(Path::new("/project/.env"))
            .expect_err(".env should be denied");
        match err {
            FileError::PermissionDenied { reason, .. } => {
                assert!(
                    reason.contains("denied by rule"),
                    "expected 'denied by rule', got: {reason}"
                );
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }

        // lib.rs is allowed by the earlier allow rule (no later deny matches).
        policy
            .check_access(Path::new("/project/src/lib.rs"))
            .expect("lib.rs should be allowed");
    }

    /// AC2.10 example 2: deny-list with a carve-out.
    /// `deny /project/**` then `allow /project/notes/*.md` →
    /// `notes/foo.md` allowed despite the broad deny.
    #[test]
    fn last_match_wins_deny_then_allow() {
        let policy =
            FilePolicy::from_rules(vec![deny("/project/**"), allow("/project/notes/*.md")])
                .unwrap();

        // notes/foo.md is allowed by the later allow rule.
        policy
            .check_access(Path::new("/project/notes/foo.md"))
            .expect("notes/foo.md should be allowed");

        // Other paths remain denied.
        policy
            .check_access(Path::new("/project/secrets/key.pem"))
            .expect_err("secrets/key.pem should be denied");
    }

    /// AC2.10 example 3: nested re-allow inside a re-deny.
    /// Three rules: `allow /project/**`, `deny /project/secrets/**`,
    /// `allow /project/secrets/public.txt` →
    /// `public.txt` accessible; rest of `secrets/` blocked.
    #[test]
    fn nested_re_allow_inside_re_deny() {
        let policy = FilePolicy::from_rules(vec![
            allow("/project/**"),
            deny("/project/secrets/**"),
            allow("/project/secrets/public.txt"),
        ])
        .unwrap();

        // public.txt is re-allowed by the third rule.
        policy
            .check_access(Path::new("/project/secrets/public.txt"))
            .expect("public.txt should be re-allowed");

        // private.txt stays denied by the second rule.
        policy
            .check_access(Path::new("/project/secrets/private.txt"))
            .expect_err("private.txt should be denied");

        // A normal project file is allowed by the first rule.
        policy
            .check_access(Path::new("/project/src/main.rs"))
            .expect("main.rs should be allowed");
    }

    /// Empty policy: every path is denied with the default-deny message.
    #[test]
    fn default_deny_when_no_rules() {
        let policy = FilePolicy::default_deny_all();
        let err = policy
            .check_access(Path::new("/any/path.txt"))
            .expect_err("empty policy should deny everything");
        match err {
            FileError::PermissionDenied { reason, .. } => {
                assert_eq!(
                    reason, "no matching rule (default deny)",
                    "unexpected denial reason: {reason}"
                );
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
        assert!(policy.is_empty(), "no-rules policy should report is_empty");
    }

    /// Non-empty policy with no rule matching the path: default deny fires.
    #[test]
    fn default_deny_when_no_match() {
        let policy = FilePolicy::from_rules(vec![allow("/project/**")]).unwrap();

        // A path outside `/project/` doesn't match any rule → default deny.
        let err = policy
            .check_access(Path::new("/etc/passwd"))
            .expect_err("/etc/passwd should be denied by default-deny");
        match err {
            FileError::PermissionDenied { reason, .. } => {
                assert_eq!(
                    reason, "no matching rule (default deny)",
                    "unexpected denial reason: {reason}"
                );
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }

    /// Malformed glob patterns must fail loudly with `FileError::BadGlob`.
    #[test]
    fn invalid_glob_fails_loudly() {
        let result = FilePolicy::from_rules(vec![allow("**][bad")]);
        match result {
            Err(FileError::BadGlob(msg)) => {
                assert!(
                    !msg.is_empty(),
                    "BadGlob message should describe the problem"
                );
            }
            Ok(_) => panic!("expected BadGlob error for malformed pattern"),
            Err(other) => panic!("expected BadGlob, got: {other:?}"),
        }
    }

    /// Path traversal via `..` must not escape the policy.
    ///
    /// With only `/project/**` allowed, `/project/../etc/passwd` must be
    /// denied because canonicalization resolves it to `/etc/passwd`.
    ///
    /// Note: this test requires `/project/` to NOT exist on the test machine
    /// so canonicalization falls back to best-effort (parent-only). In CI the
    /// path `/project/` typically doesn't exist, so the traversal check
    /// reduces to verifying that `../..` in the path is not silently elided.
    #[test]
    fn canonicalisation_resists_dotdot_escape() {
        // Prepare a tempdir whose name is "project" so we can test with a
        // real on-disk path — this ensures canonicalize actually fires.
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).expect("create project dir");

        // An adjacent directory that is NOT inside project/.
        let outside_dir = tmp.path().join("etc");
        std::fs::create_dir_all(&outside_dir).expect("create etc dir");
        let outside_file = outside_dir.join("passwd");
        std::fs::write(&outside_file, b"root:x:0:0").expect("write passwd");

        // Allow only project/** (using the actual tempdir path).
        let project_glob = format!("{}/**", project_dir.display());
        let policy = FilePolicy::from_rules(vec![allow(&project_glob)]).unwrap();

        // Direct access to outside_file is denied (no rule covers it).
        policy
            .check_access(&outside_file)
            .expect_err("direct access to /etc/passwd should be denied");

        // Traversal path: project/sub/../../etc/passwd should resolve to
        // tmp/etc/passwd, which is NOT inside project/**.
        let traversal = project_dir
            .join("sub")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");
        policy
            .check_access(&traversal)
            .expect_err("dotdot traversal must not escape policy");
    }

    /// KDL-decoded rules preserve order and produce the same policy as
    /// `from_rules` with an identical sequence. This is the KDL round-trip
    /// test specified in the phase plan.
    ///
    /// `FilePolicySection` implements `knus::DecodeChildren` so the rules
    /// are parsed from a document whose top-level nodes are `allow` / `deny`
    /// lines (without an outer `file-policy` wrapper — the wrapper is handled
    /// by `MountConfig`'s `#[knus(child, default)]` annotation). The test
    /// provides the inner rules directly.
    #[test]
    fn kdl_round_trip_preserves_order() {
        use pattern_memory::config::FilePolicySection;

        // KDL input: top-level allow/deny nodes, order must be preserved.
        // This is what knus sees when it decodes the children of a
        // `file-policy { ... }` node — the outer wrapper is stripped by
        // the `#[knus(child)]` annotation in `MountConfig`.
        let kdl_input = r#"
allow "/tmp/project/**"
deny "/tmp/project/.env"
"#;

        let section: FilePolicySection =
            knus::parse("<test>", kdl_input).expect("KDL parse failed");

        let policy = FilePolicy::from_section(section).expect("from_section failed");

        // .env must be denied (last match: deny rule wins).
        policy
            .check_access(Path::new("/tmp/project/.env"))
            .expect_err(".env must be denied by KDL-decoded policy");

        // A non-.env path under project must be allowed.
        policy
            .check_access(Path::new("/tmp/project/main.rs"))
            .expect("main.rs must be allowed by KDL-decoded policy");

        // Hand-built policy with identical rule sequence must produce the
        // same results, confirming that order was preserved through the KDL
        // decode step.
        let hand_built =
            FilePolicy::from_rules(vec![allow("/tmp/project/**"), deny("/tmp/project/.env")])
                .unwrap();

        assert_eq!(
            hand_built
                .check_access(Path::new("/tmp/project/.env"))
                .is_err(),
            policy.check_access(Path::new("/tmp/project/.env")).is_err(),
            "KDL-decoded and hand-built policies must agree on .env"
        );
        assert_eq!(
            hand_built
                .check_access(Path::new("/tmp/project/main.rs"))
                .is_ok(),
            policy
                .check_access(Path::new("/tmp/project/main.rs"))
                .is_ok(),
            "KDL-decoded and hand-built policies must agree on main.rs"
        );
    }
}
