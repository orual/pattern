//! Mirror of `Pattern.Time` (`haskell/Pattern/Time.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Time` GADT.
#[derive(Debug, FromCore)]
pub enum TimeReq {
    /// Haskell: `Now :: Time Integer`.
    #[core(name = "Now")]
    Now,
    /// Haskell: `Sleep :: Integer -> Time ()`.
    #[core(name = "Sleep")]
    Sleep(i64),
}
