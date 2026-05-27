// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! SDK handler failure routing tests.
//!
//! The legacy static-program path (`SessionMachine.run` + `Session::step`) that
//! this file tested was retired in Phase 6 Task B. The `SdkHandlerFailed` error
//! variant still exists and is covered by the error-map unit tests in
//! `src/tidepool/error_map.rs`. Integration coverage through the agent-loop path
//! will be added in a future phase when the handler dispatch surface stabilises.
