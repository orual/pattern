//! Mirror of `Pattern.Port` (`haskell/Pattern/Port.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Port` GADT.
///
/// - `List`: returns JSON list of `PortMetadata` visible to this agent.
/// - `Call(port_id, method, payload_json)`: one-shot call to a port method.
/// - `Subscribe(port_id, config_json)`: subscribe to a port's event stream.
///   Events arrive as `MessageAttachment::PortEvent` on subsequent turns.
/// - `Unsubscribe(port_id)`: cancel an active subscription.
#[derive(Debug, FromCore)]
pub enum PortReq {
    #[core(module = "Pattern.Port", name = "List")]
    List,
    #[core(module = "Pattern.Port", name = "Call")]
    Call(String, String, String), // (port_id, method, payload_json)
    #[core(module = "Pattern.Port", name = "Subscribe")]
    Subscribe(String, String), // (port_id, config_json)
    #[core(module = "Pattern.Port", name = "Unsubscribe")]
    Unsubscribe(String), // port_id
}
