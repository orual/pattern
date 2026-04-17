//! Mirror of `Pattern.File` (`haskell/Pattern/File.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `File` GADT.
#[derive(Debug, FromCore)]
pub enum FileReq {
    #[core(module = "Pattern.File", name = "Read")]
    Read(String),
    #[core(module = "Pattern.File", name = "Write")]
    Write(String, String),
    #[core(module = "Pattern.File", name = "ListDir")]
    ListDir(String),
}
