//! Bundle the full 11-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is Prelude-5 first (`Memory, Message, Display, Time,
//! Log`), then the rarer effects (`Shell, File, Sources, Mcp, Ipc, Spawn`).
//!
//! **Why Prelude-5-first:** tidepool-bridge's `FromCore` derive looks up
//! data constructors by their unqualified name. When an agent imports
//! multiple Pattern.* modules that export constructors with overlapping
//! names (e.g. both `Pattern.Memory.Read` and `Pattern.File.Read`), the
//! lookup becomes ambiguous and the bridge errors with
//! "Unknown DataCon name: Read". Putting Prelude-5 at the prefix lets
//! agents using only those five effects declare `Eff '[Memory, Message,
//! Display, Time, Log] a` and avoid importing the modules whose
//! constructors collide. See
//! `/home/orual/Projects/PatternProject/tidepool/tidepool-bridge/src/impls.rs`
//! for the `get_by_name` call sites that drive this constraint.
//!
//! Individual handler structs remain available for ad-hoc bundles (see
//! `crate::sdk::handlers`).

use crate::sdk::handlers::{
    DisplayHandler, FileHandler, IpcHandler, LogHandler, McpHandler, MemoryHandler, MessageHandler,
    ShellHandler, SourcesHandler, SpawnHandler, TimeHandler,
};

/// The full 11-handler SDK bundle, typed as a `frunk::HList`.
///
/// Order (Prelude-5 first, then rarer effects):
/// `Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Ipc,
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
    IpcHandler,
    SpawnHandler,
];
