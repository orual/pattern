// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Log` (`haskell/Pattern/Log.hs`).

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Log` GADT.
#[derive(Debug, FromCore)]
pub enum LogReq {
    #[core(module = "Pattern.Log", name = "Debug")]
    Debug(String),
    #[core(module = "Pattern.Log", name = "Info")]
    Info(String),
    #[core(module = "Pattern.Log", name = "Warn")]
    Warn(String),
    #[core(module = "Pattern.Log", name = "Error")]
    Error(String),
}
