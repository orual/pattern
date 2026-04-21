//! Pattern SDK Rust-side bindings.
//!
//! Mirrors the Haskell-side effect algebra at
//! `crates/pattern_runtime/haskell/Pattern/`. One Rust enum per Haskell
//! GADT; variant names match Haskell constructor names byte-for-byte via
//! the `#[core(name = "...")]` attribute from `tidepool-bridge-derive`.
//!
//! Handler implementations live in `sdk::handlers` (Phase 3: time, log,
//! display fully implemented; shell / file / sources / mcp / rpc / spawn
//! stubbed with NotImplemented diagnostics).

pub mod bundle;
pub mod code_tool;
pub mod describe;
pub mod handlers;
pub mod lib_modules;
pub mod location;
pub mod preamble;
pub mod requests;

pub use bundle::SdkBundle;
pub use code_tool::CODE_TOOL;
pub use describe::{CollectEffectDecls, DescribeEffect, EffectDecl};
pub use location::SdkLocation;
