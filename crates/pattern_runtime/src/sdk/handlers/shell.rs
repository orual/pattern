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
            // Real session agent_id is load-bearing for per-agent
            // isolation of the broker's scope cache (keyed
            // `(agent_id, scope)`). Without it we'd silently share
            // grants across agents in the same runtime — fail closed.
            let Some(agent) = user.dispatch_agent_id() else {
                return Err(EffectError::Handler(format!(
                    "{PERMISSION_DENIED_PREFIX}shell gated but no agent identity \
                     available for broker attribution"
                )));
            };
            let scope = PermissionScope::ToolExecution {
                tool: "shell".into(),
                args_digest: Some(short_digest(command)),
            };
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
        agent_id: pattern_core::AgentId,
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
        fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId> {
            Some(self.agent_id.clone())
        }
    }

    fn make_test_user(
        agent_id: &str,
        policies: pattern_core::PolicySet,
        bridge: Option<Arc<crate::permission::PermissionBridge>>,
    ) -> TestUser {
        TestUser {
            agent_id: pattern_core::AgentId::from(agent_id),
            policies,
            bridge,
            origin: Some(human_origin()),
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
        let user = make_test_user(
            "agent-deny",
            PolicySet::from_rules([shell_rule(PolicyAction::Deny {
                reason: Some("explicit deny".into()),
            })]),
            None,
        );
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
        let user = make_test_user(
            "agent-no-bridge",
            PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval { reason: None })]),
            None,
        );
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
            let user = make_test_user(
                "agent-approve",
                PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval {
                    reason: Some("rm-rf-style command".into()),
                })]),
                Some(bridge_for_thread),
            );
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
            let user = make_test_user(
                "agent-deny",
                PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval { reason: None })]),
                Some(bridge_for_thread),
            );
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

    /// Handler-level Partner-bypass: when the dispatch origin IS a
    /// Partner (only possible from a future direct-execution path —
    /// `drive_step` always installs `Author::Agent(self)`), the broker
    /// short-circuits via `bypasses_permission_gate()` and the handler
    /// returns GateApproved without any responder firing.
    #[tokio::test]
    async fn partner_origin_short_circuits_at_handler_level() {
        use pattern_core::types::origin::Partner;
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let _watcher = tokio::spawn(async move {
            if rx.recv().await.is_ok() {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut user = make_test_user(
                "agent-shell-partner",
                PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval { reason: None })]),
                Some(bridge_for_thread),
            );
            user.origin = Some(MessageOrigin::new(
                Author::Partner(Partner {
                    user_id: pattern_core::types::ids::new_id(),
                }),
                Sphere::Private,
            ));
            let mut h = ShellHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            // Even an Always RequireApproval rule should yield to the
            // partner-bypass when the broker sees a Partner origin.
            h.handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        assert!(
            msg.contains(GATE_APPROVED_PREFIX),
            "Partner-origin should produce GateApproved (synthesized grant), got: {msg}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "Partner-origin must short-circuit at the broker — no request should land in the queue"
        );
    }

    /// **Critical security invariant** (review fix): the broker's
    /// `scope_cache` is keyed `(agent_id, scope)`. Two agents in the
    /// same runtime sharing one bridge MUST NOT cross-pollinate
    /// approvals — agent A's `ApproveForScope` for `rm -rf /tmp/x`
    /// must not silently allow agent B to run the same command.
    #[tokio::test]
    async fn per_agent_scope_grants_do_not_cross_pollinate() {
        let broker = Arc::new(PermissionBroker::new());
        // Approve the FIRST request only; subsequent requests get
        // denied. Agent B should re-prompt and hit the denial
        // because its scope key differs from agent A's.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let prompts_for_thread = prompts.clone();
        let responder = tokio::spawn(async move {
            while let Ok(req) = rx.recv().await {
                let n = prompts_for_thread.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let decision = if n == 0 {
                    PermissionDecisionKind::ApproveForScope
                } else {
                    PermissionDecisionKind::Deny
                };
                broker_for_responder.resolve(&req.id, decision).await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));

        let bridge_a = bridge.clone();
        let bridge_b = bridge.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut h = ShellHandler;
            let table = DataConTable::new();

            let user_a = make_test_user(
                "agent-A",
                PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval { reason: None })]),
                Some(bridge_a),
            );
            let cx_a = EffectContext::with_user(&table, &user_a);
            // Agent A: first request, broker approves-for-scope.
            let a_result = h
                .handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx_a)
                .expect_err("stub error");

            let user_b = make_test_user(
                "agent-B",
                PolicySet::from_rules([shell_rule(PolicyAction::RequireApproval { reason: None })]),
                Some(bridge_b),
            );
            let cx_b = EffectContext::with_user(&table, &user_b);
            // Agent B: same scope, but different agent_id — must
            // NOT hit agent A's cached grant. Broker re-prompts;
            // responder denies.
            let b_result = h
                .handle(ShellReq::Execute("rm -rf /tmp/x".into()), &cx_b)
                .expect_err("stub error");

            (a_result.to_string(), b_result.to_string())
        })
        .await
        .expect("blocking task");

        // Allow the responder a beat to record both prompts.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let (a_msg, b_msg) = outcome;
        assert!(
            a_msg.contains(GATE_APPROVED_PREFIX),
            "agent A should be approved, got: {a_msg}"
        );
        assert!(
            b_msg.contains(PERMISSION_DENIED_PREFIX),
            "agent B must NOT inherit agent A's grant, got: {b_msg}"
        );
        let final_count = prompts.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            final_count, 2,
            "broker must observe two distinct prompts (one per agent), got {final_count}"
        );

        drop(bridge);
        responder.abort();
    }
}
