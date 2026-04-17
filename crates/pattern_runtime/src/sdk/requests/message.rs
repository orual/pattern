//! Mirror of `Pattern.Message` (`haskell/Pattern/Message.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Message` GADT.
#[derive(Debug, FromCore)]
pub enum MessageReq {
    #[core(name = "Ask")]
    Ask(String),
    #[core(name = "Send")]
    Send(String, String),
    #[core(name = "Reply")]
    Reply(String, String),
    #[core(name = "Notify")]
    Notify(String, String),
}
