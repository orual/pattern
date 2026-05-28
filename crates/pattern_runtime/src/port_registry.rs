// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Port registry — runtime-global storage of registered `Port`
//! implementations + dispatcher actor for handler-side `Call` / `Subscribe` /
//! `Unsubscribe` requests.
//!
//! ## Why split into registry + dispatcher
//!
//! The registry itself (CRUD on the ports map) is sync and infrequent — boot
//! time + plugin load. A `DashMap<PortId, Arc<dyn Port>>` is the right tool:
//! atomic check-and-insert via `entry`, no actor needed.
//!
//! The dispatcher is the async actor that drives plugin code. Handler-side
//! `Call` and `Subscribe` originate on the eval-worker thread (sync, no
//! ambient tokio runtime). The dispatcher actor runs on the runtime's tokio
//! runtime and handles all `await`s into plugin code. Communication: handler
//! → actor via `tokio::sync::mpsc::Sender::blocking_send`; actor → handler
//! reply via `crossbeam_channel::Sender` embedded in each Op variant. The
//! handler waits for the reply with `recv_timeout`. No `block_on` against
//! arbitrary plugin code anywhere.
//!
//! ## Subscriptions
//!
//! Subscriptions are tracked per-`(session_key, port_id)` in the dispatcher.
//! `Subscribe` spawns a drain task that reads from the port's
//! `BoxStream<PortEvent>` and pushes `MessageAttachment::PortEvent` entries
//! onto the session's async-reminder buffer. The drain task's
//! `tokio::task::AbortHandle` is stored so `Unsubscribe`,
//! `CancelSubscriptionsFor` (fired by `unregister`), and `Shutdown` can stop
//! it.
//!
//! ## Race-window analysis
//!
//! The dispatcher actor processes ops serially via a single `recv().await`
//! loop — no `tokio::select` over multiple channels. Each op runs to
//! completion (including spawn-drain-task + insert-AbortHandle on Subscribe)
//! before the next op is received. So an `Unsubscribe` queued after a
//! `Subscribe` cannot land before the AbortHandle is in the map.
//!
//! `Subscribe` for an existing `(session_key, port_id)` aborts the prior
//! handle before installing the new one (re-subscribe is supported and idempotent).
//!
//! `unregister(port_id)` fires an `Op::CancelSubscriptionsFor(port_id)` that
//! aborts every active subscription for that port across all sessions. Race:
//! a new `Subscribe` for the same port_id arriving at the actor between the
//! `dashmap.remove` and the `CancelSubscriptionsFor` op simply returns
//! `NotFound` (port lookup fails); no leak.
//!
//! ## Shutdown
//!
//! `TidepoolRuntime`'s Drop sends `Op::Shutdown` to the dispatcher via
//! best-effort `try_send`. The actor breaks its loop, aborts all live
//! subscriptions, and exits. If the runtime's tokio runtime is already gone
//! (process tearing down), the task is leaked at process exit — acceptable.

pub mod dispatcher;
pub mod registry;

pub use dispatcher::Op;
pub use registry::PortRegistryImpl;
