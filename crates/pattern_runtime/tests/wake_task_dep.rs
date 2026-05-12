//! End-to-end firing of `WakeCondition::TaskDependencyResolved`.
//!
//! Verifies AC7.3 (the wake fires when a task transitions to
//! `Completed`) using the in-memory store + `BlockChangeNotifier`
//! pair. The full `MemoryCache` + subscriber-worker stack is exercised
//! by `pattern_memory`'s tests; this file isolates the wake-side
//! glue.
//!
//! Test-support feature gating: `pattern_runtime::testing::*` is
//! behind `cfg(any(test, feature = "test-support"))`. Integration
//! tests in `tests/` see only `test-support`, so the dev-dep
//! self-reference at the bottom of `Cargo.toml` enables it.

use std::sync::Arc;
use std::time::Duration;

use smol_str::SmolStr;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope, TaskEdgeRef};
use pattern_core::types::origin::{Author, SystemReason};

use pattern_runtime::mailbox::Mailbox;
use pattern_runtime::sdk::handlers::tasks::{handle_create, handle_transition};
use pattern_runtime::testing::in_memory_store::InMemoryMemoryStore;
use pattern_runtime::wake::{WakeCondition, WakeRegistry};

fn task_list_schema() -> BlockSchema {
    BlockSchema::TaskList {
        default_status: None,
        default_owner: None,
        display_limit: None,
    }
}

fn seed_block(store: &dyn MemoryStore, agent: &str, label: &str) -> String {
    let create = BlockCreate::new(
        label.to_string(),
        MemoryBlockType::Working,
        task_list_schema(),
    )
    .with_description("test".to_string())
    .with_char_limit(4096);
    let sdoc = store
        .create_block(&Scope::global(agent), create)
        .expect("create TaskList block");
    sdoc.metadata().id.clone()
}

fn sample_spec(subject: &str) -> String {
    // TaskCreateRequest JSON wrapping a single TaskSpec
    format!(
        "{{\"items\":[{{\"subject\":\"{subject}\",\"description\":\"\",\"metadata\":null}}]}}"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_dep_resolved_fires_on_completion() {
    let agent = "agent-x";
    let label = "tasks";

    let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let block_id = seed_block(&*store, agent, label);
    let scope = Scope::global(agent);

    // Create a Pending task. handle_create returns a Vec<TaskItemId>; this
    // single-item request returns exactly one.
    let item_id = handle_create(&*store, &scope, agent, label, &sample_spec("ship-it"))
        .expect("create task")
        .into_iter()
        .next()
        .expect("single-item request returns one id");

    let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
    let (mailbox, _) = Mailbox::new(PersonaId::from(agent));
    let registry = WakeRegistry::new(mailbox.clone(), tokio::runtime::Handle::current())
        .with_block_change_notifier(notifier.clone())
        .with_memory_store(store.clone());

    let edge = TaskEdgeRef {
        block: SmolStr::new(label),
        task_item: Some(SmolStr::new(&item_id)),
    };
    let wake_id = registry
        .register_test(
            SmolStr::new("dep-1"),
            WakeCondition::TaskDependencyResolved {
                task: edge.clone(),
                agent_id: SmolStr::new(agent),
            },
        )
        .expect("register");
    assert_eq!(wake_id.as_str(), "dep-1");

    // Yield so the spawned evaluator task is scheduled.
    tokio::task::yield_now().await;

    let mut rx = mailbox.lock_rx().await;

    // First fire while still Pending — should NOT produce a wake.
    let bref = pattern_core::types::block_ref::BlockRef::with_owner(
        label.to_string(),
        block_id.clone(),
        agent.to_string(),
    );
    notifier.fire(&block_id, &bref);
    let no_wake = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_wake.is_err(),
        "no wake expected while task is still Pending; got {no_wake:?}"
    );

    // Transition the item to Completed (writes through the same store
    // the evaluator reads from). The transition is in-memory; the
    // notifier fire below stands in for the subscriber render that
    // would normally announce the change.
    let edge_ref_str = format!("{label}#{item_id}");
    let completed_status = "\"completed\"".to_string();
    handle_transition(&*store, &scope, agent, &edge_ref_str, &completed_status).expect("transition");

    notifier.fire(&block_id, &bref);

    let input = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("wake should fire within 1s")
        .expect("mailbox channel open");
    match input.from.author {
        Author::System {
            reason: SystemReason::TaskDependencyResolved { task },
        } => {
            assert_eq!(task.block.as_str(), label);
            assert_eq!(
                task.task_item.as_ref().expect("item id").as_str(),
                item_id.as_str()
            );
        }
        other => panic!("expected TaskDependencyResolved, got {other:?}"),
    }

    // Subsequent fires must not produce additional wakes — the
    // condition is one-shot.
    notifier.fire(&block_id, &bref);
    let extra = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        extra.is_err(),
        "wake should be one-shot; got extra activation {extra:?}"
    );

    drop(registry);
}
