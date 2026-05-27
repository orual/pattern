// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Supervisor pattern end-to-end integration test.
//!
//! Verifies AC8.2, AC8.3 composed together: three-persona setup (supervisor
//! with FrontingControl, math specialist, chat specialist), message routing
//! via rule-match and fallback, and persistence of FrontingSet to an
//! in-memory DB followed by reload.
//!
//! AC8.7 (Human-as-caller uses fronting persona's SessionContext) is daemon-
//! level behaviour and is better tested in pattern_server integration tests
//! where a real TidepoolSession is opened; it is not verified here.
//!
//! ## Test outline
//!
//! 1. Load three persona fixtures via `persona_loader::load_persona`.
//! 2. Register all three as Active in an AgentRegistry with mpsc receivers.
//! 3. Build a FrontingSet with:
//!    - active: [supervisor]
//!    - fallback: Some(supervisor)
//!    - routing: Prefix("!math") → math-specialist (priority 10),
//!      Contains("chat") → chat-specialist (priority 5)
//! 4. Dispatch three messages via `dispatch_to_mailboxes`:
//!    - "hello"      → no rule match → Fallback(supervisor)
//!    - "!math 2+2"  → Prefix("!math") match → math-specialist
//!    - "lets chat"  → Contains("chat") match → chat-specialist
//! 5. Assert each mailbox received exactly the right message.
//! 6. Save FrontingSet to an in-memory ConstellationDb.
//! 7. Reload via `load_fronting_set`; assert it round-trips correctly.
//! 8. Re-create FrontingState from the loaded set and re-dispatch "hello";
//!    assert supervisor's mailbox receives it again.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use jiff::Timestamp;
use smol_str::SmolStr;

use pattern_core::constellation::ConstellationRegistry;
use pattern_core::fronting::{FrontingSet, MessagePattern, RoutingRule, RoutingTable};
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_db::queries::fronting::{load_fronting_set, save_fronting_set};
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::fronting_dispatch::{FrontingState, dispatch_to_mailboxes};
use pattern_runtime::mailbox::{Mailbox, MailboxInput};
use pattern_runtime::persona_loader::load_persona;
use pattern_runtime::testing::InMemoryConstellationRegistry;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(name);
    p
}

fn test_msg(text: &str) -> Message {
    Message {
        chat_message: genai::chat::ChatMessage::new(genai::chat::ChatRole::User, text.to_string()),
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        owner_id: AgentId::from("test-origin"),
        created_at: Timestamp::now(),
        batch: BatchId::from(new_snowflake_id()),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    }
}

fn system_origin() -> MessageOrigin {
    MessageOrigin::new(
        Author::System {
            reason: SystemReason::Timer,
        },
        Sphere::System,
    )
}

fn build_fronting_state(supervisor_id: &str, math_id: &str, chat_id: &str) -> FrontingState {
    let math_rule = RoutingRule::new(
        "rule-math".to_string(),
        MessagePattern::Prefix("!math".to_string()),
        SmolStr::from(math_id),
        10,
    );
    let chat_rule = RoutingRule::new(
        "rule-chat".to_string(),
        MessagePattern::Contains("chat".to_string()),
        SmolStr::from(chat_id),
        5,
    );
    let table = RoutingTable::try_from_rules(vec![math_rule, chat_rule])
        .expect("routing rules must compile");

    let fronting_set = FrontingSet::from_parts(
        vec![SmolStr::from(supervisor_id)],
        Some(SmolStr::from(supervisor_id)),
        table,
    );

    let registry: Arc<dyn ConstellationRegistry> = Arc::new(InMemoryConstellationRegistry::new());

    FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry)
}

fn extract_body(input: &MailboxInput) -> &str {
    input.msg.chat_message.content.first_text().unwrap_or("")
}

// ── Main integration test ─────────────────────────────────────────────────────

/// Supervisor pattern routing: correct persona receives each message.
///
/// - "hello"     → fallback → supervisor
/// - "!math 2+2" → Prefix rule → math-specialist
/// - "lets chat" → Contains rule → chat-specialist
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supervisor_pattern_routes_messages_correctly() {
    // Step 1: load persona fixtures.
    let supervisor =
        load_persona(&fixture("supervisor_persona.kdl")).expect("supervisor persona must load");
    let math = load_persona(&fixture("math_specialist.kdl")).expect("math specialist must load");
    let chat = load_persona(&fixture("chat_specialist.kdl")).expect("chat specialist must load");

    // Verify the supervisor has FrontingControl (belt-and-suspenders: the KDL
    // fixture declares it; the loader must honour it).
    {
        use pattern_core::{CapabilityFlag, CapabilitySet};
        let caps: &CapabilitySet = supervisor
            .capabilities
            .as_ref()
            .expect("supervisor must have explicit capabilities");
        assert!(
            caps.has_flag(CapabilityFlag::FrontingControl),
            "supervisor fixture must carry FrontingControl; verify capabilities.flags in supervisor_persona.kdl"
        );
    }

    // Step 2: set up agent registry + per-agent receivers.
    let agent_reg = Arc::new(AgentRegistry::new());

    let supervisor_id = supervisor.agent_id.as_str();
    let math_id = math.agent_id.as_str();
    let chat_id = chat.agent_id.as_str();

    let (sup_mailbox, _) = Mailbox::new(SmolStr::from(supervisor_id));
    let (math_mailbox, _) = Mailbox::new(SmolStr::from(math_id));
    let (chat_mailbox, _) = Mailbox::new(SmolStr::from(chat_id));

    agent_reg.register(
        SmolStr::from(supervisor_id),
        sup_mailbox.clone(),
        SessionStatus::Active,
    );
    agent_reg.register(
        SmolStr::from(math_id),
        math_mailbox.clone(),
        SessionStatus::Active,
    );
    agent_reg.register(
        SmolStr::from(chat_id),
        chat_mailbox.clone(),
        SessionStatus::Active,
    );

    // Step 3: build FrontingState with routing rules.
    let fronting = build_fronting_state(supervisor_id, math_id, chat_id);

    // Step 4: dispatch three messages (serially — routing is deterministic).

    // "hello" → no rule match → fallback → supervisor.
    dispatch_to_mailboxes(&agent_reg, &fronting, &system_origin(), &test_msg("hello"))
        .await
        .expect("dispatch 'hello' must succeed");

    // "!math 2+2" → Prefix("!math") rule → math-specialist.
    dispatch_to_mailboxes(
        &agent_reg,
        &fronting,
        &system_origin(),
        &test_msg("!math 2+2"),
    )
    .await
    .expect("dispatch '!math 2+2' must succeed");

    // "lets chat" → Contains("chat") rule → chat-specialist.
    dispatch_to_mailboxes(
        &agent_reg,
        &fronting,
        &system_origin(),
        &test_msg("lets chat"),
    )
    .await
    .expect("dispatch 'lets chat' must succeed");

    // Step 5: assert each mailbox received exactly the right message.

    // Supervisor: "hello" only.
    let sup_msg = sup_mailbox
        .lock_rx()
        .await
        .recv()
        .await
        .expect("supervisor must have received a message");
    assert_eq!(
        extract_body(&sup_msg),
        "hello",
        "supervisor must receive 'hello' (fallback path)"
    );
    assert!(
        sup_mailbox.lock_rx().await.try_recv().is_err(),
        "supervisor must NOT have received additional messages"
    );

    // Math specialist: "!math 2+2" only.
    let math_msg = math_mailbox
        .lock_rx()
        .await
        .recv()
        .await
        .expect("math-specialist must have received a message");
    assert_eq!(
        extract_body(&math_msg),
        "!math 2+2",
        "math-specialist must receive '!math 2+2' (Prefix rule)"
    );
    assert!(
        math_mailbox.lock_rx().await.try_recv().is_err(),
        "math-specialist must NOT have received additional messages"
    );

    // Chat specialist: "lets chat" only.
    let chat_msg = chat_mailbox
        .lock_rx()
        .await
        .recv()
        .await
        .expect("chat-specialist must have received a message");
    assert_eq!(
        extract_body(&chat_msg),
        "lets chat",
        "chat-specialist must receive 'lets chat' (Contains rule)"
    );
    assert!(
        chat_mailbox.lock_rx().await.try_recv().is_err(),
        "chat-specialist must NOT have received additional messages"
    );
}

// ── Persistence + reload test ─────────────────────────────────────────────────

/// FrontingSet survives a save → reload cycle (AC8.1 persistence).
///
/// Saves a FrontingSet with two routing rules to an in-memory DB, then
/// loads it back and verifies the active persona, fallback, and rules
/// all round-trip. Finally re-creates a FrontingState from the loaded
/// set and dispatches one message to confirm routing is still correct.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fronting_set_survives_restart() {
    // Step 1: build the original set.
    let supervisor_id = "supervisor";
    let math_id = "math-specialist";
    let chat_id = "chat-specialist";

    let fronting = build_fronting_state(supervisor_id, math_id, chat_id);
    let original_set = fronting.set.read().unwrap().clone();

    // Step 2: save to an in-memory ConstellationDb.
    let db = pattern_db::ConstellationDb::open_in_memory().expect("in-memory DB must open");
    {
        let mut conn = db.get().expect("pool connection must be available");
        save_fronting_set(&mut conn, &original_set).expect("save_fronting_set must succeed");
    }

    // Step 3: load from the same DB and verify round-trip.
    let loaded = {
        let conn = db.get().expect("pool connection must be available");
        load_fronting_set(&conn)
            .expect("load_fronting_set must not error")
            .expect("loaded set must be Some (we just saved it)")
    };

    // Active personas must match.
    assert_eq!(
        loaded.active.len(),
        original_set.active.len(),
        "active persona count must survive round-trip"
    );
    for id in &original_set.active {
        assert!(
            loaded.active.contains(id),
            "active persona '{id}' must survive round-trip"
        );
    }

    // Fallback must match.
    assert_eq!(
        loaded.fallback, original_set.fallback,
        "fallback must survive round-trip"
    );

    // Routing rules must match by id and count.
    assert_eq!(
        loaded.routing.rules.len(),
        original_set.routing.rules.len(),
        "routing rule count must survive round-trip"
    );
    for rule in &original_set.routing.rules {
        let loaded_rule = loaded.routing.rules.iter().find(|r| r.id == rule.id);
        assert!(
            loaded_rule.is_some(),
            "rule '{}' must survive round-trip",
            rule.id
        );
        assert_eq!(
            loaded_rule.unwrap().target,
            rule.target,
            "rule '{}' target must survive round-trip",
            rule.id
        );
        assert_eq!(
            loaded_rule.unwrap().priority,
            rule.priority,
            "rule '{}' priority must survive round-trip",
            rule.id
        );
    }

    // Step 4: re-create FrontingState from loaded set and verify routing.
    let reloaded_registry: Arc<dyn ConstellationRegistry> =
        Arc::new(InMemoryConstellationRegistry::new());
    let reloaded_state = FrontingState::new(Arc::new(RwLock::new(loaded)), reloaded_registry);

    let agent_reg = Arc::new(AgentRegistry::new());
    let (sup_mailbox, _) = Mailbox::new(supervisor_id.into());
    let (math_mailbox, _) = Mailbox::new(math_id.into());
    let (chat_mailbox, _) = Mailbox::new(chat_id.into());

    agent_reg.register(
        SmolStr::from(supervisor_id),
        sup_mailbox.clone(),
        SessionStatus::Active,
    );
    agent_reg.register(
        SmolStr::from(math_id),
        math_mailbox.clone(),
        SessionStatus::Active,
    );
    agent_reg.register(
        SmolStr::from(chat_id),
        chat_mailbox.clone(),
        SessionStatus::Active,
    );

    // "hello" → fallback → supervisor (same path as in the first test).
    dispatch_to_mailboxes(
        &agent_reg,
        &reloaded_state,
        &system_origin(),
        &test_msg("hello"),
    )
    .await
    .expect("dispatch after reload must succeed");

    let sup_msg = sup_mailbox
        .lock_rx()
        .await
        .recv()
        .await
        .expect("supervisor must receive 'hello' after reload");
    assert_eq!(
        extract_body(&sup_msg),
        "hello",
        "routing via reloaded FrontingState must still send 'hello' to supervisor"
    );
    assert!(
        math_mailbox.lock_rx().await.try_recv().is_err(),
        "math must not receive 'hello'"
    );
    assert!(
        chat_mailbox.lock_rx().await.try_recv().is_err(),
        "chat must not receive 'hello'"
    );

    // Re-verify the math rule is also still live after reload.
    dispatch_to_mailboxes(
        &agent_reg,
        &reloaded_state,
        &system_origin(),
        &test_msg("!math sqrt(9)"),
    )
    .await
    .expect("math dispatch after reload must succeed");

    let math_msg = math_mailbox
        .lock_rx()
        .await
        .recv()
        .await
        .expect("math-specialist must receive '!math sqrt(9)' after reload");
    assert_eq!(
        extract_body(&math_msg),
        "!math sqrt(9)",
        "Prefix rule must still route correctly after reload"
    );
}
