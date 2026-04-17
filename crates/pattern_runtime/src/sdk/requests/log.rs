//! Mirror of `Pattern.Log` (`haskell/Pattern/Log.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Log` GADT.
#[derive(Debug, FromCore)]
pub enum LogReq {
    #[core(name = "Debug")]
    Debug(String),
    #[core(name = "Info")]
    Info(String),
    #[core(name = "Warn")]
    Warn(String),
    #[core(name = "Error")]
    Error(String),
}
