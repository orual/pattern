// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! File manager subsystem for Pattern agents.
//!
//! This module implements the `Pattern.File` effect handler backing. It
//! provides:
//!
//! - [`error::FileError`] — typed error enum covering permission denials,
//!   config-KDL approval gating, CRDT sync failures, and conflict detection.
//! - [`types::FileInfo`] — JSON-serialisable directory-entry metadata returned
//!   by `File.ListDir`.
//! - [`policy::FilePolicy`] — KDL-backed ordered rules with last-match-wins
//!   evaluation and default-deny.
//! - [`config_detect`] — Pattern config-file shape detection.
//! - [`manager::FileManager`] — pooled `DirWatcher` coordinator with open-file
//!   lifecycle, watch-only subscriptions, and between-turn async-reminder
//!   delivery.

pub mod config_detect;
pub mod error;
pub mod manager;
pub(crate) mod path_util;
pub mod policy;
pub mod types;

pub use error::FileError;
pub use manager::FileManager;
pub use policy::{FilePolicy, RuleMode};
pub use types::FileInfo;
