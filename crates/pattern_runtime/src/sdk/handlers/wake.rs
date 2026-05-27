// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Handler for `Pattern.Wake` (v3-multi-agent Phase 4 Task 9).
//!
//! Surface to the agent program: register a [`WakeCondition`] under a
//! caller-visible string id; later, unregister it. Registration is
//! capability-gated on
//! [`pattern_core::CapabilityFlag::WakeConditionRegistration`] —
//! callers without the flag receive a
//! [`crate::policy::CAPABILITY_DENIED_PREFIX`]-marked
//! [`EffectError::Handler`].
//!
//! The actual evaluator wiring lives in [`crate::wake`]. This module
//! is the glue: it converts the wire form into a domain
//! [`WakeCondition`], attaches the dispatching agent's id where the
//! variant requires it, and delegates to the session's
//! [`crate::wake::WakeRegistry`].
//!
//! # Custom conditions (Phase 7 Task 6)
//!
//! Custom wake-condition programs are evaluated by a
//! [`crate::wake::CustomEvaluator`] on a read-only restricted bundle
//! (Observe-class effects only). The evaluator runs the user's
//! Haskell program on a dedicated 256 MiB OS thread with a 30s
//! timeout. If the program returns `True`, a wake activation is
//! delivered to the session's mailbox.

use std::sync::Arc;

use smol_str::SmolStr;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::CapabilityFlag;
use pattern_core::types::ids::new_id;

use crate::policy::CAPABILITY_DENIED_PREFIX;
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::WakeReq;
use crate::session::{HasPermissionBridge, SessionContext};
use crate::wake::WakeRegistry;

/// Prefix attached to `EffectError::Handler` messages produced when
/// the `Pattern.Wake` handler is invoked on a session that has no
/// [`WakeRegistry`] wired.
///
/// Tests that verify the missing-registry path (e.g. unit tests that
/// do not call `with_wake_registry`) match on this prefix to distinguish
/// this error from capability-denial or registration failures without
/// parsing free-form prose.
///
/// After Critical-2 lands, the daemon always wires a registry — so this
/// prefix should only appear in test sessions and non-daemon paths.
pub const WAKE_REGISTRY_MISSING_PREFIX: &str = "WakeRegistryMissing: ";

/// Handler for the `Pattern.Wake` effect.
#[derive(Default, Clone)]
pub struct WakeHandler;

impl DescribeEffect for WakeHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Wake",
            description: "Register and unregister wake conditions (timers, block changes, task dependencies, custom programs)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Register   :: Maybe Text -> WakeCondition -> Wake WakeId",
                "Unregister :: WakeId                       -> Wake Bool",
                "List       :: Wake [WakeListItem]",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type WakeId = Text",
                // Typed records — full field definitions live in Pattern.Wake.hs.
                "data BlockRef    = BlockRef    { blockRefLabel :: Text, blockRefBlockId :: Text, blockRefAgentId :: Text }",
                "data WakeListItem = WakeListItem { wakeListItemWakeId :: WakeId, wakeListItemCondition :: WakeCondition }",
                "data TaskEdgeRef = TaskEdgeRef { taskEdgeBlock :: Text, taskEdgeItem :: Maybe Text }",
                // Wake condition sum. Constructor names are `Wake`-prefixed to
                // keep them out of the GADT constructor namespace; see
                // Pattern.Spawn's `Cat` / `Flag` precedent.
                "data WakeCondition \
                 = WakeInterval Int \
                 | WakeTaskTimeout BlockRef Int \
                 | WakeBlockChanged BlockRef \
                 | WakeTaskDependencyResolved TaskEdgeRef \
                 | WakeCustom Text Text",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "register :: Member Wake effs => WakeCondition -> Eff effs Text\nregister cond = send (Register Nothing cond)",
                "registerNamed :: Member Wake effs => Text -> WakeCondition -> Eff effs Text\nregisterNamed name cond = send (Register (Just name) cond)",
                "unregister :: Member Wake effs => Text -> Eff effs Bool\nunregister wid = send (Unregister wid)",
                "list :: Member Wake effs => Eff effs [WakeListItem]\nlist = send List",
            ]),
        }
    }
}

impl EffectHandler<SessionContext> for WakeHandler {
    type Request = WakeReq;

    fn handle(
        &mut self,
        req: WakeReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let user: &SessionContext = cx.user();

        // Effect-class runtime guard. Runs BEFORE the WakeConditionRegistration
        // flag check. Register/Unregister are both Coordinate/Skip, so this
        // returns Ok(()) immediately, but it's defensive for future reclassification.
        let constructor_name = match &req {
            WakeReq::Register(_, _) => "Register",
            WakeReq::Unregister(_) => "Unregister",
            WakeReq::List => "List",
        };
        crate::sdk::effect_classes::check_effect_class(
            user.capabilities(),
            "Wake",
            constructor_name,
        )?;

        // Capability gate. Both register + unregister sit behind it —
        // unregister-without-the-flag is denied to keep the flag the
        // single point of authorisation.
        //
        // Fail-closed: `None` capabilities means the session has no explicit
        // capability set configured, which is a security misconfiguration —
        // do not treat it as full-power. The daemon always opens sessions
        // with `CapabilitySet::all()` explicitly (see `get_or_open_session`
        // in pattern_server) so production sessions will never hit this path.
        // Sessions opened without an explicit capability set (e.g. tests,
        // pre-capability code) must now pass `CapabilitySet::all()` if they
        // want wake access.
        let caps = user.capabilities();
        let has_flag = caps
            .map(|c| c.has_flag(CapabilityFlag::WakeConditionRegistration))
            .unwrap_or(false); // None → fail closed.
        if !has_flag {
            return Err(EffectError::Handler(format!(
                "{CAPABILITY_DENIED_PREFIX}{}",
                CapabilityFlag::WakeConditionRegistration.name()
            )));
        }

        let registry = user.wake_registry().cloned().ok_or_else(|| {
            EffectError::Handler(format!(
                "{WAKE_REGISTRY_MISSING_PREFIX}Pattern.Wake handler invoked \
                 but no WakeRegistry is wired on the SessionContext"
            ))
        })?;

        match req {
            WakeReq::Register(name, wire_cond) => handle_register(name, wire_cond, user, &registry, cx),
            WakeReq::Unregister(id) => handle_unregister(id, &registry, cx),
            WakeReq::List => handle_list(user, &registry, cx),
        }
    }
}

fn handle_register(
    name: Option<String>,
    wire_cond: crate::sdk::requests::wake::WireWakeCondition,
    user: &SessionContext,
    registry: &Arc<WakeRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    // Agent id is required for `TaskDependencyResolved` so the
    // evaluator can scope its memory-store reads. We resolve it
    // unconditionally to keep the success path simple — variants that
    // don't need it ignore the value.
    let agent_id = user.dispatch_agent_id().unwrap_or_else(|| {
        // dispatch_agent_id() is `None` only for the `()` test shim or
        // pre-drive_step paths. Production sessions always populate
        // it from `agent_loop::drive_step`. Use the session's
        // configured agent_id as a fallback.
        SmolStr::from(user.agent_id())
    });

    let wire_for_listing = wire_cond.clone();
    let condition = wire_cond.into_condition(agent_id.clone());
    // Caller-supplied name takes precedence; validate basic shape.
    let wake_id = match name {
        Some(n) => {
            let trimmed = n.trim();
            if trimmed.is_empty() {
                return Err(EffectError::Handler(
                    "wake registration: name must not be empty".to_string(),
                ));
            }
            if trimmed.len() > 128 {
                return Err(EffectError::Handler(format!(
                    "wake registration: name too long ({} > 128 chars)",
                    trimmed.len()
                )));
            }
            if trimmed.chars().any(|c| c.is_control()) {
                return Err(EffectError::Handler(
                    "wake registration: name must not contain control chars".to_string(),
                ));
            }
            SmolStr::from(trimmed)
        }
        None => SmolStr::from(new_id().to_string()),
    };
    match registry.register(wake_id.clone(), condition, agent_id, wire_for_listing) {
        Ok(returned) => {
            cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                pattern_core::hooks::tags::WAKE_REGISTERED,
                serde_json::json!({ "wake_id": returned.to_string() }),
            ));
            cx.respond(returned.to_string())
        }
        Err(e) => Err(EffectError::Handler(format!(
            "wake registration failed: {e}"
        ))),
    }
}

fn handle_unregister(
    id: String,
    registry: &Arc<WakeRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let removed = registry.unregister(&SmolStr::from(&id));
    if removed {
        cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
            pattern_core::hooks::tags::WAKE_UNREGISTERED,
            serde_json::json!({ "wake_id": id }),
        ));
    }
    cx.respond(removed)
}

fn handle_list(
    user: &SessionContext,
    registry: &Arc<WakeRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    // Scope listing to the dispatching agent — cross-agent listing would
    // expose other personas' wake state. Falls back to the session's
    // configured agent_id for the `()` test shim path, matching the
    // resolution used in handle_register.
    let agent_id = user.dispatch_agent_id().unwrap_or_else(|| {
        SmolStr::from(user.agent_id())
    });
    let entries = registry.list_for_agent(&agent_id);
    let wires: Vec<crate::sdk::requests::wake::WireWakeListItem> = entries
        .into_iter()
        .map(|(wake_id, condition)| crate::sdk::requests::wake::WireWakeListItem {
            wake_id: wake_id.to_string(),
            condition,
        })
        .collect();
    cx.respond(wires)
}
