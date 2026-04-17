//! Pattern SDK Rust-side bindings.
//!
//! Mirrors the Haskell-side effect algebra at
//! `crates/pattern_runtime/haskell/Pattern/`. One Rust enum per Haskell
//! GADT; variant names match Haskell constructor names byte-for-byte via
//! the `#[core(name = "...")]` attribute from `tidepool-bridge-derive`.
//!
//! Handler implementations live in `sdk::handlers` (Phase 3: time, log,
//! display fully implemented; shell / file / sources / mcp / ipc / spawn
//! stubbed with NotImplemented diagnostics).

pub mod bundle;
pub mod handlers;
pub mod location;
pub mod requests;

pub use bundle::SdkBundle;
pub use location::SdkLocation;
