//! Routing-aware message dispatch.
//!
//! `AgentRouter::route` calls into [`dispatch_to_mailboxes`] when the
//! recipient is unspecified (empty or sentinel-targeted, `agent:` /
//! `agent:auto`). The dispatch function consults the session's
//! [`FrontingResolver`] to pick a target persona (or fan out / fall
//! back to a default), then drives delivery through the same
//! [`AgentRegistry::route_or_queue`] primitive that direct deliveries
//! use.
//!
//! AC8.8 (in-flight routing updates) is structurally enforced: the
//! resolver is consulted *once* per `dispatch_to_mailboxes` call, the
//! decision is committed by pushing the [`MailboxInput`] into the
//! recipient's mpsc channel, and any subsequent `FrontingSet` mutation
//! cannot re-route the queued message. New messages routed after the
//! mutation see the new state.

use std::sync::{Arc, RwLock};

use pattern_core::constellation::ConstellationRegistry;
use pattern_core::fronting::{FrontingResolver, FrontingSet, ResolveOutcome};
use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;

use crate::agent_registry::AgentRegistry;
use crate::mailbox::MailboxInput;
use crate::router::RouterError;

/// Per-mount fronting state. Holds the live `FrontingSet` (under a
/// read/write lock for runtime mutations) and the
/// [`ConstellationRegistry`] used for default-persona resolution when
/// the fronting set is empty.
///
/// Constructed by the daemon at mount-attach time and shared with the
/// session's [`crate::router::AgentRouter`] so each routing decision
/// reads the current state without holding a long-lived snapshot.
#[derive(Clone)]
pub struct FrontingState {
    /// Current fronting set. Read locked per-route; write locked by
    /// the daemon's `update_fronting` helper which also persists to
    /// pattern_db.
    pub set: Arc<RwLock<FrontingSet>>,
    /// Registry used for the default-persona fallback when the
    /// fronting set is empty (no `active`, no `fallback`).
    pub registry: Arc<dyn ConstellationRegistry>,
}

impl FrontingState {
    /// Construct a [`FrontingState`] from its components.
    pub fn new(set: Arc<RwLock<FrontingSet>>, registry: Arc<dyn ConstellationRegistry>) -> Self {
        Self { set, registry }
    }
}

impl std::fmt::Debug for FrontingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrontingState").finish_non_exhaustive()
    }
}

/// Route `body` to one or more persona mailboxes via the fronting
/// resolver. Targets are determined entirely from the snapshot of the
/// `FrontingSet` taken at the start of this call — concurrent mutations
/// to the set do not affect this dispatch.
///
/// Returns:
/// - `Ok(())` on successful delivery (or successful queuing for Draft
///   personas; same semantics as the direct `agent:` path).
/// - The first [`RouterError`] encountered for FanOut / multi-target
///   outcomes (subsequent targets are not retried — the channel
///   contract is best-effort per-target).
///
/// AC8.8 invariant: the FrontingSet is read-locked once at the top
/// and the resolver outcome is computed under that lock. Once the
/// outcome is in hand, the lock is dropped and the message is pushed
/// into the recipient mailbox(es). New messages dispatched afterward
/// see whatever state the lock holds at *their* dispatch time.
pub async fn dispatch_to_mailboxes(
    registry: &AgentRegistry,
    fronting: &FrontingState,
    sender: &MessageOrigin,
    body: &Message,
) -> Result<(), RouterError> {
    let outcome = {
        // Snapshot the resolver under a short-lived read lock so the
        // decision is stable for this dispatch even if the set is
        // mutated concurrently. The lock is `std::sync::RwLock` because
        // it's also accessed from the sync `Pattern.Fronting` handler
        // running on the eval-worker OS thread (no ambient tokio
        // runtime there). The lock is released before the awaited
        // `resolve()` call so the runtime never holds the lock across
        // an await point.
        let set = fronting
            .set
            .read()
            .map_err(|_| {
                RouterError::PersonaNotFound(PersonaId::from("<lock-poisoned>"))
            })?
            .clone();
        let resolver = FrontingResolver::new(set, fronting.registry.clone());
        resolver.resolve(body_text(body)).await
    };

    deliver_resolved(registry, sender, body, outcome).await
}

/// Helper: extract the message body text used for resolver matching.
/// `MessagePattern::Prefix` / `Contains` / `TopicTag` / `Regex` all
/// match against this string.
fn body_text(msg: &Message) -> &str {
    msg.chat_message.content.first_text().unwrap_or("")
}

/// Deliver `body` to whichever target(s) the resolver returned.
///
/// FanOut delivers in-order to each target; the first error is
/// returned but subsequent deliveries are NOT attempted. This matches
/// the `agent:` direct-send semantics where each call to
/// `route_or_queue` is independent — there's no transactional
/// "either all or none" promise across multiple targets in a single
/// dispatch.
async fn deliver_resolved(
    registry: &AgentRegistry,
    sender: &MessageOrigin,
    body: &Message,
    outcome: ResolveOutcome,
) -> Result<(), RouterError> {
    match outcome {
        ResolveOutcome::Direct(id)
        | ResolveOutcome::Rule { target: id, .. }
        | ResolveOutcome::Fallback(id)
        | ResolveOutcome::DefaultPersona(id) => deliver_one(registry, sender, body, id),
        ResolveOutcome::FanOut(ids) => {
            for id in ids {
                deliver_one(registry, sender, body, id)?;
            }
            Ok(())
        }
        ResolveOutcome::SystemDefault => {
            // Plan line 38: "SystemDefault — a synthetic persona that
            // logs the message and ack-nowledges — so human messages
            // are never silently dropped." Phase 5 ships this as a
            // tracing-warn + Ok rather than constructing an actual
            // persona; the human-visible TUI surfaces a "no fronting
            // configured" status separately (T6's FrontingChanged
            // event drives that).
            tracing::warn!(
                target = "pattern_runtime::fronting_dispatch",
                from = ?sender.author,
                "no fronting configured and no Active personas available; \
                 message acked but not routed"
            );
            Ok(())
        }
    }
}

/// Deliver a single message to one persona's mailbox via the registry's
/// atomic `route_or_queue` primitive (Phase 4 cycle-3 consolidation).
fn deliver_one(
    registry: &AgentRegistry,
    sender: &MessageOrigin,
    body: &Message,
    id: PersonaId,
) -> Result<(), RouterError> {
    let input = MailboxInput {
        from: sender.clone(),
        msg: body.clone(),
    };
    registry.route_or_queue(&id, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_registry::SessionStatus;
    use crate::testing::InMemoryConstellationRegistry;
    use jiff::Timestamp;
    use pattern_core::PersonaRecord;
    use pattern_core::constellation::{EdgeDirection, PersonaStatus};
    use pattern_core::fronting::{
        FrontingSet, MessagePattern, RoutingRule, RoutingTable,
    };
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use smol_str::SmolStr;
    use tokio::sync::mpsc;

    fn test_msg(text: &str) -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::User,
                text.to_string(),
            ),
            id: MessageId::from(new_id().to_string()),
            position: new_snowflake_id(),
            owner_id: AgentId::from("test-agent"),
            created_at: Timestamp::now(),
            batch: BatchId::from(new_snowflake_id()),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        }
    }

    fn test_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::System {
                reason: SystemReason::Timer,
            },
            Sphere::System,
        )
    }

    fn seed_active(reg: &InMemoryConstellationRegistry, id: &str) {
        reg.seed(PersonaRecord::new(SmolStr::from(id), id.to_string(), PersonaStatus::Active));
    }

    /// AC8.3: no rule matches → fallback persona receives.
    #[tokio::test]
    async fn fallback_receives_unmatched_message() {
        let agent_reg = AgentRegistry::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent_reg.register("alice".into(), tx, SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("alice")),
            RoutingTable::default(),
        );
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry);

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("hello"))
            .await
            .unwrap();

        let received = rx.recv().await.expect("alice should receive");
        assert_eq!(
            received
                .msg
                .chat_message
                .content
                .first_text()
                .unwrap_or(""),
            "hello"
        );
    }

    /// AC8.2: Prefix("!math") rule routes to math persona.
    #[tokio::test]
    async fn rule_match_routes_to_target() {
        let agent_reg = AgentRegistry::new();
        let (math_tx, mut math_rx) = mpsc::unbounded_channel();
        let (chat_tx, mut chat_rx) = mpsc::unbounded_channel();
        agent_reg.register("math".into(), math_tx, SessionStatus::Active);
        agent_reg.register("chat".into(), chat_tx, SessionStatus::Active);

        let rules = vec![RoutingRule::new(
            "math-rule".to_string(),
            MessagePattern::Prefix("!math".to_string()),
            SmolStr::from("math"),
            10,
        )];
        let table = RoutingTable::try_from_rules(rules).unwrap();
        let fronting_set =
            FrontingSet::from_parts(Vec::new(), Some(SmolStr::from("chat")), table);
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry);

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("!math 2+2"))
            .await
            .unwrap();

        let received = math_rx.recv().await.expect("math should receive");
        assert_eq!(
            received
                .msg
                .chat_message
                .content
                .first_text()
                .unwrap_or(""),
            "!math 2+2"
        );
        // Chat mailbox must NOT have received the message.
        assert!(chat_rx.try_recv().is_err());

        let _ = EdgeDirection::Outgoing; // keep the type referenced
    }

    /// AC8.5: co-fronting fan-out — both active personas receive a copy
    /// when no rule matches and no fallback is set.
    #[tokio::test]
    async fn fan_out_delivers_to_all_active() {
        let agent_reg = AgentRegistry::new();
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        agent_reg.register("a".into(), a_tx, SessionStatus::Active);
        agent_reg.register("b".into(), b_tx, SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            vec![SmolStr::from("a"), SmolStr::from("b")],
            None,
            RoutingTable::default(),
        );
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry);

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("hi all"))
            .await
            .unwrap();

        assert!(a_rx.recv().await.is_some(), "a should receive");
        assert!(b_rx.recv().await.is_some(), "b should receive");
    }

    /// Empty fronting + Active personas in the registry → DefaultPersona
    /// (lowest-id) receives.
    #[tokio::test]
    async fn empty_fronting_uses_default_persona() {
        let agent_reg = AgentRegistry::new();
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        agent_reg.register("alpha".into(), a_tx, SessionStatus::Active);
        agent_reg.register("beta".into(), b_tx, SessionStatus::Active);

        let constellation = InMemoryConstellationRegistry::new();
        seed_active(&constellation, "alpha");
        seed_active(&constellation, "beta");
        let registry: Arc<dyn ConstellationRegistry> = Arc::new(constellation);
        let state = FrontingState::new(
            Arc::new(RwLock::new(FrontingSet::default())),
            registry,
        );

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("anything"))
            .await
            .unwrap();

        assert!(a_rx.recv().await.is_some(), "alpha (lowest id) should receive");
        assert!(b_rx.try_recv().is_err(), "beta should NOT receive");
    }

    /// SystemDefault — empty fronting AND empty registry. No mailbox
    /// receives anything; dispatch returns Ok and emits a tracing warn.
    #[tokio::test]
    async fn system_default_when_no_active_personas() {
        let agent_reg = AgentRegistry::new();
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(
            Arc::new(RwLock::new(FrontingSet::default())),
            registry,
        );

        let result =
            dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("orphan")).await;
        assert!(result.is_ok());
    }

    /// AC8.8: routing decision is taken at dispatch time. A message
    /// dispatched before a fronting mutation goes to the OLD target;
    /// a message dispatched after goes to the NEW target. Mutating
    /// the lock between the two dispatches must not re-route the
    /// already-queued message.
    #[tokio::test]
    async fn in_flight_routing_uses_snapshot_at_dispatch_time() {
        let agent_reg = AgentRegistry::new();
        let (alice_tx, mut alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        agent_reg.register("alice".into(), alice_tx, SessionStatus::Active);
        agent_reg.register("bob".into(), bob_tx, SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("alice")),
            RoutingTable::default(),
        );
        let set_lock = Arc::new(RwLock::new(fronting_set));
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(set_lock.clone(), registry);

        // First dispatch: fronting fallback = alice. Should land in alice.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("first"))
            .await
            .unwrap();

        // Now mutate fronting to fall back to bob. The first message is
        // already in alice's mpsc — no re-routing happens.
        {
            let mut guard = set_lock.write().expect("lock not poisoned");
            guard.fallback = Some(SmolStr::from("bob"));
        }

        // Second dispatch: should land in bob.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("second"))
            .await
            .unwrap();

        let alice_msg = alice_rx
            .recv()
            .await
            .expect("alice should have first message");
        assert_eq!(
            alice_msg.msg.chat_message.content.first_text().unwrap_or(""),
            "first"
        );
        assert!(
            alice_rx.try_recv().is_err(),
            "alice must not have a second message — routing committed at dispatch time"
        );
        let bob_msg = bob_rx.recv().await.expect("bob should have second message");
        assert_eq!(
            bob_msg.msg.chat_message.content.first_text().unwrap_or(""),
            "second"
        );
    }
}
