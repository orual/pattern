//! Bundle the Phase-3-visible SDK handlers into a single `DispatchEffect`.
//!
//! **Scope note for Phase 3 Subcomp D:** the session's bundle lists
//! handlers in the same order as `Pattern.Prelude` re-exports its
//! modules — `Memory, Message, Display, Time, Log`. Handler position in
//! the HList is the effect tag in the JIT, so agent programs MUST declare
//! `Eff '[Memory, Message, Display, Time, Log] a` (or a prefix thereof)
//! to line up with this bundle.
//!
//! The full 11-handler bundle (adding Shell / File / Sources / Mcp /
//! Ipc / Spawn) cannot currently be flattened by the SDK inliner due to
//! constructor-name collisions across modules (e.g. both `Memory.Read`
//! and `File.Read`). Until the upstream inliner grows multi-module
//! qualified-rename support, agent programs in Phase 3 are limited to the
//! Prelude subset. Rarer-effect handlers remain available as independent
//! structs — downstream code can build custom ad-hoc bundles if all
//! imports are limited to a collision-free subset.

use crate::sdk::handlers::{
    DisplayHandler, LogHandler, MemoryHandler, MessageHandler, TimeHandler,
};

/// The 5-handler Prelude SDK bundle, typed as a `frunk::HList`.
///
/// Order mirrors `Pattern.Prelude`'s module re-export order:
/// `Memory, Message, Display, Time, Log`. Agent programs that use a
/// subset must still match this ordering in their `Eff '[...]` list so
/// JIT effect-tag lookups resolve correctly.
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    MessageHandler,
    DisplayHandler,
    TimeHandler,
    LogHandler,
];
