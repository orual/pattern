// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Recall` (`haskell/Pattern/Recall.hs`).
//!
//! Archival-entry CRUD with optional scope on the search operation.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Recall` GADT.
#[derive(Debug, FromCore)]
pub enum RecallReq {
    /// `RecallInsert :: ArchivalContent -> Recall EntryId`
    #[core(module = "Pattern.Recall", name = "RecallInsert")]
    Insert(String),

    /// `RecallSearch :: RecallQuery -> Maybe Scope -> Recall [ArchivalHit]`
    #[core(module = "Pattern.Recall", name = "RecallSearch")]
    Search(String, Option<String>),
}
