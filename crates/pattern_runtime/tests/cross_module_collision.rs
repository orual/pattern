// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Cross-module DataCon collision tests.
//!
//! The tests that exercised the multi-module dispatch through the legacy
//! static-program `Session::step` path were retired in Phase 6 Task B alongside
//! that path. The underlying disambiguation mechanism (arity + module-qualified
//! lookup) is already covered by `bundle_non_prelude5.rs` (single-handler
//! `compile_and_run`) and the inline unit tests on the derive layer.
