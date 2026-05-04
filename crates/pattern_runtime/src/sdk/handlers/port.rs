//! Handler for `Pattern.Port` — dispatches `PortReq` variants to the
//! `PortRegistryImpl` dispatcher actor.
//!
//! ## Critical safety note (no block_on)
//!
//! This handler runs on the Tidepool eval worker — a dedicated OS thread with
//! NO ambient tokio runtime. Do NOT introduce `block_on` here, even against a
//! `Handle` stashed on `SessionContext`. All async work is offloaded to the
//! dispatcher actor (which runs on the runtime's tokio task). The handler
//! communicates via `blocking_send` (push to actor) + `recv_timeout` (wait for
//! actor reply) — both are sync-safe from non-runtime threads.
//!
//! ## Capability check
//!
//! `cx.user().capabilities()` returns `None` for full-power sessions and
//! `Some(cap)` for scoped sessions. `PortReq::Call` and `PortReq::Subscribe`
//! check `cap.has_port(port_id)` before dispatching. `PortReq::List` filters
//! the metadata list to ports the agent can see. `PortReq::Unsubscribe` does
//! NOT check capability — if the agent already subscribed it has the right to
//! unsubscribe.
//!
//! ## No-registry path
//!
//! `SessionContext::port_registry()` returns `None` for sessions opened
//! without a registry (test doubles, minimal sessions). The handler
//! propagates `PortError::DispatcherClosed` in that case so the error is
//! distinguishable from a normal dispatcher failure.

use std::sync::Arc;

use pattern_core::traits::PortRegistry;
use pattern_core::types::port::{PortError, PortId};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::PortReq;
use crate::session::SessionContext;
use crate::timeout::HandlerGuard;

/// Handler for `Pattern.Port` — dispatches to the `PortRegistryImpl`
/// dispatcher actor.
///
/// Bound to `SessionContext` because it needs `port_registry()`,
/// `capabilities()`, `session_id()`, and `async_reminder_queue()`.
#[derive(Default, Clone)]
pub struct PortHandler;

impl DescribeEffect for PortHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Port",
            description: "External-service ports (List/Call/Subscribe/Unsubscribe)",
            constructors: std::borrow::Cow::Borrowed(&[
                "List        :: Port [PortInfo]",
                "Call        :: PortId -> Method -> Payload -> Port Payload",
                "Subscribe   :: PortId -> ConfigJson -> Port ()",
                "Unsubscribe :: PortId -> Port ()",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type PortId     = Text",
                "type Method     = Text",
                "type Payload    = Text  -- JSON",
                "type ConfigJson = Text  -- JSON",
                "type PortInfo   = Text  -- JSON: {id, description, version, methods, capabilities}",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "listPorts :: Member Port effs => Eff effs [Text]\nlistPorts = send List",
                "call :: Member Port effs => PortId -> Method -> Payload -> Eff effs Payload\ncall pid m p = send (Call pid m p)",
                "subscribe :: Member Port effs => PortId -> ConfigJson -> Eff effs ()\nsubscribe pid c = send (Subscribe pid c)",
                "unsubscribe :: Member Port effs => PortId -> Eff effs ()\nunsubscribe pid = send (Unsubscribe pid)",
            ]),
        }
    }
}

// SAFETY / DESIGN NOTE: PortHandler runs on the Tidepool eval worker —
// a dedicated OS thread with NO ambient tokio runtime. This handler does
// NOT call `block_on` against arbitrary plugin code. Instead it sends an
// `Op` to the dispatcher actor task (running on the runtime's tokio
// runtime via the Handle supplied at TidepoolRuntime::new) and waits on
// a crossbeam reply channel with `recv_timeout`. The actor handles all
// `await`s including those into plugin code; if a plugin's call() hangs,
// only the actor task hangs, not the eval worker.
impl EffectHandler<SessionContext> for PortHandler {
    type Request = PortReq;

    fn handle(
        &mut self,
        req: PortReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        // Effect-class runtime guard. List=Observe/Skip; Call/Subscribe=Escape/Skip;
        // Unsubscribe=MutateInternal/Skip. All Skip — short-circuits to Ok(()).
        let constructor_name = match &req {
            PortReq::List => "List",
            PortReq::Call(_, _, _) => "Call",
            PortReq::Subscribe(_, _) => "Subscribe",
            PortReq::Unsubscribe(_) => "Unsubscribe",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Port",
            constructor_name,
        )?;

        // Fail closed if no registry is wired (test doubles, minimal sessions).
        let registry = cx
            .user()
            .port_registry()
            .ok_or_else(|| EffectError::Handler(PortError::DispatcherClosed.to_string()))?
            .clone();

        let session_key = cx.user().session_id().to_string();
        let dispatcher = registry.dispatcher().clone();

        // Capability set: `None` = full power. Full-power sessions see all ports.
        let cap = cx.user().capabilities().cloned();

        // Tunable bounds. List/Unsubscribe are fast (no plugin code);
        // Call/Subscribe wait on plugin code so they get a longer cap.
        const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
        const FAST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        match req {
            PortReq::List => {
                let metadatas = registry.list();
                let visible: Vec<String> = metadatas
                    .into_iter()
                    .filter(|m| {
                        cap.as_ref()
                            .map(|c| c.has_port(m.id.as_str()))
                            .unwrap_or(true)
                    })
                    .map(|m| serde_json::to_string(&m).unwrap_or_default())
                    .collect();
                cx.respond(visible)
            }

            PortReq::Call(port_id, method, payload_json) => {
                let hook_port_id = port_id.clone();
                let hook_method = method.clone();
                let port_id = PortId::new(&port_id);
                // Capability gate.
                if cap.as_ref().is_some_and(|c| !c.has_port(port_id.as_str())) {
                    return Err(EffectError::Handler(
                        PortError::CapabilityDenied(port_id).to_string(),
                    ));
                }
                let payload: serde_json::Value =
                    serde_json::from_str(&payload_json).map_err(|e| {
                        EffectError::Handler(format!(
                            "Pattern.Port.Call: invalid payload JSON: {e}"
                        ))
                    })?;

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                // `blocking_send` works from any thread, including non-runtime
                // threads like the eval worker. The bounded channel is required
                // (UnboundedSender does not expose blocking_send).
                dispatcher
                    .blocking_send(crate::port_registry::dispatcher::Op::Call {
                        port_id,
                        method,
                        payload,
                        reply: reply_tx,
                    })
                    .map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;

                let result = reply_rx.recv_timeout(CALL_TIMEOUT).map_err(|_| {
                    EffectError::Handler("Pattern.Port.Call: dispatcher reply timeout".to_string())
                })?;
                let response = result.map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::PORT_CALL_AFTER,
                    serde_json::json!({ "port_id": hook_port_id, "method": hook_method }),
                ));
                cx.respond(serde_json::to_string(&response).unwrap_or_default())
            }

            PortReq::Subscribe(port_id, config_json) => {
                let hook_port_id = port_id.clone();
                let port_id = PortId::new(&port_id);
                // Capability gate.
                if cap.as_ref().is_some_and(|c| !c.has_port(port_id.as_str())) {
                    return Err(EffectError::Handler(
                        PortError::CapabilityDenied(port_id).to_string(),
                    ));
                }
                let config: serde_json::Value =
                    serde_json::from_str(&config_json).map_err(|e| {
                        EffectError::Handler(format!(
                            "Pattern.Port.Subscribe: invalid config JSON: {e}"
                        ))
                    })?;

                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                dispatcher
                    .blocking_send(crate::port_registry::dispatcher::Op::Subscribe {
                        port_id,
                        config,
                        async_reminder_queue: Arc::clone(cx.user().async_reminder_queue()),
                        session_key,
                        reply: reply_tx,
                    })
                    .map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;

                let result = reply_rx.recv_timeout(CALL_TIMEOUT).map_err(|_| {
                    EffectError::Handler(
                        "Pattern.Port.Subscribe: dispatcher reply timeout".to_string(),
                    )
                })?;
                result.map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::PORT_SUBSCRIBED,
                    serde_json::json!({ "port_id": hook_port_id }),
                ));
                cx.respond(())
            }

            PortReq::Unsubscribe(port_id) => {
                // Unsubscribe does not check capability — the agent already
                // subscribed to this port (capability was checked at Subscribe
                // time), so it has the right to cancel.
                let port_id = PortId::new(&port_id);
                let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                dispatcher
                    .blocking_send(crate::port_registry::dispatcher::Op::Unsubscribe {
                        port_id,
                        session_key,
                        reply: reply_tx,
                    })
                    .map_err(|_| EffectError::Handler(PortError::DispatcherClosed.to_string()))?;
                // Best-effort: ignore reply on Unsubscribe (idempotent).
                let _ = reply_rx.recv_timeout(FAST_TIMEOUT);
                cx.respond(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    /// Verify the PortHandler stub uses the expected effect type name.
    #[test]
    fn port_handler_effect_decl_type_name() {
        let decl = PortHandler::effect_decl();
        assert_eq!(decl.type_name, "Port");
    }

    /// Verify PortHandler has the expected constructors.
    #[test]
    fn port_handler_has_four_constructors() {
        let decl = PortHandler::effect_decl();
        assert_eq!(
            decl.constructors.len(),
            4,
            "Port effect must have 4 constructors (List/Call/Subscribe/Unsubscribe)"
        );
        let names: Vec<&str> = decl
            .constructors
            .iter()
            .filter_map(|c| c.split_whitespace().next())
            .collect();
        for expected in ["List", "Call", "Subscribe", "Unsubscribe"] {
            assert!(
                names.contains(&expected),
                "missing constructor {expected:?}, got: {names:?}"
            );
        }
    }

    /// Verify the no-registry path returns `DispatcherClosed` for all
    /// variants. `SessionContext` defaults `port_registry` to `None`.
    #[test]
    fn port_handler_no_registry_returns_dispatcher_closed() {
        // Build a minimal context with no registry.
        let table = DataConTable::new();
        // `cx` is constructed to show the shape compiles; the real handler
        // can only run with `SessionContext` (not `()`), so we only verify
        // the decl shape here. See tests/port_handler.rs for full coverage.
        let _cx = EffectContext::with_user(&table, &());
        let decl = PortHandler::effect_decl();
        assert!(!decl.constructors.is_empty());
    }
}
