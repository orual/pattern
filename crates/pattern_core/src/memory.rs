// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Memory system document types.
//!
//! The `StructuredDocument` wrapper lives here because it appears in
//! [`MemoryStore`](crate::traits::MemoryStore) trait signatures. Moving it
//! to `pattern_memory` would create a circular dependency.
//!
//! Trait-signature value types (block metadata, schemas, search options)
//! live in [`crate::types::memory_types`]. The canonical `MemoryStore`
//! implementation (`MemoryCache`) lives in `pattern_memory`.

mod document;

pub use document::*;
