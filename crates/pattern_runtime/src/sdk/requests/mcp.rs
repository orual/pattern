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
