// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Evaluator for [`super::WakeCondition::TaskDependencyResolved`].
//!
//! Piggybacks on the same
//! [`pattern_memory::subscriber::BlockChangeNotifier`] fan-out that
//! [`super::block_changed::spawn_block_changed`] uses (T8): subscribes
//! to the parent `TaskList` block's id, and on each fired callback
//! re-reads the targeted task item's status. When the item transitions
//! to [`pattern_core::types::memory_types::TaskStatus::Completed`] the
//! evaluator pushes a wake [`crate::mailbox::MailboxInput`] and
//! self-disables (one-shot — dependency resolution fires exactly once
//! per registration).
//!
//! The status-read is naive on purpose: walk the parent block's
//! `items` movable list, find the entry whose `id` field matches
//! `task_ref.task_item`, read its `status` string. This mirrors how
//! `sdk::handlers::tasks` reads the same shape, but inlined here so
//! the wake module doesn't take a dependency on the tasks handler's
//! private helpers (which are tied to handler-local error types).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use loro::LoroValue;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::memory_types::{TaskEdgeRef, TaskStatus};
use pattern_core::types::origin::SystemReason;
use smol_str::SmolStr;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::mailbox::Mailbox;
use crate::wake::registry::wake_mailbox_input;

/// Spawn the evaluator task for a
/// [`super::WakeCondition::TaskDependencyResolved`].
///
/// `parent_block` is the resolved `BlockRef` for the `TaskList` block
/// whose `block_id` keys the subscription. `task` is the agent-supplied
/// reference (item-level — block-level rejected at registration in
/// [`super::WakeRegistry::register`]). `agent_id` scopes the
/// `MemoryStore` reads used to check status.
///
/// The evaluator self-disables after the first observed
/// `Completed` — `TaskDependencyResolved` is one-shot per the design
/// plan (`docs/implementation-plans/2026-04-19-v3-multi-agent/phase_04.md`
/// Task 9). Future `BlockChanged` fires after that point are dropped.
/// The subscription guard stays alive until the registry aborts the
/// task; the `fired` flag is the actual disabling primitive.
///
/// `tokio_handle` is required because this function may be called from
/// the eval-worker OS thread, which has no ambient tokio runtime.
pub(super) fn spawn_task_dependency_resolved(
    parent_block: BlockRef,
    task: TaskEdgeRef,
    agent_id: SmolStr,
    block_scope: pattern_core::types::memory_types::Scope,
    store: Arc<dyn MemoryStore>,
    notifier: pattern_memory::subscriber::BlockChangeNotifier,
    mailbox: Arc<Mailbox>,
    tokio_handle: &Handle,
) -> JoinHandle<()> {
    let fired = Arc::new(AtomicBool::new(false));
    let fired_cb = fired.clone();
    let task_for_cb = task.clone();
    let _agent_for_cb = agent_id.clone();
    let scope_for_cb = block_scope.clone();
    let store_for_cb = store.clone();
    let mailbox_for_cb = mailbox.clone();
    let parent_label = parent_block.label.clone();

    let callback: pattern_memory::subscriber::BlockChangeCallback = Arc::new(move |_bref| {
        // Self-echo and post-fire short-circuit.
        if fired_cb.load(Ordering::SeqCst) {
            return;
        }
        let item_id = task_for_cb
            .task_item
            .as_ref()
            .expect("registry rejects block-level refs at register time");

        match read_task_status(&*store_for_cb, &scope_for_cb, &parent_label, item_id) {
            Ok(Some(TaskStatus::Completed)) => {
                // Race: another concurrent fire might have flipped the
                // flag after our load. swap returns the *prior* value;
                // exit if someone else got here first.
                if fired_cb.swap(true, Ordering::SeqCst) {
                    return;
                }
                let body = format!("wake: task dependency resolved — {}", task_for_cb);
                let input = wake_mailbox_input(
                    SystemReason::TaskDependencyResolved {
                        task: task_for_cb.clone(),
                    },
                    &body,
                );
                // send_input bumps `pending` so the drain loop's
                // note_consumed call balances out.
                let _ = mailbox_for_cb.send_input(input);
            }
            Ok(_) | Err(_) => {
                // Not yet completed, or transient read failure. The
                // subscription stays alive; the next change event will
                // re-check.
            }
        }
    });

    // Subscribe synchronously so the callback is registered before we
    // hand back the JoinHandle — same rationale as
    // `spawn_block_changed`: callers (and tests) shouldn't have to
    // yield to the executor before the subscription is live.
    let subscription = notifier.subscribe(&parent_block.block_id, callback);

    tokio_handle.spawn(async move {
        let _subscription = subscription;
        std::future::pending::<()>().await;
    })
}

/// Read the status of a single task item from a `TaskList` block.
///
/// Returns `Ok(Some(status))` on a valid read, `Ok(None)` if the item
/// id wasn't found in the block's items list, and `Err(_)` for store
/// failures or schema corruption.
fn read_task_status(
    store: &dyn MemoryStore,
    scope: &pattern_core::types::memory_types::Scope,
    block_label: &str,
    item_id: &str,
) -> Result<Option<TaskStatus>, ReadStatusError> {
    let sdoc = store
        .get_block(scope, block_label)
        .map_err(|e| ReadStatusError::Store(e.to_string()))?
        .ok_or(ReadStatusError::BlockMissing)?;
    let doc = sdoc.inner();
    let list = doc.get_movable_list("items");
    let LoroValue::List(items) = list.get_deep_value() else {
        return Err(ReadStatusError::ItemsNotAList);
    };
    for v in items.iter() {
        let LoroValue::Map(m) = v else {
            continue;
        };
        let Some(LoroValue::String(id_val)) = m.get("id") else {
            continue;
        };
        if id_val.as_str() != item_id {
            continue;
        }
        let Some(LoroValue::String(status_str)) = m.get("status") else {
            return Ok(None);
        };
        return status_str
            .as_str()
            .parse::<TaskStatus>()
            .map(Some)
            .map_err(|e| ReadStatusError::UnknownStatus(e.0));
    }
    Ok(None)
}

#[derive(Debug)]
enum ReadStatusError {
    Store(String),
    BlockMissing,
    ItemsNotAList,
    #[allow(dead_code)]
    UnknownStatus(String),
}

impl std::fmt::Display for ReadStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(s) => write!(f, "store error: {s}"),
            Self::BlockMissing => write!(f, "block missing at status-read time"),
            Self::ItemsNotAList => write!(f, "task-list 'items' field is not a list"),
            Self::UnknownStatus(s) => write!(f, "unknown task status: {s:?}"),
        }
    }
}

impl std::error::Error for ReadStatusError {}
