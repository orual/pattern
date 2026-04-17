//! Mirror of `Pattern.Sources` (`haskell/Pattern/Sources.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Sources` GADT.
#[derive(Debug, FromCore)]
pub enum SourcesReq {
    #[core(module = "Pattern.Sources", name = "Stream")]
    Stream(String),
    #[core(module = "Pattern.Sources", name = "Subscribe")]
    Subscribe(String, String),
    #[core(module = "Pattern.Sources", name = "List")]
    List,
}
