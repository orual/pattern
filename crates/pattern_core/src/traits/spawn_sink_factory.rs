// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! [`SpawnSinkFactory`]: vends per-spawn turn-sinks that tag emitted
//! events with a [`SpawnSource`].
//!
//! ## Why a separate trait
//!
//! [`TurnSink`](crate::traits::TurnSink) is intentionally minimal — its
//! contract is `emit(event)`. Most implementations (`NoOpSink`,
//! `VecSink`, ad-hoc test sinks) have no notion of routing tags or
//! sub-spawns. Adding a `fork_for_spawn` method to `TurnSink` would
//! force every implementor to carry a no-op default for a method only
//! the daemon's wire-bridge cares about.
//!
//! `SpawnSinkFactory` is the right shape: a separate capability that
//! the daemon's bridge implements and stashes on `SessionContext`
//! alongside the turn-sink. When a child session is spawned, the
//! runtime asks the factory (if present) for a child sink with the
//! appropriate [`SpawnSource`] tag. Headless / test sessions don't
//! install a factory and child sinks just inherit the parent's.

use std::sync::Arc;

use smol_str::SmolStr;

use crate::spawn::SpawnSource;
use crate::traits::TurnSink;

/// Vends per-spawn turn-sinks that tag emitted events with a
/// [`SpawnSource`].
///
/// Concrete implementations live in `pattern_server` (the daemon's
/// wire-bridge) so the runtime never has to know about wire types.
/// Stored as `Option<Arc<dyn SpawnSinkFactory>>` on
/// `SessionContext`: `None` for headless/test sessions, `Some` when
/// the daemon mints a tagged bridge.
pub trait SpawnSinkFactory: Send + Sync + std::fmt::Debug {
    /// Mint a turn-sink for a child session whose events should be
    /// tagged with `source`. The returned sink shares whatever
    /// downstream channel the factory was constructed for, so the
    /// daemon's actor receives child events on the same bus as the
    /// parent's.
    fn fork_for_spawn(
        &self,
        batch_id: SmolStr,
        agent_id: SmolStr,
        source: SpawnSource,
    ) -> Arc<dyn TurnSink>;
}
