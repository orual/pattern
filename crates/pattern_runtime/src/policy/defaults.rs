//! Rust default policy rules — the conservative baseline applied to
//! every session before KDL config or runtime overrides layer on top.
//!
//! Defaults are kept short and documented with a one-line `// why:`
//! comment per rule. Speculative or "feels prudent" rules don't belong
//! here — every entry must point at a concrete failure mode the rule
//! prevents.

use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};

/// Build the baseline `Vec<PolicyRule>` seeded into every session's
/// [`pattern_core::PolicySet`] before KDL / runtime overrides layer on.
///
/// Returns `Vec` so callers can extend or shadow individual rules
/// before composing the final set (Phase 1 Task 14's `PolicySet::merge`).
pub fn rust_defaults() -> Vec<PolicyRule> {
    vec![
        // why: rm -rf is destructive and cannot be reasonably automated
        // without a human in the loop.
        shell_require_approval("rm -rf*", "rm -rf invocation"),
        // why: sudo elevates privileges beyond the agent's process, an
        // explicit consent moment for the partner.
        shell_require_approval("sudo*", "sudo invocation"),
        // why: mkfs reformats block devices; trivial typo is catastrophic.
        shell_require_approval("mkfs*", "mkfs reformats block devices"),
        // why: `dd if=` can clobber arbitrary blocks given a wrong `of=`;
        // gate any `dd` reading from a source.
        shell_require_approval("dd if=*", "dd write potentially clobbers data"),
        // why: chmod -R 000 locks files out of every user, including
        // root in some configurations; recovery is painful.
        shell_require_approval("chmod -R 000*", "chmod -R 000 locks files"),
        // (Pattern config KDL writes are gated at the File-handler
        // level via `policy::config_guard::is_pattern_config_kdl`, not
        // through a PolicyRule. See `sdk/handlers/file.rs` for the
        // handler-level short-circuit; the policy system is therefore
        // never consulted for config-KDL writes, so no `KdlConfig` or
        // `RuntimeOverride` rule can loosen the gate.)

        // why: spawning a new persona identity (rather than a child of
        // the calling agent) is a high-trust operation — Phase 2 wires
        // the Spawn handler that consults this rule.
        PolicyRule::new(
            EffectCategory::Spawn,
            PolicyMatcher::Always,
            PolicyAction::RequireApproval {
                reason: Some("spawning a new persona identity".into()),
            },
            Precedence::RustDefault,
        ),
    ]
}

fn shell_require_approval(pattern: &str, reason: &str) -> PolicyRule {
    PolicyRule::new(
        EffectCategory::Shell,
        PolicyMatcher::ShellCommand {
            pattern: pattern.to_string(),
        },
        PolicyAction::RequireApproval {
            reason: Some(reason.to_string()),
        },
        Precedence::RustDefault,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::PolicyContext;
    use pattern_core::PolicySet;

    fn shell_ctx(command: &str) -> PolicyContext<'_> {
        PolicyContext::Shell { command }
    }

    #[test]
    fn defaults_gate_destructive_shell_commands() {
        let set = PolicySet::from_rules(rust_defaults());
        for cmd in &[
            "rm -rf /",
            "rm -rf /tmp/foo",
            "sudo apt install nope",
            "mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
            "chmod -R 000 /etc",
        ] {
            assert!(
                matches!(
                    set.evaluate(EffectCategory::Shell, &shell_ctx(cmd)),
                    PolicyAction::RequireApproval { .. }
                ),
                "{cmd:?} should require approval"
            );
        }
    }

    #[test]
    fn defaults_allow_benign_shell_commands() {
        let set = PolicySet::from_rules(rust_defaults());
        for cmd in &["ls", "echo hi", "git status", "cargo check"] {
            assert_eq!(
                set.evaluate(EffectCategory::Shell, &shell_ctx(cmd)),
                PolicyAction::Allow,
                "{cmd:?} should pass under defaults"
            );
        }
    }

    #[test]
    fn defaults_do_not_gate_arbitrary_file_writes() {
        // Phase 1 default policy intentionally has NO File rule —
        // config-KDL writes are gated at the handler level (see
        // `sdk/handlers/file.rs`); other File writes pass through.
        // The locked-invariant tests for config writes live with the
        // File handler in Task 15.
        use std::path::PathBuf;
        let set = PolicySet::from_rules(rust_defaults());
        let path = PathBuf::from("/proj/notes.md");
        let ctx = PolicyContext::FileWrite {
            path: &path,
            content: b"",
        };
        assert_eq!(
            set.evaluate(EffectCategory::File, &ctx),
            PolicyAction::Allow
        );
    }

    #[test]
    fn defaults_gate_spawn_new_identity() {
        let set = PolicySet::from_rules(rust_defaults());
        let ctx = PolicyContext::Generic;
        assert!(matches!(
            set.evaluate(EffectCategory::Spawn, &ctx),
            PolicyAction::RequireApproval { .. }
        ));
    }
}
