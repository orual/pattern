//! Mirror of `Pattern.Shell` (`haskell/Pattern/Shell.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Shell` GADT.
#[derive(Debug, FromCore)]
pub enum ShellReq {
    #[core(module = "Pattern.Shell", name = "Execute")]
    Execute(String),
    #[core(module = "Pattern.Shell", name = "Spawn")]
    Spawn(String),
    #[core(module = "Pattern.Shell", name = "Kill")]
    Kill(i64),
    #[core(module = "Pattern.Shell", name = "Status")]
    Status(i64),
}
