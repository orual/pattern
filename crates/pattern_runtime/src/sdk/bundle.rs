//! Bundle all 11 SDK handlers into a single `DispatchEffect` via `frunk::HList`.
//! Handler position in the list maps to the effect tag in Haskell code.
//!
//! ORDERING IS SEMANTIC. If the Haskell `agent` program declares
//! `Eff '[Memory, Message, Display, Shell, File, Sources, Mcp, Time, Ipc, Log, Spawn]`,
//! this bundle must list handlers in that same order. The parity test in
//! `sdk::requests::parity` catches mismatches between the Haskell declaration
//! and Rust bundle.

use crate::sdk::handlers::{
    DisplayHandler, FileHandler, IpcHandler, LogHandler, McpHandler, MemoryHandler, MessageHandler,
    ShellHandler, SourcesHandler, SpawnHandler, TimeHandler,
};

/// The full 11-handler SDK bundle, typed as a `frunk::HList`.
///
/// Handler order must match the `Eff '[...]` declaration in
/// `Pattern.Prelude` + the rarer-effects convention. Programs that use
/// fewer effects use a reduced bundle (constructed ad hoc in tests or
/// session setup) so that tag indices line up correctly.
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    MessageHandler,
    DisplayHandler,
    ShellHandler,
    FileHandler,
    SourcesHandler,
    McpHandler,
    TimeHandler,
    IpcHandler,
    LogHandler,
    SpawnHandler,
];

/// Construct a default SDK bundle with all 11 handlers at their default state.
///
/// Sessions that need custom handler state (e.g., `LogHandler::for_session`)
/// build bundles directly via `frunk::hlist![...]`.
pub fn default_bundle() -> SdkBundle {
    frunk::hlist![
        MemoryHandler,
        MessageHandler,
        DisplayHandler::default(),
        ShellHandler,
        FileHandler,
        SourcesHandler,
        McpHandler,
        TimeHandler,
        IpcHandler,
        LogHandler::default(),
        SpawnHandler,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the bundle type-checks and `default_bundle()` produces an instance.
    /// This is the minimal smoke test: if the HList length or handler types
    /// drift, this will fail to compile.
    #[test]
    fn default_bundle_type_checks_with_11_handlers() {
        let _bundle: SdkBundle = default_bundle();
        // The HList has 11 elements. We verify via type — if the count changes,
        // the type alias `SdkBundle` won't match `default_bundle()`.
    }
}
