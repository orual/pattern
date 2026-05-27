// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Diagnostics` (`haskell/Pattern/Diagnostics.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Diagnostics` GADT.
#[derive(Debug, FromCore)]
pub enum DiagnosticsReq {
    #[core(module = "Pattern.Diagnostics", name = "GetDiagnostics")]
    GetDiagnostics,
}
