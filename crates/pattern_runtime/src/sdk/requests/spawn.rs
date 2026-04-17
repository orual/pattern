//! Mirror of `Pattern.Spawn` (`haskell/Pattern/Spawn.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Spawn` GADT.
#[derive(Debug, FromCore)]
pub enum SpawnReq {
    #[core(module = "Pattern.Spawn", name = "Start")]
    Start(String),
    #[core(module = "Pattern.Spawn", name = "Stop")]
    Stop(String),
}
