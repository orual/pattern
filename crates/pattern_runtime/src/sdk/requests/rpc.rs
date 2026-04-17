//! Mirror of `Pattern.Rpc` (`haskell/Pattern/Rpc.hs`).

use tidepool_bridge_derive::FromCore;

/// Mirror of the Haskell `Rpc` GADT.
///
/// `Call` is the sync request/response verb; `Recv` is a passive receive
/// for mailbox-shaped endpoints. Note: this module is NOT for
/// agent-to-agent messaging — that's `Pattern.Message.Send`, which pulls
/// caller identity from session context automatically.
#[derive(Debug, FromCore)]
pub enum RpcReq {
    #[core(module = "Pattern.Rpc", name = "Call")]
    Call(String, String),
    #[core(module = "Pattern.Rpc", name = "Recv")]
    Recv(String),
}
