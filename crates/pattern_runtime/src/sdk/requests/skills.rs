// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Skills` (`haskell/Pattern/Skills.hs`).
//!
//! Five skill-operation variants supporting the SDK surface methods:
//! `list`, `get_metadata`, `load`, `search`, and `get_usage_stats`.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Skills` GADT.
#[derive(Debug, FromCore)]
pub enum SkillsReq {
    #[core(module = "Pattern.Skills", name = "List")]
    List,

    #[core(module = "Pattern.Skills", name = "GetMetadata")]
    GetMetadata(String /* BlockHandle */),

    #[core(module = "Pattern.Skills", name = "Load")]
    Load(String /* BlockHandle */),

    #[core(module = "Pattern.Skills", name = "Search")]
    Search(String /* query text */),

    #[core(module = "Pattern.Skills", name = "GetUsageStats")]
    GetUsageStats(String /* BlockHandle */),
}
