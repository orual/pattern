// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Atomic `messages.db` backup, restore, and rotation.
//!
//! All logic is library functions; `pattern_cli` is a thin consumer.
//!
//! # Submodules
//!
//! - [`snapshot`] — atomic snapshot creation via rusqlite's `Backup` API.
//! - [`rotation`] — GFS-style retention policy (keep-N + hourly/daily/monthly
//!   thinning). Includes [`rotation::list_snapshots`].
//! - [`restore`] — pre-restore safety snapshot + atomic swap into `messages.db`.
//!
//! # Snapshot filename format
//!
//! Filenames use `%Y-%m-%dT%H%M%SZ` (e.g. `2026-04-19T120000Z.sqlite`).
//! The format is Windows-safe (no colons), ISO-8601-like, and sorts
//! lexicographically by recency.

pub mod error;
pub mod restore;
pub mod rotation;
pub mod scheduler;
pub mod snapshot;
pub mod types;

pub use error::BackupError;
pub use types::{RetentionPolicy, SnapshotInfo};
