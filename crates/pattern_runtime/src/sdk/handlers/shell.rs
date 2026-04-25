//! Stub handler for `Pattern.Shell`, gated by the session's
//! [`pattern_core::PolicySet`] and per-runtime
//! [`pattern_core::permission::PermissionBroker`].
//!
//! Phase 1 Task 10: the handler now evaluates the policy pipeline
//! before its existing "not implemented" stub error. Real command
//! execution still arrives in the post-foundation shell-tool plan; this
//! task only wires the gate.
//!
//! Decision flow per [`ShellReq::Execute`]:
//!
//! - [`pattern_core::PolicyAction::Deny`] → handler errors with the
//!   [`crate::policy::PERMISSION_DENIED_PREFIX`] prefix.
//! - [`pattern_core::PolicyAction::RequireApproval`] → escalate via
//!   [`crate::permission::PermissionBridge::request_sync`]. On approval,
//!   error with [`crate::policy::GATE_APPROVED_PREFIX`] (real exec
//!   lives in a later plan); on denial / timeout, error with
//!   `PERMISSION_DENIED_PREFIX`.
//! - [`pattern_core::PolicyAction::Allow`] → existing "not implemented"
//!   stub, so AC2.2's gate-skip path is observable (no `GateApproved:`
//!   marker means the gate did not fire).
//!
//! Spawn/Kill/Status are not yet gated — Phase 1 Task 10's scope is
//! only Execute. Spawn arrives with the real shell-tool plan.

use std::time::Duration;

use pattern_core::permission::PermissionScope;
use pattern_core::{EffectCategory, PolicyAction, PolicyContext};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::policy::{GATE_APPROVED_PREFIX, PERMISSION_DENIED_PREFIX};
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::ShellReq;
use crate::session::{HasCancelState, HasPermissionBridge, HasPolicySet};
use crate::timeout::HandlerGuard;

/// Default broker-request timeout. Long enough to absorb a human
/// thinking; short enough that a stalled responder surfaces as a
/// denial rather than hanging the agent indefinitely.
const SHELL_GATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Not-implemented placeholder for the Shell effect. Real implementation
/// arrives in the post-foundation shell-tool plan (reuses preserved PTY
/// backend + `ProcessSource`). Phase 1 Task 10 gates the stub through
/// the policy pipeline.
#[derive(Default, Clone)]
pub struct ShellHandler;

impl DescribeEffect for ShellHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Shell",
            description: "Shell command execution (Execute/Spawn/Kill/Status)",
            constructors: &[
                "Execute :: Command -> Shell Text",
                "Spawn   :: Command -> Shell Pid",
                "Kill    :: Pid -> Shell ()",
                "Status  :: Pid -> Shell Text",
            ],
            type_defs: &["type Command = Text", "type Pid = Integer"],
            helpers: &[
                "execute :: Member Shell effs => Command -> Eff effs Text\nexecute c = send (Execute c)",
                "spawn_ :: Member Shell effs => Command -> Eff effs Pid\nspawn_ c = send (Spawn c)",
                "kill :: Member Shell effs => Pid -> Eff effs ()\nkill p = send (Kill p)",
                "status :: Member Shell effs => Pid -> Eff effs Text\nstatus p = send (Status p)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for ShellHandler
where
    U: HasCancelState + HasPolicySet + HasPermissionBridge,
{
    type Request = ShellReq;

    fn handle(&mut self, req: ShellReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Enter the HandlerGate uniformly with the wired handlers so the
        // watchdog's "has any handler been entered recently" bookkeeping
        // does not mistakenly see a stub-only agent as non-yielding.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        match req {
            ShellReq::Execute(command) => evaluate_execute(&command, cx.user()),
            other => stub_not_implemented(&other),
        }
    }
}

/// Evaluate an Execute request against the policy pipeline + broker.
fn evaluate_execute<U>(command: &str, user: &U) -> Result<Value, EffectError>
where
    U: HasPolicySet + HasPermissionBridge,
{
    let policy_ctx = PolicyContext::Shell { command };
    match user.policies().evaluate(EffectCategory::Shell, &policy_ctx) {
        PolicyAction::Deny { reason } => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}{}",
            reason.unwrap_or_else(|| "shell denied by policy".into())
        ))),
        PolicyAction::RequireApproval { reason } => {
            let Some(bridge) = user.permission_bridge() else {
                // Bridge missing means the runtime hasn't wired the
                // broker yet — fail closed rather than allowing.
                return Err(EffectError::Handler(format!(
                    "{PERMISSION_DENIED_PREFIX}shell gated by policy but no permission bridge \
                     is wired"
                )));
            };
            // Origin defaults to a benign system origin if the runtime
            // hasn't published a dispatch origin (e.g. handler invoked
            // outside drive_step). Direct-execution paths overwrite the
            // slot before invocation; production turns always have one.
            let origin = user.current_dispatch_origin().unwrap_or_else(|| {
                pattern_core::types::origin::MessageOrigin::new(
                    pattern_core::types::origin::Author::System {
                        reason: pattern_core::types::origin::SystemReason::Timer,
                    },
                    pattern_core::types::origin::Sphere::System,
                )
            });
            let scope = PermissionScope::ToolExecution {
                tool: "shell".into(),
                args_digest: Some(short_digest(command)),
            };
            let agent = pattern_core::AgentId::from("shell-handler-agent");
            let grant = bridge.request_sync(
                agent,
                "shell".into(),
                scope,
                &origin,
                reason,
                None,
                SHELL_GATE_TIMEOUT,
            );
            if grant.is_some() {
                Err(EffectError::Handler(format!(
                    "{GATE_APPROVED_PREFIX}Pattern.Shell.Execute is not implemented in v3 \
                     foundation (phase: post-foundation shell-tool plan); gate cleared, real \
                     command execution lands later"
                )))
            } else {
                Err(EffectError::Handler(format!(
                    "{PERMISSION_DENIED_PREFIX}shell denied or timed out at the broker"
                )))
            }
        }
        PolicyAction::Allow => Err(EffectError::Handler(
            "Pattern.Shell.Execute is not implemented in v3 foundation \
             (phase: post-foundation shell-tool plan). Agent code should \
             not call Shell effects in v3-foundation-scope programs."
                .into(),
        )),
        // PolicyAction is #[non_exhaustive]; treat any future variant
        // as a denial until the handler is taught about it.
        other => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}unhandled policy action {other:?}"
        ))),
    }
}

/// Return the existing not-implemented stub for non-Execute variants.
/// Spawn/Kill/Status come online with the real shell handler in a
/// later plan; Phase 1 leaves them ungated.
fn stub_not_implemented(req: &ShellReq) -> Result<Value, EffectError> {
    Err(EffectError::Handler(format!(
        "Pattern.Shell.{req:?} is not implemented in v3 foundation \
         (phase: post-foundation shell-tool plan). Agent code should \
         not call Shell effects in v3-foundation-scope programs."
    )))
}

/// Short stable digest of a command string. Used to namespace
/// approve-for-scope cache entries so two distinct commands don't
/// share a single grant.
fn short_digest(command: &str) -> String {
    blake3::hash(command.as_bytes()).to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::permission::{PermissionBroker, PermissionDecisionKind};
    use pattern_core::types::origin::{Author, Human, MessageOrigin, Sphere};
    use pattern_core::{PolicyMatcher, PolicyRule, PolicySet, Precedence};
    use std::sync::Arc;
    use tidepool_repr::DataConTable;

    /// Minimal user struct that satisfies all three trait bounds for the
    /// Shell handler. Lets us drive the gate paths without standing up a
    /// full SessionContext.
    struct TestUser {
        policies: pattern_core::PolicySet,
        bridge: Option<Arc<crate::permission::PermissionBridge>>,
        origin: Option<MessageOrigin>,
    }

    impl HasCancelState for TestUser {
        fn cancel_state(&self) -> Arc<crate::timeout::CancelState> {
            Arc::new(crate::timeout::CancelState::new())
        }
    }
    impl HasPolicySet for TestUser {
        fn policies(&self) -> &pattern_core::PolicySet {
            &self.policies
        }
    }
    impl HasPermissionBridge for TestUser {
        fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
            self.bridge.as_ref()
        }
        fn current_dispatch_origin(&self) -> Option<MessageOrigin> {
            self.origin.clone()
        }
    }

    fn human_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Human(Human {
                user_id: pattern_core::types::ids::new_id(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    fn shell_rule(action: PolicyAction) -> PolicyRule {
        PolicyRule::new(
            EffectCategory::Shell,
            PolicyMatcher::Always,
            action,
            Precedence::RuntimeOverride,
        )
    }

    #[test]
    fn shell_stub_reports_not_implemented_with_empty_policies() {
        // AC2.2 gate-skip path: empty PolicySet produces Allow → existing
        // stub error fires unchanged (no `GateApproved:` marker).
        let mut h = ShellHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(ShellReq::Execute("ls".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Shell"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(
            !msg.contains(GATE_APPROVED_PREFIX),
            "Allow path must not carry GateApproved marker, got: {msg}"
        );
        assert!(
            !msg.contains(PERMISSION_DENIED_PREFIX),
            "Allow path must not carry PermissionDenied marker, got: {msg}"
        );
    }

    #[test]
    fn deny_action_returns_permission_denied_prefix() {
        let user = TestUser {
            policies: PolicySet::from_rules([shell_rule(PolicyAction::Deny {
                reason: Some("explicit deny".into()),
            })]),
            bridge: None,
            origin: Some(human_origin()),
        };
        let mut h = ShellHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &user);
        let err = h
            .handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with(&format!("Handler error: {PERMISSION_DENIED_PREFIX}"))
                || msg.contains(PERMISSION_DENIED_PREFIX),
            "expected PermissionDenied prefix in message, got: {msg}"
        );
        assert!(msg.contains("explicit deny"), "got: {msg}");
    }

    #[test]
    fn require_approval_without_bridge_fails_closed() {
        let user = TestUser {
            policies: PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval {
                reason: None,
            })]),
            bridge: None,
            origin: Some(human_origin()),
        };
        let mut h = ShellHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &user);
        let err = h.handle(ShellReq::Execute("ls".into()), &cx).unwrap_err();
        assert!(
            err.to_string().contains(PERMISSION_DENIED_PREFIX),
            "missing bridge should fail closed, got: {err}"
        );
    }

    #[tokio::test]
    async fn require_approval_with_approving_bridge_returns_gate_approved() {
        // AC2.1 approve path (stub): broker approves → handler returns
        // GateApproved-prefixed error so the test can distinguish the
        // approve-after-gate path from the gate-skip path.
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));

        // The handler's request_sync blocks the calling thread, so it
        // must run on a worker thread to keep the tokio runtime free
        // to poll the bridge pump.
        let bridge_for_thread = bridge.clone();
        let result = tokio::task::spawn_blocking(move || {
            let user = TestUser {
                policies: PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval {
                    reason: Some("rm-rf-style command".into()),
                })]),
                bridge: Some(bridge_for_thread),
                origin: Some(human_origin()),
            };
            let mut h = ShellHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("handler always errors in Phase 1");
        let msg = result.to_string();
        assert!(
            msg.contains(GATE_APPROVED_PREFIX),
            "expected GateApproved marker after approval, got: {msg}"
        );
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn require_approval_with_denying_bridge_returns_permission_denied() {
        // AC2.1 deny path: broker denies → handler returns
        // PermissionDenied-prefixed error.
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));

        let bridge_for_thread = bridge.clone();
        let result = tokio::task::spawn_blocking(move || {
            let user = TestUser {
                policies: PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval {
                    reason: None,
                })]),
                bridge: Some(bridge_for_thread),
                origin: Some(human_origin()),
            };
            let mut h = ShellHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("handler always errors in Phase 1");
        let msg = result.to_string();
        assert!(
            msg.contains(PERMISSION_DENIED_PREFIX),
            "expected PermissionDenied marker after denial, got: {msg}"
        );
        responder.await.unwrap();
    }
}
