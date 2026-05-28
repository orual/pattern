// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Event routing trait for `DirWatcher`.
//!
//! Pluggable routing strategy that decides what to do with batches of
//! debounced filesystem events. Implementations ship in `routers.rs`.

use notify_debouncer_full::DebouncedEvent;

/// Pluggable event routing strategy for `DirWatcher`. Called from the
/// ingest thread with a batch of debounced events. Implementations decide
/// what to do — fanout to per-path subscribers (PathFanoutRouter), dispatch
/// to a block cache (BlockFanoutRouter), etc.
///
/// Must be `Send` because it runs on a dedicated thread. No `Sync` bound
/// because `handle(&mut self, ...)` gives exclusive access per call.
pub trait EventRouter: Send + 'static {
    fn handle(&mut self, events: Vec<DebouncedEvent>);
}
