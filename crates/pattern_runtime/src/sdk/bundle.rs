//! Bundle the full 11-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is Prelude-5 first (`Memory, Message, Display, Time,
//! Log`), then the rarer effects (`Shell, File, Sources, Mcp, Rpc, Spawn`).
//!
//! **Why Prelude-5-first (historical note):** originally this ordering was
//! required to avoid DataCon name collisions: tidepool-bridge looked up
//! constructors by unqualified name, which failed when distinct modules
//! declared same-named constructors. The fork at `github:orual/tidepool`
//! first added `get_by_name_arity` (arity disambiguation) and later
//! module-qualified lookup via `#[core(module = "Pattern.<Module>",
//! name = "...")]`. Memory uses `Get`/`Put` (KV semantics) rather than
//! `Read`/`Write`, so the remaining residual collisions (e.g. both
//! `Memory.Get` and no `File.Get`) are handled entirely at the
//! derive-layer disambiguation stage — agent programs can mix
//! unqualified imports across all eleven modules without ambiguity in
//! current Pattern. Prelude-5-first is kept for backwards compatibility
//! and authoring clarity.
//!
//! Individual handler structs remain available for ad-hoc bundles (see
//! `crate::sdk::handlers`).

use crate::sdk::handlers::{
    DisplayHandler, FileHandler, LogHandler, McpHandler, MemoryHandler, MessageHandler, RpcHandler,
    ShellHandler, SourcesHandler, SpawnHandler, TimeHandler,
};

/// The full 11-handler SDK bundle, typed as a `frunk::HList`.
///
/// Order (Prelude-5 first, then rarer effects):
/// `Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc,
/// Spawn`. Agent `Eff '[...]` rows must line up with this order so JIT
/// effect-tag lookups resolve correctly.
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    MessageHandler,
    DisplayHandler,
    TimeHandler,
    LogHandler,
    ShellHandler,
    FileHandler,
    SourcesHandler,
    McpHandler,
    RpcHandler,
    SpawnHandler,
];
