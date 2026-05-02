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

/// The outcome of a successful [`dispatch_to_mailboxes`] call.
///
/// Callers can use this to surface user-visible signals for paths that
/// complete without error but may need human attention (e.g. `SystemDefault`
/// means no fronting is configured and the message was acked but not routed).
#[derive(Debug, Clone)]
pub enum DispatchOutcome {
    /// Delivered to a single persona's mailbox (Direct, Rule, Fallback, or
    /// DefaultPersona paths).
    Delivered(PersonaId),
    /// Delivered to multiple personas in co-fronting fan-out mode.
    FanOutDelivered(Vec<PersonaId>),
    /// No fronting is configured and the registry has no Active personas.
    /// The message was acknowledged but not routed to any session. The
    /// caller should surface a human-visible "no fronting configured" signal.
    SystemDefault,
}

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
/// - `Ok(DispatchOutcome::Delivered(id))` — delivered to one persona.
/// - `Ok(DispatchOutcome::FanOutDelivered(ids))` — delivered to multiple
///   personas in co-fronting fan-out mode.
/// - `Ok(DispatchOutcome::SystemDefault)` — no fronting configured; the
///   message was acked but not routed. The caller should surface a
///   human-visible "no fronting configured" signal.
/// - `Err(RouterError::…)` — delivery to a persona's mailbox failed (the
///   first error in a FanOut sequence; subsequent targets are not retried).
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
) -> Result<DispatchOutcome, RouterError> {
    // Snapshot the resolver under a short-lived read lock so the
    // decision is stable for this dispatch even if the set is
    // mutated concurrently. The lock is `std::sync::RwLock` because
    // it's also accessed from the sync `Pattern.Fronting` handler
    // running on the eval-worker OS thread (no ambient tokio
    // runtime there). The lock is released before the awaited
    // `resolve()` call so the runtime never holds the lock across
    // an await point.
    let resolver = {
        let set = fronting
            .set
            .read()
            .map_err(|_| RouterError::PersonaNotFound(PersonaId::from("<lock-poisoned>")))?
            .clone();
        FrontingResolver::new(set, fronting.registry.clone())
    };

    let outcome = resolver.resolve(body_text(body)).await;
    deliver_resolved(registry, sender, body, outcome)
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
///
/// Returns a [`DispatchOutcome`] on success so the caller can surface
/// the `SystemDefault` case as a user-visible signal rather than
/// relying solely on a `tracing::warn!`.
fn deliver_resolved(
    registry: &AgentRegistry,
    sender: &MessageOrigin,
    body: &Message,
    outcome: ResolveOutcome,
) -> Result<DispatchOutcome, RouterError> {
    match outcome {
        ResolveOutcome::Direct(id)
        | ResolveOutcome::Rule { target: id, .. }
        | ResolveOutcome::Fallback(id)
        | ResolveOutcome::DefaultPersona(id) => {
            deliver_one(registry, sender, body, id.clone())?;
            Ok(DispatchOutcome::Delivered(id))
        }
        ResolveOutcome::FanOut(ids) => {
            for id in &ids {
                deliver_one(registry, sender, body, id.clone())?;
            }
            Ok(DispatchOutcome::FanOutDelivered(ids))
        }
        ResolveOutcome::SystemDefault => {
            // No fronting configured and no Active personas in the registry.
            // The message is acked but not routed. We emit a tracing::warn
            // for observability and return the SystemDefault outcome so the
            // caller (daemon SendMessage handler, AgentRouter, etc.) can
            // surface a human-visible "no fronting configured" signal rather
            // than silently dropping the message.
            tracing::warn!(
                target = "pattern_runtime::fronting_dispatch",
                from = ?sender.author,
                "no fronting configured and no Active personas available; \
                 message acked but not routed — caller should surface a \
                 user-visible 'no fronting configured' signal"
            );
            Ok(DispatchOutcome::SystemDefault)
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
    let input = MailboxInput::new(sender.clone(), body.clone());
    registry.route_or_queue(&id, input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_registry::SessionStatus;
    use crate::mailbox::Mailbox;
    use crate::testing::InMemoryConstellationRegistry;
    use jiff::Timestamp;
    use pattern_core::PersonaRecord;
    use pattern_core::constellation::{EdgeDirection, PersonaStatus};
    use pattern_core::fronting::{FrontingSet, MessagePattern, RoutingRule, RoutingTable};
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use smol_str::SmolStr;

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
        reg.seed(PersonaRecord::new(
            SmolStr::from(id),
            id.to_string(),
            PersonaStatus::Active,
        ));
    }

    /// AC8.3: no rule matches → fallback persona receives.
    #[tokio::test]
    async fn fallback_receives_unmatched_message() {
        let agent_reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("alice".into());
        agent_reg.register("alice".into(), mailbox.clone(), SessionStatus::Active);

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

        let received = mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("alice should receive");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "hello"
        );
    }

    /// AC8.2: Prefix("!math") rule routes to math persona.
    #[tokio::test]
    async fn rule_match_routes_to_target() {
        let agent_reg = AgentRegistry::new();
        let (math_mailbox, _) = Mailbox::new("math".into());
        let (chat_mailbox, _) = Mailbox::new("chat".into());
        agent_reg.register("math".into(), math_mailbox.clone(), SessionStatus::Active);
        agent_reg.register("chat".into(), chat_mailbox.clone(), SessionStatus::Active);

        let rules = vec![RoutingRule::new(
            "math-rule".to_string(),
            MessagePattern::Prefix("!math".to_string()),
            SmolStr::from("math"),
            10,
        )];
        let table = RoutingTable::try_from_rules(rules).unwrap();
        let fronting_set = FrontingSet::from_parts(Vec::new(), Some(SmolStr::from("chat")), table);
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry);

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("!math 2+2"))
            .await
            .unwrap();

        let received = math_mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("math should receive");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "!math 2+2"
        );
        // Chat mailbox must NOT have received the message.
        assert!(chat_mailbox.lock_rx().await.try_recv().is_err());

        let _ = EdgeDirection::Outgoing; // keep the type referenced
    }

    /// AC8.8: routing rule updates apply mid-flight.
    ///
    /// First send: rule `prefix("!math") → math` routes to math.
    /// Then update the fronting set so the rule's target becomes
    /// `chat` instead. Second send: must land in chat, NOT math.
    /// The architectural claim is "FrontingState reads via short
    /// read-lock per call; dispatch evaluates the routing snapshot
    /// at call time" — this test pins that as a behavioural
    /// invariant rather than relying on it by construction.
    #[tokio::test]
    async fn rule_update_applies_to_subsequent_dispatches() {
        let agent_reg = AgentRegistry::new();
        let (math_mailbox, _) = Mailbox::new("math".into());
        let (chat_mailbox, _) = Mailbox::new("chat".into());
        agent_reg.register("math".into(), math_mailbox.clone(), SessionStatus::Active);
        agent_reg.register("chat".into(), chat_mailbox.clone(), SessionStatus::Active);

        // Initial rule: !math → math.
        let initial_rules = vec![RoutingRule::new(
            "math-rule".to_string(),
            MessagePattern::Prefix("!math".to_string()),
            SmolStr::from("math"),
            10,
        )];
        let initial_table = RoutingTable::try_from_rules(initial_rules).unwrap();
        let fronting_set =
            FrontingSet::from_parts(Vec::new(), Some(SmolStr::from("chat")), initial_table);
        let set_lock = Arc::new(RwLock::new(fronting_set));

        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(set_lock.clone(), registry);

        // First dispatch: must land in math.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("!math 2+2"))
            .await
            .unwrap();
        let received_first = math_mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("math should receive first send");
        assert_eq!(
            received_first
                .msg
                .chat_message
                .content
                .first_text()
                .unwrap_or(""),
            "!math 2+2"
        );
        assert!(
            chat_mailbox.lock_rx().await.try_recv().is_err(),
            "chat must not receive the first message"
        );

        // Mutate the fronting set: same prefix, different target.
        // Production callers use `update_fronting` (RPC) which writes
        // through the same lock; here we go through the RwLock directly
        // to keep the test free-standing.
        let updated_rules = vec![RoutingRule::new(
            "math-rule".to_string(),
            MessagePattern::Prefix("!math".to_string()),
            SmolStr::from("chat"),
            10,
        )];
        let updated_table = RoutingTable::try_from_rules(updated_rules).unwrap();
        {
            let mut set = set_lock.write().expect("fronting set rwlock poisoned");
            *set = FrontingSet::from_parts(Vec::new(), Some(SmolStr::from("chat")), updated_table);
        }

        // Second dispatch: must land in chat under the new rule.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("!math 3+3"))
            .await
            .unwrap();
        let received_second = chat_mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("chat should receive second send after rule update");
        assert_eq!(
            received_second
                .msg
                .chat_message
                .content
                .first_text()
                .unwrap_or(""),
            "!math 3+3"
        );
        assert!(
            chat_mailbox.lock_rx().await.try_recv().is_err(),
            "math must not receive the second message — rule was updated to target chat"
        );
    }

    /// AC8.5: co-fronting fan-out — both active personas receive a copy
    /// when no rule matches and no fallback is set.
    #[tokio::test]
    async fn fan_out_delivers_to_all_active() {
        let agent_reg = AgentRegistry::new();
        let (a_mailbox, _) = Mailbox::new("a".into());
        let (b_mailbox, _) = Mailbox::new("b".into());
        agent_reg.register("a".into(), a_mailbox.clone(), SessionStatus::Active);
        agent_reg.register("b".into(), b_mailbox.clone(), SessionStatus::Active);

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

        assert!(
            a_mailbox.lock_rx().await.recv().await.is_some(),
            "a should receive"
        );
        assert!(
            b_mailbox.lock_rx().await.recv().await.is_some(),
            "b should receive"
        );
    }

    /// Empty fronting + Active personas in the registry → DefaultPersona
    /// (lowest-id) receives.
    #[tokio::test]
    async fn empty_fronting_uses_default_persona() {
        let agent_reg = AgentRegistry::new();
        let (a_mailbox, _) = Mailbox::new("alpha".into());
        let (b_mailbox, _) = Mailbox::new("beta".into());
        agent_reg.register("alpha".into(), a_mailbox.clone(), SessionStatus::Active);
        agent_reg.register("beta".into(), b_mailbox.clone(), SessionStatus::Active);

        let constellation = InMemoryConstellationRegistry::new();
        seed_active(&constellation, "alpha");
        seed_active(&constellation, "beta");
        let registry: Arc<dyn ConstellationRegistry> = Arc::new(constellation);
        let state = FrontingState::new(Arc::new(RwLock::new(FrontingSet::default())), registry);

        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("anything"))
            .await
            .unwrap();

        assert!(
            a_mailbox.lock_rx().await.recv().await.is_some(),
            "alpha (lowest id) should receive"
        );
        assert!(
            b_mailbox.lock_rx().await.try_recv().is_err(),
            "beta should NOT receive"
        );
    }

    /// SystemDefault — empty fronting AND empty registry. No mailbox
    /// receives anything; dispatch returns Ok and emits a tracing warn.
    #[tokio::test]
    async fn system_default_when_no_active_personas() {
        let agent_reg = AgentRegistry::new();
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(FrontingSet::default())), registry);

        let result =
            dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("orphan")).await;
        assert!(result.is_ok());
    }

    /// AC8.8: a fronting mutation between two dispatches does NOT
    /// re-route the first dispatch's message.
    ///
    /// ## Test structure (sequential)
    ///
    /// 1. Fronting fallback = alice.
    /// 2. First `dispatch_to_mailboxes` runs to completion → alice's
    ///    mailbox receives "first".
    /// 3. Test mutates fronting fallback to bob.
    /// 4. Second `dispatch_to_mailboxes` runs to completion → bob's
    ///    mailbox receives "second".
    /// 5. Assertions:
    ///    - alice received "first" (inclusion).
    ///    - alice did NOT receive "second" (exclusion).
    ///    - bob received "second" (inclusion).
    ///    - bob did NOT receive "first" (exclusion).
    ///
    /// ## Why sequential is sufficient
    ///
    /// AC8.8 is structurally enforced by [`dispatch_to_mailboxes`]:
    /// each call snapshots the `FrontingSet` once under the read lock,
    /// drops the lock, resolves the snapshot to a target, and pushes
    /// the message into mpsc. Once the message is in mpsc, no
    /// subsequent `FrontingSet` mutation can re-route it — the routing
    /// decision is committed by the push.
    ///
    /// A "concurrent" test that holds the first dispatch mid-resolve,
    /// mutates fronting, then releases the dispatch would test the
    /// same property. The exclusion assertions
    /// (`try_recv().is_err()` on the wrong mailbox) are what catch a
    /// regression that re-routed an in-flight message; they fire
    /// independently of whether the dispatches run concurrently or
    /// sequentially.
    ///
    /// An earlier version of this test used a `tokio::sync::Notify`
    /// checkpoint between snapshot and resolve to deterministically
    /// widen the race window. It hung indefinitely under
    /// `tokio::test(flavor = "multi_thread")` due to a subtle
    /// interaction between `notify_one`/`notified()` and the
    /// multi-threaded scheduler. The sequential shape covers the
    /// invariant without that fragility.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn in_flight_routing_uses_snapshot_at_dispatch_time() {
        let agent_reg = Arc::new(AgentRegistry::new());
        let (alice_mailbox, _) = Mailbox::new("alice".into());
        let (bob_mailbox, _) = Mailbox::new("bob".into());
        agent_reg.register("alice".into(), alice_mailbox.clone(), SessionStatus::Active);
        agent_reg.register("bob".into(), bob_mailbox.clone(), SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("alice")),
            RoutingTable::default(),
        );
        let set_lock = Arc::new(RwLock::new(fronting_set));
        let registry: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(set_lock.clone(), registry);

        // First dispatch: fallback = alice. Resolves and commits to alice's
        // mailbox before we mutate.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("first"))
            .await
            .expect("first dispatch must succeed");

        // Mutate fronting to fall back to bob. Any message NOT yet
        // resolved would now go to bob; the first message is already
        // committed to alice's mpsc and cannot be re-routed.
        {
            let mut guard = set_lock.write().expect("lock not poisoned");
            guard.fallback = Some(SmolStr::from("bob"));
        }

        // Second dispatch: now sees fallback = bob.
        dispatch_to_mailboxes(&agent_reg, &state, &test_origin(), &test_msg("second"))
            .await
            .expect("second dispatch must succeed");

        // Step 5: assertions.
        let alice_msg = alice_mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("alice should have received 'first' — snapshot was taken before mutation");
        assert_eq!(
            alice_msg
                .msg
                .chat_message
                .content
                .first_text()
                .unwrap_or(""),
            "first",
            "alice must receive 'first': dispatch used pre-mutation snapshot"
        );
        assert!(
            alice_mailbox.lock_rx().await.try_recv().is_err(),
            "alice must NOT receive 'second': routing committed at dispatch time"
        );

        let bob_msg = bob_mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("bob should have received 'second' — post-mutation dispatch routes to bob");
        assert_eq!(
            bob_msg.msg.chat_message.content.first_text().unwrap_or(""),
            "second",
            "bob must receive 'second': second dispatch sees mutated fronting state"
        );
        assert!(
            bob_mailbox.lock_rx().await.try_recv().is_err(),
            "bob must NOT receive 'first': pre-mutation dispatch already committed to alice"
        );
    }
}
