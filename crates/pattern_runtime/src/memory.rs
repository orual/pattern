// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Memory subsystem: adapter, turn history, and supporting types.
//!
//! - [`MemoryStoreAdapter`] — thin delegating wrapper over `Arc<dyn MemoryStore>`
//!   with a pending `BlockWrite` buffer, drained at turn close.
//! - [`TurnHistory`] — in-memory active turn history + cached archive-summary
//!   head, with running estimated-token count.

pub mod adapter;
pub mod turn_history;

pub use adapter::MemoryStoreAdapter;
pub use turn_history::TurnHistory;
