//! Mirror of `Pattern.Mcp` (`haskell/Pattern/Mcp.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Mcp` GADT.
#[derive(Debug, FromCore)]
pub enum McpReq {
    #[core(name = "Call")]
    Call(String, String),
}
