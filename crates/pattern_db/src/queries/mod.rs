// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Database query functions.
//!
//! Organized by domain. All queries use rusqlite directly with
//! inherent `fn from_row` on each row struct.

mod agent;
mod atproto_endpoints;
pub mod constellation;
mod event;
mod folder;
pub mod fronting;
mod memory;
mod message;
mod queue;
pub mod skill_usage;
mod source;
pub mod stats;
mod task;
pub mod task_row;
pub mod wake;

pub use agent::*;
pub use atproto_endpoints::*;
pub use constellation::ConstellationRegistryDb;
pub use event::*;
pub use folder::*;
pub use fronting::{clear_fronting_set, load_fronting_set, save_fronting_set};
pub use memory::*;
pub use message::*;
pub use queue::*;
pub use skill_usage::{get_usage_stats, get_usage_stats_batch, record_usage};
pub use source::*;
pub use task::*;
pub use task_row::{TaskEdgeRow, TaskRow};
pub use wake::{WakeRegistrationRow, delete_wake_registration, insert_wake_registration, list_wakes_for_agent};
