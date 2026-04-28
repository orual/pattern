//! Multi-agent smoke test — Phase 7 AC10 end-to-end.
//!
//! Verifies AC10.1 (two-persona constellation), AC10.2 (deterministic scripted
//! flow), AC10.4 (delegation libraries available), AC10.5 (clear assertion
//! context), AC10.6 (no shared-state collisions).
//!
//! Nine steps:
//!
//! 1. Setup — persona loading, registry, memory store.
//! 2. Persona loading — capabilities verified.
//! 3. Human message — dispatched to supervisor via fronting.
//! 4. Delegation — supervisor routes to specialist via AgentRegistry.
//! 5. Capability enforcement — specialist cannot compile Shell.execute (tidepool-gated).
//! 6. Fork-and-merge — lightweight fork writes, merges back to parent.
//! 7. Result propagation — specialist result routes back to supervisor.
//! 8. Concurrency check — unique tempdir per run.
//! 9. Error clarity — every assertion has a step-identifying context message.

#[path = "support/multi_agent_scripts.rs"]
mod scripts;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use smol_str::SmolStr;
use tokio::sync::mpsc;

use pattern_core::constellation::ConstellationRegistry;
use pattern_core::fronting::{FrontingSet, RoutingTable};
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{AgentAuthor, Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::fronting_dispatch::{FrontingState, dispatch_to_mailboxes};
use pattern_runtime::mailbox::MailboxInput;
use pattern_runtime::persona_loader::load_persona;
use pattern_runtime::spawn::fork::ForkHandle;
use pattern_runtime::testing::InMemoryConstellationRegistry;
use pattern_runtime::timeout::CancelState;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("multi_agent");
    p.push(name);
    p
}

fn test_msg(text: &str) -> Message {
    Message {
        chat_message: genai::chat::ChatMessage::new(
            genai::chat::ChatRole::User,
            text.to_string(),
        ),
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        owner_id: AgentId::from("test-origin"),
        created_at: jiff::Timestamp::now(),
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

fn extract_body(input: &MailboxInput) -> &str {
    input.msg.chat_message.content.first_text().unwrap_or("")
}

fn open_cache(parent_id: &str, child_id: &str) -> Arc<MemoryCache> {
    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    for id in [parent_id, child_id] {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: format!("Smoke Agent {id}"),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
            .expect("step 6: create_agent FK seed");
    }
    Arc::new(MemoryCache::new(db))
}

fn seed_text_block(cache: &MemoryCache, agent_id: &str, label: &str, content: &str) {
    let bc = BlockCreate::new(
        label.to_string(),
        MemoryBlockType::Working,
        BlockSchema::text(),
    );
    cache.create_block(agent_id, bc).expect("create_block");
    let doc = cache
        .get(agent_id, label)
        .expect("get after create")
        .expect("block must exist after create");
    doc.set_text(content, true).expect("set_text");
}

// ── Main smoke test ──────────────────────────────────────────────────────────

/// Multi-agent smoke test: two-persona constellation with delegation, fronting,
/// capability enforcement, and fork-and-merge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_agent_smoke() {
    // Step 8 (AC10.6): unique tempdir per run — no shared-state collisions.
    let _tempdir = tempfile::TempDir::new()
        .expect("step 8: must create unique tempdir for test isolation");

    // ── Step 1: setup ────────────────────────────────────────────────────

    // Step 2: persona loading — verify capabilities from KDL.
    let supervisor =
        load_persona(&fixture("supervisor.kdl")).expect("step 2: supervisor persona must load");
    let specialist =
        load_persona(&fixture("specialist.kdl")).expect("step 2: specialist persona must load");

    let supervisor_id = supervisor.agent_id.as_str();
    let specialist_id = specialist.agent_id.as_str();

    // Verify supervisor capabilities.
    {
        let caps = supervisor
            .capabilities
            .as_ref()
            .expect("step 2: supervisor must have explicit capabilities");
        assert!(
            caps.contains(EffectCategory::Memory),
            "step 2: supervisor must have Memory capability"
        );
        assert!(
            caps.contains(EffectCategory::Message),
            "step 2: supervisor must have Message capability"
        );
        assert!(
            caps.contains(EffectCategory::Spawn),
            "step 2: supervisor must have Spawn capability"
        );
        assert!(
            caps.contains(EffectCategory::Constellation),
            "step 2: supervisor must have Constellation capability"
        );
        assert!(
            caps.has_flag(CapabilityFlag::FrontingControl),
            "step 2: supervisor must have FrontingControl flag"
        );
        assert!(
            caps.has_flag(CapabilityFlag::SpawnNewIdentities),
            "step 2: supervisor must have SpawnNewIdentities flag"
        );
    }

    // Verify specialist capabilities — restricted set.
    {
        let caps = specialist
            .capabilities
            .as_ref()
            .expect("step 2: specialist must have explicit capabilities");
        assert!(
            caps.contains(EffectCategory::Memory),
            "step 2: specialist must have Memory capability"
        );
        assert!(
            caps.contains(EffectCategory::Message),
            "step 2: specialist must have Message capability"
        );
        assert!(
            !caps.contains(EffectCategory::Shell),
            "step 2: specialist must NOT have Shell capability"
        );
        assert!(
            !caps.contains(EffectCategory::Spawn),
            "step 2: specialist must NOT have Spawn capability"
        );
        assert!(
            !caps.contains(EffectCategory::File),
            "step 2: specialist must NOT have File capability"
        );
    }

    // ── Step 3: human message — dispatch via fronting ─────────────────────

    let agent_reg = Arc::new(AgentRegistry::new());

    let (sup_tx, mut sup_rx) = mpsc::unbounded_channel::<MailboxInput>();
    let (spec_tx, mut spec_rx) = mpsc::unbounded_channel::<MailboxInput>();

    agent_reg.register(
        SmolStr::from(supervisor_id),
        sup_tx,
        SessionStatus::Active,
    );
    agent_reg.register(
        SmolStr::from(specialist_id),
        spec_tx,
        SessionStatus::Active,
    );

    // FrontingSet: active = [supervisor], fallback = supervisor. No routing rules.
    let fronting_set = FrontingSet::from_parts(
        vec![SmolStr::from(supervisor_id)],
        Some(SmolStr::from(supervisor_id)),
        RoutingTable::try_from_rules(vec![]).expect("empty routing table"),
    );

    let registry: Arc<dyn ConstellationRegistry> = Arc::new(InMemoryConstellationRegistry::new());
    let fronting = FrontingState::new(Arc::new(RwLock::new(fronting_set)), registry);

    // Dispatch the human message.
    dispatch_to_mailboxes(
        &agent_reg,
        &fronting,
        &system_origin(),
        &test_msg("please delegate: compute 2+2"),
    )
    .await
    .expect("step 3: dispatch human message must succeed");

    // Supervisor receives the human message via fronting fallback.
    let sup_msg = sup_rx
        .recv()
        .await
        .expect("step 3: supervisor must receive human message");
    assert_eq!(
        extract_body(&sup_msg),
        "please delegate: compute 2+2",
        "step 3: supervisor must receive the exact human message"
    );

    // ── Step 4: delegation lands in specialist's mailbox ─────────────────

    // Simulate the supervisor delegating to the specialist via AgentRegistry.
    let delegation_msg = MailboxInput {
        from: MessageOrigin::new(
            Author::Agent(AgentAuthor {
                agent_id: SmolStr::from(supervisor_id),
            }),
            Sphere::Internal,
        ),
        msg: test_msg("compute 2+2"),
    };

    agent_reg
        .route_or_queue(&SmolStr::from(specialist_id), delegation_msg)
        .expect("step 4: delegation routing must succeed");

    let spec_msg = spec_rx
        .recv()
        .await
        .expect("step 4: specialist must receive delegation");
    assert_eq!(
        extract_body(&spec_msg),
        "compute 2+2",
        "step 4: specialist must receive the delegated task body"
    );

    // ── Step 5: capability enforcement (AC1.2 end-to-end) ────────────────

    // This step requires tidepool-extract to compile a Haskell program
    // against the specialist's restricted prelude. Skip gracefully if
    // tidepool-extract is not available.
    if pattern_runtime::preflight::check().is_ok() {
        let sdk_dir = pattern_runtime::SdkLocation::default()
            .resolve()
            .expect("step 5: SDK dir must exist when tidepool-extract is available");

        // Build a specialist prelude that excludes Shell. The specialist's
        // CapabilitySet has only Memory + Message — no Shell constructors
        // should be in scope.
        let specialist_caps: CapabilitySet =
            [EffectCategory::Memory, EffectCategory::Message]
                .into_iter()
                .collect();

        // Build preamble with the specialist's restricted capabilities.
        let preamble = pattern_runtime::sdk::preamble::build_for(&specialist_caps);

        // Program that tries to call Shell.execute — should fail at compile time.
        // Build a source that tries Shell.execute — absent from the restricted preamble.
        let source = format!(
            "{preamble}\n\
             agent :: M ()\n\
             agent = do\n\
             \x20 _ <- Shell.execute \"echo capability-probe\"\n\
             \x20 pure ()"
        );

        // The bundle type doesn't matter — GHC rejects the source before
        // runtime dispatch. Use a minimal Time+Log bundle that compiles.
        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tidepool_runtime::compile_and_run(
                    &source,
                    "agent",
                    &[sdk_dir.as_path()],
                    &mut frunk::hlist![
                        pattern_runtime::sdk::handlers::time::TimeHandler,
                        pattern_runtime::sdk::handlers::log::LogHandler::default(),
                    ],
                    &(),
                )
            })
            .expect("step 5: thread spawn should succeed")
            .join()
            .expect("step 5: thread should not panic");

        assert!(
            result.is_err(),
            "step 5: Shell.execute must fail to compile when Shell capability is absent"
        );
        let err_msg = format!("{:?}", result.unwrap_err());
        // GHC should report the constructor/variable is not in scope.
        let has_scope_error = err_msg.contains("not in scope")
            || err_msg.contains("Not in scope")
            || err_msg.contains("Variable not in scope")
            || err_msg.contains("unknown constructor");
        assert!(
            has_scope_error,
            "step 5: error must be a compile-time scope error, not a runtime error; got: {err_msg}"
        );
        eprintln!("step 5: capability enforcement verified (compile-time Shell.execute rejection)");
    } else {
        eprintln!(
            "step 5: SKIPPED — tidepool-extract not available; \
             capability compile-time enforcement not verified"
        );
    }

    // ── Step 6: fork-and-merge ───────────────────────────────────────────

    let parent_id = supervisor_id;
    let child_id = "smoke-fork-child";

    let parent_cache = open_cache(parent_id, child_id);

    // Seed a notes block on the parent.
    seed_text_block(&parent_cache, parent_id, "notes", "initial-notes");

    // Fork the parent cache for the child.
    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(parent_id, child_id)
            .expect("step 6: fork_for_child must succeed"),
    );

    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "smoke-fork".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_id.into(),
        Arc::downgrade(&parent_cache),
        cancel,
    );

    // Child writes to its notes block.
    seed_text_block(&child_cache, child_id, "notes", "fork-note");

    // Merge back.
    let report = handle
        .merge_back_lightweight()
        .expect("step 6: merge_back_lightweight must succeed");

    assert!(
        report.blocks_merged > 0,
        "step 6: merge must report at least one block merged; got {}",
        report.blocks_merged
    );

    // Verify the parent sees the fork's write.
    let parent_notes = parent_cache
        .get(parent_id, "notes")
        .expect("step 6: parent get notes must succeed")
        .expect("step 6: parent notes block must exist after merge");
    let notes_content = parent_notes.text_content();
    assert!(
        notes_content.contains("fork-note"),
        "step 6: parent notes must contain 'fork-note' after merge; got: {notes_content}"
    );

    // ── Step 7: result propagation ───────────────────────────────────────

    // Simulate the specialist sending results back to the supervisor.
    let result_msg = MailboxInput {
        from: MessageOrigin::new(
            Author::Agent(AgentAuthor {
                agent_id: SmolStr::from(specialist_id),
            }),
            Sphere::Internal,
        ),
        msg: test_msg("result: 4"),
    };

    agent_reg
        .route_or_queue(&SmolStr::from(supervisor_id), result_msg)
        .expect("step 7: result routing back to supervisor must succeed");

    let result_received = sup_rx
        .recv()
        .await
        .expect("step 7: supervisor must receive specialist result");
    assert_eq!(
        extract_body(&result_received),
        "result: 4",
        "step 7: supervisor must receive the specialist's result '4'"
    );

    // Verify no stray messages in either mailbox.
    assert!(
        sup_rx.try_recv().is_err(),
        "step 7: supervisor must have no additional stray messages"
    );
    assert!(
        spec_rx.try_recv().is_err(),
        "step 7: specialist must have no additional stray messages"
    );

    eprintln!("multi_agent_smoke: all steps passed");
}
