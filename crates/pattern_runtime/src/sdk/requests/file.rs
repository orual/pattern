//! Mirror of `Pattern.File` (`haskell/Pattern/File.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `File` GADT.
#[derive(Debug, FromCore)]
pub enum FileReq {
    #[core(name = "Read")]
    Read(String),
    #[core(name = "Write")]
    Write(String, String),
    #[core(name = "List")]
    List(String),
}
