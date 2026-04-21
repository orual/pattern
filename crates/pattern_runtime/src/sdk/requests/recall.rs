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

    /// `RecallGet :: EntryId -> Recall ArchivalContent`
    #[core(module = "Pattern.Recall", name = "RecallGet")]
    Get(String),
}
