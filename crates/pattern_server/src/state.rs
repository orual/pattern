// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Daemon state file management — moved to `pattern_core::daemon_state`.
//!
//! Re-export shim so existing `pattern_server::state::DaemonState` imports keep working.
//! New code should use `pattern_core::daemon_state::DaemonState` directly.

pub use pattern_core::daemon_state::*;
