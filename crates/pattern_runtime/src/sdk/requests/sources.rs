//! Mirror of `Pattern.Sources` (`haskell/Pattern/Sources.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Sources` GADT.
#[derive(Debug, FromCore)]
pub enum SourcesReq {
    #[core(name = "Stream")]
    Stream(String),
    #[core(name = "Subscribe")]
    Subscribe(String, String),
    #[core(name = "List")]
    List,
}
