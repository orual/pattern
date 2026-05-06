//! Mirror of `Pattern.Time` (`haskell/Pattern/Time.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Time` GADT.
#[derive(Debug, FromCore)]
pub enum TimeReq {
    /// Haskell: `Now :: Time Text`. Returns RFC 3339 formatted timestamp.
    #[core(module = "Pattern.Time", name = "Now")]
    Now,
    /// Haskell: `NowNanos :: Time Int`. Returns epoch nanoseconds for arithmetic.
    #[core(module = "Pattern.Time", name = "NowNanos")]
    NowNanos,
    /// Haskell: `Sleep :: Int -> Time ()`.
    #[core(module = "Pattern.Time", name = "Sleep")]
    Sleep(i64),
}
