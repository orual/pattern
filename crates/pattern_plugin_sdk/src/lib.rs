//! Pattern plugin SDK.
//!
//! Authors implement [`PluginExtension`] and (for out-of-process plugins)
//! call `register_plugin` from their plugin's `main()`. The SDK handles
//! transport wiring (direct trait dispatch in-process; IRPC over iroh QUIC
//! out-of-process), serialization, and host callback dispatch via
//! [`PluginContext`] accessors.
//!
//! # Phase 6 status
//!
//! - Task 1: pattern_core feature-gate audit + `Port::library` SmolStr swap (landed)
//! - **Task 2 (this crate): slim re-export surface (landed)**
//! - Task 3-5: wire protocols, in-process + out-of-process transports (pending —
//!   `register_plugin` + `PluginMemorySync` land then)
//! - Task 6: McpPluginAdapter (pending)
//! - Task 7-8: smoke test + integration suite (pending)
//!
//! # Minimum example (Task 5+ will make `register_plugin` available)
//!
//! ```rust,ignore
//! use pattern_plugin_sdk::{PluginExtension, PortDeclaration};
//!
//! #[derive(Debug, Default)]
//! struct MyPlugin;
//!
//! impl PluginExtension for MyPlugin {
//!     fn ports(&self) -> Vec<PortDeclaration> { Vec::new() }
//! }
//! ```

// ── Slim core re-exports (no loro, no genai pulled into plugin author tree) ──

// Plugin trait surface.
pub use pattern_core::traits::plugin::{
    PluginContext, PluginError, PluginExtension, PluginHost, PortDeclaration,
};

// Hook lifecycle + tag catalog.
pub use pattern_core::hooks::{
    HookEvent, HookEventMetadata, HookFilter, HookResponse, HookSemantics,
    cc_aliases, payloads, tags,
};

// Capability surface.
pub use pattern_core::capability::{CapabilityFlag, CapabilitySet, EffectCategory};

// Port trait + types.
pub use pattern_core::traits::port::Port;
pub use pattern_core::types::port::{PortCapabilities, PortError, PortEvent, PortId, PortMetadata};

// Author identity (for credit/attribution at the message-origin layer).
pub use pattern_core::types::origin::Author;

// ── Opt-in: memory-sync surface ──────────────────────────────────────────────
//
// `MemoryStore` trait API references `pattern_core::memory::StructuredDocument`
// (loro-shaped), so this re-export is gated behind the `memory-sync` feature.
// Enabling it pulls loro into the plugin author's dep tree via
// pattern_core's `memory` feature.

#[cfg(feature = "memory-sync")]
pub use pattern_core::traits::MemoryStore;

// `StructuredDocument` appears in MemoryStore trait signatures. Plugins
// interact with memory contents through this wrapper (not raw LoroDoc) —
// it carries attribution, structured access, and the conflict-tracking
// invariants. LoroDoc itself stays an implementation detail of pattern_core
// and of PluginMemorySync's internals.
#[cfg(feature = "memory-sync")]
pub use pattern_core::memory::StructuredDocument;

// Task 5 will add: `mod memory_sync; pub use memory_sync::PluginMemorySync;`.
mod registration;
pub use registration::{register_plugin, PluginHandle, RegisterError};
