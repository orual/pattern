// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Web` (`haskell/Pattern/Web.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Web` GADT.
///
/// - `WebSearch(query, limit)`: search the web using Brave/DDG cascade.
///   Returns JSON array of `{title, url, snippet}` objects.
/// - `WebFetch(url, format)`: fetch URL content. Format: "readable" (default)
///   extracts article text, "raw" returns HTML. Returns text content.
/// - `WebFetchContinue(url, offset, limit)`: continue reading a previously
///   fetched page from the given character offset.
#[derive(Debug, FromCore)]
pub enum WebReq {
    #[core(module = "Pattern.Web", name = "WebSearch")]
    WebSearch(String, Option<i64>),
    #[core(module = "Pattern.Web", name = "WebFetch")]
    WebFetch(String, Option<String>),
    #[core(module = "Pattern.Web", name = "WebFetchContinue")]
    WebFetchContinue(String, i64, Option<i64>),
}
