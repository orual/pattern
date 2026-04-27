//! Handler for `Pattern.Fronting` (v3-multi-agent Phase 5 Task 6).
//!
//! Surface to the agent program: read the current fronting set (`Current`),
//! set active personas and fallback (`Set`), replace routing rules (`Route`),
//! or clear all fronting state (`Clear`).
//!
//! # Capability gate
//!
//! All constructors require [`pattern_core::CapabilityFlag::FrontingControl`]
//! on the dispatching agent's capability set — including the read-only
//! `Current` constructor. Fronting state is privileged routing metadata.
//!
//! Callers without the flag receive a
//! [`crate::policy::CAPABILITY_DENIED_PREFIX`]-marked
//! [`tidepool_effect::EffectError::Handler`].
//!
//! # Missing-set path
//!
//! If the session's [`crate::session::SessionContext`] has no `FrontingSet`
//! wired (`fronting_set()` returns `None`), the handler returns a
//! [`FRONTING_NOT_WIRED_PREFIX`]-marked error. This covers test sessions and
//! non-daemon paths. After Block B (T3) lands, the daemon always wires the
//! set on session open.
//!
//! # Mutation semantics
//!
//! The handler updates the in-memory `Arc<RwLock<FrontingSet>>` only (lock →
//! mutate → release). Persistence (save to DB) and `FrontingChanged` event
//! emission are the daemon's responsibility (T3/server-side wiring). A
//! `fronting_dirty` flag or post-handler observer can trigger these without
//! coupling the SDK handler to the daemon's DB layer.
//!
//! For `Route`, rules are compiled via `RoutingTable::try_from_rules` before
//! the write lock is acquired. If compilation fails, the error is surfaced and
//! the existing rules are left unchanged — the write lock is never taken on a
//! compile failure.
//!
//! # `Current` response encoding
//!
//! `Current` serializes the fronting state as a JSON string (same pattern as
//! `Pattern.Diagnostics.GetDiagnostics`). The Haskell side receives `Text`
//! and can decode with Aeson. This avoids requiring a `ToCore` derive on the
//! snapshot type at the cost of one JSON parse on the Haskell side.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::CapabilityFlag;
use pattern_core::fronting::{FrontingLoadError, FrontingSet, RoutingRule, RoutingTable};
use pattern_core::types::ids::PersonaId;

// ── FrontingCommitter (Phase 6 T5b) ───────────────────────────────────────────

/// Closure shape accepted by [`FrontingCommitter::commit_sync`].
///
/// The committer applies the mutator under the write lock, then persists
/// and fans out a `FrontingChanged` event. Failure paths (mutator rejection,
/// DB save failure) revert the in-memory state to its pre-mutation snapshot.
pub type FrontingMutator =
    Box<dyn FnOnce(&mut FrontingSet) -> Result<(), FrontingLoadError> + Send + 'static>;

/// Synchronous commit boundary for SDK-driven `FrontingSet` mutations.
///
/// The Pattern.Fronting handler runs on the eval-worker thread (no ambient
/// tokio runtime). When wired, it delegates `Set` / `Route` / `Clear` to a
/// committer that:
/// 1. Snapshots the current state.
/// 2. Applies the mutator under the write lock.
/// 3. Persists to the per-mount DB (rolls back on failure).
/// 4. Fans out [`pattern_server::protocol::WireTurnEvent::FrontingChanged`]
///    to subscribed clients.
///
/// Daemon-side implementations bridge through `tokio_handle.block_on(...)` to
/// reuse the existing async three-phase commit; test sessions use
/// [`InMemoryFrontingCommitter`] which wraps the same lock with no-op
/// persistence and emission.
///
/// The committer **owns** the `Arc<RwLock<FrontingSet>>` it operates on and
/// exposes it via [`Self::fronting_set`]. Read-only access (e.g. the handler's
/// `Current` constructor) goes through this method so there is exactly one
/// source of truth — the lock the committer mutates is the lock the handler
/// reads, axiomatically.
pub trait FrontingCommitter: Send + Sync + std::fmt::Debug {
    /// Shared lock over the canonical fronting set. Read-only callers
    /// (e.g. the `Current` handler) acquire a read guard; the committer
    /// itself takes the write lock during `commit_sync`.
    fn fronting_set(&self) -> &Arc<std::sync::RwLock<FrontingSet>>;

    /// Apply `mutator` synchronously. Returns the post-mutation snapshot on
    /// success.
    fn commit_sync(&self, mutator: FrontingMutator) -> Result<FrontingSet, EffectError>;
}

/// In-memory `FrontingCommitter` for test sessions: wraps a lock with no-op
/// persistence and no event emission. Mutations land in the lock and stop
/// there.
///
/// Use this in tests that need to exercise the SDK handler's read/write
/// surface without a daemon, or to wire a session for fronting reads when
/// the handler should only succeed on the [`crate::policy::CAPABILITY_DENIED_PREFIX`]
/// path before reaching any commit.
#[derive(Debug, Clone)]
pub struct InMemoryFrontingCommitter {
    fronting: Arc<std::sync::RwLock<FrontingSet>>,
}

impl InMemoryFrontingCommitter {
    pub fn new(fronting: Arc<std::sync::RwLock<FrontingSet>>) -> Self {
        Self { fronting }
    }

    /// Construct a committer wrapping a fresh, empty `FrontingSet`.
    pub fn empty() -> Self {
        Self::new(Arc::new(std::sync::RwLock::new(FrontingSet::default())))
    }
}

impl FrontingCommitter for InMemoryFrontingCommitter {
    fn fronting_set(&self) -> &Arc<std::sync::RwLock<FrontingSet>> {
        &self.fronting
    }

    fn commit_sync(&self, mutator: FrontingMutator) -> Result<FrontingSet, EffectError> {
        let mut guard = self
            .fronting
            .write()
            .map_err(|e| EffectError::Handler(format!("fronting lock poisoned: {e}")))?;
        let snap = guard.clone();
        if let Err(e) = mutator(&mut guard) {
            *guard = snap;
            return Err(EffectError::Handler(format!("mutator: {e}")));
        }
        Ok(guard.clone())
    }
}

use crate::policy::{CAPABILITY_DENIED_PREFIX, FRONTING_NOT_WIRED_PREFIX};
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::FrontingReq;
use crate::sdk::requests::fronting::WireRoutingRule;
use crate::session::SessionContext;

// ── Snapshot serialization ────────────────────────────────────────────────────

/// A serializable snapshot of the fronting state, returned by `Current` as JSON.
///
/// Haskell agents receive this as `Text` and can decode it with Aeson via the
/// `FrontingSnapshot` record defined in `Pattern.Fronting`.
#[derive(Debug, Serialize, Deserialize)]
struct FrontingSnapshotJson {
    pub active: Vec<String>,
    pub fallback: Option<String>,
    pub rules: Vec<RoutingRuleJson>,
}

/// A serializable routing rule for the snapshot.
///
/// `pattern_type` is `String` (not `&'static str`) so the struct can
/// derive `Deserialize` for the agent-side decode path. The serialised
/// form uses one of the four canonical names: "Prefix", "Contains",
/// "TopicTag", "Regex".
#[derive(Debug, Serialize, Deserialize)]
struct RoutingRuleJson {
    pub id: String,
    pub pattern_type: String,
    pub pattern_value: String,
    pub target: String,
    pub priority: u32,
}

fn pattern_to_json(p: &pattern_core::fronting::MessagePattern) -> (&'static str, String) {
    match p {
        pattern_core::fronting::MessagePattern::Prefix(s) => ("Prefix", s.clone()),
        pattern_core::fronting::MessagePattern::Contains(s) => ("Contains", s.clone()),
        pattern_core::fronting::MessagePattern::TopicTag(s) => ("TopicTag", s.clone()),
        pattern_core::fronting::MessagePattern::Regex(s) => ("Regex", s.clone()),
        // Non-exhaustive forward-compat.
        _ => ("Unknown", String::new()),
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// Handler for the `Pattern.Fronting` effect.
#[derive(Default, Clone)]
pub struct FrontingHandler;

impl DescribeEffect for FrontingHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Fronting",
            description: "Read and mutate the constellation's active fronting set and routing rules",
            constructors: &[
                "Current :: Fronting Text",
                "Set     :: [PersonaId] -> Maybe PersonaId -> Fronting ()",
                "Route   :: [RoutingRule] -> Fronting ()",
                "Clear   :: Fronting ()",
            ],
            type_defs: &[
                "type PersonaId = Text",
                // RoutingRule is a record: (id, pattern, target, priority).
                "data RoutingRule = RoutingRule Text MessagePattern Text Word32",
                "data MessagePattern = PatternPrefix Text | PatternContains Text | PatternTopicTag Text | PatternRegex Text",
            ],
            helpers: &[
                "current :: Member Fronting effs => Eff effs Text\ncurrent = send Current",
                "set :: Member Fronting effs => [PersonaId] -> Maybe PersonaId -> Eff effs ()\nset personas fb = send (Set personas fb)",
                "route :: Member Fronting effs => [RoutingRule] -> Eff effs ()\nroute rules = send (Route rules)",
                "clear :: Member Fronting effs => Eff effs ()\nclear = send Clear",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for FrontingHandler {
    type Request = FrontingReq;

    fn handle(
        &mut self,
        req: FrontingReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let user: &SessionContext = cx.user();

        // Capability gate — fail-closed: `None` capabilities means the session
        // has no explicit capability set configured. The daemon always opens
        // sessions with `CapabilitySet::all()` explicitly so production sessions
        // never hit this path. Tests must pass `CapabilitySet::all()` explicitly
        // if they want fronting access.
        let caps = user.capabilities();
        let has_flag = caps
            .map(|c| c.has_flag(CapabilityFlag::FrontingControl))
            .unwrap_or(false); // None → fail closed.
        if !has_flag {
            return Err(EffectError::Handler(format!(
                "{CAPABILITY_DENIED_PREFIX}{}",
                CapabilityFlag::FrontingControl.name()
            )));
        }

        // Resolve the committer. Returns an error with FRONTING_NOT_WIRED_PREFIX
        // if the daemon has not yet wired one (T3 path not active, test session,
        // or non-daemon session). The committer owns the canonical `FrontingSet`
        // lock — read-only and write paths both flow through it.
        let committer: Arc<dyn FrontingCommitter> =
            user.fronting_committer().cloned().ok_or_else(|| {
                EffectError::Handler(format!(
                    "{FRONTING_NOT_WIRED_PREFIX}Pattern.Fronting handler invoked \
                     but no FrontingCommitter is wired on the SessionContext"
                ))
            })?;

        match req {
            FrontingReq::Current => handle_current(committer.as_ref(), cx),
            FrontingReq::Set(active, fallback) => {
                handle_set(active, fallback, committer.as_ref(), cx)
            }
            FrontingReq::Route(wire_rules) => handle_route(wire_rules, committer.as_ref(), cx),
            FrontingReq::Clear => handle_clear(committer.as_ref(), cx),
        }
    }
}

fn handle_current(
    committer: &dyn FrontingCommitter,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let set = committer
        .fronting_set()
        .read()
        .map_err(|e| EffectError::Handler(format!("fronting lock poisoned: {e}")))?;

    let snapshot = FrontingSnapshotJson {
        active: set.active.iter().map(|id| id.to_string()).collect(),
        fallback: set.fallback.as_ref().map(|id| id.to_string()),
        rules: set
            .routing
            .rules
            .iter()
            .map(|r| {
                let (pt, pv) = pattern_to_json(&r.pattern);
                RoutingRuleJson {
                    id: r.id.clone(),
                    pattern_type: pt.to_string(),
                    pattern_value: pv,
                    target: r.target.to_string(),
                    priority: r.priority,
                }
            })
            .collect(),
    };

    let json_str = serde_json::to_string(&snapshot)
        .map_err(|e| EffectError::Handler(format!("failed to serialize fronting snapshot: {e}")))?;

    cx.respond(json_str)
}

fn handle_set(
    active_ids: Vec<String>,
    fallback_id: Option<String>,
    committer: &dyn FrontingCommitter,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let mutator: FrontingMutator = Box::new(move |set: &mut FrontingSet| {
        set.active = active_ids
            .into_iter()
            .map(|s| PersonaId::new(s.as_str()))
            .collect();
        set.fallback = fallback_id.map(|s| PersonaId::new(s.as_str()));
        Ok(())
    });
    committer.commit_sync(mutator)?;
    cx.respond(())
}

fn handle_route(
    wire_rules: Vec<WireRoutingRule>,
    committer: &dyn FrontingCommitter,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    // Compile rules BEFORE entering the committer's critical section so the
    // existing rules are preserved on compile failure.
    let domain_rules: Vec<RoutingRule> = wire_rules.into_iter().map(RoutingRule::from).collect();
    let table = RoutingTable::try_from_rules(domain_rules)
        .map_err(|e| EffectError::Handler(format!("route compile failed: {e}")))?;

    let mutator: FrontingMutator = Box::new(move |set: &mut FrontingSet| {
        set.routing = table;
        Ok(())
    });
    committer.commit_sync(mutator)?;
    cx.respond(())
}

fn handle_clear(
    committer: &dyn FrontingCommitter,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let mutator: FrontingMutator = Box::new(|set: &mut FrontingSet| {
        *set = FrontingSet::default();
        Ok(())
    });
    committer.commit_sync(mutator)?;
    cx.respond(())
}
