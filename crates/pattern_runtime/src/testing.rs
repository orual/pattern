//! Test fixtures for `pattern_runtime` and downstream integration tests.
//!
//! Re-exports commonly-needed `tidepool_testing` helpers under paths that
//! work under Rust 2024 edition. The upstream `tidepool-testing` crate is on
//! edition 2021 and defines a `gen` submodule, which is a reserved keyword
//! under 2024 — downstream callers would need `tidepool_testing::r#gen::…` at
//! every call site. This module localises that escape to one place.
//!
//! Remove the `gen` re-export (and update call sites to the upstream paths)
//! when tidepool either renames its `gen` module or moves to edition 2024
//! itself.

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::ProviderError;
use pattern_core::traits::provider_client::ChunkStream;
use pattern_core::types::provider::{CompletionRequest, TokenCount};

/// Standard Haskell-boxing `DataConTable` with `I#`, `W#`, `D#`, `()`,
/// `Maybe`/`Just`/`Nothing`, `Bool`/`True`/`False`, pair `(,)`, and list
/// `[]`/`:` constructors pre-registered. Use in handler tests rather than
/// hand-building a table per test.
///
/// Gated on `cfg(test)` because `tidepool-testing` is a dev-dependency
/// (not available to library builds); consumers who need this in their
/// own `#[cfg(test)]` scope should depend on `tidepool-testing` directly.
#[cfg(test)]
pub use tidepool_testing::r#gen::standard_datacon_table;

pub mod in_memory_store;
pub use in_memory_store::InMemoryMemoryStore;

/// Minimal `ProviderClient` implementation that panics on any method call.
///
/// Used in tests that construct `TidepoolRuntime` but never invoke the
/// provider. If a test actually needs to call provider methods, use a
/// proper mock instead.
#[derive(Debug)]
pub struct NopProviderClient;

#[async_trait]
impl ProviderClient for NopProviderClient {
    async fn complete(&self, _: CompletionRequest) -> Result<ChunkStream, ProviderError> {
        panic!("NopProviderClient::complete called — test must not invoke the provider");
    }

    async fn count_tokens(&self, _: &CompletionRequest) -> Result<TokenCount, ProviderError> {
        panic!("NopProviderClient::count_tokens called — test must not invoke the provider");
    }
}
