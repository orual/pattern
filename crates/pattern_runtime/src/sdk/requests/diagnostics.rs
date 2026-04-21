//! Mirror of `Pattern.Diagnostics` (`haskell/Pattern/Diagnostics.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Diagnostics` GADT.
#[derive(Debug, FromCore)]
pub enum DiagnosticsReq {
    #[core(module = "Pattern.Diagnostics", name = "GetDiagnostics")]
    GetDiagnostics,
}
