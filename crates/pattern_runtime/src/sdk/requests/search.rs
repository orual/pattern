// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Search` (`haskell/Pattern/Search.hs`).
//!
//! Three search domain variants — messages, archival, or all — each
//! taking an optional scope string that the handler parses into a
//! [`pattern_core::types::SearchScope`].

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Search` GADT.
#[derive(Debug, FromCore)]
pub enum SearchReq {
    /// `SearchMessages :: SearchQuery -> Maybe Scope -> Search [SearchHit]`
    #[core(module = "Pattern.Search", name = "SearchMessages")]
    SearchMessages(String, Option<String>),

    /// `SearchArchival :: SearchQuery -> Maybe Scope -> Search [SearchHit]`
    #[core(module = "Pattern.Search", name = "SearchArchival")]
    SearchArchival(String, Option<String>),

    /// `SearchAll :: SearchQuery -> Maybe Scope -> Search [SearchHit]`
    #[core(module = "Pattern.Search", name = "SearchAll")]
    SearchAll(String, Option<String>),
}
