//! Mirror of `Pattern.Memory` (`haskell/Pattern/Memory.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Memory` GADT.
#[derive(Debug, FromCore)]
pub enum MemoryReq {
    #[core(name = "Read")]
    Read(String),
    #[core(name = "Write")]
    Write(String, String),
    #[core(name = "Append")]
    Append(String, String),
    #[core(name = "Search")]
    Search(String),
    #[core(name = "Recall")]
    Recall(String),
    #[core(name = "Archive")]
    Archive(String),
}
