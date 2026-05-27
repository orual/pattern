// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Runtime-provided port implementations.
//!
//! Ports expose external services (HTTP, Slack, databases, etc.) to agents
//! through the `Port` trait. This module owns the implementations that
//! `pattern_runtime` ships — the first being [`HttpPort`]. Plugin-provided
//! ports register via the same `PortRegistry` surface but live outside this
//! crate.

pub mod http;

pub use http::HttpPort;
