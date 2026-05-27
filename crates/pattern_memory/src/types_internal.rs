// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Implementation-only types for the memory subsystem.
//!
//! These types are used internally by `MemoryCache` and do not appear in
//! [`pattern_core::traits::MemoryStore`] signatures. They are not part of
//! the public API surface.

use chrono::{DateTime, Utc};
use loro::VersionVector;
use serde::{Deserialize, Serialize};

use pattern_core::memory::StructuredDocument;

/// A cached memory block with its LoroDoc.
///
/// Metadata (id, agent_id, label, etc.) is now embedded in the StructuredDocument
/// and accessed via `doc.id()`, `doc.label()`, etc.
#[derive(Debug)]
pub struct CachedBlock {
    /// The structured document wrapper with embedded metadata.
    /// (LoroDoc is internally Arc'd and thread-safe)
    pub doc: StructuredDocument,

    /// Last sequence number we've seen from DB.
    pub last_seq: i64,

    /// Frontier at last persist (for delta export).
    pub last_persisted_frontier: Option<VersionVector>,

    /// Whether we have unpersisted changes.
    pub dirty: bool,

    /// When this was last accessed (for eviction).
    pub last_accessed: DateTime<Utc>,
}

/// Source of a memory change (for audit trails).
///
/// Not yet used in the current phase but will be wired into the change-tracking
/// pipeline in later phases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ChangeSource {
    /// Change made by an agent.
    Agent(String),
    /// Change made by a human/partner.
    Human(String),
    /// Change made by system (e.g., compression, migration).
    System,
    /// Change from external integration (e.g., Discord, Bluesky).
    Integration(String),
}
