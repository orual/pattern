//! Mirror of `Pattern.Shell` (`haskell/Pattern/Shell.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Shell` GADT.
#[derive(Debug, FromCore)]
pub enum ShellReq {
    #[core(name = "Execute")]
    Execute(String),
    #[core(name = "Spawn")]
    Spawn(String),
    #[core(name = "Kill")]
    Kill(i64),
    #[core(name = "Status")]
    Status(i64),
}
