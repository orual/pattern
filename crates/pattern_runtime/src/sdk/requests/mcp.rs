//! Mirror of `Pattern.Mcp` (`haskell/Pattern/Mcp.hs`).

use tidepool_bridge_derive::FromCore;

/// Mirror of the Haskell `Mcp` GADT.
///
/// `Use` is chosen to match AI-agent parlance — "the agent uses the
/// search tool".
#[derive(Debug, FromCore)]
pub enum McpReq {
    #[core(module = "Pattern.Mcp", name = "Use")]
    Use(String, String),
}
