//! Stub handler for `Pattern.File`, with the Phase 1 Task 15 policy
//! gate wired in front of the existing not-implemented placeholder.
//!
//! Real read / write / list mechanics arrive in the post-foundation
//! filesystem-sandbox plan; Phase 1 ships the gate so the security
//! semantic is in place when the real implementation lands.
//!
//! Decision flow per [`FileReq::Write`]:
//!
//! 1. **Shape guard (locked invariant)**: if the destination looks
//!    like a Pattern config KDL
//!    ([`crate::policy::is_pattern_config_kdl`]), the handler escalates
//!    directly to the broker with a [`PermissionScope::FileWrite`]
//!    keyed on the path. The [`pattern_core::PolicySet`] is **not**
//!    consulted on this path; no rule (including KDL-loaded
//!    `Allow`-everything rules and runtime overrides) can loosen the
//!    gate. The user can grant temporary access via the broker's
//!    `ApproveForDuration` flow — that grant lives in the broker's
//!    in-memory `scope_cache` only and dies with the session.
//!
//! 2. **Policy pipeline**: non-config writes flow through the standard
//!    [`pattern_core::PolicySet::evaluate`] → [`pattern_core::PolicyAction`]
//!    fan-out (`Deny` / `RequireApproval` / `Allow`). The decisions
//!    surface as `PERMISSION_DENIED_PREFIX` / `GATE_APPROVED_PREFIX`-
//!    marked stub errors per the Shell handler convention.
//!
//! `FileReq::Read` and `FileReq::ListDir` remain ungated stubs in
//! Phase 1 — the sandbox-IO plan delivers their real implementations
//! and gates them at that point.

use std::time::Duration;

use pattern_core::permission::PermissionScope;
use pattern_core::{EffectCategory, PolicyAction, PolicyContext};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::policy::config_guard::is_pattern_config_kdl;
use crate::policy::{GATE_APPROVED_PREFIX, PERMISSION_DENIED_PREFIX};
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::FileReq;
use crate::session::{HasCancelState, HasPermissionBridge, HasPolicySet};
use crate::timeout::HandlerGuard;

/// Default broker-request timeout for file-write gates. Same envelope
/// as the Shell handler's gate timeout — long enough for human
/// thinking, short enough that a stalled responder surfaces as denial.
const FILE_GATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Not-implemented placeholder for the File effect, gated by the
/// Phase 1 Task 15 policy pipeline. Real implementation lands in the
/// post-foundation filesystem-sandbox plan.
#[derive(Default, Clone)]
pub struct FileHandler;

impl DescribeEffect for FileHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "File",
            description: "Sandboxed filesystem access (Read/Write/ListDir)",
            constructors: &[
                "Read    :: Path -> File Content",
                "Write   :: Path -> Content -> File ()",
                "ListDir :: Path -> File [Path]",
            ],
            type_defs: &["type Path = Text"],
            helpers: &[
                "read :: Member File effs => Path -> Eff effs Content\nread p = Freer.send (Read p)",
                "write :: Member File effs => Path -> Content -> Eff effs ()\nwrite p c = Freer.send (Write p c)",
                "listDir :: Member File effs => Path -> Eff effs [Path]\nlistDir p = Freer.send (ListDir p)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for FileHandler
where
    U: HasCancelState + HasPolicySet + HasPermissionBridge,
{
    type Request = FileReq;

    fn handle(&mut self, req: FileReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        match req {
            FileReq::Write(path, content) => evaluate_write(&path, content.as_bytes(), cx.user()),
            FileReq::Read(path) => Err(EffectError::Handler(format!(
                "Pattern.File.Read({path:?}) is not implemented in v3 foundation \
                 (phase: post-foundation filesystem-sandbox plan)."
            ))),
            FileReq::ListDir(path) => Err(EffectError::Handler(format!(
                "Pattern.File.ListDir({path:?}) is not implemented in v3 foundation \
                 (phase: post-foundation filesystem-sandbox plan)."
            ))),
        }
    }
}

fn evaluate_write<U>(path_str: &str, content: &[u8], user: &U) -> Result<Value, EffectError>
where
    U: HasPolicySet + HasPermissionBridge,
{
    let path = std::path::Path::new(path_str);

    // Path-normalization deferral (review item, Phase 1 minor #1):
    // `PermissionScope::FileWrite { path }` keys the broker's scope
    // cache on the literal path string handed in by the agent. This
    // means `/proj/.pattern.kdl` and `/proj/./.pattern.kdl` are
    // distinct cache entries, and symlinks bypass the cache. Phase 1
    // ships the gate as a stub; the real File.Write handler in the
    // sandbox-IO plan owns the canonicalization machinery — `Create`
    // flows will check the resolved path before allowing a write,
    // `Read` / `ListDir` / overwrite flows will canonicalize via
    // `Path::canonicalize` and key the cache on the canonical form.
    // Until that machinery exists, over-prompting on path aliases is
    // the conservative direction.

    // (1) Locked invariant — Pattern config KDL writes always escalate
    //     to the broker. PolicySet is not consulted on this path.
    if is_pattern_config_kdl(path, content).is_config() {
        return escalate(
            user,
            PermissionScope::FileWrite {
                path: path_str.to_string(),
            },
            "write to Pattern config KDL",
        );
    }

    // (2) Non-config writes flow through the policy pipeline.
    let policy_ctx = PolicyContext::FileWrite { path, content };
    match user.policies().evaluate(EffectCategory::File, &policy_ctx) {
        PolicyAction::Deny { reason } => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}{}",
            reason.unwrap_or_else(|| "file write denied by policy".into())
        ))),
        PolicyAction::RequireApproval { reason } => escalate(
            user,
            PermissionScope::FileWrite {
                path: path_str.to_string(),
            },
            reason.as_deref().unwrap_or("file write requires approval"),
        ),
        PolicyAction::Allow => Err(EffectError::Handler(format!(
            "{GATE_APPROVED_PREFIX}Pattern.File.Write gate cleared; actual write \
             mechanics land in the filesystem-sandbox plan"
        ))),
        // PolicyAction is `#[non_exhaustive]` — fail closed on any future variant.
        other => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}unhandled policy action {other:?}"
        ))),
    }
}

/// Escalate a write through the [`crate::permission::PermissionBridge`].
/// Returns [`GATE_APPROVED_PREFIX`]-marked success on grant, or
/// [`PERMISSION_DENIED_PREFIX`]-marked error on denial / timeout /
/// missing bridge.
fn escalate<U>(user: &U, scope: PermissionScope, reason: &str) -> Result<Value, EffectError>
where
    U: HasPermissionBridge,
{
    let Some(bridge) = user.permission_bridge() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write gated but no permission bridge wired"
        )));
    };
    let origin = user.current_dispatch_origin().unwrap_or_else(|| {
        pattern_core::types::origin::MessageOrigin::new(
            pattern_core::types::origin::Author::System {
                reason: pattern_core::types::origin::SystemReason::Timer,
            },
            pattern_core::types::origin::Sphere::System,
        )
    });
    // Real session agent_id is load-bearing for per-agent isolation
    // of the broker's scope cache (keyed `(agent_id, scope)`); fail
    // closed if absent.
    let Some(agent) = user.dispatch_agent_id() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write gated but no agent identity \
             available for broker attribution"
        )));
    };
    let grant = bridge.request_sync(
        agent,
        "file".into(),
        scope,
        &origin,
        Some(reason.to_string()),
        None,
        FILE_GATE_TIMEOUT,
    );
    if grant.is_some() {
        Err(EffectError::Handler(format!(
            "{GATE_APPROVED_PREFIX}Pattern.File.Write gate cleared; actual write \
             mechanics land in the filesystem-sandbox plan"
        )))
    } else {
        Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write denied or timed out at the broker"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::permission::{PermissionBroker, PermissionDecisionKind};
    use pattern_core::types::origin::{Author, Human, MessageOrigin, Sphere};
    use pattern_core::{PolicyAction, PolicyMatcher, PolicyRule, PolicySet, Precedence};
    use std::sync::Arc;
    use tidepool_repr::DataConTable;

    /// Minimal user struct that satisfies the File handler's trait
    /// bounds without standing up a full SessionContext.
    struct TestUser {
        agent_id: pattern_core::AgentId,
        policies: PolicySet,
        bridge: Option<Arc<crate::permission::PermissionBridge>>,
        origin: Option<MessageOrigin>,
    }

    impl HasCancelState for TestUser {
        fn cancel_state(&self) -> Arc<crate::timeout::CancelState> {
            Arc::new(crate::timeout::CancelState::new())
        }
    }
    impl HasPolicySet for TestUser {
        fn policies(&self) -> &PolicySet {
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
        policies: PolicySet,
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

    /// AC2.7 core: agent calls File.Write to a Pattern config KDL.
    /// Broker observes a request with `FileWrite { path }` scope; test
    /// responds Deny; agent sees PERMISSION_DENIED_PREFIX-marked error.
    #[tokio::test]
    async fn config_kdl_write_escalates_to_broker_and_can_be_denied() {
        let broker = Arc::new(PermissionBroker::new());

        // Subscribe synchronously so the responder cannot miss.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let observed_scope = Arc::new(std::sync::Mutex::new(None));
        let observed_for_thread = observed_scope.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                *observed_for_thread.lock().unwrap() = Some(req.scope.clone());
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });

        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let result = tokio::task::spawn_blocking(move || {
            let user = make_test_user(
                "agent-cfg-deny",
                PolicySet::from_rules([]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(
                FileReq::Write("/tmp/.pattern.kdl".into(), "mount mode=\"A\"\n".into()),
                &cx,
            )
        })
        .await
        .expect("blocking task")
        .expect_err("denial should surface");
        let msg = result.to_string();
        assert!(
            msg.contains(PERMISSION_DENIED_PREFIX),
            "expected PermissionDenied prefix, got: {msg}"
        );
        responder.await.unwrap();

        // Confirm the broker saw a FileWrite-scoped request keyed on
        // the actual path (not a tool-execution scope).
        let scope = observed_scope.lock().unwrap().clone();
        match scope {
            Some(PermissionScope::FileWrite { path }) => {
                assert_eq!(path, "/tmp/.pattern.kdl");
            }
            other => panic!("expected FileWrite scope, got {other:?}"),
        }
    }

    /// AC2.7 locked-default: even with a KDL `Allow` rule for all
    /// file writes, config-KDL writes still escalate (because the
    /// shape guard short-circuits before the policy is consulted).
    #[tokio::test]
    async fn config_kdl_write_locked_against_kdl_allow_all() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        // Persona-style KDL Allow rule for everything — must NOT override
        // the shape guard.
        let kdl_allow_all = PolicyRule::new(
            EffectCategory::File,
            PolicyMatcher::FilePath {
                pattern: "*".into(),
            },
            PolicyAction::Allow,
            Precedence::KdlConfig,
        );
        let _ = tokio::task::spawn_blocking(move || {
            let user = make_test_user(
                "agent-cfg-locked",
                PolicySet::from_rules([kdl_allow_all]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            // Result discarded — the test only asserts on the broker's
            // observed request, not the handler's stub-error message.
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task");
        responder.await.unwrap();

        assert!(
            saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "broker must observe a request despite KDL Allow-all"
        );
    }

    /// AC2.7 locked-default vs RuntimeOverride: even the highest
    /// configurable precedence cannot loosen the shape guard. Adversarial
    /// review focus #2 — the structural property must be observable in
    /// the test suite, not just argued from code shape.
    #[tokio::test]
    async fn config_kdl_write_locked_against_runtime_override_allow() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        // RuntimeOverride is the highest configurable precedence;
        // shape guard must still beat it because the policy is never
        // consulted on a config-KDL write.
        let runtime_allow_all = PolicyRule::new(
            EffectCategory::File,
            PolicyMatcher::Always,
            PolicyAction::Allow,
            Precedence::RuntimeOverride,
        );
        let _ = tokio::task::spawn_blocking(move || {
            let user = make_test_user(
                "agent-cfg-runtime",
                PolicySet::from_rules([runtime_allow_all]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task");
        responder.await.unwrap();

        assert!(
            saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "broker must observe a request despite RuntimeOverride Allow-all"
        );
    }

    /// Handler-level Partner-bypass: when the dispatch origin IS a
    /// Partner (only possible from a future direct-execution path —
    /// `drive_step` always installs `Author::Agent(self)`), the broker
    /// short-circuits via `bypasses_permission_gate()` and the handler
    /// returns GateApproved without any responder firing. Adversarial
    /// review focus #1 — wire-correctness predicate at the handler
    /// layer, distinct from `drive_step`'s constant-Agent installation.
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
                "agent-partner-bypass",
                PolicySet::new(),
                Some(bridge_for_thread),
            );
            user.origin = Some(MessageOrigin::new(
                Author::Partner(Partner {
                    user_id: pattern_core::types::ids::new_id(),
                }),
                Sphere::Private,
            ));
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            // Config-KDL write — would normally escalate, but Partner
            // origin should short-circuit at the broker.
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        assert!(
            msg.contains(GATE_APPROVED_PREFIX),
            "Partner-origin should produce GateApproved (synthesized grant), got: {msg}"
        );
        // Allow a beat for the watcher to record any prompt.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "Partner-origin must short-circuit at the broker — no request should land in the queue"
        );
    }

    /// AC2.7 non-config: writes to non-config paths must NOT trigger
    /// the broker. Distinct prefix lets the test discriminate.
    #[tokio::test]
    async fn non_config_write_does_not_escalate() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        // Drop any broadcast we observe — the test only asserts the
        // handler's RESULT, not the broker traffic, but we want to
        // ensure no unexpected hang.
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
            // Empty policy set → Allow everywhere (and shape guard
            // must NOT fire on a non-config path).
            let user = make_test_user("agent-non-cfg", PolicySet::new(), Some(bridge_for_thread));
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Write("/tmp/notes.txt".into(), "hi".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        assert!(
            msg.contains(GATE_APPROVED_PREFIX),
            "Allow path should produce GateApproved marker, got: {msg}"
        );
        // Allow short-circuits the broker, so no request was observed.
        // Sleep a beat to ensure the watcher would have fired if it
        // were going to.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "Allow path must not hit the broker"
        );
    }

    /// Approve-for-duration on a config write caches; same path within
    /// the window does NOT re-prompt; different path DOES re-prompt.
    /// Demonstrates the FileWrite scope's path granularity.
    #[tokio::test]
    async fn approve_for_duration_caches_per_path() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let prompts_for_thread = prompts.clone();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            // Approve every prompt with a long-duration grant.
            while let Ok(req) = rx.recv().await {
                prompts_for_thread.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(
                        &req.id,
                        PermissionDecisionKind::ApproveForDuration(jiff::Span::new().minutes(5)),
                    )
                    .await;
            }
        });

        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let join: tokio::task::JoinHandle<()> = tokio::task::spawn_blocking(move || {
            let user = make_test_user("agent-cfg-dur", PolicySet::new(), Some(bridge_for_thread));
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx = EffectContext::with_user(&table, &user);

            // First write to /a/.pattern.kdl — broker prompted, approves.
            h.handle(FileReq::Write("/a/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("stub error after approval");
            // Second write to the SAME path within the window — must be
            // satisfied from the cache without re-broadcasting.
            h.handle(FileReq::Write("/a/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("stub error after cached approval");
            // Write to a DIFFERENT config path — distinct scope, must
            // re-prompt.
            h.handle(FileReq::Write("/b/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("stub error after fresh approval");
        });
        join.await.expect("blocking task");

        // Allow the responder to drain.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let count = prompts.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            count, 2,
            "expected exactly 2 broker prompts (first /a write, first /b write); got {count}"
        );
        // Drop the bridge to close the channel and let the responder exit.
        drop(bridge);
        // Responder will exit when the broker is dropped from inside the
        // bridge — abort to free the join handle without awaiting (we
        // intentionally don't care about its return).
        responder.abort();
    }
}
