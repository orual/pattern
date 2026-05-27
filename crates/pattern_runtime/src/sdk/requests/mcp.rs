// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Mcp` (`haskell/Pattern/Mcp.hs`).

use tidepool_bridge_derive::FromCore;

/// Mirror of the Haskell `Mcp` GADT.
///
/// Four operations:
/// - `Call` — invoke a tool on a named MCP server
/// - `Introspect` — list tools available on a server
/// - `ListServers` — list all connected servers
/// - `Unload` — disconnect a server
#[derive(Debug, FromCore)]
pub enum McpReq {
    #[core(module = "Pattern.Mcp", name = "Call")]
    Call(String, String, String),

    #[core(module = "Pattern.Mcp", name = "Introspect")]
    Introspect(String),

    #[core(module = "Pattern.Mcp", name = "ListServers")]
    ListServers,

    #[core(module = "Pattern.Mcp", name = "Unload")]
    Unload(String),
}
