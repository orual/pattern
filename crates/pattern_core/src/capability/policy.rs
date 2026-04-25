//! Policy types: declarative rules that govern whether an effect call
//! is allowed, gated, or denied at runtime.
//!
//! Rules are pure data — `pattern_core` keeps them as values so that the
//! runtime can layer different rule sources (Rust defaults, KDL config,
//! runtime overrides) into a single [`PolicySet`] and evaluate them
//! against per-call [`PolicyContext`]s. Concrete enforcement (Shell
//! handler, File handler) lives in `pattern_runtime`; this module only
//! defines the language.
//!
//! Precedence order (highest wins): [`Precedence::RuntimeOverride`] →
//! [`Precedence::KdlConfig`] → [`Precedence::RustDefault`]. Within a
//! single precedence tier, the first matching rule wins.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::capability::EffectCategory;
use crate::permission::PermissionScope;

/// What a [`PolicyRule`] dictates when its matcher fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PolicyAction {
    /// Effect proceeds without escalation.
    Allow,
    /// Effect must escalate through the
    /// [`crate::permission::PermissionBroker`] for approval.
    RequireApproval {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Effect is rejected outright; no approval is solicited.
    Deny {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Where a rule sits in the precedence chain.
///
/// Higher-precedence rules win conflicts. Within a single precedence,
/// the first matching rule in iteration order wins (so callers ordering
/// rules within their own tier can still express tiebreakers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Precedence {
    /// Baseline — built-in conservative defaults seeded by
    /// `pattern_runtime::policy::defaults`.
    RustDefault,
    /// Loaded from `.pattern.kdl` (project) or persona KDL.
    KdlConfig,
    /// Imperative override — admin command, debug surface, etc.
    /// Wins over `RustDefault` and `KdlConfig` but yields to
    /// `LockedDefault`.
    RuntimeOverride,
    /// Built-in rule that no KDL config or runtime override can
    /// loosen. Reserved for security-critical defaults whose action
    /// must hold regardless of how the persona / partner / admin
    /// configures the session — e.g. the shape-detection guard for
    /// writes to Pattern's own config files.
    LockedDefault,
}

impl Precedence {
    /// Numeric weight used by [`PolicySet::evaluate`] to sort rules.
    /// Higher = wins.
    fn weight(self) -> u8 {
        match self {
            Self::RustDefault => 0,
            Self::KdlConfig => 1,
            Self::RuntimeOverride => 2,
            Self::LockedDefault => 3,
        }
    }
}

/// Predicate component of a [`PolicyRule`]. The runtime's
/// [`PolicyContext`] decides whether the matcher fires.
///
/// Glob semantics for [`PolicyMatcher::ShellCommand`] /
/// [`PolicyMatcher::FilePath`]: `*` matches any run of characters, `?`
/// matches one character; `[abc]`-style classes pass through to the
/// regex backend. Brace expansion is *not* supported. Patterns compile
/// to anchored regexes at evaluation time, so a pattern like `rm -rf*`
/// matches `rm -rf /tmp/x` but not `do rm -rf x`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PolicyMatcher {
    /// Always fires — useful as a catchall under a tighter rule.
    Always,
    /// Matches a shell command string against a glob.
    ShellCommand { pattern: String },
    /// Matches a filesystem path against a glob.
    FilePath { pattern: String },
    /// Matches a [`PermissionScope`] exactly. Useful for tying a
    /// policy rule to a specific tool / data-source action.
    Scope(PermissionScope),
    /// Built-in shape-based predicate over `(path, content)`. Used by
    /// the runtime to wire a `LikelyConfig` shape-guard rule that no
    /// KDL config can construct.
    ///
    /// Carries a function pointer rather than a closure so the rule
    /// remains `Clone` + `Debug` without hidden state. Not serializable
    /// — KDL-loaded rules can never produce this variant; runtime
    /// defaults are kept in memory only.
    #[serde(skip)]
    FileWriteShape(fn(&Path, &[u8]) -> bool),
}

/// One declarative gate rule: when a call against `effect` matches
/// `matcher` at evaluation time, apply `action`. `precedence` decides
/// who wins layered conflicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PolicyRule {
    pub effect: EffectCategory,
    pub matcher: PolicyMatcher,
    pub action: PolicyAction,
    pub precedence: Precedence,
}

impl PolicyRule {
    /// Construct a rule. Use this rather than struct-literal syntax so
    /// the `#[non_exhaustive]` marker holds — future fields can be
    /// added without breaking external callers.
    pub fn new(
        effect: EffectCategory,
        matcher: PolicyMatcher,
        action: PolicyAction,
        precedence: Precedence,
    ) -> Self {
        Self {
            effect,
            matcher,
            action,
            precedence,
        }
    }
}

/// Runtime context fed into [`PolicySet::evaluate`]. Each variant
/// carries the per-call data a [`PolicyMatcher`] needs.
///
/// Borrows lifetimes from the caller — the runtime constructs one of
/// these per effect dispatch and discards it after evaluation.
#[derive(Debug)]
#[non_exhaustive]
pub enum PolicyContext<'a> {
    Shell {
        command: &'a str,
    },
    FileWrite {
        path: &'a Path,
        content: &'a [u8],
    },
    /// Catch-all for effects that don't carry a matchable predicate.
    /// Matches `PolicyMatcher::Always` only.
    Generic,
}

/// A composed set of rules.
///
/// Construct via [`PolicySet::new`] (empty), [`PolicySet::from_rules`],
/// or by merging multiple `Vec<PolicyRule>` sources at session open
/// (Phase 1 Task 14). Evaluation is `O(n)` over the rules each call —
/// the rule count is small (low double digits) so a sort + linear scan
/// is fine.
#[derive(Debug, Clone, Default)]
pub struct PolicySet {
    rules: Vec<PolicyRule>,
}

impl PolicySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rules<I: IntoIterator<Item = PolicyRule>>(iter: I) -> Self {
        Self {
            rules: iter.into_iter().collect(),
        }
    }

    pub fn rules(&self) -> &[PolicyRule] {
        &self.rules
    }

    /// Add a rule. Used by tests / direct callers; production code
    /// composes via [`PolicySet::merge`] or builds the full vec
    /// up-front.
    pub fn push(&mut self, rule: PolicyRule) {
        self.rules.push(rule);
    }

    /// Evaluate the set against an effect call.
    ///
    /// Returns the action of the highest-precedence matching rule.
    /// If no rules match, returns [`PolicyAction::Allow`] — policy is
    /// opt-in; the broker is the gate of last resort.
    ///
    /// Within a precedence tier, the first matching rule wins.
    pub fn evaluate(&self, effect: EffectCategory, context: &PolicyContext<'_>) -> PolicyAction {
        // Highest precedence first; stable sort preserves source-order
        // tiebreakers within a tier.
        let mut by_precedence: Vec<&PolicyRule> =
            self.rules.iter().filter(|r| r.effect == effect).collect();
        by_precedence.sort_by_key(|r| std::cmp::Reverse(r.precedence.weight()));

        for rule in by_precedence {
            if matcher_fires(&rule.matcher, context) {
                return rule.action.clone();
            }
        }
        PolicyAction::Allow
    }
}

/// Test whether a matcher fires against the given context.
///
/// Mismatched shapes (e.g. a [`PolicyMatcher::ShellCommand`] against a
/// [`PolicyContext::FileWrite`]) never fire — rules are scoped by their
/// `effect` field, but the runtime can in principle hand any context
/// to any rule, so the matcher must defend itself.
fn matcher_fires(matcher: &PolicyMatcher, context: &PolicyContext<'_>) -> bool {
    match (matcher, context) {
        (PolicyMatcher::Always, _) => true,
        (PolicyMatcher::ShellCommand { pattern }, PolicyContext::Shell { command }) => {
            glob_matches(pattern, command)
        }
        (PolicyMatcher::FilePath { pattern }, PolicyContext::FileWrite { path, .. }) => path
            .to_str()
            .map(|s| glob_matches(pattern, s))
            .unwrap_or(false),
        (PolicyMatcher::FileWriteShape(check), PolicyContext::FileWrite { path, content }) => {
            check(path, content)
        }
        // Scope matcher is currently unused at this layer — Phase 1 wires
        // it in when policy gates start consulting `PermissionScope`
        // directly. Returns false until then.
        (PolicyMatcher::Scope(_), _) => false,
        // Shape mismatch.
        _ => false,
    }
}

/// Translate a small glob vocabulary into an anchored regex match.
///
/// Supported metacharacters: `*` (run of any chars), `?` (one char),
/// `[abc]` (char class — passes through to the regex backend).
/// Everything else is regex-escaped. Brace expansion is intentionally
/// not supported.
fn glob_matches(pattern: &str, input: &str) -> bool {
    let mut regex_src = String::with_capacity(pattern.len() + 4);
    regex_src.push('^');
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => regex_src.push_str(".*"),
            '?' => regex_src.push('.'),
            '[' => {
                // Pass through char class verbatim — caller is
                // responsible for escaping any nested metachars they
                // don't want interpreted by the regex engine.
                regex_src.push('[');
                for inner in chars.by_ref() {
                    regex_src.push(inner);
                    if inner == ']' {
                        break;
                    }
                }
            }
            other => regex_src.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex_src.push('$');
    match regex::Regex::new(&regex_src) {
        Ok(re) => re.is_match(input),
        Err(err) => {
            tracing::warn!(
                "policy glob {pattern:?} compiled to invalid regex {regex_src:?}: {err}"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(
        effect: EffectCategory,
        matcher: PolicyMatcher,
        action: PolicyAction,
        precedence: Precedence,
    ) -> PolicyRule {
        PolicyRule {
            effect,
            matcher,
            action,
            precedence,
        }
    }

    fn shell_ctx(command: &str) -> PolicyContext<'_> {
        PolicyContext::Shell { command }
    }

    #[test]
    fn empty_set_evaluates_to_allow() {
        let set = PolicySet::new();
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("anything")),
            PolicyAction::Allow
        );
    }

    #[test]
    fn precedence_runtime_override_beats_kdl_beats_default() {
        let set = PolicySet::from_rules([
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::RequireApproval { reason: None },
                Precedence::RustDefault,
            ),
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::Allow,
                Precedence::KdlConfig,
            ),
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::Deny { reason: None },
                Precedence::RuntimeOverride,
            ),
        ]);
        // RuntimeOverride wins.
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("ls")),
            PolicyAction::Deny { reason: None }
        );
    }

    #[test]
    fn precedence_kdl_overrides_default() {
        let set = PolicySet::from_rules([
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::RequireApproval { reason: None },
                Precedence::RustDefault,
            ),
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::Allow,
                Precedence::KdlConfig,
            ),
        ]);
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("ls")),
            PolicyAction::Allow
        );
    }

    #[test]
    fn shell_command_matcher_matches_globs() {
        let m = PolicyMatcher::ShellCommand {
            pattern: "rm -rf*".into(),
        };
        assert!(matcher_fires(&m, &shell_ctx("rm -rf /")));
        assert!(matcher_fires(&m, &shell_ctx("rm -rf foo/bar")));
        assert!(!matcher_fires(&m, &shell_ctx("ls")));
    }

    #[test]
    fn shell_command_matcher_handles_question_mark_and_classes() {
        let m = PolicyMatcher::ShellCommand {
            pattern: "ec?o *".into(),
        };
        assert!(matcher_fires(&m, &shell_ctx("echo hi")));
        assert!(matcher_fires(&m, &shell_ctx("ecbo hi")));
        assert!(!matcher_fires(&m, &shell_ctx("echox hi")));

        let m = PolicyMatcher::ShellCommand {
            pattern: "git [pf]ush*".into(),
        };
        assert!(matcher_fires(&m, &shell_ctx("git push origin main")));
        assert!(matcher_fires(&m, &shell_ctx("git fush")));
        assert!(!matcher_fires(&m, &shell_ctx("git rebase")));
    }

    #[test]
    fn rules_for_other_effects_are_ignored() {
        let set = PolicySet::from_rules([rule(
            EffectCategory::File,
            PolicyMatcher::Always,
            PolicyAction::Deny { reason: None },
            Precedence::RuntimeOverride,
        )]);
        // Asking about Shell — File rule must not apply.
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("ls")),
            PolicyAction::Allow
        );
    }

    #[test]
    fn first_match_wins_within_precedence_tier() {
        let set = PolicySet::from_rules([
            rule(
                EffectCategory::Shell,
                PolicyMatcher::ShellCommand {
                    pattern: "rm -rf*".into(),
                },
                PolicyAction::Deny { reason: None },
                Precedence::RustDefault,
            ),
            rule(
                EffectCategory::Shell,
                PolicyMatcher::Always,
                PolicyAction::RequireApproval { reason: None },
                Precedence::RustDefault,
            ),
        ]);
        // First matching rule (ShellCommand) wins over the catchall.
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("rm -rf /tmp")),
            PolicyAction::Deny { reason: None }
        );
        // Non-matching first rule falls through to the Always.
        assert_eq!(
            set.evaluate(EffectCategory::Shell, &shell_ctx("ls")),
            PolicyAction::RequireApproval { reason: None }
        );
    }

    #[test]
    fn file_path_matcher_against_file_write_context() {
        use std::path::PathBuf;
        let m = PolicyMatcher::FilePath {
            pattern: "*/.pattern.kdl".into(),
        };
        let path = PathBuf::from("/proj/.pattern.kdl");
        let ctx = PolicyContext::FileWrite {
            path: &path,
            content: b"",
        };
        assert!(matcher_fires(&m, &ctx));

        let path2 = PathBuf::from("/proj/notes.md");
        let ctx2 = PolicyContext::FileWrite {
            path: &path2,
            content: b"",
        };
        assert!(!matcher_fires(&m, &ctx2));
    }
}
