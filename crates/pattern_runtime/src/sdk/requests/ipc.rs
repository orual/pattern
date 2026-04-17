//! Mirror of `Pattern.Ipc` (`haskell/Pattern/Ipc.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Ipc` GADT.
#[derive(Debug, FromCore)]
pub enum IpcReq {
    #[core(name = "Send")]
    Send(String, String),
    #[core(name = "Recv")]
    Recv(String),
}
