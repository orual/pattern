//! Mirror of `Pattern.Log` (`haskell/Pattern/Log.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Log` GADT.
#[derive(Debug, FromCore)]
pub enum LogReq {
    #[core(module = "Pattern.Log", name = "Debug")]
    Debug(String),
    #[core(module = "Pattern.Log", name = "Info")]
    Info(String),
    #[core(module = "Pattern.Log", name = "Warn")]
    Warn(String),
    #[core(module = "Pattern.Log", name = "Error")]
    Error(String),
}
