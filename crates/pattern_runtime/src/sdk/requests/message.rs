//! Mirror of `Pattern.Message` (`haskell/Pattern/Message.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Message` GADT.
#[derive(Debug, FromCore)]
pub enum MessageReq {
    #[core(module = "Pattern.Message", name = "Ask")]
    Ask(String),
    #[core(module = "Pattern.Message", name = "Send")]
    Send(String, String),
    #[core(module = "Pattern.Message", name = "Reply")]
    Reply(String, String),
    #[core(module = "Pattern.Message", name = "Notify")]
    Notify(String, String),
}
