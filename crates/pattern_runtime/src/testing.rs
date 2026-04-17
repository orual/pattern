//! Test fixtures for `pattern_runtime` and dependent crates.
//!
//! Re-exports commonly-needed [`tidepool_testing`] helpers under paths that
//! work under Rust 2024 edition. The upstream `tidepool-testing` crate is on
//! edition 2021 and defines a `gen` submodule, which is a reserved keyword
//! under 2024 — downstream callers would need `tidepool_testing::r#gen::…` at
//! every call site. This module localises that escape to one place.
//!
//! Remove this module (and update call sites to the upstream paths) when
//! tidepool either renames its `gen` module or moves to edition 2024 itself.

/// Standard Haskell-boxing `DataConTable` with `I#`, `W#`, `D#`, `()`,
/// `Maybe`/`Just`/`Nothing`, `Bool`/`True`/`False`, pair `(,)`, and list
/// `[]`/`:` constructors pre-registered. Use in handler tests rather than
/// hand-building a table per test.
pub use tidepool_testing::r#gen::standard_datacon_table;
