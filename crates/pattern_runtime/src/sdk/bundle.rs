//! Bundle the full 11-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is Prelude-5 first (`Memory, Message, Display, Time,
//! Log`), then the rarer effects (`Shell, File, Sources, Mcp, Rpc, Spawn`).
//!
//! **Why Prelude-5-first (historical note):** originally this ordering was
//! required to avoid DataCon name collisions: tidepool-bridge looked up
//! constructors by unqualified name, which failed when e.g. both
//! `Pattern.Memory.Read` and `Pattern.File.Read` existed in the same
//! DataConTable. The fork at `github:orual/tidepool` (commit 16b6ead)
//! switched `FromCore`/`ToCore` codegen to `get_by_name_arity`, which
//! disambiguates by arity — `Memory.Write` (arity 3) and `File.Write`
//! (arity 2) now resolve correctly. Prelude-5-first is kept for
//! backwards compatibility and because the remaining ambiguous pair
//! (`Memory.Read` / `File.Read`, both arity 1) still requires agents to
//! avoid importing both unqualified simultaneously.
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
