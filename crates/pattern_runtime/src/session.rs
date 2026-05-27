// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Concrete [`pattern_core::traits::Session`] impl backed by Tidepool.
//!
//! Lifecycle:
//! 1. [`TidepoolSession::open_with_agent_loop`] — preflight, construct
//!    handler bundle, spawn `EvalWorker`, build preamble.
//! 2. Repeat: [`TidepoolSession::step_with_agent_loop`] — drive the full
//!    wire-turn loop: compose → provider → stream → tool dispatch → chain.
//! 3. [`TidepoolSession::checkpoint`] / [`TidepoolSession::restore`] —
//!    event-log based.
//!
//! The legacy static-program path (`TidepoolSession::open` + `Session::step`
//! driving `SessionMachine.run`) was retired in Phase 6 Task B. The
//! `TidepoolRuntime::open_session` trait impl now delegates to
//! `open_with_agent_loop` (or can open a minimal session without an eval
//! worker for checkpoint-only use). See `runtime.rs` for the trait bridge.
//!

use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pattern_core::ProviderClient;
use pattern_core::error::RuntimeError;
use pattern_core::plugin::manifest::ComponentSpec;
use pattern_core::traits::{MemoryStore, NoOpSink, Session, TurnSink};
use pattern_core::types::snapshot::{PersonaSnapshot, SessionSnapshot};
use pattern_core::types::turn::{StepReply, TurnInput};

use crate::agent_loop::EvalWorker;
use crate::spawn::{ForkRegistry, InMemoryForkRegistry, SpawnRegistry};

/// Mount-level metadata describing where the session's memory lives on disk.
///
/// Populated by daemon callers that want to enable persistent forks; left
/// `None` for sessions constructed via [`SessionContext::from_persona`]
/// directly (test paths, in-memory-only scenarios). The persistent-fork
/// path in `handle_fork` consults this to locate the jj repo root and
/// workspace directory; without it, persistent forks fail with
/// `ForkError::PersistentNotAvailable` and lightweight forks proceed
/// against the in-memory cache only.
#[derive(Debug, Clone)]
pub struct MountInfo {
    /// Repository root used for jj `workspace_add` / `bookmark_set`.
    pub repo_root: std::path::PathBuf,
    /// Root under which fork workspaces are created (typically
    /// `<repo_root>/.pattern/workspaces` or similar).
    pub workspace_root: std::path::PathBuf,
    /// Storage mode of the mount (controls whether jj is required).
    pub mode: pattern_memory::modes::StorageMode,
    /// Whether jj is available + enabled for this mount. Even on
    /// `InRepo` mode this may be `true` if the project opted into a
    /// jj checkout.
    pub jj_enabled: bool,
}

/// Compose the session's effective [`pattern_core::PolicySet`] from
/// runtime defaults plus the persona's KDL-loaded rules.
///
/// Order is irrelevant for evaluation correctness — `PolicySet::evaluate`
/// sorts by `Precedence` at lookup time — but constructing the vec
/// once at session open keeps allocation off the hot path.
///
/// Phase 1 Task 14 wires persona-level rules; project-level
/// `.pattern.kdl` policy and runtime overrides will layer in via
/// follow-up phases without changing this composition site (just
/// extend the iterator chain).
/// Extract the module name from a Haskell source file. Looks past
/// `--` line comments, `{-# … #-}` pragmas (single- and multi-line),
/// and `{- … -}` block comments to find the `module Foo.Bar where`
/// header.
///
/// Returns `None` only when no `module` keyword is found in the
/// cleaned source. Used by `open_with_agent_loop`'s port-library
/// materialization path to derive each port library's on-disk path
/// from its module declaration.
fn parse_module_name(src: &str) -> Option<String> {
    let cleaned = strip_haskell_noise(src);
    let mut tokens = cleaned.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "module" {
            let raw = tokens.next()?;
            let name: String = raw.chars().take_while(|c| *c != '(').collect();
            let name = name.trim_end_matches(',').trim();
            if name.is_empty() {
                return None;
            }
            return Some(name.to_string());
        }
    }
    None
}

/// Strip `--` line comments, `{-# … #-}` pragma blocks, and `{- … -}`
/// block comments (with proper nesting per Haskell spec) from `src`.
/// Newlines are preserved so downstream parsers' line numbers stay
/// aligned with the original source.
///
/// Token markers (`--`, `{-`, `{-#`, `-}`, `#-}`) are pure ASCII so
/// we look for them via byte-level comparisons; non-marker content is
/// preserved by copying matching `&str` slices verbatim (never by
/// casting individual bytes to `char`, which would Latin-1
/// re-interpret UTF-8 continuation bytes).
fn strip_haskell_noise(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    let mut run = 0;

    while i < bytes.len() {
        // `{-# … #-}` pragma — scan to matching `#-}`.
        if i + 2 < bytes.len() && &bytes[i..i + 3] == b"{-#" {
            out.push_str(&src[run..i]);
            let mut j = i + 3;
            while j + 2 < bytes.len() && &bytes[j..j + 3] != b"#-}" {
                if bytes[j] == b'\n' {
                    out.push('\n');
                }
                j += 1;
            }
            i = (j + 3).min(bytes.len());
            run = i;
            continue;
        }
        // `{- … -}` block comment with Haskell nesting.
        if i + 1 < bytes.len() && &bytes[i..i + 2] == b"{-" {
            out.push_str(&src[run..i]);
            let mut depth = 1;
            let mut j = i + 2;
            while j + 1 < bytes.len() && depth > 0 {
                if &bytes[j..j + 2] == b"{-" {
                    depth += 1;
                    j += 2;
                } else if &bytes[j..j + 2] == b"-}" {
                    depth -= 1;
                    j += 2;
                } else {
                    if bytes[j] == b'\n' {
                        out.push('\n');
                    }
                    j += 1;
                }
            }
            i = j;
            run = i;
            continue;
        }
        // `--` line comment per Haskell 2010 §2.3: opens a comment
        // iff NOT preceded by another symbol char (so `<--`, `--->`,
        // `|--|` etc. remain operators, not comments).
        if i + 1 < bytes.len() && &bytes[i..i + 2] == b"--" {
            let prev_is_symbol = i > 0 && is_haskell_symbol_byte(bytes[i - 1]);
            if !prev_is_symbol {
                out.push_str(&src[run..i]);
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                run = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&src[run..]);
    out
}

/// Convert a Haskell module name (`Pattern.Http`) to its on-disk relative
/// path (`Pattern/Http.hs`). Used by `open_with_agent_loop`'s
/// port-library materialization path.
fn module_name_to_path(module_name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::new();
    let mut parts = module_name.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            p.push(format!("{part}.hs"));
        } else {
            p.push(part);
        }
    }
    p
}

/// True iff `b` is one of the ASCII characters that participate in
/// Haskell symbolic operators (Haskell 2010 §2.4 `symbol`). Used by
/// `strip_haskell_noise` to disambiguate `--` line comments from
/// operators like `<--` and `--->`.
fn is_haskell_symbol_byte(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'*'
            | b'+'
            | b'.'
            | b'/'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'@'
            | b'\\'
            | b'^'
            | b'|'
            | b'-'
            | b'~'
            | b':'
    )
}

fn merge_policies(persona: &PersonaSnapshot) -> pattern_core::PolicySet {
    let defaults = crate::policy::rust_defaults();
    let kdl = persona.policy_rules.iter().cloned();
    pattern_core::PolicySet::from_rules(defaults.into_iter().chain(kdl))
}

/// Compute the default draft persona directory.
///
/// Routes through [`pattern_core::PatternRoots::default_paths`] so
/// `$PATTERN_HOME` overrides apply uniformly across the system.
/// Falls back to `.pattern/drafts` relative to the current working
/// directory if root resolution fails (restricted CI containers
/// without home / config / data dirs).
fn default_drafts_dir() -> std::path::PathBuf {
    pattern_core::PatternRoots::default_paths()
        .map(|r| r.data_root().join("drafts"))
        .unwrap_or_else(|_| std::path::PathBuf::from(".").join("pattern").join("drafts"))
}
use crate::checkpoint::{CheckpointEvent, CheckpointLog};
use crate::memory::{MemoryStoreAdapter, TurnHistory};
use crate::router::{RouterBridge, RouterRegistry};
use crate::sdk::SdkLocation;
use crate::sdk::handlers::DisplayHandler;
use crate::timeout::{Budget, CancelState};

/// Session-scoped context threaded into every handler as the
/// [`tidepool_effect::EffectContext::user`] value.
#[derive(Debug)]
pub struct SessionContext {
    agent_id: String,
    /// Default [`Scope`] for memory operations that don't carry an
    /// explicit scope on the wire. Set by [`Self::with_scope_binding`]
    /// to `Scope::Local(project_id)` when a project mount is present;
    /// otherwise `Scope::Global(agent_id)` for unmounted sessions.
    ///
    /// Phase 1 redesign: SDK handlers route reads/writes through this
    /// scope rather than synthesising a string from `agent_id`. Phase 2
    /// will add an explicit `Maybe Scope` parameter to the wire shape;
    /// callers that pass `Nothing` will fall back to this default.
    default_scope: pattern_core::types::memory_types::Scope,
    /// Model identifier for provider completion requests (e.g.
    /// `"claude-opus-4-7"`). Threaded from `persona.model.choice.model_id`
    /// at session open. Use [`ModelSpec::default`] to get the workspace
    /// default (`"claude-sonnet-4-6"`).
    ///
    /// [`ModelSpec::default`]: pattern_core::types::snapshot::ModelSpec
    model_id: String,
    /// Optional slot-\[1\] override for the composer's system prompt.
    /// Threaded from `persona.system_prompt` at session open. When
    /// `Some`, the agent-loop composer substitutes this in place of
    /// [`pattern_core::DEFAULT_BASE_INSTRUCTIONS`]; `None` keeps the
    /// workspace default.
    system_prompt: Option<String>,
    /// Chat-options baseline threaded from `persona.model.chat_options`
    /// at session open. The agent loop clones this into the composer's
    /// `PartialRequest.options` and layers on streaming-capture flags
    /// (capture_usage, capture_content, capture_tool_calls,
    /// capture_reasoning_content). Persona-declared sampling, reasoning
    /// effort, verbosity, seed, stop_sequences, cache_control, etc. all
    /// reach the wire via this path.
    chat_options: genai::chat::ChatOptions,
    budget: Budget,
    cancel_state: Arc<CancelState>,
    /// Memory store adapter: delegates to the underlying `MemoryStore` and
    /// records `BlockWrite` entries for the current turn. Handlers access
    /// via [`SessionContext::adapter`] to call `record_write` after
    /// mutations; trait-object callers use [`SessionContext::memory_store`]
    /// which returns the adapter (it implements `MemoryStore`).
    adapter: Arc<MemoryStoreAdapter>,
    /// Provider-client handle. Phase 5 wires it in; Phase 5 Task 20
    /// consumes it from the agent loop. Held here so the construction
    /// signature is stable across phase boundaries.
    provider: Arc<dyn ProviderClient>,
    /// Constellation database handle. Required for message persistence
    /// and compaction (Pass B steps). Every session must have DB access;
    /// in-memory-only sessions are no longer supported.
    db: Arc<pattern_db::ConstellationDb>,
    /// Scheme-dispatched message router registry. Handlers dispatch
    /// Send/Reply/Notify through this. Set at session open; read-only
    /// thereafter.
    router: Arc<RouterRegistry>,
    /// Sync-to-async bridge for routing messages from the eval worker
    /// thread to the async router task. Lazily initialised by
    /// [`SessionContext::with_router`]; handlers call
    /// [`RouterBridge::route_sync`] instead of `Handle::current().block_on`.
    router_bridge: Option<RouterBridge>,
    /// Pending messages accumulated during a turn. Handlers push
    /// messages here; the agent loop drains them into `TurnOutput`
    /// at turn close.
    pending_messages: Arc<std::sync::Mutex<Vec<pattern_core::types::message::Message>>>,
    /// Streaming event sink for the agent loop + Display handler.
    /// CLI/TUI bindings swap in a real sink; tests use
    /// `pattern_core::traits::VecSink`; headless runs use
    /// [`NoOpSink`] (the default).
    turn_sink: Arc<dyn TurnSink>,
    /// Optional [`SpawnSinkFactory`] for minting child turn-sinks tagged
    /// with the appropriate [`SpawnSource`] variant. Set by the daemon
    /// (which has access to a stable wire `EventTx`); `None` for headless
    /// and test sessions, in which case child sessions inherit the
    /// parent's `turn_sink` verbatim. Wrapped in `RwLock` so the daemon
    /// can install the factory after `open_with_agent_loop` returns
    /// without breaking that constructor's signature.
    spawn_sink_factory:
        Arc<std::sync::RwLock<Option<Arc<dyn pattern_core::traits::SpawnSinkFactory>>>>,
    /// Shared checkpoint log. Handlers record `(request, response)` pairs
    /// after a successful effect dispatch so restart-then-replay can
    /// deterministically re-drive the JIT. Wired to the same `Arc` as
    /// [`TidepoolSession::checkpoint_log`].
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Current turn number. Incremented by [`TidepoolSession::run_turn`]
    /// before each turn; read by handlers when stamping recorded
    /// exchanges.
    current_turn: Arc<AtomicU64>,
    /// Full snapshot policy: block selection filter + mid-batch delta
    /// behavior. Default includes Core and Working blocks (Archival and
    /// Log excluded) with `IncludeSelfEdits` mid-batch behavior.
    /// Future: per-agent/constellation config overrides.
    snapshot_policy: pattern_core::types::message::SnapshotPolicy,
    /// Per-persona context policy: compression strategy, gate floors,
    /// and snapshot policy. Threaded from `persona.context` at session
    /// open. Consumed by the compaction driver (`crate::compaction`)
    /// before each wire turn.
    context_policy: pattern_core::types::snapshot::ContextPolicy,
    /// Session-scoped diagnostic events. Populated during session
    /// construction (e.g. lib-module compile failures) and read by the
    /// `Pattern.Diagnostics` effect handler. Read-only after construction.
    diagnostics: Arc<std::sync::Mutex<Vec<crate::sdk::handlers::diagnostics::DiagnosticEvent>>>,
    /// Capability set scoping which effects this session may invoke.
    /// `None` means "full power" — back-compat for sessions that pre-date
    /// capability scoping. Phase 2 spawn paths read this to restrict
    /// child sessions to a subset of the parent's capabilities.
    capabilities: Option<pattern_core::CapabilitySet>,
    /// Composed policy set: Rust defaults seeded at session open, with
    /// KDL config + runtime overrides layered on by Tasks 13/14. Read
    /// by handlers (Task 10 Shell, Task 15 File) before each effect
    /// dispatch.
    policies: Arc<pattern_core::PolicySet>,
    /// Per-runtime [`PermissionBroker`].
    permission_broker: Arc<pattern_core::permission::PermissionBroker>,
    /// Sync-to-async bridge for handlers running on the eval-worker
    /// thread. `None` until [`Self::with_permission_bridge`] is called
    /// from an async context (typically `open_with_agent_loop`).
    permission_bridge: Option<Arc<crate::permission::PermissionBridge>>,
    /// Origin of the *immediate dispatcher* of an effect — i.e. who is
    /// asking right now, not what activated this turn. Written by
    /// `agent_loop::drive_step` per orchestrate iteration with
    /// `Author::Agent(self)` (the model is the immediate caller of every
    /// effect during normal model-driven flow); cleared on Drop
    /// (panic-safe via RAII guard).
    ///
    /// The activating turn's origin (which may be `Author::Partner(_)`)
    /// stays on the `TurnInput` for batch-type inference, persistence
    /// attribution, and routing. It is NOT what the broker's
    /// partner-bypass predicate reads — that distinction prevents the
    /// agent's autonomous activity from inheriting Partner authority on
    /// a Partner-activated turn.
    ///
    /// Future direct-execution paths (admin REPL, audited sandboxed
    /// code) may override this slot with a Partner origin before
    /// invoking a handler directly — that is the only path where the
    /// broker's partner-bypass actually fires. Phase 1 has none.
    current_dispatch_origin:
        Arc<std::sync::RwLock<Option<pattern_core::types::origin::MessageOrigin>>>,
    /// Between-turn buffer for `MessageAttachment`s pushed by background
    /// listener threads (FileManager external-edit watcher, ProcessManager
    /// spawn-output bridge, PortRegistry subscription drain task). The
    /// agent loop drains this at compose-time and splices the attachments
    /// onto the next turn's first user message. Distinct from the
    /// adapter's `record_attachment` buffer (which handles in-turn
    /// handler-originated attachments at turn close).
    async_reminder_queue:
        Arc<std::sync::Mutex<Vec<pattern_core::types::message::MessageAttachment>>>,
    /// Per-eval-scope buffer for multi-modal `ContentPart` attachments pushed by
    /// effect handlers during a single tool-call eval (e.g. File.read of an image
    /// produces a marker Text via `cx.respond` AND pushes a `ContentPart::Binary`
    /// onto this vec). The eval_worker drains this vec after each `compile_and_run`
    /// success and chains the parts into the `ToolOutcome::Success` content vec.
    /// Cleared at eval start, drained at eval end — single-threaded per session.
    pending_tool_attachments: Arc<std::sync::Mutex<Vec<genai::chat::ContentPart>>>,
    /// Per-session file manager. `None` until session open constructs it
    /// from the mount config's `file_policy`. Wired via
    /// [`Self::with_file_manager`] inside [`TidepoolSession::open_with_agent_loop`]
    /// when `SessionRegistries.file_policy` is `Some`.
    file_manager: Option<Arc<crate::file_manager::FileManager>>,
    /// Per-session process manager wrapping the local PTY backend.
    /// Always present (never `None`); `from_persona` constructs a fresh
    /// `ProcessManager` rooted at `current_dir`. Test fixtures override
    /// via [`Self::with_process_manager`].
    process_manager: Arc<crate::process_manager::ProcessManager>,
    /// Optional embedding-queue sender. Set by the session opener via
    /// `with_reembed_tx` when the cache has an embedding pipeline
    /// configured. `persist_messages` in agent_loop pushes per-message
    /// ReembedRequests with `ContentType::Message` so message rows land
    /// in the vector index alongside FTS — gives hybrid retrieval over
    /// conversation history. None for sessions without embeddings.
    reembed_tx: Option<tokio::sync::mpsc::UnboundedSender<pattern_memory::subscriber::event::ReembedRequest>>,
    /// Per-session port registry. `None` for sessions opened without a
    /// `SessionRegistries.port_registry` — the SDK preamble filters
    /// `Pattern.Port` out of the agent's effect row in that case so
    /// missing-registry errors surface at compile time, not at dispatch.
    port_registry: Option<Arc<crate::port_registry::PortRegistryImpl>>,
    /// Per-session hook event bus.
    hook_bus: Arc<pattern_core::hooks::HookBus>,
    /// Plugin registry (shared across sessions within a mount).
    plugin_registry: Option<Arc<crate::plugin::registry::PluginRegistry>>,
    /// Daemon-shared plugin route table (pubkey → session_id). Populated at
    /// session-open from the registry; entries removed at session drop.
    plugin_routes: Option<Arc<pattern_core::plugin::auth::PluginRouteTable>>,
    /// Daemon-shared session-routing protocol handler. Sessions register their
    /// per-session host handler at open (carrying their HostApiContext bundle)
    /// and unregister on drop. None disables OOP plugin host-callback dispatch
    /// for this session.
    plugin_routing_handler: Option<Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>>,
    /// Daemon-shared routing handler for the memory-sync ALPN. Sessions
    /// register their per-session memory_sync_handler here at open + unregister
    /// at drop. None disables MemorySync for this session.
    plugin_memory_sync_handler: Option<Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>>,
    /// Daemon iroh endpoint used to dial spawned plugin processes for the
    /// guest-side `pattern-plugin-guest/1` ALPN. Required to construct
    /// `OutOfProcessPluginConnection` for native plugins at session-open.
    /// `None` leaves native plugin spawn disabled.
    daemon_endpoint: Option<iroh::Endpoint>,
    /// Bridge for sync handler → async hook dispatch.
    hook_bridge: crate::hooks::HookBridge,
    /// Session-scoped UUID minted at open. Used by handlers that key
    /// per-session state by stable id (e.g. `PortHandler` keys
    /// subscription channels by session_id so multiple sessions don't
    /// cross-talk). Mirrors the id held on `TidepoolSession` —
    /// `TidepoolSession::open` aligns the two via `with_session_id`.
    session_id: String,
    /// Default timeout for `Shell.Execute` when the agent doesn't
    /// supply one. Initialised to 30 seconds in `from_persona`;
    /// overridable via `with_shell_default_timeout` for test fixtures
    /// and future per-persona configuration.
    shell_default_timeout: std::time::Duration,

    /// Registry tracking live child session handles spawned by this session.
    ///
    /// Enforces a per-parent concurrency limit on ephemeral children via a
    /// `tokio::sync::Semaphore`. When the parent session ends (this registry
    /// is dropped), all registered children have their cancel state flipped.
    ///
    /// The default limit of 8 is a conservative starting point for ensembles.
    /// Revisit when ensemble patterns in Phase 7 stress this ceiling.
    spawn_registry: Arc<SpawnRegistry>,
    /// GHC include paths threaded into the session's eval worker. Persisted
    /// here so child sessions (ephemerals, forks) can extend the parent's
    /// include set with their own synthesized lib directories without
    /// re-deriving the path list. Populated at session-open time by
    /// [`TidepoolSession::open_with_agent_loop`]; left as an empty `Arc<Vec>`
    /// for sessions constructed via `from_persona` directly (test paths
    /// that don't run an eval worker).
    include_paths: Arc<Vec<std::path::PathBuf>>,
    /// Caller-supplied tokio runtime handle. Borrowed for sync handler
    /// paths (e.g. the eval-worker thread) that need to `block_on` an
    /// async future without magic-capturing via `Handle::current()`.
    ///
    /// Note on existing bridges: `PermissionBridge` could later migrate
    /// to this approach (broker calls are well-bounded; no plugin code
    /// in the await path). `RouterBridge` deliberately stays as a bridge
    /// — router endpoints may dispatch to plugin-provided code where the
    /// await path crosses arbitrary user code, and the sync→async glue
    /// keeps the eval worker isolated from that risk.
    tokio_handle: tokio::runtime::Handle,
    /// Resolver for the sibling spawn path: maps `PersonaId` → KDL file path.
    ///
    /// Defaults to [`crate::spawn::sibling::UnconfiguredSiblingResolver`]
    /// which fails every lookup with `PersonaNotFound`. Phase 6 replaces this
    /// with a `pattern_db`-backed resolver that queries the persona registry.
    ///
    /// The `Arc<dyn ...>` indirection allows tests to inject a
    /// `StubSiblingResolver` without constructing a full database.
    sibling_resolver: Arc<dyn crate::spawn::sibling::SiblingPersonaResolver>,
    /// Root directory for draft persona KDL files written by
    /// `spawn_sibling_new`. Defaults to
    /// `<XDG_DATA_HOME>/pattern/drafts` (falling back to `.pattern/drafts`
    /// relative to the current directory when `XDG_DATA_HOME` is unset).
    drafts_dir: std::path::PathBuf,
    /// Per-session registry tracking outstanding fork handles.
    ///
    /// Forks created via `Spawn.fork` are inserted here so subsequent
    /// `ForkOp` dispatches (`MergeBack`, `Discard`, `Promote`) can reach
    /// them by id. The `Arc<dyn ForkRegistry>` indirection mirrors
    /// [`Self::sibling_resolver`] — Phase 6 swaps the in-memory default
    /// for a DB-backed registry.
    fork_registry: Arc<dyn ForkRegistry>,
    /// Concrete `Arc<MemoryCache>` for the parent's memory state.
    ///
    /// Populated by daemon paths that want lightweight forks to copy
    /// the parent's actual block set; left `None` for the test path
    /// where `from_persona` constructs the session against the
    /// `MemoryStoreAdapter` only. When `None`, lightweight forks fall
    /// back to an empty child cache; persistent forks return
    /// `ForkError::PersistentNotAvailable`.
    memory_cache: Option<Arc<pattern_memory::MemoryCache>>,
    /// Mount metadata used by the persistent-fork path.
    ///
    /// Set via [`Self::with_mount_info`]; `None` means persistent forks
    /// are not available on this session.
    mount_info: Option<MountInfo>,
    /// Per-session inbox for messages, task assignments, and wake
    /// activations. Constructed eagerly at session-open time so peers
    /// can hand a sender clone to the [`AgentRegistry`] (T4) without
    /// races. The [`MailboxTask`] (T3) drains this when the session
    /// is idle.
    mailbox: Arc<crate::mailbox::Mailbox>,
    /// Live busy flag for the agent's turn loop.
    ///
    /// Set to `true` by `agent_loop::drive_step` at entry, cleared at
    /// exit (panic-safe via RAII guard). The `MailboxTask` reads it to
    /// decide whether to pull the next input or park on
    /// [`Self::turn_done`] until the current turn finishes.
    is_in_turn: Arc<std::sync::atomic::AtomicBool>,
    /// Notify edge raised whenever `is_in_turn` flips back to `false`.
    /// The `MailboxTask` parks on `notified()` while the session is
    /// busy and wakes on each turn-end edge to drain the queue.
    turn_done: Arc<tokio::sync::Notify>,
    /// Optional shared [`AgentRegistry`] for inter-session routing.
    ///
    /// When `Some`, the session's persona is registered with `Active`
    /// status at open time (via [`AgentRegistry::register`]) and
    /// unregistered on drop (via [`RegistryGuard`] held by
    /// [`TidepoolSession`]). When `None`, the session participates in
    /// no inter-agent registry — suitable for ephemeral children and
    /// test sessions that do not need peer-to-peer routing.
    ///
    /// The `AgentRouter` looks up recipients here. Wired by the daemon
    /// via [`SessionContext::with_agent_registry`] before
    /// `Arc::new(ctx)`.
    agent_registry: Option<Arc<crate::agent_registry::AgentRegistry>>,
    /// Per-session wake-condition registry. Reachable via
    /// [`Self::wake_registry`] when wired (the `Pattern.Wake` handler
    /// returns `EffectError::Handler` if it isn't).
    ///
    /// Wake evaluator tasks are owned by this registry; dropping the
    /// session drops the registry, which aborts every evaluator task
    /// and unsubscribes the loro callbacks they hold. Eligible
    /// receivers for wake activations are this session's [`Mailbox`].
    wake_registry: Option<Arc<crate::wake::WakeRegistry>>,
    /// Constellation registry handle. Populated by daemon callers via
    /// `with_constellation_registry`; the `Pattern.Constellation` handler
    /// reads from it. `None` for test sessions that don't need agent
    /// program access to persona records.
    constellation_registry: Option<Arc<dyn pattern_core::ConstellationRegistry>>,
    /// Owns the canonical [`pattern_core::fronting::FrontingSet`] lock
    /// AND the synchronous commit path for SDK-driven `Pattern.Fronting`
    /// mutations. Read-only access (the `Current` handler) goes through
    /// `committer.fronting_set()`; mutations go through `commit_sync`.
    ///
    /// Bundling the lock and the commit path in one trait object means
    /// the lock the handler reads is axiomatically the lock the daemon
    /// persists — they cannot drift.
    ///
    /// Daemon sessions wire a `DaemonFrontingCommitter` (three-phase commit
    /// plus DB persist plus `FrontingChanged` fan-out). Test sessions wire an
    /// `InMemoryFrontingCommitter` (no-op persist, no event emission). When
    /// `None`, the `Pattern.Fronting` effect is unwired entirely; the handler
    /// returns a `FRONTING_NOT_WIRED_PREFIX`-marked error.
    fronting_committer: Option<Arc<dyn crate::sdk::handlers::fronting::FrontingCommitter>>,
    /// Per-session MCP server registry. Always present (may be empty).
    mcp_registry: Arc<crate::mcp::McpRegistry>,
}

/// Handlers call this to decide whether to short-circuit on soft-cancel.
///
/// The session's `SessionContext` implements this to expose the shared
/// [`CancelState`]; the no-op blanket impl on `()` lets existing unit
/// tests keep passing `&()` as the user context.
pub trait HasCancelState {
    /// Shared cancel state used by the watchdog + handlers. A no-op
    /// implementation (e.g. on `()`) may return a fresh, unrelated
    /// state — handlers will just observe `false` and proceed.
    fn cancel_state(&self) -> Arc<CancelState>;
}

impl HasCancelState for SessionContext {
    fn cancel_state(&self) -> Arc<CancelState> {
        SessionContext::cancel_state(self)
    }
}

/// Handlers that dispatch MCP calls need access to the per-session registry.
pub trait HasMcpRegistry {
    fn mcp_registry(&self) -> &Arc<crate::mcp::McpRegistry>;
}

impl HasMcpRegistry for SessionContext {
    fn mcp_registry(&self) -> &Arc<crate::mcp::McpRegistry> {
        &self.mcp_registry
    }
}

impl HasMcpRegistry for () {
    fn mcp_registry(&self) -> &Arc<crate::mcp::McpRegistry> {
        static EMPTY: std::sync::LazyLock<Arc<crate::mcp::McpRegistry>> =
            std::sync::LazyLock::new(|| Arc::new(crate::mcp::McpRegistry::default()));
        &EMPTY
    }
}
/// Handlers call this to read the active [`pattern_core::PolicySet`].
///
/// `SessionContext` exposes the live, KDL-merged set; the `()` shim
/// returns an always-empty set so unit tests using `&()` see every
/// effect as [`pattern_core::PolicyAction::Allow`] (i.e. they fall
/// straight through to the handler's existing "no gate" path).
/// Handlers call this to perform per-constructor effect-class checks.
///
/// `SessionContext` provides the live `CapabilitySet`; the no-op `()` impl
/// returns `None` (no restrictions — backwards-compatible full access).
pub trait HasCapabilities {
    fn capabilities(&self) -> Option<&pattern_core::CapabilitySet>;
}

impl HasCapabilities for SessionContext {
    fn capabilities(&self) -> Option<&pattern_core::CapabilitySet> {
        SessionContext::capabilities(self)
    }
}

impl HasCapabilities for () {
    fn capabilities(&self) -> Option<&pattern_core::CapabilitySet> {
        None
    }
}

pub trait HasPolicySet {
    fn policies(&self) -> &pattern_core::PolicySet;
}

impl HasPolicySet for SessionContext {
    fn policies(&self) -> &pattern_core::PolicySet {
        SessionContext::policies(self)
    }
}

impl HasPolicySet for () {
    fn policies(&self) -> &pattern_core::PolicySet {
        static EMPTY: std::sync::OnceLock<pattern_core::PolicySet> = std::sync::OnceLock::new();
        EMPTY.get_or_init(pattern_core::PolicySet::new)
    }
}

/// Handlers call this to consult the per-session
/// [`crate::permission::PermissionBridge`] and the current turn's
/// originator.
///
/// `SessionContext` provides the live wiring; the no-op `()` impl lets
/// unit tests pass `&()` as the user value (handlers will observe the
/// gate as missing and fall back to allow-by-default policy paths or
/// surface a clear error).
pub trait HasPermissionBridge {
    /// Sync-to-async bridge to the per-session broker. `None` for
    /// sessions that haven't been wired yet (or for the `()` test
    /// shim).
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>>;

    /// Origin of the immediate dispatcher of the current effect —
    /// `Author::Agent(self)` during normal model-driven dispatch; can
    /// be a `Partner` only when a future direct-execution path
    /// explicitly overrides the slot before invoking a handler.
    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin>;

    /// Agent identifier for broker attribution. The broker's
    /// `scope_cache` is keyed `(agent_id, scope)`; using the real
    /// session agent here is **load-bearing for per-agent isolation** —
    /// two agents in the same runtime asking for the same scope must
    /// NOT share a single grant. Returning `None` (the `()` shim's
    /// behaviour) tells handlers to fail closed: the broker call is
    /// skipped and the request is treated as a denial.
    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId>;
}

impl HasPermissionBridge for SessionContext {
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        SessionContext::permission_bridge(self)
    }

    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        SessionContext::current_dispatch_origin(self)
    }

    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId> {
        Some(pattern_core::AgentId::from(SessionContext::agent_id(self)))
    }
}

impl HasPermissionBridge for () {
    fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        None
    }
    fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        None
    }
    fn dispatch_agent_id(&self) -> Option<pattern_core::AgentId> {
        None
    }
}

impl HasCancelState for () {
    fn cancel_state(&self) -> Arc<CancelState> {
        // Return a freshly allocated, never-cancelled state. Handlers
        // using `&()` as their user context effectively bypass the
        // cancellation check: they'll observe `cancellation == false`
        // and the gate entry will simply increment a fresh counter
        // nobody observes. No caller of `cx.user().cancel_state()`
        // depends on `Arc` identity across calls within a single
        // dispatch, so allocating per call is cheap and simpler than
        // the prior thread-local caching approach.
        Arc::new(CancelState::new())
    }
}

/// Handlers call this to reach the per-session [`SpawnRegistry`].
///
/// `SessionContext` exposes the live registry; the `()` shim returns a
/// shared zero-limit registry so unit tests using `&()` as their user
/// context compile without error. The `()` registry's limit of 0 means
/// all `try_acquire_ephemeral_slot` calls return `None` — appropriate for
/// handler unit tests that are not testing spawn semantics.
pub trait HasSpawnRegistry {
    /// Per-session spawn registry. Handlers use this to acquire slots,
    /// register child handles, and surface the concurrency limit in errors.
    fn spawn_registry(&self) -> &Arc<SpawnRegistry>;
}

impl HasSpawnRegistry for SessionContext {
    fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        &self.spawn_registry
    }
}

impl HasSpawnRegistry for () {
    fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        // Zero-limit registry shared across all `()` calls. Unit tests
        // that use `&()` as their user context are not testing spawn
        // semantics; a limit-0 registry ensures no accidental spawns while
        // satisfying the trait bound.
        static SHIM: std::sync::OnceLock<Arc<SpawnRegistry>> = std::sync::OnceLock::new();
        SHIM.get_or_init(|| Arc::new(SpawnRegistry::new("test-shim", 0)))
    }
}

/// Handlers call this to access the session's file manager (when wired).
///
/// The `()` shim returns `None`, giving handlers a closed-by-default path
/// for test doubles that don't construct a full mount + FileManager.
pub trait HasFileManager {
    fn file_manager(&self) -> Option<&Arc<crate::file_manager::FileManager>>;
}

impl HasFileManager for SessionContext {
    fn file_manager(&self) -> Option<&Arc<crate::file_manager::FileManager>> {
        SessionContext::file_manager(self)
    }
}

impl HasFileManager for () {
    fn file_manager(&self) -> Option<&Arc<crate::file_manager::FileManager>> {
        None
    }
}

impl Drop for SessionContext {
    fn drop(&mut self) {
        // Unregister all this session's plugin route entries so subsequent
        // sessions can claim the same plugin pubkeys without collision.
        // Fires when the last Arc<SessionContext> is dropped — which is
        // when the TidepoolSession (+ any ephemeral clones holding the Arc)
        // are all gone. No-op if `plugin_routes` is None or no entries
        // were registered for this session.
        if let Some(routes) = self.plugin_routes.as_ref() {
            let session_id = self.agent_id.to_string();
            let removed = routes.unregister_session(&session_id);
            if removed > 0 {
                tracing::debug!(
                    session = %session_id,
                    removed,
                    "unregistered plugin routes on session-context drop"
                );
            }
        }
        // Phase A.2b: unregister this session's host_handler so the routing
        // handler stops dispatching to a dropped session.
        if let Some(routing_handler) = self.plugin_routing_handler.as_ref() {
            let session_id: smol_str::SmolStr = self.agent_id.to_string().into();
            routing_handler.unregister_handler(&session_id);
            tracing::debug!(
                session = %session_id,
                "unregistered per-session host_handler on session-context drop"
            );
        }
        // Same for the memory-sync handler.
        if let Some(routing_handler) = self.plugin_memory_sync_handler.as_ref() {
            let session_id: smol_str::SmolStr = self.agent_id.to_string().into();
            routing_handler.unregister_handler(&session_id);
            tracing::debug!(
                session = %session_id,
                "unregistered per-session memory_sync_handler on session-context drop"
            );
        }
    }
}

impl SessionContext {
    /// Build a context from a persona + store handle. The store is wrapped
    /// in a [`MemoryStoreAdapter`] that records `BlockWrite` entries;
    /// handlers call [`SessionContext::adapter`] to access `record_write`.
    /// Shared cancel state starts un-cancelled and with no handlers in
    /// flight. The checkpoint log is a fresh empty log; the session wires
    /// a shared log via the crate-private `with_checkpoint_log` builder so
    /// handlers record into the same log the session exposes.
    pub fn from_persona(
        persona: &PersonaSnapshot,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        let agent_id = persona.agent_id.to_string();
        // Default scope without a mount is the persona's own Global scope.
        // `with_scope_binding` overrides this to `Scope::Local(project_id)`
        // when a project mount is wired.
        let default_scope =
            pattern_core::types::memory_types::Scope::Global(agent_id.clone().into());
        let budget = Budget::from_persona(persona);
        let adapter = Arc::new(MemoryStoreAdapter::new(memory_store, &agent_id));
        // Default concurrency limit of 8: a conservative starting point for
        // ensemble patterns. Revisit when Phase 7 ensemble patterns stress
        // this ceiling. The agent_id is the natural parent identifier at this
        // stage; TidepoolSession::open will have the session_id but from_persona
        // does not — agent_id is stable and unambiguous as a parent label.
        let spawn_registry = Arc::new(SpawnRegistry::new(agent_id.clone(), 8));
        let hook_bus__ = Arc::new(pattern_core::hooks::HookBus::new());
        let hook_bridge__ =
            crate::hooks::HookBridge::spawn_on(hook_bus__.clone(), tokio_handle.clone());
        Self {
            plugin_registry: None,
            plugin_routes: None,
            plugin_routing_handler: None,
            plugin_memory_sync_handler: None,
            daemon_endpoint: None,
            agent_id,
            default_scope,
            // Thread the caller's declared model through so the composer's
            // `ctx.model_id()` matches the persona's intent. Callers that
            // want to override a persona's default at open time should
            // mutate `persona.model.choice` before calling into the
            // runtime.
            model_id: persona.model.choice.model_id.to_string(),
            system_prompt: persona.system_prompt.clone(),
            chat_options: persona.model.chat_options.clone(),
            budget,
            cancel_state: Arc::new(CancelState::new()),
            adapter,
            provider,
            db,
            router: Arc::new(RouterRegistry::new()),
            router_bridge: None,
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            turn_sink: Arc::new(NoOpSink),
            spawn_sink_factory: Arc::new(std::sync::RwLock::new(None)),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
            snapshot_policy: persona.context.snapshot_policy.clone(),
            context_policy: persona.context.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(Vec::new())),
            capabilities: persona.capabilities.clone(),
            policies: Arc::new(merge_policies(persona)),
            permission_broker: Arc::new(pattern_core::permission::PermissionBroker::new()),
            permission_bridge: None,
            current_dispatch_origin: Arc::new(std::sync::RwLock::new(None)),
            // v3-sandbox-io I/O subsystems (Phases 2-5).
            async_reminder_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            pending_tool_attachments: Arc::new(std::sync::Mutex::new(Vec::new())),
            file_manager: None,
            process_manager: Arc::new(crate::process_manager::ProcessManager::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
                dirs::cache_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("pattern"),
            )),
            reembed_tx: None,
            port_registry: None,
            hook_bus: hook_bus__.clone(),
            hook_bridge: hook_bridge__,
            session_id: pattern_core::types::ids::new_id().to_string(),
            shell_default_timeout: std::time::Duration::from_secs(30),
            spawn_registry,
            tokio_handle,
            include_paths: Arc::new(Vec::new()),
            sibling_resolver: Arc::new(crate::spawn::sibling::UnconfiguredSiblingResolver),
            drafts_dir: default_drafts_dir(),
            fork_registry: Arc::new(InMemoryForkRegistry::new()),
            memory_cache: None,
            mount_info: None,
            mailbox: crate::mailbox::Mailbox::new(persona.agent_id.clone()).0,
            is_in_turn: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            turn_done: Arc::new(tokio::sync::Notify::new()),
            agent_registry: None,
            wake_registry: None,
            constellation_registry: None,
            fronting_committer: None,
            mcp_registry: Arc::new(crate::mcp::McpRegistry::default()),
        }
    }

    /// Resolver for the sibling spawn path.
    ///
    /// Returns the `Arc<dyn SiblingPersonaResolver>` wired at construction
    /// time. The default is [`crate::spawn::sibling::UnconfiguredSiblingResolver`];
    /// tests inject a [`crate::spawn::sibling::StubSiblingResolver`].
    pub fn sibling_resolver(&self) -> &Arc<dyn crate::spawn::sibling::SiblingPersonaResolver> {
        &self.sibling_resolver
    }

    /// Builder-style: replace the sibling persona resolver.
    ///
    /// Tests inject a [`crate::spawn::sibling::StubSiblingResolver`]; Phase 6
    /// replaces the default [`crate::spawn::sibling::UnconfiguredSiblingResolver`]
    /// with a `pattern_db`-backed resolver.
    #[must_use]
    pub fn with_sibling_resolver(
        mut self,
        resolver: std::sync::Arc<dyn crate::spawn::sibling::SiblingPersonaResolver>,
    ) -> Self {
        self.sibling_resolver = resolver;
        self
    }

    /// Root directory for draft persona KDL files.
    pub fn drafts_dir(&self) -> &std::path::Path {
        &self.drafts_dir
    }

    /// Builder-style: replace the drafts directory.
    ///
    /// Used by tests and by Phase 6 daemon wiring to route draft KDL files to
    /// an explicit location rather than the XDG default.
    #[must_use]
    pub fn with_drafts_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.drafts_dir = dir;
        self
    }

    /// Per-session fork registry. Read by the spawn handler when
    /// inserting freshly-created forks and dispatching `ForkOp`s.
    pub fn fork_registry(&self) -> &Arc<dyn ForkRegistry> {
        &self.fork_registry
    }

    /// Builder-style: replace the fork registry. Phase 6 wires a
    /// DB-backed implementation here; Phase 3 production paths use
    /// the [`InMemoryForkRegistry`] default seeded by `from_persona`.
    #[must_use]
    pub fn with_fork_registry(mut self, registry: Arc<dyn ForkRegistry>) -> Self {
        self.fork_registry = registry;
        self
    }

    /// Concrete `Arc<MemoryCache>` for the parent's memory state.
    ///
    /// Populated only when the daemon (or test fixture) explicitly
    /// wires it via [`Self::with_memory_cache`]. The lightweight-fork
    /// path uses this to call `MemoryCache::fork_for_child`; without
    /// it the fork starts from an empty child cache.
    pub fn memory_cache(&self) -> Option<&Arc<pattern_memory::MemoryCache>> {
        self.memory_cache.as_ref()
    }

    /// Builder-style: attach the parent's `Arc<MemoryCache>`.
    #[must_use]
    pub fn with_memory_cache(mut self, cache: Arc<pattern_memory::MemoryCache>) -> Self {
        self.memory_cache = Some(cache);
        self
    }

    /// Mount-level metadata for this session. `None` means no mount is
    /// attached (persistent forks unavailable).
    pub fn mount_info(&self) -> Option<&MountInfo> {
        self.mount_info.as_ref()
    }

    /// Builder-style: attach mount metadata.
    #[must_use]
    pub fn with_mount_info(mut self, info: MountInfo) -> Self {
        self.mount_info = Some(info);
        self
    }

    // ─── v3-sandbox-io I/O accessors + builders (Phases 2-5) ────────────────

    /// Per-session file manager, if wired. `None` when the session was
    /// opened without a `file_policy` (test fixtures, sessions on
    /// non-mount paths).
    pub fn file_manager(&self) -> Option<&Arc<crate::file_manager::FileManager>> {
        self.file_manager.as_ref()
    }

    /// Builder-style: replace the file manager. Used by
    /// [`TidepoolSession::open_with_agent_loop`] when the caller passed
    /// `Some(file_policy)` in `SessionRegistries`; tests inject mocks.
    #[must_use]
    pub fn with_file_manager(mut self, fm: Arc<crate::file_manager::FileManager>) -> Self {
        self.file_manager = Some(fm);
        self
    }

    /// Per-session process manager wrapping the local PTY backend.
    /// Always present — `from_persona` constructs a default rooted at
    /// `current_dir`.
    pub fn process_manager(&self) -> &Arc<crate::process_manager::ProcessManager> {
        &self.process_manager
    }

    /// Builder-style: replace the process manager. Test fixtures use
    /// this to inject a manager rooted at a controlled cache dir; the
    /// production path keeps the `from_persona` default.
    #[must_use]
    pub fn with_process_manager(mut self, pm: Arc<crate::process_manager::ProcessManager>) -> Self {
        self.process_manager = pm;
        self
    }

    /// Per-session port registry. `None` for sessions opened without
    /// a `SessionRegistries.port_registry` — Pattern.Port is filtered
    /// out of the agent's effect row at preamble-build time.
    /// The session's hook event bus.
    pub fn hook_bus(&self) -> &Arc<pattern_core::hooks::HookBus> {
        &self.hook_bus
    }

    /// The hook bridge for sync handler → async dispatch.
    pub fn hook_bridge(&self) -> &crate::hooks::HookBridge {
        &self.hook_bridge
    }

    /// The plugin registry.
    pub fn plugin_registry(&self) -> Option<&Arc<crate::plugin::registry::PluginRegistry>> {
        self.plugin_registry.as_ref()
    }

    /// Set the plugin registry.
    pub fn with_plugin_registry(
        mut self,
        reg: Arc<crate::plugin::registry::PluginRegistry>,
    ) -> Self {
        self.plugin_registry = Some(reg);
        self
    }

    /// Borrow the plugin route table.
    pub fn plugin_routes(
        &self,
    ) -> Option<&Arc<pattern_core::plugin::auth::PluginRouteTable>> {
        self.plugin_routes.as_ref()
    }

    /// Set the plugin route table (daemon-shared).
    pub fn with_plugin_routes(
        mut self,
        routes: Arc<pattern_core::plugin::auth::PluginRouteTable>,
    ) -> Self {
        self.plugin_routes = Some(routes);
        self
    }

    /// Daemon-shared session-routing protocol handler. Sessions register their
    /// per-session host handler with this at open time.
    pub fn plugin_routing_handler(
        &self,
    ) -> Option<&Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>> {
        self.plugin_routing_handler.as_ref()
    }

    /// Accessor for the memory-sync routing handler.
    pub fn plugin_memory_sync_handler(
        &self,
    ) -> Option<&Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>> {
        self.plugin_memory_sync_handler.as_ref()
    }

    /// Set the memory-sync routing handler (daemon-shared).
    pub fn with_plugin_memory_sync_handler(
        mut self,
        handler: Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>,
    ) -> Self {
        self.plugin_memory_sync_handler = Some(handler);
        self
    }

    /// Set the session-routing protocol handler (daemon-shared).
    pub fn with_plugin_routing_handler(
        mut self,
        handler: Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>,
    ) -> Self {
        self.plugin_routing_handler = Some(handler);
        self
    }

    /// Daemon iroh endpoint accessor (Phase 6 Task 5). Used at session-open
    /// to construct `OutOfProcessPluginConnection` for native plugins.
    pub fn daemon_endpoint(&self) -> Option<&iroh::Endpoint> {
        self.daemon_endpoint.as_ref()
    }

    pub fn with_daemon_endpoint(mut self, endpoint: iroh::Endpoint) -> Self {
        self.daemon_endpoint = Some(endpoint);
        self
    }

    pub fn port_registry(&self) -> Option<&Arc<crate::port_registry::PortRegistryImpl>> {
        self.port_registry.as_ref()
    }

    /// Builder-style: wire a shared port registry. Daemon paths build
    /// the registry once via `PortRegistryImpl::with_runtime_ports` and
    /// pass it through `SessionRegistries` to every session opened
    /// against the mount.
    #[must_use]
    pub fn with_port_registry(
        mut self,
        registry: Arc<crate::port_registry::PortRegistryImpl>,
    ) -> Self {
        self.port_registry = Some(registry);
        self
    }

    /// Shared handle to the between-turn async-reminder queue.
    /// Sub-coordinators (FileManager external-edit watcher,
    /// ProcessManager spawn-output bridge, port-subscription drain
    /// task) clone this to push reminders from background threads.
    pub fn async_reminder_queue(
        &self,
    ) -> &Arc<std::sync::Mutex<Vec<pattern_core::types::message::MessageAttachment>>> {
        &self.async_reminder_queue
    }

    /// Push a multi-modal ContentPart onto the per-eval attachment buffer.
    /// Drained by eval_worker after the current eval completes.
    pub fn push_pending_tool_attachment(&self, part: genai::chat::ContentPart) {
        self.pending_tool_attachments.lock().unwrap().push(part);
    }

    /// Drain (and reset) the per-eval attachment buffer. Called by eval_worker.
    pub fn drain_pending_tool_attachments(&self) -> Vec<genai::chat::ContentPart> {
        std::mem::take(&mut *self.pending_tool_attachments.lock().unwrap())
    }


    /// Drain all pending async reminders. Called by `compose_request_for_turn`
    /// to splice attachments onto the next turn's first user message.
    pub fn drain_async_reminders(&self) -> Vec<pattern_core::types::message::MessageAttachment> {
        std::mem::take(&mut *self.async_reminder_queue.lock().unwrap())
    }

    /// Record an async reminder for delivery on the next turn. Used by
    /// callers that already hold a `&SessionContext` rather than a clone
    /// of the queue Arc.
    pub fn record_async_reminder(
        &self,
        attachment: pattern_core::types::message::MessageAttachment,
    ) {
        self.async_reminder_queue.lock().unwrap().push(attachment);
    }

    /// Session-scoped UUID. Used by handlers that key per-session state by
    /// stable id (e.g. `PortHandler` keys subscription channels by
    /// session_id). Mirrors the id held on `TidepoolSession`.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Crate-internal: align the session id with `TidepoolSession::open`.
    pub(crate) fn with_session_id(mut self, id: String) -> Self {
        self.session_id = id;
        self
    }

    /// Default timeout for `Shell.Execute` when the agent doesn't supply
    /// one. Read by the shell handler; overridable for test fixtures.
    pub fn shell_default_timeout(&self) -> std::time::Duration {
        self.shell_default_timeout
    }

    /// Builder-style: override the default execute timeout.
    #[must_use]
    pub fn with_shell_default_timeout(mut self, d: std::time::Duration) -> Self {
        self.shell_default_timeout = d;
        self
    }

    // ────────────────────────────────────────────────────────────────────────

    /// Replace the session's include-paths set. Called by
    /// [`TidepoolSession::open_with_agent_loop`] after lib-module
    /// validation; child-session forks (`fork_for_ephemeral`) read this
    /// to inherit the parent's resolved set.
    pub(crate) fn set_include_paths(&mut self, paths: Arc<Vec<std::path::PathBuf>>) {
        self.include_paths = paths;
    }

    /// Replace the session's spawn registry with a fresh one carrying
    /// a custom concurrency ceiling. Only available under
    /// `feature = "test-support"` — production code MUST use the
    /// default ceiling threaded via `from_persona`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_spawn_registry_for_test(&mut self, limit: usize) {
        self.spawn_registry = Arc::new(SpawnRegistry::new(self.agent_id.clone(), limit));
    }

    /// GHC include paths threaded into this session's eval worker.
    ///
    /// Empty for sessions constructed via `from_persona` without an
    /// eval worker (test paths). Populated at session-open time by
    /// [`TidepoolSession::open_with_agent_loop`].
    pub fn include_paths(&self) -> &Arc<Vec<std::path::PathBuf>> {
        &self.include_paths
    }

    /// Build a child session context for an ephemeral spawn.
    ///
    /// What's shared (Arc-cloned from parent): provider, db, router,
    /// adapter (MemoryStoreAdapter — child reads parent's memory),
    /// cancel_state (parent's cancel propagates to child), tokio_handle.
    ///
    /// What's fresh: pending_messages, checkpoint_log, current_turn,
    /// spawn_registry (sub-registry), current_dispatch_origin, diagnostics,
    /// permission_broker (no bridge yet — handler-side concern).
    ///
    /// What's overridden: capabilities (caller-supplied subset),
    /// system_prompt (`cfg.costume` when set; otherwise inherits parent's),
    /// agent_id (same as parent — persona identity stays in logs per
    /// AC3.3).
    ///
    /// Returns the constructed child context as `Arc<SessionContext>`.
    /// Caller is responsible for the spawn-lib synthesis + child
    /// include-path extension.
    pub fn fork_for_ephemeral(
        &self,
        cfg: &pattern_core::spawn::EphemeralConfig,
        child_caps: pattern_core::CapabilitySet,
        child_include_paths: Arc<Vec<std::path::PathBuf>>,
        child_id: smol_str::SmolStr,
        progress_log_label: smol_str::SmolStr,
    ) -> Arc<SessionContext> {
        // Sub-registry concurrency limit. Half the parent's default is a
        // conservative starting point — ensembles-of-ensembles are a
        // Phase 7 concern; revisit when the workload demands it.
        let sub_limit: usize = (self.spawn_registry.concurrent_ephemeral_limit() / 2).max(1);
        let child_registry = Arc::new(SpawnRegistry::new(self.agent_id.clone(), sub_limit));

        // Costume override: replace the system_prompt slot when set;
        // otherwise inherit parent's.
        let system_prompt = cfg.costume.clone().or_else(|| self.system_prompt.clone());

        // Cancel-propagation chain (Phase 2 Task 5): the child SHARES
        // the parent's cancel_state Arc, so flipping the parent's
        // cancellation atomic immediately makes `is_cancelled()` true
        // on the child as well. To cascade further down (the child's
        // OWN children — i.e., grandchildren of the parent), spawn a
        // watcher that calls cancel_all() on the child's sub-registry
        // once the parent cancel flag flips.
        //
        // The watcher closure captures a `Weak<SpawnRegistry>` rather
        // than a strong `Arc` — load-bearing for AC3.6 leak-freedom on
        // the happy path. If the closure held an Arc, the registry's
        // strong count would never drop to zero (the closure is parked
        // on `notify.notified()` indefinitely until the parent cancels),
        // so `SpawnRegistry::Drop` would be unreachable in long-lived
        // parents. With Weak, the consumer-side drop of
        // `Arc<SessionContext>` brings the registry's strong count to
        // zero, `Drop` fires, and the stored watcher handle is
        // `abort()`'d immediately — no leaked tokio task.
        let parent_cancel_for_watcher = self.cancel_state.clone();
        let child_registry_weak = Arc::downgrade(&child_registry);
        let watcher_handle = self.tokio_handle.spawn(async move {
            parent_cancel_for_watcher.wait_for_cancel().await;
            if let Some(reg) = child_registry_weak.upgrade() {
                reg.cancel_all();
            }
            // If `upgrade` returned None, the registry was already
            // dropped — nothing to cancel. Closure exits, releasing
            // its `Arc<CancelState>`.
        });
        child_registry.install_watcher(watcher_handle);

        // Child gets its own CancelState so that child-side cancellation
        // (e.g. ephemeral timeout) does NOT propagate UP to the parent.
        // A one-way watcher propagates parent cancel → child cancel.
        // Uses Weak for the child so the closure doesn't prevent child drop.
        // The JoinHandle is installed on child_registry so it's aborted when
        // the registry drops (same pattern as the grandchild watcher above).
        let child_cancel = Arc::new(CancelState::new());
        let parent_cancel_for_child = self.cancel_state.clone();
        let child_cancel_weak = Arc::downgrade(&child_cancel);
        let cancel_watcher = self.tokio_handle.spawn(async move {
            parent_cancel_for_child.wait_for_cancel().await;
            if let Some(child_cs) = child_cancel_weak.upgrade() {
                child_cs.request_cancel();
            }
        });
        child_registry.install_watcher(cancel_watcher);

        // Namespace the child's execution agent_id so storage / wire
        // routing naturally distinguish spawn batches from the parent's
        // own work. Format: `<parent_agent_id>:spawn:<spawn_id>`. The
        // memory scope still inherits from parent (`default_scope` below)
        // so block access stays parent-scoped — execution identity and
        // persona/scope identity are intentionally split. See spawn-fork
        // design notes for rationale.
        // Suffix is `name` if the caller supplied one, otherwise the
        // spawn_id. Named spawns let the caller see clear ids in TUI/storage
        // (`pattern:spawn:retrieval-helper` instead of `pattern:spawn:37dd...`);
        // multiple spawns sharing a name share an agent_id by design — name
        // = group, spawn_id = instance, distinct batch_ids preserve per-turn
        // routing.
        let suffix: String = match cfg.name.as_deref() {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => child_id.to_string(),
        };
        let child_exec_agent_id: String = format!("{}:spawn:{}", self.agent_id, suffix);

        let child = SessionContext {
            agent_id: child_exec_agent_id,
            default_scope: self.default_scope.clone(),
            model_id: cfg
                .model_id
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_else(|| self.model_id.clone()),
            system_prompt,
            chat_options: self.chat_options.clone(),
            budget: self.budget,
            // Child's own cancel state — parent cancel propagates down
            // via the watcher above, but child cancel doesn't propagate up.
            cancel_state: child_cancel,
            // Shared adapter — child reads parent's memory. Write
            // restriction is enforced by the child's capability set
            // (the caller restricted it via restrict_to() before this
            // call).
            adapter: self.adapter.clone(),
            provider: self.provider.clone(),
            db: self.db.clone(),
            router: self.router.clone(),
            // No router bridge: the child runs without RouterBridge
            // wired; messaging effects must be opt-in via the child's
            // CapabilitySet.
            router_bridge: None,
            pending_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            // Inherit parent's turn sink as a default. If the parent has
            // a `spawn_sink_factory` installed, the child's turn_sink is
            // post-construction replaced with a freshly minted bridge
            // tagged `SpawnSource::Ephemeral { ... }` (see immediately
            // after this struct literal). Headless / test sessions don't
            // install a factory, so the inherit-verbatim path is the
            // operative one for them.
            turn_sink: self.turn_sink.clone(),
            // Children inherit the parent's factory so any grand-child
            // ephemerals also get tagged sinks.
            spawn_sink_factory: self.spawn_sink_factory.clone(),
            checkpoint_log: Arc::new(std::sync::Mutex::new(CheckpointLog::new())),
            current_turn: Arc::new(AtomicU64::new(0)),
            snapshot_policy: self.snapshot_policy.clone(),
            context_policy: self.context_policy.clone(),
            diagnostics: Arc::new(std::sync::Mutex::new(Vec::new())),
            capabilities: Some(child_caps),
            policies: self.policies.clone(),
            permission_broker: self.permission_broker.clone(),
            // Permission bridge is None; ephemerals don't currently
            // route gated effects through the broker (Phase 4+ may
            // revisit).
            permission_bridge: self.permission_bridge.clone(),
            current_dispatch_origin: Arc::new(std::sync::RwLock::new(None)),
            // v3-sandbox-io I/O subsystems: ephemeral children inherit
            // the parent's `file_manager` (same project files in scope)
            // and `port_registry` (same external services available),
            // but get a fresh `process_manager` so their shell state
            // (cwd, env, running tasks) is isolated from the parent's.
            //
            // FUTURE WORK (Phase 8+, 2026-04-28): wire per-effect permission
            // scoping so that an ephemeral child whose `child_caps` excludes
            // File or Port doesn't carry the parent's manager handle into
            // scope. Right now the inherited manager is a hard reference;
            // capability-driven access control happens at the handler level
            // via the child's CapabilitySet, which is sufficient for current
            // use but couples capability enforcement to per-handler checks.
            // A cleaner design would be `Option<Arc<...>>` populated only
            // when the child has the relevant capability.
            async_reminder_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            pending_tool_attachments: Arc::new(std::sync::Mutex::new(Vec::new())),
            file_manager: self.file_manager.clone(),
            process_manager: Arc::new(crate::process_manager::ProcessManager::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
                dirs::cache_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("pattern"),
            )),
            // Ephemeral children inherit parent's reembed_tx so messages
            // they persist also land in the vector index.
            reembed_tx: self.reembed_tx.clone(),
            port_registry: self.port_registry.clone(),
            hook_bus: self.hook_bus.clone(),
            hook_bridge: self.hook_bridge.clone(),
            plugin_registry: self.plugin_registry.clone(),
            plugin_routes: self.plugin_routes.clone(),
            plugin_routing_handler: self.plugin_routing_handler.clone(),
            plugin_memory_sync_handler: self.plugin_memory_sync_handler.clone(),
            daemon_endpoint: self.daemon_endpoint.clone(),
            // Each ephemeral child gets a fresh session_id (so its
            // PortHandler subscription channels don't collide with the
            // parent's). Inherit `shell_default_timeout` — children
            // share the parent's shell config defaults.
            session_id: pattern_core::types::ids::new_id().to_string(),
            shell_default_timeout: self.shell_default_timeout,
            spawn_registry: child_registry,
            tokio_handle: self.tokio_handle.clone(),
            include_paths: child_include_paths,
            // Inherit parent's resolver so ephemerals can spawn siblings.
            sibling_resolver: self.sibling_resolver.clone(),
            // Inherit parent's drafts dir so ephemerals write to the same
            // location.
            drafts_dir: self.drafts_dir.clone(),
            // Each child gets its own fork registry; forks scoped to the
            // child's own session lifetime do not bleed up to the parent.
            fork_registry: Arc::new(InMemoryForkRegistry::new()),
            // Inherit parent's memory cache + mount info so children that
            // use Spawn.fork can copy the same block set the parent holds.
            memory_cache: self.memory_cache.clone(),
            mount_info: self.mount_info.clone(),
            // Each child gets its own mailbox + busy flag — children
            // run independent turn loops, and a parent's busy state
            // says nothing about whether the child is mid-turn.
            mailbox: crate::mailbox::Mailbox::new(self.agent_id.clone().into()).0,
            is_in_turn: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            turn_done: Arc::new(tokio::sync::Notify::new()),
            // Ephemeral children do not register with the agent registry —
            // they are transient and not addressable by peer sessions.
            agent_registry: None,
            // Ephemeral children do not get a wake registry — they are
            // transient and cannot register long-running wake conditions.
            wake_registry: None,
            // Inherit the constellation registry so child sessions can
            // observe the same persona graph as the parent.
            constellation_registry: self.constellation_registry.clone(),
            // Children share the parent's fronting committer (which carries
            // the shared lock) so any SDK-driven fronting mutation from a
            // child also persists and fans out via the same path.
            fronting_committer: self.fronting_committer.clone(),
            mcp_registry: self.mcp_registry.clone(),
        };

        // If parent has a SpawnSinkFactory installed (daemon-driven
        // sessions), mint a tagged turn-sink for the child so its events
        // reach the daemon's bus stamped with `SpawnSource::Ephemeral`.
        // Headless / test sessions don't install a factory, in which case
        // child.turn_sink stays as the inherited parent sink (typically
        // NoOpSink) and this whole block is a no-op.
        let mut child = child;
        if let Some(factory) = self
            .spawn_sink_factory
            .read()
            .expect("spawn_sink_factory lock poisoned")
            .clone()
        {
            child.turn_sink = factory.fork_for_spawn(
                child_id.clone(),
                child.agent_id.clone().into(),
                pattern_core::spawn::SpawnSource::Ephemeral {
                    spawn_id: child_id.to_string(),
                    parent_agent_id: self.agent_id.to_string(),
                    progress_log_label: progress_log_label.to_string(),
                },
            );
        }

        Arc::new(child)
    }

    /// Caller-supplied tokio runtime handle. Borrowed for sync handler
    /// paths that need to `block_on` an async future without
    /// magic-capturing via `Handle::current()`.
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle {
        &self.tokio_handle
    }

    /// The session's mailbox — peers clone the `Arc<Mailbox>` and
    /// dispatch activations via [`crate::mailbox::Mailbox::send_input`].
    /// The [`MailboxTask`](crate::mailbox) (T3) holds the receiver
    /// guard for the lifetime of the session.
    pub fn mailbox(&self) -> &Arc<crate::mailbox::Mailbox> {
        &self.mailbox
    }

    /// Live busy flag — `true` while `agent_loop::drive_step` is
    /// executing on this session. The mailbox task reads it (and parks
    /// on [`Self::turn_done`] when set) before pulling the next input.
    pub fn is_in_turn(&self) -> &Arc<std::sync::atomic::AtomicBool> {
        &self.is_in_turn
    }

    /// Notify edge raised whenever the busy flag flips back to
    /// `false`. The mailbox task awaits `notified()` while busy and
    /// resumes drain on each turn-end edge.
    pub fn turn_done(&self) -> &Arc<tokio::sync::Notify> {
        &self.turn_done
    }

    /// Active policy set for this session. Handlers consult this
    /// before each effect dispatch; the result drives the broker
    /// escalation decision.
    pub fn policies(&self) -> &Arc<pattern_core::PolicySet> {
        &self.policies
    }

    /// Builder-style: replace the policy set (Phase 1 Task 14 wires
    /// KDL + runtime overrides over the seeded defaults).
    #[must_use]
    pub fn with_policies(mut self, policies: Arc<pattern_core::PolicySet>) -> Self {
        self.policies = policies;
        self
    }

    /// Per-runtime [`pattern_core::permission::PermissionBroker`]. Each
    /// session owns its own broker — there is no shared singleton.
    pub fn permission_broker(&self) -> &Arc<pattern_core::permission::PermissionBroker> {
        &self.permission_broker
    }

    /// Sync-to-async bridge to the broker, used by handlers running on
    /// the eval-worker thread. `None` until
    /// [`Self::with_permission_bridge`] has been called.
    pub fn permission_bridge(&self) -> Option<&Arc<crate::permission::PermissionBridge>> {
        self.permission_bridge.as_ref()
    }

    /// Origin of the immediate dispatcher invoking an effect during
    /// the active orchestrate iteration. Returns `Author::Agent(self)`
    /// during normal model-driven dispatch — handlers consult this
    /// (not the activating turn's origin) when feeding the broker's
    /// partner-bypass predicate, so autonomous agent activity does
    /// not inherit Partner authority on Partner-activated turns.
    pub fn current_dispatch_origin(&self) -> Option<pattern_core::types::origin::MessageOrigin> {
        self.current_dispatch_origin.read().ok()?.clone()
    }

    /// Internal handle to the current-dispatch-origin slot. Used by
    /// `agent_loop::drive_step`'s RAII guard to write the origin per
    /// orchestrate iteration and clear it on Drop.
    pub(crate) fn current_dispatch_origin_slot(
        &self,
    ) -> &Arc<std::sync::RwLock<Option<pattern_core::types::origin::MessageOrigin>>> {
        &self.current_dispatch_origin
    }

    /// Builder-style: install a [`crate::permission::PermissionBridge`]
    /// pumping this session's broker. Must be called from an async
    /// context (the bridge spawns a tokio task).
    #[must_use]
    pub fn with_permission_bridge(
        mut self,
        bridge: Arc<crate::permission::PermissionBridge>,
    ) -> Self {
        self.permission_bridge = Some(bridge);
        self
    }

    /// Effective capabilities for this session.
    ///
    /// `None` means "full power" — sessions that pre-date capability
    /// scoping or that omit a capability set on open. Phase 2 spawn
    /// paths use this to enforce that children cannot escalate
    /// beyond the parent.
    pub fn capabilities(&self) -> Option<&pattern_core::CapabilitySet> {
        self.capabilities.as_ref()
    }

    /// Builder-style: set this session's capability set. Pass `None`
    /// to leave capabilities unscoped.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Option<pattern_core::CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Persona-supplied slot-\[1\] system prompt override, if any.
    /// Composer consumes this in `compose_request_for_turn` when
    /// building the system-blocks array; `None` falls through to
    /// [`pattern_core::DEFAULT_BASE_INSTRUCTIONS`].
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    /// Baseline [`genai::chat::ChatOptions`] for requests composed in
    /// this session. Callers clone and layer on per-turn overrides
    /// (streaming capture flags, etc.). Persona-declared temperature,
    /// reasoning_effort, max_tokens, stop_sequences, etc. originate
    /// here.
    pub fn chat_options(&self) -> &genai::chat::ChatOptions {
        &self.chat_options
    }

    /// Wrap the underlying memory store in a [`pattern_memory::scope::MemoryScope`]
    /// with the given binding. This inserts the scope layer between the
    /// adapter and the raw store, enabling persona isolation per the
    /// [`IsolatePolicy`](pattern_core::types::memory_types::IsolatePolicy).
    ///
    /// Must be called before the session is shared (i.e., before
    /// `Arc::new(ctx)` in `TidepoolSession::open`). Calling after the
    /// adapter has been cloned elsewhere is a logic error (but harmless —
    /// only the original adapter sees the scope).
    #[must_use]
    pub fn with_scope_binding(mut self, binding: pattern_memory::scope::ScopeBinding) -> Self {
        use pattern_memory::scope::MemoryScope;
        // Update default_scope: project-bound sessions default to the
        // project's `Scope::Local`; passthrough sessions keep
        // `Scope::Global(persona_id)`.
        self.default_scope = match &binding.project_id {
            Some(project_id) => {
                pattern_core::types::memory_types::Scope::Local(project_id.clone().into())
            }
            None => {
                pattern_core::types::memory_types::Scope::Global(binding.persona_id.clone().into())
            }
        };
        let old_inner = self.adapter.inner().clone();
        let scoped: Arc<dyn MemoryStore> = Arc::new(MemoryScope::new(old_inner, binding));
        self.adapter = Arc::new(MemoryStoreAdapter::new(scoped, &self.agent_id));
        self
    }

    /// Default [`Scope`] for memory operations on this session.
    ///
    /// Project-bound sessions (mount with a project_id) default to
    /// `Scope::Local(project_id)` — agent reads/writes hit the shared
    /// project workspace by default. Unmounted sessions default to
    /// `Scope::Global(persona_id)`.
    pub fn default_scope(&self) -> &pattern_core::types::memory_types::Scope {
        &self.default_scope
    }

    /// Persona scope for this session (`Scope::Global(persona_id)`).
    /// Used by handlers that explicitly target persona memory regardless
    /// of the session's default routing.
    pub fn persona_scope(&self) -> pattern_core::types::memory_types::Scope {
        pattern_core::types::memory_types::Scope::Global(self.agent_id.clone().into())
    }

    /// Install a [`SpawnSinkFactory`] on this session. Called by the
    /// daemon (typically right after `open_with_agent_loop`) so that
    /// `fork_for_ephemeral` can mint child sinks tagged with the right
    /// [`pattern_core::spawn::SpawnSource`] variant. Headless and test
    /// sessions don't call this; their children inherit the parent's
    /// (NoOp) sink unchanged.
    pub fn install_spawn_sink_factory(
        &self,
        factory: Arc<dyn pattern_core::traits::SpawnSinkFactory>,
    ) {
        let mut guard = self
            .spawn_sink_factory
            .write()
            .expect("spawn_sink_factory lock poisoned");
        *guard = Some(factory);
    }

    /// Read the currently installed [`SpawnSinkFactory`], if any. Returns
    /// a clone of the `Arc` so the caller can use it without holding the
    /// internal lock.
    pub fn spawn_sink_factory(&self) -> Option<Arc<dyn pattern_core::traits::SpawnSinkFactory>> {
        let guard = self
            .spawn_sink_factory
            .read()
            .expect("spawn_sink_factory lock poisoned");
        guard.clone()
    }

    /// Replace the default [`NoOpSink`] with a caller-provided sink.
    /// Builder style; typical callers:
    /// `SessionContext::from_persona(...).with_turn_sink(sink)`.
    #[must_use]
    pub fn with_turn_sink(mut self, sink: Arc<dyn TurnSink>) -> Self {
        self.turn_sink = sink;
        self
    }

    /// Replace the checkpoint log handle and turn counter with externally
    /// owned ones. Used by [`TidepoolSession::open`] so the handler path
    /// records into the same log the session exposes via
    /// [`TidepoolSession::checkpoint_log`].
    pub(crate) fn with_checkpoint_log(
        mut self,
        log: Arc<std::sync::Mutex<CheckpointLog>>,
        turn: Arc<AtomicU64>,
    ) -> Self {
        self.checkpoint_log = log;
        self.current_turn = turn;
        self
    }

    /// Agent id this session runs as.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Model identifier for provider completion requests.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Provider adapter kind derived from `model_id` via
    /// `genai::adapter::AdapterKind::from_model`. genai's routing is the
    /// single source of truth for which protocol each model speaks
    /// (`gpt-5*` / `codex*` → `OpenAIResp`, `claude*` → `Anthropic`, etc.);
    /// no Pattern-side catalog is maintained.
    ///
    /// Used by the compose path to pick provider-aware [`CacheProfile`]
    /// and [`ShaperCompatMode`] defaults. Falls back to `Anthropic` if
    /// genai can't classify the model (which would itself fail at the
    /// gateway — this is a defense-in-depth default, not a routing
    /// decision).
    pub fn provider_kind(&self) -> genai::adapter::AdapterKind {
        genai::adapter::AdapterKind::from_model(&self.model_id)
            .unwrap_or(genai::adapter::AdapterKind::Anthropic)
    }

    /// Per-turn budget snapshot.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Shared cancel-state handle. Handlers check
    /// [`CancelState::is_cancelled`] at entry; the watchdog flips the flag
    /// and the gate when escalating.
    pub fn cancel_state(&self) -> Arc<CancelState> {
        self.cancel_state.clone()
    }

    /// Memory store used by MemoryHandler. Returns the adapter, which
    /// implements `MemoryStore` via delegation. Cheap clone (Arc).
    pub fn memory_store(&self) -> Arc<dyn MemoryStore> {
        self.adapter.clone()
    }

    /// Memory store adapter. Handlers call `adapter().record_write(..)`
    /// after mutations to populate `TurnOutput.block_writes`.
    pub fn adapter(&self) -> &Arc<MemoryStoreAdapter> {
        &self.adapter
    }

    /// Shared checkpoint log handle. Handlers record exchanges here
    /// after a successful dispatch (see the module-private
    /// `record_exchange` helper).
    pub fn checkpoint_log(&self) -> Arc<std::sync::Mutex<CheckpointLog>> {
        self.checkpoint_log.clone()
    }

    /// Current turn number (monotonic; bumped by `run_turn` before each
    /// turn). Handlers read this when stamping recorded events.
    pub fn current_turn(&self) -> u64 {
        self.current_turn.load(Ordering::SeqCst)
    }

    /// Provider client handle for LLM completion calls.
    pub fn provider(&self) -> &Arc<dyn ProviderClient> {
        &self.provider
    }

    /// Constellation database handle. Used for message persistence,
    /// turn-history loading, and compaction.
    pub fn db(&self) -> &Arc<pattern_db::ConstellationDb> {
        &self.db
    }

    /// Optional embedding-queue sender. `Some` when the cache has an
    /// embedding pipeline configured; `None` for tests or configurations
    /// without an embedding provider. `persist_messages` in agent_loop
    /// uses this to dispatch per-message ReembedRequests for hybrid
    /// retrieval over conversation history.
    pub fn reembed_tx(
        &self,
    ) -> Option<&tokio::sync::mpsc::UnboundedSender<pattern_memory::subscriber::event::ReembedRequest>>
    {
        self.reembed_tx.as_ref()
    }

    /// Builder-style: attach the embedding-queue sender. Called by the
    /// session opener after both the cache and SessionContext exist —
    /// pulls `cache.reembed_tx().cloned()` into the context so handlers
    /// (and the message persistence path) can dispatch reembed events.
    pub fn with_reembed_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<pattern_memory::subscriber::event::ReembedRequest>,
    ) -> Self {
        self.reembed_tx = Some(tx);
        self
    }

    /// Full snapshot policy: block-selection filter + mid-batch delta
    /// behavior. Controls which blocks appear in
    /// `MessageAttachment::BatchOpeningSnapshot` and whether this turn's
    /// own tool writes trigger mid-batch delta attachments.
    pub fn snapshot_policy(&self) -> &pattern_core::types::message::SnapshotPolicy {
        &self.snapshot_policy
    }

    /// Convenience accessor for the block-selection part of the snapshot
    /// policy. Equivalent to `snapshot_policy().selection`. Minimises
    /// call-site churn for code that only needs the selection filter.
    pub fn snapshot_selection(&self) -> &pattern_core::types::message::SnapshotSelection {
        &self.snapshot_policy.selection
    }

    /// Per-persona context policy (compression, gate floors, snapshot).
    /// Consumed by `crate::compaction::maybe_compact` before each wire
    /// turn in `drive_step`.
    pub fn context_policy(&self) -> &pattern_core::types::snapshot::ContextPolicy {
        &self.context_policy
    }

    /// Session diagnostics. Accumulated during construction; read by
    /// the `Pattern.Diagnostics` handler.
    pub fn diagnostics(
        &self,
    ) -> &Arc<std::sync::Mutex<Vec<crate::sdk::handlers::diagnostics::DiagnosticEvent>>> {
        &self.diagnostics
    }

    /// Scheme-dispatched router registry for message routing.
    pub fn router(&self) -> &Arc<RouterRegistry> {
        &self.router
    }

    /// Sync-to-async router bridge. Returns `None` if no router has
    /// been wired via [`Self::with_router`]. Handlers should prefer
    /// this over direct `router()` access — it is safe to call from
    /// a plain OS thread without a tokio runtime context.
    pub fn router_bridge(&self) -> Option<&RouterBridge> {
        self.router_bridge.as_ref()
    }

    /// Pending messages accumulated during the current turn.
    pub fn pending_messages(
        &self,
    ) -> &Arc<std::sync::Mutex<Vec<pattern_core::types::message::Message>>> {
        &self.pending_messages
    }

    /// Streaming event sink for this session. Handlers + the agent
    /// loop call `turn_sink().emit(event)` as events happen during a
    /// wire turn. Never `None` — sessions default to [`NoOpSink`].
    pub fn turn_sink(&self) -> &Arc<dyn TurnSink> {
        &self.turn_sink
    }

    /// Per-session spawn registry. Tracks live child handles, enforces
    /// the ephemeral concurrency limit, and cancels all children when
    /// the registry is dropped.
    pub fn spawn_registry(&self) -> &Arc<SpawnRegistry> {
        &self.spawn_registry
    }

    /// Replace the router registry and spawn the async router bridge.
    /// Used by session open (and tests) to inject a pre-configured
    /// registry — typically registered with a `CliRouter` or other
    /// scheme handlers before the session starts.
    ///
    /// Must be called from within a tokio runtime context (the bridge
    /// spawns a tokio task). After this call, handlers can use
    /// [`Self::router_bridge`] to dispatch messages from a plain OS
    /// thread without needing `Handle::current()`.
    #[allow(dead_code)]
    pub(crate) fn with_router(mut self, router: Arc<RouterRegistry>) -> Self {
        self.router_bridge = Some(RouterBridge::spawn(router.clone()));
        self.router = router;
        self
    }

    /// Shared [`AgentRegistry`] for inter-session routing, if wired.
    ///
    /// `None` for ephemeral children and test sessions that do not need
    /// peer-to-peer routing. `Some` for daemon-backed sessions where the
    /// `agent:` scheme router resolves recipients.
    pub fn agent_registry(&self) -> Option<&Arc<crate::agent_registry::AgentRegistry>> {
        self.agent_registry.as_ref()
    }

    /// Builder-style: attach the shared agent registry.
    ///
    /// Must be called before `Arc::new(ctx)`. The daemon wires a single
    /// shared `Arc<AgentRegistry>` to every session it opens; the
    /// `AgentRouter` holds the same `Arc` and looks up senders here.
    ///
    /// Registering the session with `Active` status is done separately
    /// via [`crate::agent_registry::RegistryGuard::register_active`] at
    /// the session-open site so the caller can control the persona-id
    /// used.
    #[must_use]
    pub fn with_agent_registry(
        mut self,
        registry: Arc<crate::agent_registry::AgentRegistry>,
    ) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Per-session wake-condition registry. `None` for sessions that
    /// were not wired with one — the `Pattern.Wake` handler surfaces
    /// `EffectError::Handler` in that case rather than silently
    /// dropping registrations.
    pub fn wake_registry(&self) -> Option<&Arc<crate::wake::WakeRegistry>> {
        self.wake_registry.as_ref()
    }

    /// Builder-style: attach a [`crate::wake::WakeRegistry`] to this
    /// session. Production callers wire one whose mailbox sender
    /// targets `self.mailbox.input_sender()`, with the
    /// `block_change_notifier` and `memory_store` builders applied
    /// when a `MemoryCache` is available.
    #[must_use]
    pub fn with_wake_registry(mut self, registry: Arc<crate::wake::WakeRegistry>) -> Self {
        self.wake_registry = Some(registry);
        self
    }

    /// Shared fronting set for read/write access from the
    /// `Pattern.Fronting` handler. `None` for sessions that have not
    /// been wired with one (test sessions, ephemeral children).
    ///
    /// Read-only accessor for the canonical `FrontingSet` lock.
    ///
    /// Returns `None` when no `FrontingCommitter` is wired (test sessions
    /// without fronting, or non-daemon paths). The `Pattern.Fronting`
    /// handler returns
    /// [`crate::sdk::handlers::fronting::FRONTING_NOT_WIRED_PREFIX`]-marked
    /// errors on the same `None` path.
    ///
    /// The lock returned here is the same lock the committer mutates —
    /// drift between read and write paths is structurally impossible.
    pub fn fronting_set(
        &self,
    ) -> Option<&Arc<std::sync::RwLock<pattern_core::fronting::FrontingSet>>> {
        self.fronting_committer.as_ref().map(|c| c.fronting_set())
    }

    /// Constellation registry handle, if wired.
    ///
    /// The `Pattern.Constellation` handler returns
    /// `EffectError::Handler` with a "registry not wired" prefix when this
    /// is `None` (test sessions, single-agent sessions).
    pub fn constellation_registry(&self) -> Option<&Arc<dyn pattern_core::ConstellationRegistry>> {
        self.constellation_registry.as_ref()
    }

    /// Builder-style: attach a `ConstellationRegistry` to this session.
    ///
    /// Daemon callers wire the per-project registry (typically
    /// `ConstellationRegistryDb`) here so agent programs can read persona
    /// records via the `Pattern.Constellation` SDK.
    #[must_use]
    pub fn with_constellation_registry(
        mut self,
        registry: Arc<dyn pattern_core::ConstellationRegistry>,
    ) -> Self {
        self.constellation_registry = Some(registry);
        self
    }

    /// Synchronous fronting committer, if wired.
    pub fn fronting_committer(
        &self,
    ) -> Option<&Arc<dyn crate::sdk::handlers::fronting::FrontingCommitter>> {
        self.fronting_committer.as_ref()
    }

    /// Builder-style: attach a `FrontingCommitter` so SDK-driven
    /// `Pattern.Fronting.Set` / `Route` / `Clear` mutations go through the
    /// daemon's three-phase commit (snapshot → mutate → persist → fan-out).
    #[must_use]
    pub fn with_fronting_committer(
        mut self,
        committer: Arc<dyn crate::sdk::handlers::fronting::FrontingCommitter>,
    ) -> Self {
        self.fronting_committer = Some(committer);
        self
    }
}

/// Optional registries passed to [`TidepoolSession::open_with_agent_loop`] to
/// wire inter-session routing and wake-condition support.
///
/// All fields default to `None`; callers that need them pass `Some(...)`.
/// The `WakeRegistry` is built inside `open_with_agent_loop` from the session's
/// own mailbox sender — supply `wake_registry_extras` instead of a pre-built
/// registry.
///
/// Daemon sessions pass a `SessionRegistries` with all three wired; test and
/// ephemeral-child sessions typically pass `None` for all fields (or just skip
/// the parameter by using `open_with_agent_loop` which accepts this struct as
/// `Option<SessionRegistries>`).
pub struct SessionRegistries {
    /// Shared agent registry for `agent:` scheme routing. Sessions opened with
    /// this registry register themselves at `Active` status so peers can route
    /// messages to them. Pass `None` for test / ephemeral sessions.
    pub agent_registry: Option<Arc<crate::agent_registry::AgentRegistry>>,
    /// Pre-built `RouterRegistry` to wire into the session. Callers that need
    /// both `agent:` and `cli:` routing build the registry and pass it here.
    /// `None` leaves the session's default empty registry in place.
    pub router_registry: Option<Arc<RouterRegistry>>,
    /// Extras used to build a `WakeRegistry` during session open. The registry
    /// itself is constructed inside `open_with_agent_loop` so it can be seeded
    /// with the session's own mailbox sender (not yet available at call-site).
    /// Pass `None` to leave wake support unwired.
    pub wake_registry_extras: Option<WakeRegistryExtras>,
    /// Optional shared `PortRegistryImpl`. When set, the session's
    /// `Pattern.Port` handler dispatches against this registry; agents
    /// import port libraries (e.g. `Pattern.Http`) that the registry
    /// materializes into the session's port-lib tempdir. `None` leaves
    /// the Port effect unavailable and the SDK preamble filters it out
    /// of the agent's effect row (so attempts to use Port at agent
    /// level fail at compile time, not at dispatch).
    ///
    /// v3-sandbox-io Phase 4-5: the daemon builds the registry via
    /// `PortRegistryImpl::with_runtime_ports(handle)` so `HttpPort`
    /// (and any future runtime-provided ports) are always registered.
    pub port_registry: Option<Arc<crate::port_registry::PortRegistryImpl>>,
    /// Optional `FilePolicy` for `Pattern.File` access control. When
    /// set, a `FileManager` is constructed and wired into the session
    /// before the eval worker spawns. `None` leaves the File effect
    /// unwired (sessions surface "no file manager configured" — used
    /// for non-mount test fixtures only).
    ///
    /// v3-sandbox-io Phase 5 safe-default contract (daemon): every
    /// daemon-mounted session passes `Some(policy)` even when the
    /// `.pattern.kdl` `file_policy {}` block is empty (default-deny
    /// via the policy module's "no matching rule") or malformed
    /// (logged at error level + falls back to default-deny).
    pub file_policy: Option<crate::file_manager::FilePolicy>,
    /// Optional `FrontingCommitter`. The committer owns the canonical
    /// `Arc<RwLock<FrontingSet>>` AND drives the synchronous commit path
    /// for SDK-driven `Pattern.Fronting` mutations.
    ///
    /// Daemon sessions wire a `DaemonFrontingCommitter` (three-phase commit
    /// plus DB persist plus `FrontingChanged` fan-out). Test sessions wire an
    /// `InMemoryFrontingCommitter` (no-op persist, no event emission). When
    /// `None`, the `Pattern.Fronting` effect is unwired entirely.
    ///
    /// v3-multi-agent Phase 6 T5b. Replaces the prior split between
    /// `fronting_set` and `fronting_committer` — bundling them eliminates
    /// the possibility of read/write-lock drift.
    pub fronting_committer: Option<Arc<dyn crate::sdk::handlers::fronting::FrontingCommitter>>,
    /// Optional constellation persona registry (Phase 6). Wired onto the
    /// `SessionContext` so the `Pattern.Constellation` SDK and sibling
    /// auto-registration both see the same per-mount handle.
    pub constellation_registry: Option<Arc<dyn pattern_core::ConstellationRegistry>>,
    /// Optional sibling persona resolver. When `None`, the session uses the
    /// default `UnconfiguredSiblingResolver` and every
    /// `ctx.spawn.sibling(Existing(id))` call fails with `PersonaNotFound`.
    /// Production daemons should pass
    /// `ConstellationSiblingResolver::new(constellation_registry)`
    /// so siblings can be resolved against the persona registry.
    pub sibling_resolver: Option<Arc<dyn crate::spawn::sibling::SiblingPersonaResolver>>,
    /// Optional plugin registry. When set, plugins are enabled at session open
    /// and their hook subscriptions are wired to the session's HookBus.
    pub plugin_registry: Option<Arc<crate::plugin::registry::PluginRegistry>>,
    /// Optional daemon-shared plugin route table. When set alongside
    /// `plugin_registry`, session-open walks the registry's routable plugins
    /// and registers their (pubkey → session_id) entries so the
    /// SessionRoutingProtocolHandler can route incoming OOP plugin connections
    /// to this session. None leaves OOP plugins unreachable for this session.
    pub plugin_routes: Option<Arc<pattern_core::plugin::auth::PluginRouteTable>>,
    /// Optional daemon-shared session-routing handler. Sessions register their
    /// per-session host handler into this at open so plugin dials get dispatched
    /// to the right session. None leaves OOP plugin host-callbacks disabled.
    pub plugin_routing_handler: Option<Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>>,
    pub plugin_memory_sync_handler: Option<Arc<pattern_core::plugin::auth::SessionRoutingProtocolHandler>>,
    /// Optional daemon iroh endpoint. Required to spawn native (OOP) plugins
    /// at session-open. `None` disables native plugin spawn (CC plugins work).
    pub daemon_endpoint: Option<iroh::Endpoint>,
    /// Optional embedding-queue sender. Daemon callers pull this from
    /// `cache.reembed_tx().cloned()` and pass it here so message persistence
    /// in agent_loop dispatches per-message ReembedRequests for hybrid
    /// retrieval. None for tests / configurations without an embedding provider.
    pub reembed_tx: Option<tokio::sync::mpsc::UnboundedSender<pattern_memory::subscriber::event::ReembedRequest>>,
}

/// Extras required to construct a [`crate::wake::WakeRegistry`] inside
/// [`TidepoolSession::open_with_agent_loop`].
///
/// The `WakeRegistry` itself is built from the session's mailbox sender, which
/// is only available after `SessionContext::from_persona`. Callers supply
/// these extras; the open path constructs the registry internally.
pub struct WakeRegistryExtras {
    /// Optional block-change notifier for [`crate::wake::WakeCondition::BlockChanged`]
    /// and [`crate::wake::WakeCondition::TaskDependencyResolved`]. Production
    /// callers pass `cache.block_change_notifier().clone()`.
    pub block_change_notifier: Option<pattern_memory::subscriber::BlockChangeNotifier>,
    /// Optional memory store for `TaskDependencyResolved` evaluators. Production
    /// callers pass `cx.user().memory_store()` or the mounted store.
    pub memory_store: Option<Arc<dyn MemoryStore>>,
    /// Optional ConstellationDb for wake persistence. When set, the wake
    /// registry will mirror register/unregister operations to the
    /// `wake_registrations` table and call `restore_for_agent` at session
    /// open so wakes survive daemon restarts.
    pub persistence_db: Option<Arc<pattern_db::ConstellationDb>>,
}

/// A running session: owns the handler bundle, eval worker, and checkpoint log.
///
/// Open via [`TidepoolSession::open_with_agent_loop`] and drive turns with
/// [`TidepoolSession::step_with_agent_loop`]. The `Session` trait's `step`
/// method delegates to `step_with_agent_loop`; callers should prefer the
/// typed method directly for clarity.
pub struct TidepoolSession {
    ctx: Arc<SessionContext>,
    session_id: String,
    checkpoint_log: Arc<std::sync::Mutex<CheckpointLog>>,
    /// Shared DisplayHandler so callers (CLI, tests) can register
    /// subscribers after `open`.
    display_handle: DisplayHandler,
    /// In-memory active turn history + cached archive-summary head.
    /// Populated on session open via `TurnHistory::load` (when a DB is
    /// available) or `TurnHistory::empty` (tests). `drive_step` records
    /// each completed turn here; compaction strategies consume the oldest
    /// entries.
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
    /// Long-lived Haskell eval worker. Spawned by
    /// [`TidepoolSession::open_with_agent_loop`]. Required by
    /// [`TidepoolSession::step_with_agent_loop`].
    eval_worker: Option<Arc<EvalWorker>>,
    /// Shared Haskell preamble: GADT declarations + effect-row alias +
    /// helpers assembled once at session open from
    /// [`crate::sdk::bundle::canonical_effect_decls`]. Passed verbatim
    /// to every [`EvalWorker::dispatch`] call.
    ///
    /// Stored as `Arc<str>` so the per-session mailbox task (Phase 4
    /// T3) can hold its own clone for drive_step calls without
    /// duplicating the (~6 KB) preamble buffer.
    preamble: Option<Arc<str>>,
    /// Per-session task tracker for spawned async machinery (Phase 4
    /// T3 mailbox-drain task; future per-session tasks slot in here).
    /// `tokio::task::JoinSet::Drop` aborts every tracked task when
    /// `TidepoolSession` is dropped, so the runtime never leaks
    /// detached tasks across session lifetimes — without needing a
    /// custom `Drop` impl on `TidepoolSession` (which would conflict
    /// with the `Arc::try_unwrap(session.ctx)` move during open).
    tasks: tokio::task::JoinSet<()>,
    /// Session-latched cache profile. Consumed by the composer
    /// pipeline inside [`crate::agent_loop::drive_step`] to place
    /// segment-1/2/3 `cache_control` markers with the configured
    /// TTLs. Latched at open-time to prevent mid-session TTL flips
    /// (which cause ~20K-token cache busts on Anthropic's
    /// subscription tier). Default:
    /// [`CacheProfile::default_anthropic_subscriber`] — all-1h per
    /// the research note in `docs/notes/2026-04-18-cache-ttl-research.md`.
    cache_profile: pattern_provider::compose::CacheProfile,
    /// RAII guard that unregisters this session from the
    /// [`AgentRegistry`](crate::agent_registry::AgentRegistry) when the
    /// session is dropped.
    ///
    /// `None` for sessions opened without an agent registry (e.g. test
    /// sessions, ephemeral children). `Some` when the daemon wires a
    /// registry via [`SessionContext::with_agent_registry`] and the session
    /// is registered at open time.
    ///
    /// The `JoinSet::Drop` cleans up async tasks; this guard handles the
    /// synchronous registry unregistration. We hold it here rather than
    /// on `SessionContext` because `TidepoolSession` owns the session
    /// lifecycle — the guard fires on `TidepoolSession::drop`, not on
    /// `SessionContext::drop` (which may be shared via `Arc`).
    _registry_guard: Option<crate::agent_registry::RegistryGuard>,
    /// Per-session tempdir holding materialized port-library Haskell
    /// modules. Each registered port whose [`pattern_core::traits::Port::library`]
    /// returns `Some` writes its source here at session open under the
    /// path implied by its `module X.Y.Z where` declaration; the
    /// tempdir's path is added to the GHC include path so agent code
    /// can `import qualified Pattern.Http as Http`. `None` for sessions
    /// opened without a port registry.
    ///
    /// Load-bearing despite never being read after initialisation:
    /// `tempfile::TempDir` removes the directory from disk on drop, so
    /// the field's lifetime IS the directory's lifetime. Do not remove
    /// or rename.
    _port_lib_tempdir: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for TidepoolSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TidepoolSession")
            .field("session_id", &self.session_id)
            .field("agent_id", &self.ctx.agent_id())
            .field("eval_worker", &self.eval_worker.is_some())
            .finish_non_exhaustive()
    }
}

impl TidepoolSession {
    /// Return a clone of the session's DisplayHandler (Arc-shared
    /// subscriber list). Subscribers registered on this handle also see
    /// events produced by the bundle's internal clone.
    pub fn display(&self) -> DisplayHandler {
        self.display_handle.clone()
    }

    /// Session-scoped id (new UUID minted at open).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Agent id this session runs as (delegated from [`SessionContext`]).
    pub fn agent_id(&self) -> &str {
        self.ctx.agent_id()
    }

    /// Accessor for the checkpoint log — exposed so tests can assert on
    /// recorded events.
    pub fn checkpoint_log(&self) -> Arc<std::sync::Mutex<CheckpointLog>> {
        self.checkpoint_log.clone()
    }

    /// The session's shared cancel state. Callers can call
    /// [`CancelState::request_cancel`] to soft-cancel a running step.
    pub fn cancel_state(&self) -> Arc<CancelState> {
        self.ctx.cancel_state()
    }

    /// The Haskell preamble built for this session at open-time.
    /// Returns `None` for sessions opened via [`Self::open`] (no eval
    /// worker, no preamble); `Some(_)` for sessions opened via
    /// [`Self::open_with_agent_loop`].
    ///
    /// Capability-scoped open paths (Phase 1) build the preamble via
    /// [`crate::sdk::preamble::build_for`], so the returned string
    /// already has effects absent from the session's capability set
    /// stripped from imports and the `type M` row.
    pub fn preamble(&self) -> Option<&str> {
        self.preamble.as_deref()
    }

    /// Shared handle to the session's [`SessionContext`]. Tests can
    /// borrow this as the `user` argument to
    /// `tidepool_runtime::compile_and_run` when exercising the
    /// agent-loop substrate without driving a full step.
    pub fn context(&self) -> Arc<SessionContext> {
        self.ctx.clone()
    }

    /// Open a minimal session: initialise context, checkpoint log, and handler
    /// display handle but do NOT spawn an eval worker. Used internally by
    /// [`Self::open_with_agent_loop`] and by `TidepoolRuntime::open_session`
    /// for checkpoint-restore use-cases that don't need the eval worker.
    ///
    /// Runs preflight so missing tidepool-extract produces an actionable error
    /// before any work happens. The session returned here is not wired for
    /// `step_with_agent_loop` — call `open_with_agent_loop` for that.
    /// Construct a base `TidepoolSession` with a fully-wired
    /// `SessionContext` (port registry included) but without the
    /// agent-loop machinery (eval worker, mailbox drain task, port-lib
    /// materialization, FileManager).
    ///
    /// Most production callers should use [`Self::open_with_agent_loop`]
    /// — it sets up the rest of the session lifecycle and drives the
    /// agent through `step_with_agent_loop`. The basic `open` is
    /// retained for tests and the rare caller that wants to run a
    /// session without an eval worker (e.g. for snapshot/restore
    /// inspection).
    ///
    /// `port_registry` is wired into the `SessionContext` here so the
    /// `Pattern.Port` SDK row is visible at the agent level. Pass the
    /// runtime's port registry (typically built via
    /// `PortRegistryImpl::with_runtime_ports`).
    ///
    /// **Deprecation note (post-merge follow-up):** the long-term
    /// goal is to fold this into [`Self::open_with_agent_loop`] so
    /// callers stop having to choose between two near-identical
    /// constructors. Until then, treat this as the "minimal" path and
    /// `open_with_agent_loop` as the "production" path.
    pub fn open(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
        port_registry: Arc<crate::port_registry::PortRegistryImpl>,
    ) -> Result<Self, RuntimeError> {
        crate::preflight::check()?;
        let _ = sdk; // sdk.resolve() is deferred to open_with_agent_loop
        let session_id = pattern_core::types::ids::new_id().to_string();
        // Share the checkpoint log + current-turn counter between the
        // session and the handler-facing SessionContext so handlers'
        // `record_exchange` calls land in the same log the session
        // publishes via `TidepoolSession::checkpoint_log`.
        let checkpoint_log = Arc::new(std::sync::Mutex::new(CheckpointLog::new()));
        // The turn counter is owned by `ctx` via `with_checkpoint_log`; handlers
        // read it through `SessionContext::current_turn()`. Nothing on
        // `TidepoolSession` reads it directly in the agent-loop path.
        let current_turn = Arc::new(AtomicU64::new(0));
        let ctx = Arc::new(
            SessionContext::from_persona(
                &persona,
                memory_store,
                provider.clone(),
                db,
                tokio_handle,
            )
            .with_checkpoint_log(checkpoint_log.clone(), current_turn)
            .with_port_registry(port_registry)
            .with_session_id(session_id.clone()),
        );

        let display = DisplayHandler::new();

        Ok(Self {
            ctx,
            session_id,
            checkpoint_log,
            display_handle: display,
            turn_history: Arc::new(std::sync::Mutex::new(TurnHistory::empty())),
            eval_worker: None,
            preamble: None,
            tasks: tokio::task::JoinSet::new(),
            // Provider-aware: Anthropic gets the extended-TTL subscriber
            // profile; OpenAI/Gemini/others get the no-cache profile (cache
            // markers would never reach the wire for them anyway because the
            // NoOpShaper flattens `system_blocks` into a plain `chat.system`
            // string before genai's adapter serializes the request).
            //
            // Derive via `AdapterKind::from_model(model_id)` rather than
            // `persona.model.choice.provider` because genai's routing IS
            // the catalog — if the persona declares a model but mis-tags
            // the provider, the gateway would route by `from_model`
            // anyway, so the cache profile must match that decision.
            cache_profile: pattern_provider::compose::CacheProfile::default_for(
                genai::adapter::AdapterKind::from_model(
                    &persona.model.choice.model_id,
                )
                .unwrap_or(genai::adapter::AdapterKind::Anthropic),
            ),
            _registry_guard: None,
            _port_lib_tempdir: None,
        })
    }

    /// Load archived summary-head from the constellation DB for the
    /// composer's segment 2 "earlier context" prepend. Call after `open`
    /// and before the first `step` when a DB handle is available.
    /// No-op skip is safe: the composer will simply have no summary head.
    pub async fn load_turn_history(
        &self,
        db: &pattern_db::ConstellationDb,
    ) -> Result<(), pattern_db::error::DbError> {
        let history = TurnHistory::load(db, self.ctx.agent_id()).await?;
        if let Ok(mut guard) = self.turn_history.lock() {
            *guard = history;
        }
        Ok(())
    }

    /// Access the session's turn history. Exposed for the context
    /// composer and compaction strategies.
    pub fn turn_history(&self) -> Arc<std::sync::Mutex<TurnHistory>> {
        self.turn_history.clone()
    }

    /// Open a session wired for the agent-loop wire-turn-loop driver.
    ///
    /// - Runs preflight so missing tidepool-extract produces an actionable error.
    /// - Initialises context with the caller-supplied `turn_sink`.
    /// - Builds the shared Haskell preamble from
    ///   [`crate::sdk::bundle::canonical_effect_decls`].
    /// - Spawns an [`EvalWorker`] with an include path of `[sdk.resolve()]`
    ///   plus the optional `prelude_dir`.
    ///
    /// Use [`Self::step_with_agent_loop`] to drive turns on sessions
    /// opened via this constructor.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_with_agent_loop(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: Arc<dyn MemoryStore>,
        provider: Arc<dyn ProviderClient>,
        db: Arc<pattern_db::ConstellationDb>,
        tokio_handle: tokio::runtime::Handle,
        turn_sink: Arc<dyn TurnSink>,
        prelude_dir: Option<PathBuf>,
        mount_path: Option<PathBuf>,
        capabilities: Option<pattern_core::CapabilitySet>,
        registries: Option<SessionRegistries>,
    ) -> Result<Self, RuntimeError> {
        // Capture persona-scoped state we'll seed into the store after the
        // session is constructed. We consume `persona` via `Self::open`
        // below; extracting these now keeps the rest of the open path
        // simple.
        let agent_id_for_seed = persona.agent_id.to_string();
        let memory_blocks_for_seed = persona.memory_blocks.clone();
        let persona_mcp_configs = persona.mcp_servers.clone();
        let store_for_seed = memory_store.clone();

        // Capture the alias mapping (display name → agent_id) for later
        // registration with the AgentRegistry, when one is wired. Empty
        // when name == agent_id.
        let persona_alias_for_registry: Option<smol_str::SmolStr> =
            (persona.name != persona.agent_id).then(|| persona.name.clone());

        // The basic `open` path now requires a port registry so the
        // `SessionContext` is fully wired before the eval-worker /
        // mailbox / FileManager add-ons land here. Pull it out of the
        // caller's `registries` if present; otherwise build a fresh
        // empty registry on the caller's tokio handle. The empty
        // registry has no ports, so the SDK preamble filters
        // `Pattern.Port` out of the agent's effect row — same fail-
        // closed behaviour as before.
        let port_registry_for_open = registries
            .as_ref()
            .and_then(|r| r.port_registry.clone())
            .unwrap_or_else(|| {
                Arc::new(crate::port_registry::PortRegistryImpl::new(&tokio_handle))
            });

        // Initialise the base session (preflight, context, checkpoint log,
        // port registry wired into ctx).
        let mut session = Self::open(
            persona,
            sdk,
            memory_store,
            provider,
            db,
            tokio_handle,
            port_registry_for_open,
        )?;

        // Seed persona-declared memory blocks into the store. Blocks that
        // already exist (e.g. restored from a persistent DB on re-spawn)
        // are left as-is — persona declares INITIAL content; live state
        // wins.
        seed_persona_memory_blocks(
            &*store_for_seed,
            &agent_id_for_seed,
            &memory_blocks_for_seed,
        )?;

        // Replace the NoOpSink on the freshly constructed SessionContext.
        // We have exclusive ownership of `session` here (just returned
        // from open), so Arc::try_unwrap on ctx will always succeed.
        let ctx_owned =
            Arc::try_unwrap(session.ctx).expect("ctx has no other clones immediately after open()");
        // Spawn a permission bridge over this session's broker. Must
        // happen in async context (bridge spawns a tokio task).
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(
            ctx_owned.permission_broker().clone(),
        ));
        let ctx_with_sink_base = ctx_owned
            .with_turn_sink(turn_sink.clone())
            .with_capabilities(capabilities.clone())
            .with_permission_bridge(bridge.clone());

        // Wire inter-session registries if the caller supplied them (daemon
        // path). Test and ephemeral-child sessions pass `None`.
        let ctx_with_sink = if let Some(regs) = registries {
            // Wire AgentRegistry (agent: scheme routing).
            let ctx = if let Some(agent_reg) = regs.agent_registry {
                ctx_with_sink_base.with_agent_registry(agent_reg)
            } else {
                ctx_with_sink_base
            };

            // Wire RouterRegistry (scheme-dispatched message routing).
            // with_router also spawns the RouterBridge task.
            let ctx = if let Some(router_reg) = regs.router_registry {
                ctx.with_router(router_reg)
            } else {
                ctx
            };

            // Build and wire WakeRegistry from the session's own mailbox sender.
            // Thread the tokio_handle so evaluator tasks can be spawned from the
            // eval-worker OS thread (which has no ambient runtime context).
            let ctx = if let Some(extras) = regs.wake_registry_extras {
                let mailbox = ctx.mailbox().clone();
                let tokio_handle = ctx.tokio_handle().clone();
                let default_scope_for_wake = ctx.default_scope().clone();
                let mut wake_reg = crate::wake::WakeRegistry::new(mailbox, tokio_handle)
                    .with_default_scope(default_scope_for_wake);
                if let Some(notifier) = extras.block_change_notifier {
                    wake_reg = wake_reg.with_block_change_notifier(notifier);
                }
                if let Some(store) = extras.memory_store {
                    wake_reg = wake_reg.with_memory_store(store);
                }
                let restore_db = extras.persistence_db.clone();
                if let Some(db) = extras.persistence_db {
                    wake_reg = wake_reg.with_persistence(db);
                }
                let wake_reg = Arc::new(wake_reg);
                // Replay persisted wakes for this agent. Best-effort: a
                // failure to restore (or zero rows) doesn't fail session open.
                if restore_db.is_some() {
                    let restored = wake_reg.restore_for_agent(&smol_str::SmolStr::from(ctx.agent_id()));
                    if restored > 0 {
                        tracing::info!(
                            target: "pattern_runtime::session",
                            agent_id = %ctx.agent_id(),
                            count = restored,
                            "restored persisted wakes at session open"
                        );
                    }
                }
                ctx.with_wake_registry(wake_reg)
            } else {
                ctx
            };

            // Wire FrontingCommitter (Phase 6 T5b). The committer owns the
            // canonical `FrontingSet` lock — read access via the handler's
            // `Current` constructor and write access via `commit_sync` both
            // route through the same trait object, so no drift between
            // them is structurally possible.
            let ctx = if let Some(committer) = regs.fronting_committer {
                ctx.with_fronting_committer(committer)
            } else {
                ctx
            };

            // Wire ConstellationRegistry (Phase 6). Used by the
            // `Pattern.Constellation` SDK handler AND by sibling auto-
            // registration in `spawn_sibling_*`.
            let ctx = if let Some(reg) = regs.constellation_registry {
                ctx.with_constellation_registry(reg)
            } else {
                ctx
            };

            // Wire SiblingPersonaResolver (production: the daemon passes a
            // `ConstellationSiblingResolver` backed by the per-mount registry;
            // tests pass `StubSiblingResolver`; if `None`, the default
            // `UnconfiguredSiblingResolver` makes every sibling lookup fail).
            let ctx = if let Some(resolver) = regs.sibling_resolver {
                ctx.with_sibling_resolver(resolver)
            } else {
                ctx
            };

            // Wire embedding-queue sender. Daemon callers pass
            // `cache.reembed_tx().cloned()` so message persistence in
            // agent_loop dispatches per-message ReembedRequests for hybrid
            // retrieval over conversation history. None for tests.
            let ctx = if let Some(tx) = regs.reembed_tx {
                ctx.with_reembed_tx(tx)
            } else {
                ctx
            };

            // Wire shared port registry (v3-sandbox-io Phase 4-5).
            // The daemon builds the registry once via
            // `PortRegistryImpl::with_runtime_ports` and shares it
            // across every session opened against the mount.
            let ctx = if let Some(port_reg) = regs.port_registry {
                ctx.with_port_registry(port_reg)
            } else {
                ctx
            };

            // Construct the per-session FileManager when the caller
            // supplied a FilePolicy (v3-sandbox-io Phase 5). FM hooks
            // into `async_reminder_queue` (so external-edit watchers
            // can splice FileEdit attachments) and the permission
            // bridge (so config-KDL writes escalate correctly). FM
            // construction MUST happen before the eval worker spawns
            // so the worker observes the wired session context.
            // Wire PluginRegistry if supplied.
            let ctx = if let Some(plugin_reg) = regs.plugin_registry {
                ctx.with_plugin_registry(plugin_reg)
            } else {
                ctx
            };

            // Wire daemon-shared plugin route table if supplied.
            let ctx = if let Some(routes) = regs.plugin_routes {
                ctx.with_plugin_routes(routes)
            } else {
                ctx
            };

            // Wire daemon-shared session-routing handler if supplied. Sessions
            // register their per-session host handler with this at open + unregister on drop.
            let ctx = if let Some(h) = regs.plugin_routing_handler {
                ctx.with_plugin_routing_handler(h)
            } else {
                ctx
            };

            // Wire daemon iroh endpoint for OOP plugin spawn at session-open.
            let ctx = if let Some(endpoint) = regs.daemon_endpoint {
                ctx.with_daemon_endpoint(endpoint)
            } else {
                ctx
            };

            if let Some(policy) = regs.file_policy {
                let fm_caps = Arc::new(
                    capabilities
                        .clone()
                        .unwrap_or_else(pattern_core::CapabilitySet::all),
                );
                let fm = Arc::new(crate::file_manager::FileManager::new(
                    policy,
                    ctx.async_reminder_queue().clone(),
                    fm_caps,
                    bridge.clone(),
                    pattern_core::AgentId::from(agent_id_for_seed.as_str()),
                ));
                ctx.with_file_manager(fm)
            } else {
                ctx
            }
        } else {
            ctx_with_sink_base
        };
        // Wire MemoryScope if a mount config declares an isolation policy.
        // Must happen before Arc::new(ctx) so the scope wraps the store
        // before any other reference to ctx exists.
        let ctx_with_scope = if let Some(mount) = mount_path.as_deref() {
            let kdl_path = mount.join(".pattern.kdl");
            match pattern_memory::config::load_mount_config(&kdl_path) {
                Ok(mount_config) => {
                    // Resolve the policy; default to None on validation error
                    // (log the issue but don't abort session open).
                    let policy = match mount_config.isolate_from_persona.resolve() {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "failed to resolve isolate_from_persona policy; \
                                 defaulting to IsolatePolicy::None"
                            );
                            pattern_core::types::memory_types::IsolatePolicy::None
                        }
                    };
                    let binding = pattern_memory::scope::ScopeBinding::with_project(
                        agent_id_for_seed.clone(),
                        mount_config.project.name.clone(),
                        policy,
                    );
                    ctx_with_sink.with_scope_binding(binding)
                }
                Err(e) => {
                    // Mount config missing or malformed. Log a warning and
                    // proceed without a scope: the session is still valid,
                    // just without project isolation.
                    tracing::warn!(
                        path = %kdl_path.display(),
                        error = %e,
                        "could not load .pattern.kdl for scope wiring; \
                         proceeding without MemoryScope"
                    );
                    ctx_with_sink
                }
            }
        } else {
            ctx_with_sink
        };

        // Build the shared preamble once per session, scoped to the
        // caller-supplied capability set. `None` keeps the full canonical
        // row — back-compat for sessions that pre-date capability scoping.
        let preamble = match capabilities.as_ref() {
            Some(caps) => crate::sdk::preamble::build_for(caps),
            None => crate::sdk::preamble::build(&crate::sdk::bundle::canonical_effect_decls()),
        };

        // Build include paths: SDK dir only. Pattern's haskell/Pattern/
        // tree now includes both the effect GADTs AND the prelude
        // substitute. No separate "tidepool prelude dir" is needed.
        //
        // The `prelude_dir` parameter is honoured for back-compat —
        // callers who still pass one get it appended, but it's
        // optional.
        let sdk_dir = sdk.resolve()?;
        let mut include_paths = vec![sdk_dir];
        if let Some(dir) = prelude_dir {
            include_paths.push(dir);
        }

        // Extend include path with `<mount>/lib/` if present.
        // Approach A: probe-compile each module individually via
        // compile_haskell. See `sdk::lib_modules` for details.
        let lib_failures = if let Some(mount) = mount_path.as_deref() {
            let lib_validation =
                crate::sdk::lib_modules::validate_and_resolve(mount, &include_paths);
            include_paths.extend(lib_validation.successful_paths);
            lib_validation.failures
        } else {
            Vec::new()
        };

        // Materialize port libraries (v3-sandbox-io Phase 5).
        //
        // Each registered port whose `Port::library()` returns `Some`
        // ships Haskell wrapper code that the agent needs in scope.
        // We write each library into a per-session tempdir at the path
        // implied by its `module X.Y where` declaration (e.g.
        // `Pattern.Http` → `Pattern/Http.hs`), then add the tempdir to
        // the GHC include path so agent code can
        // `import qualified Pattern.Http as Http`. The tempdir is held
        // on the session for RAII cleanup on drop.
        //
        // This is the same delivery path a third-party plugin's port
        // library uses — no special-case for runtime-provided ports.
        let port_lib_tempdir = if let Some(ref registry) = ctx_with_scope.port_registry {
            let libs = registry.port_libraries();
            if libs.is_empty() {
                None
            } else {
                let dir = tempfile::Builder::new()
                    .prefix("pattern-port-libs-")
                    .tempdir()
                    .map_err(|e| RuntimeError::PortLibrarySetupFailed {
                        port_id: "<tempdir>".to_string(),
                        op: "create-tempdir".to_string(),
                        cause: e.to_string(),
                    })?;
                for (port_id, src) in libs {
                    let pid = port_id.as_str().to_string();
                    let module_name = parse_module_name(&src).ok_or_else(|| {
                        RuntimeError::PortLibrarySetupFailed {
                            port_id: pid.clone(),
                            op: "parse-module-name".to_string(),
                            cause: "no `module X where` header in library source".to_string(),
                        }
                    })?;
                    let rel = module_name_to_path(&module_name);
                    let abs = dir.path().join(&rel);
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            RuntimeError::PortLibrarySetupFailed {
                                port_id: pid.clone(),
                                op: "create-parent-dir".to_string(),
                                cause: format!("{}: {e}", parent.display()),
                            }
                        })?;
                    }
                    std::fs::write(&abs, src).map_err(|e| {
                        RuntimeError::PortLibrarySetupFailed {
                            port_id: pid.clone(),
                            op: "write-source".to_string(),
                            cause: format!("{}: {e}", abs.display()),
                        }
                    })?;
                }
                include_paths.push(dir.path().to_path_buf());
                Some(dir)
            }
        } else {
            None
        };

        // Persist the resolved include path on SessionContext BEFORE the
        // final Arc-wrap so child-session forks (Phase 2 spawn) can
        // inherit them. Stash diagnostics from any lib-compile failures
        // here too, while we still have `&mut` access on the inner ctx.
        let mut ctx_with_paths = ctx_with_scope;
        ctx_with_paths.set_include_paths(Arc::new(include_paths.clone()));
        if !lib_failures.is_empty() {
            let mut diags = ctx_with_paths
                .diagnostics
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            diags.extend(
                lib_failures
                    .into_iter()
                    .map(crate::sdk::handlers::diagnostics::DiagnosticEvent::from),
            );
        }

        session.ctx = Arc::new(ctx_with_paths);

        // Wire the CustomEvaluator onto the WakeRegistry now that
        // include_paths and the Arc<SessionContext> are available.
        // Phase 7 Task 6: custom Haskell wake conditions.
        if let Some(wake_reg) = session.ctx.wake_registry() {
            let evaluator = crate::wake::custom::CustomEvaluator::new(
                session.ctx.mailbox().clone(),
                include_paths.clone(),
                session.ctx.tokio_handle().clone(),
                session.ctx.clone(),
            );
            wake_reg.set_custom_evaluator(Arc::new(evaluator));
        }

        // Enable loaded plugins (Phase 3). Each plugin's on_enable()
        // wires its hook subscriptions to the session's HookBus.
        // Uses block_in_place because we're inside a tokio runtime.
        if let Some(plugin_reg) = session.ctx.plugin_registry() {
            let mut plugins = plugin_reg.list();

            // OOP-spawn pass: for each native plugin (has pubkey, no connection yet),
            // spawn `OutOfProcessPluginConnection` using the daemon endpoint. This is
            // the integration wire that Phase 6 Task 5 left stubbed in `build_connection`:
            // the connection couldn't be built at registry-load time because the daemon
            // endpoint isn't available then; it has to happen here at session-open.
            if let Some(endpoint) = session.ctx.daemon_endpoint().cloned() {
                for lp in plugins.iter_mut() {
                    if lp.connection.is_some() { continue; }
                    let pubkey = match &lp.plugin_key {
                        Some(pattern_core::plugin::auth::PluginKey::Direct(pk)) => pk.clone(),
                        _ => continue,
                    };
                    // Binary lives at <plugin_root>/bin/<plugin_id>[.exe] per install spec.
                    let bin_name = if cfg!(windows) {
                        format!("{}.exe", lp.id)
                    } else {
                        lp.id.to_string()
                    };
                    let binary_path = lp.source_path.join("bin").join(&bin_name);
                    if !binary_path.exists() {
                        tracing::warn!(
                            plugin = %lp.id,
                            path = %binary_path.display(),
                            "native plugin binary not found; skipping OOP spawn",
                        );
                        continue;
                    }
                    // v0.1: pass empty user_config + all-capabilities. Per-install
                    // overlay + manifest-declared-effects narrowing land later.
                    let user_config = serde_json::Value::Null;
                    let effective_caps = pattern_core::CapabilitySet::all();
                    match crate::plugin::transport::OutOfProcessPluginConnection::spawn(
                        lp.id.clone(),
                        binary_path,
                        pubkey,
                        endpoint.clone(),
                        lp.source_path.clone(),
                        user_config,
                        effective_caps,
                    ).await {
                        Ok(conn) => {
                            tracing::info!(plugin = %lp.id, "OOP plugin spawned");
                            let conn_arc: Arc<dyn crate::plugin::transport::PluginConnection> =
                                Arc::new(conn);
                            // Mutate local copy so the rest of THIS loop sees the
                            // connection (declare_ports etc). Also write back to
                            // the registry so the Arc outlives session-open —
                            // WireBackedPort holds only a Weak to this Arc.
                            lp.connection = Some(conn_arc.clone());
                            plugin_reg.set_connection(&lp.id, conn_arc);
                        }
                        Err(e) => {
                            tracing::warn!(
                                plugin = %lp.id,
                                error = %e,
                                "OOP plugin spawn failed",
                            );
                        }
                    }
                }
            }

            // Populate the daemon-shared plugin route table with this session's
            // OOP-routable plugins. Each (pubkey, plugin_id) gets keyed by this
            // session's agent_id so the SessionRoutingProtocolHandler can dispatch
            // incoming connections to the right session. Drop side: unregister_session
            // in TidepoolSession::Drop (below). Collision = real bug (two sessions
            // claim same pubkey); for v1 we log + skip.
            if let Some(routes) = session.ctx.plugin_routes() {
                let session_id: smol_str::SmolStr = session.ctx.agent_id().to_string().into();
                for (plugin_id, pubkey) in plugin_reg.routable_pubkeys() {
                    if let Err(e) = routes.register(pubkey, plugin_id.clone(), session_id.clone()) {
                        tracing::warn!(
                            plugin = %plugin_id,
                            session = %session_id,
                            error = %e,
                            "plugin route registration failed",
                        );
                    } else {
                        tracing::debug!(
                            plugin = %plugin_id,
                            session = %session_id,
                            "plugin route registered",
                        );
                    }
                }
            }

            // Phase A.2b: register a per-session host_handler with the daemon-shared
            // routing handler. v1 the handler is a stub (returns Unimplemented for all
            // PluginHostProtocol variants); A.2c will thread real dispatch via HostApiContext.
            // Drop side: unregister in TidepoolSession::Drop (below).
            if let Some(routing_handler) = session.ctx.plugin_routing_handler() {
                use irpc::rpc::RemoteService;
                use pattern_core::plugin::protocol::PluginHostProtocol;
                let session_id: smol_str::SmolStr = session.ctx.agent_id().to_string().into();
                // Build the HostApiContext from the session's runtime registries.
                // agent_registry is Option<…>; skip handler registration if absent
                // (test paths without an AgentRegistry shouldn't register a handler
                // that can't dispatch HostSendMessage anyway).
                let host_ctx = session.ctx.agent_registry().map(|reg| {
                    crate::plugin::host_handler::HostApiContext {
                        memory_store: session.ctx.memory_store(),
                        agent_registry: Arc::clone(reg),
                        session_agent_id: pattern_core::AgentId::from(session.ctx.agent_id()),
                        default_scope: session.ctx.default_scope().clone(),
                        db: Arc::clone(session.ctx.db()),
                    }
                });
                let host_client = if let Some(ctx) = host_ctx {
                    Some(crate::plugin::host_handler::spawn(ctx))
                } else {
                    tracing::warn!(
                        session = %session_id,
                        "no agent_registry on session ctx — skipping per-session host_handler registration",
                    );
                    None
                };
                if let Some(host_client) = host_client
                    && let Some(host_local) = host_client.as_local() {
                    let host_proto = PluginHostProtocol::remote_handler(host_local);
                    routing_handler.register_handler(
                        session_id.clone(),
                        std::sync::Arc::new(irpc_iroh::IrohProtocol::new(host_proto)),
                    );
                    tracing::debug!(session = %session_id, "per-session host_handler registered");
                } else {
                    tracing::warn!(
                        session = %session_id,
                        "freshly-spawned host client unexpectedly remote — host_handler not registered",
                    );
                }
            }

            // Per-session memory_sync_handler registration. Mirrors the
            // host_handler block above but for the memory-sync ALPN bidi-stream
            // protocol. Builds a MemorySyncApiContext from the session's
            // memory_store + its observer (via trait method) + session ids.
            // Drop side unregisters in SessionContext::Drop.
            if let Some(routing_handler) = session.ctx.plugin_memory_sync_handler() {
                use irpc::rpc::RemoteService;
                use pattern_core::plugin::protocol::MemorySyncProtocol;
                let session_id: smol_str::SmolStr = session.ctx.agent_id().to_string().into();
                let store = session.ctx.memory_store();
                let observer_opt = store.observer().cloned();
                let memsync_client = if let Some(observer) = observer_opt {
                    let ctx = crate::plugin::memory_sync_handler::MemorySyncApiContext {
                        memory_store: store,
                        observer,
                        session_agent_id: pattern_core::AgentId::from(session.ctx.agent_id()),
                        default_scope: session.ctx.default_scope().clone(),
                    };
                    Some(crate::plugin::memory_sync_handler::spawn(ctx))
                } else {
                    tracing::warn!(
                        session = %session_id,
                        "memory store has no observer; skipping per-session memory_sync_handler registration",
                    );
                    None
                };
                if let Some(memsync_client) = memsync_client
                    && let Some(memsync_local) = memsync_client.as_local() {
                    let memsync_proto = MemorySyncProtocol::remote_handler(memsync_local);
                    routing_handler.register_handler(
                        session_id.clone(),
                        std::sync::Arc::new(irpc_iroh::IrohProtocol::new(memsync_proto)),
                    );
                    tracing::debug!(session = %session_id, "per-session memory_sync_handler registered");
                } else {
                    tracing::warn!(
                        session = %session_id,
                        "freshly-spawned memsync client unexpectedly remote — handler not registered",
                    );
                }
            }

            let hook_bus = session.ctx.hook_bus().clone();
            let mut pending_mcp_configs: Vec<(
                smol_str::SmolStr,
                Vec<pattern_core::mcp::McpServerConfig>,
            )> = Vec::new();
            for lp in &plugins {
                tracing::info!(plugin = %lp.id, has_ext = lp.connection.is_some(), "plugin enable: checking plugin");
                // Collect MCP configs for background loading (don't block session open).
                use crate::plugin::cc_adapter::mcp_config;
                let mcp_json = lp
                    .manifest
                    .mcp_servers
                    .first()
                    .and_then(|spec| match spec {
                        ComponentSpec::Inline(json) => Some(json.clone()),
                        _ => None,
                    })
                    .unwrap_or(serde_json::Value::Null);
                let configs = mcp_config::parse_mcp_servers(&lp.source_path, &mcp_json);
                if !configs.is_empty() {
                    pending_mcp_configs.push((lp.id.clone(), configs));
                }
                if let Some(ext) = &lp.connection {
                    let ctx = pattern_core::traits::plugin::PluginContext {
                        plugin_id: lp.id.clone(),
                        hook_bus: hook_bus.clone(),
                        plugin_root: lp.source_path.clone(),
                        mount_path: mount_path.clone(),
                        memory_store: Some(session.ctx.memory_store()),
                        scope: Some(session.ctx.default_scope().clone()),
                    };
                    if let Err(e) = ext.on_enable(&ctx).await {
                        tracing::warn!(
                            plugin = %lp.id,
                            error = %e,
                            "plugin on_enable failed"
                        );
                        continue;
                    }
                    // Register declared ports into the session's PortRegistry.
                    // In-process plugins provide Arc<dyn Port> directly via port_impls();
                    // out-of-process plugins get WireBackedPort proxies built from their
                    // WirePortDeclarations, which forward port_call/port_subscribe over the wire.
                    if let Some(port_registry) = session.ctx.port_registry() {
                        use pattern_core::traits::port_registry::PortRegistry as _;
                        match ext.declare_ports().await {
                            Ok(decls) => {
                                if let Some(impls) = ext.port_impls() {
                                    for port in impls {
                                        let port_id = port.id().clone();
                                        if let Err(e) = port_registry.register(port).await {
                                            tracing::warn!(
                                                plugin = %lp.id,
                                                port = %port_id,
                                                error = %e,
                                                "plugin in-process port registration failed",
                                            );
                                        }
                                    }
                                } else {
                                    let conn_weak = std::sync::Arc::downgrade(ext);
                                    for decl in decls {
                                        let port_id = decl.id.clone();
                                        let port: std::sync::Arc<dyn pattern_core::traits::port::Port> =
                                            std::sync::Arc::new(
                                                crate::plugin::wire_backed_port::WireBackedPort::new(
                                                    decl,
                                                    conn_weak.clone(),
                                                ),
                                            );
                                        if let Err(e) = port_registry.register(port).await {
                                            tracing::warn!(
                                                plugin = %lp.id,
                                                port = %port_id,
                                                error = %e,
                                                "plugin wire-backed port registration failed",
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => tracing::warn!(
                                plugin = %lp.id,
                                error = %e,
                                "plugin declare_ports failed",
                            ),
                        }
                    }

                    // OOP plugins can't subscribe to the daemon's hook bus directly;
                    // forward all notification-shape events over the wire. Plugin's
                    // on_event filters by tag in-process. v1: forward everything;
                    // future: plugin declares interests via HostApi.subscribe_hook.
                    // In-process plugins handle their own subscriptions in on_enable,
                    // so we use `port_impls().is_none()` as the OOP signal (set None
                    // by OutOfProcessPluginConnection, Some(Vec) by InProcess).
                    if ext.port_impls().is_none() {
                        // OOP plugin: daemon forwards declared hook events to the
                        // plugin via wire on_event. Plugin author declares interests
                        // via `hook-subscriptions "tag.glob" ...` in manifest.kdl.
                        // Future: plugin-settings overlay narrows per-install.
                        for glob_pattern in &lp.manifest.hook_subscriptions {
                            let filter = match pattern_core::hooks::filter::HookFilter::new(glob_pattern.clone()) {
                                Ok(f) => f,
                                Err(e) => {
                                    tracing::warn!(
                                        plugin = %lp.id,
                                        pattern = %glob_pattern,
                                        error = %e,
                                        "invalid hook subscription glob — skipping",
                                    );
                                    continue;
                                }
                            };
                            let (_sub_id, mut rx) = session.ctx.hook_bus().subscribe_notifications(filter);
                            let conn = std::sync::Arc::clone(ext);
                            let plugin_id = lp.id.clone();
                            let pat = glob_pattern.clone();
                            tokio::spawn(async move {
                                while let Some(event) = rx.recv().await {
                                    if let Err(e) = conn.on_event(event).await {
                                        tracing::debug!(
                                            plugin = %plugin_id,
                                            pattern = %pat,
                                            error = %e,
                                            "OOP hook forward failed (plugin may have died)",
                                        );
                                    }
                                }
                                tracing::debug!(plugin = %plugin_id, pattern = %pat, "OOP hook forwarder stopped (hook bus closed)");
                            });
                        }
                    }
                }
            }

            // Spawn MCP server connections in background (don't block session open).
            if !pending_mcp_configs.is_empty() {
                let registry = session.ctx.mcp_registry.clone();
                tokio::spawn(async move {
                    for (plugin_id, configs) in pending_mcp_configs {
                        let results = registry.load_servers(&configs).await;
                        for (name, result) in &results {
                            match result {
                                Ok(()) => {
                                    tracing::info!(plugin = %plugin_id, server = %name, "MCP server connected")
                                }
                                Err(e) => {
                                    tracing::warn!(plugin = %plugin_id, server = %name, error = %e, "MCP server failed to connect")
                                }
                            }
                        }
                    }
                });
            }
        }

        // Load MCP servers from persona KDL (native config path).
        if !persona_mcp_configs.is_empty() {
            let registry = session.ctx.mcp_registry.clone();
            let configs = persona_mcp_configs;
            tokio::spawn(async move {
                let results = registry.load_servers(&configs).await;
                for (name, result) in &results {
                    match result {
                        Ok(()) => {
                            tracing::info!(server = %name, "persona MCP server connected")
                        }
                        Err(e) => {
                            tracing::warn!(server = %name, error = %e, "persona MCP server failed to connect")
                        }
                    }
                }
            });
        }

        // Register this session with the AgentRegistry (Phase 4 T4) if the
        // caller wired a registry via `SessionContext::with_agent_registry`.
        // The RAII guard is held on `TidepoolSession` so the unregistration
        // fires when the session is dropped, not when `SessionContext` is
        // dropped (the ctx `Arc` may be shared after open).
        if let Some(registry) = session.ctx.agent_registry().cloned() {
            let persona_id: pattern_core::types::ids::PersonaId = session.ctx.agent_id().into();
            let mailbox = session.ctx.mailbox().clone();

            // Register the persona's display `name` as an alias when it
            // differs from the canonical `agent_id`. This lets peer
            // agents address the session by either form via
            // `agent:<name>` or `agent:<agent_id>`. Collisions surface
            // as a `RouterError::AliasCollision` and are logged; the
            // session still opens (canonical addressing always works).
            if let Some(alias_name) = persona_alias_for_registry.as_ref() {
                let alias: pattern_core::types::ids::PersonaId = alias_name.clone().into();
                if let Err(e) = registry.register_alias(alias, persona_id.clone()) {
                    tracing::warn!(
                        agent_id = %persona_id,
                        name = %alias_name,
                        error = %e,
                        "failed to register persona name as alias; agent remains addressable by canonical id"
                    );
                }
            }

            let guard = crate::agent_registry::RegistryGuard::register_active(
                registry, persona_id, mailbox,
            );
            session._registry_guard = Some(guard);
        }

        // Wire the turn sink into the DisplayHandler so Display events
        // flow to CLI/TUI subscribers during eval turns.
        session.display_handle.forward_to_turn_sink(turn_sink);

        // Spawn the eval worker.
        let worker = EvalWorker::spawn_with_includes(
            session.ctx.clone(),
            include_paths,
            session.session_id.clone(),
        );

        let worker = Arc::new(worker);
        let preamble: Arc<str> = Arc::from(preamble.into_boxed_str());
        session.eval_worker = Some(worker.clone());
        session.preamble = Some(preamble.clone());
        // Stash the port-library tempdir on the session for RAII
        // cleanup. Drop order: when TidepoolSession drops, the tempdir
        // is removed from disk via tempfile::TempDir's Drop impl.
        session._port_lib_tempdir = port_lib_tempdir;

        // Spawn the per-session mailbox-drain task (Phase 4 T3). The
        // task is registered on `session.tasks` (a `JoinSet`) so it
        // is aborted when the session is dropped — no detached-task
        // leak across session lifetimes.
        crate::mailbox::spawn_mailbox_task(
            &mut session.tasks,
            session.ctx.clone(),
            session.turn_history.clone(),
            worker as Arc<dyn crate::agent_loop::EvalDispatcher>,
            preamble,
            session.cache_profile.clone(),
        );

        // Restore turn history from persisted messages so re-spawning
        // against the same data-dir resumes conversation state.
        if let Err(e) = session.load_turn_history(session.ctx.db()).await {
            tracing::warn!(
                error = %e,
                "failed to restore turn history from DB; starting with empty history"
            );
        }

        Ok(session)
    }

    /// Execute one user-visible exchange via the Phase 5 agent-loop
    /// wire-turn-loop driver. Requires the session was opened via
    /// [`Self::open_with_agent_loop`] — returns
    /// `RuntimeError::SessionPoisoned` if no eval worker is
    /// configured, with a message pointing at the correct
    /// constructor.
    ///
    /// Drives the full wire-turn loop: compose → provider.complete →
    /// stream → tool dispatch → chain tool_results → repeat until
    /// `stop_reason.is_terminal()`. [`Session::step`] delegates here.
    pub async fn step_with_agent_loop(&self, input: TurnInput) -> Result<StepReply, RuntimeError> {
        let worker = self
            .eval_worker
            .as_ref()
            .ok_or_else(|| RuntimeError::SessionPoisoned {
                reason: "step_with_agent_loop called on a session \
                         opened without an eval worker; use \
                         TidepoolSession::open_with_agent_loop"
                    .into(),
            })?;
        let preamble = self.preamble.as_deref().unwrap_or("");
        let cache_profile = self.cache_profile.clone();
        crate::agent_loop::drive_step(
            input,
            self.ctx.clone(),
            self.turn_history.clone(),
            cache_profile,
            worker.as_ref(),
            preamble,
            None,
        )
        .await
    }
}

#[async_trait]
impl Session for TidepoolSession {
    async fn step(
        &mut self,
        input: TurnInput,
    ) -> Result<pattern_core::types::turn::StepReply, RuntimeError> {
        // Delegate to the agent-loop path. `&mut self` satisfies `&self` on
        // `step_with_agent_loop`.
        self.step_with_agent_loop(input).await
    }

    async fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError> {
        let log = self
            .checkpoint_log
            .lock()
            .map_err(|_| RuntimeError::CheckpointFailed {
                reason: "checkpoint log mutex poisoned".into(),
            })?;
        log.snapshot(&self.session_id, self.ctx.agent_id())
    }

    async fn restore(&mut self, snapshot: SessionSnapshot) -> Result<(), RuntimeError> {
        let events = CheckpointLog::decode_events(&snapshot)?;
        // Replay-then-continue semantics (Task 15): populate the event log
        // with the restored events so the next `step` replays them through
        // a `ReplayingBundle`. Phase 3 scope stores events verbatim;
        // follow-up phases plug this into the run loop.
        let mut log = self
            .checkpoint_log
            .lock()
            .map_err(|_| RuntimeError::CheckpointFailed {
                reason: "checkpoint log mutex poisoned".into(),
            })?;
        log.reset_to(events);
        Ok(())
    }
}

/// Record one effect exchange into the shared checkpoint log. Called by
/// handlers after they produce a response so restart-then-replay can
/// deterministically re-drive the JIT.
///
/// `request_repr` is a pre-formatted Debug string — handlers that only
/// see a typed request (not a raw `Value`) can pass
/// `format!("{req:?}")` without paying for a synthetic Value round-trip.
/// The shape written to the log matches [`CheckpointEvent::new`].
///
/// A poisoned log mutex is swallowed (logged via `tracing::warn`): we
/// do not want recording failures to affect the hot handler path. The
/// log is a best-effort artifact; if it becomes poisoned the session
/// has bigger problems than a missing event.
pub(crate) fn record_exchange(
    log: &Arc<std::sync::Mutex<CheckpointLog>>,
    tag: u32,
    request_repr: String,
    response: &tidepool_eval::Value,
    turn: u64,
) {
    match log.lock() {
        Ok(mut guard) => {
            guard.record(CheckpointEvent::from_request_repr(
                tag,
                request_repr,
                response,
                turn,
            ));
        }
        Err(_) => {
            tracing::warn!(
                tag,
                turn,
                "checkpoint log mutex poisoned; exchange not recorded"
            );
        }
    }
}

/// Seed persona-declared memory blocks into the store at session open.
///
/// For each `MemoryBlockSpec` in `persona.memory_blocks`:
/// - If a block with the same label already exists (e.g. restored from a
///   persistent DB on re-spawn), leave it untouched. Persona declares
///   INITIAL content; live state wins.
/// - Otherwise create the block via the trait's `create_block`, feed
///   `spec.content` through `StructuredDocument::import_from_json`
///   (schema-dispatched), apply `pinned` via `set_block_pinned`, then
///   persist.
///
/// `crdt_snapshot` is currently always `None` in foundation; when the
/// full-CRDT restore path lands, this helper will need to branch on it.
fn seed_persona_memory_blocks(
    store: &dyn MemoryStore,
    agent_id: &str,
    memory_blocks: &std::collections::HashMap<
        smol_str::SmolStr,
        pattern_core::types::snapshot::MemoryBlockSpec,
    >,
) -> Result<(), RuntimeError> {
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, MemoryType, Scope};

    // Persona-declared blocks seed into the persona's Global scope.
    let scope = Scope::Global(agent_id.into());

    for (label, spec) in memory_blocks {
        // shared_id is a planned feature for constellation-level cross-agent
        // block sharing. The resolver is not wired yet, so fail loudly rather
        // than silently ignoring the field and leaving the agent with wrong
        // memory configuration.
        if let Some(shared_id) = &spec.shared_id {
            return Err(RuntimeError::SharedBlockRefNotSupported {
                label: label.to_string(),
                shared_id: shared_id.to_string(),
            });
        }

        // Don't clobber existing blocks — persona is INITIAL intent.
        // Missing blocks return Ok(None) per the trait contract.
        match store.get_block(&scope, label.as_str()) {
            Ok(Some(_)) => continue, // Already exists — preserve live state.
            Ok(None) => {}           // Doesn't exist — create below.
            Err(e) => {
                return Err(RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("get_block failed: {e}"),
                });
            }
        }

        let block_type = match spec.memory_type {
            MemoryType::Core => MemoryBlockType::Core,
            // Archival persona specs create Working-tier blocks; true
            // archival storage lives in archival_entries (separate table).
            MemoryType::Working | MemoryType::Archival => MemoryBlockType::Working,
        };
        let schema = spec.schema.clone().unwrap_or_else(BlockSchema::text);

        let mut create =
            BlockCreate::new(label.as_str(), block_type, schema).with_permission(spec.permission);
        if let Some(desc) = &spec.description {
            create = create.with_description(desc.clone());
        }
        if let Some(limit) = spec.char_limit {
            create = create.with_char_limit(limit);
        }

        let doc =
            store
                .create_block(&scope, create)
                .map_err(|e| RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("create_block failed: {e}"),
                })?;

        // Schema-dispatched import of the initial content.
        doc.import_from_json(&spec.content)
            .map_err(|e| RuntimeError::MemorySeedFailed {
                label: label.to_string(),
                reason: format!("import_from_json failed: {e:?}"),
            })?;

        if spec.pinned {
            store
                .update_block_metadata(
                    &scope,
                    label.as_str(),
                    pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(true),
                )
                .map_err(|e| RuntimeError::MemorySeedFailed {
                    label: label.to_string(),
                    reason: format!("update_block_metadata failed: {e}"),
                })?;
        }

        store.persist_block(&scope, label.as_str()).map_err(|e| {
            RuntimeError::MemorySeedFailed {
                label: label.to_string(),
                reason: format!("persist_block failed: {e}"),
            }
        })?;
    }
    Ok(())
}

// ---- session tests -------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::SdkLocation;
    use crate::testing::{InMemoryMemoryStore, MockProviderClient};
    use pattern_core::ProviderClient;
    use pattern_core::traits::{MemoryStore, TurnSink, VecSink};
    use pattern_core::types::ids::{BatchId, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use pattern_core::types::snapshot::PersonaSnapshot;
    use pattern_core::types::turn::StopReason;

    // ── parse_module_name / module_name_to_path (port-library helpers) ──

    #[test]
    fn parse_module_name_simple_header() {
        assert_eq!(
            parse_module_name("module Foo where\nfoo = 1\n"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_module_name_dotted() {
        assert_eq!(
            parse_module_name("module Pattern.Http where\n"),
            Some("Pattern.Http".to_string())
        );
    }

    #[test]
    fn parse_module_name_with_export_list() {
        assert_eq!(
            parse_module_name("module Pattern.Http (httpGet, httpPost) where\n"),
            Some("Pattern.Http".to_string())
        );
    }

    #[test]
    fn parse_module_name_after_single_line_pragma() {
        assert_eq!(
            parse_module_name("{-# LANGUAGE OverloadedStrings #-}\nmodule Foo.Bar where\n"),
            Some("Foo.Bar".to_string())
        );
    }

    #[test]
    fn parse_module_name_after_multi_line_pragma() {
        let src = "{-# LANGUAGE\n      OverloadedStrings,\n      FlexibleContexts\n  #-}\nmodule Foo where\n";
        assert_eq!(parse_module_name(src), Some("Foo".to_string()));
    }

    #[test]
    fn parse_module_name_after_block_comment() {
        let src = "{- The grand description\n   spans many lines -}\nmodule Foo where\n";
        assert_eq!(parse_module_name(src), Some("Foo".to_string()));
    }

    #[test]
    fn parse_module_name_after_nested_block_comment() {
        assert_eq!(
            parse_module_name("{- outer {- inner -} still outer -}\nmodule Foo where\n"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_module_name_after_line_comments() {
        assert_eq!(
            parse_module_name("-- header banner\n-- another line\nmodule Foo where\n"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_module_name_after_operator_with_double_dash() {
        // `<--` is a valid Haskell operator. The `--` rule must NOT
        // eat it as a line comment per Haskell 2010 §2.3.
        assert_eq!(
            parse_module_name("infixl 4 <--\nmodule Foo where\n"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_module_name_with_utf8_block_comment() {
        // The cleaner uses string-slice copies, never byte-to-char
        // casts — this pins the UTF-8 correctness contract.
        assert_eq!(
            parse_module_name("{- αβγ — header with non-ASCII -}\nmodule Foo where\n"),
            Some("Foo".to_string())
        );
    }

    #[test]
    fn parse_module_name_returns_none_when_missing() {
        assert_eq!(parse_module_name("import Data.Text\nfoo = 1\n"), None);
    }

    #[test]
    fn module_name_to_path_simple() {
        assert_eq!(
            module_name_to_path("Foo"),
            std::path::PathBuf::from("Foo.hs")
        );
    }

    #[test]
    fn module_name_to_path_dotted() {
        assert_eq!(
            module_name_to_path("Pattern.Http"),
            std::path::PathBuf::from("Pattern/Http.hs")
        );
    }

    #[test]
    fn module_name_to_path_deep() {
        assert_eq!(
            module_name_to_path("A.B.C.D"),
            std::path::PathBuf::from("A/B/C/D.hs")
        );
    }

    fn test_turn_input() -> TurnInput {
        // Fresh batch start: turn_id == batch_id (first turn IS the batch).
        let id = new_snowflake_id();
        TurnInput {
            turn_id: id.clone(),
            batch_id: BatchId::from(id),
            origin: MessageOrigin::new(
                Author::System {
                    reason: SystemReason::Wakeup,
                },
                Sphere::System,
            ),
            messages: vec![],
        }
    }

    /// `step_with_agent_loop` on a session opened via the minimal
    /// `TidepoolSession::open` (no eval worker) returns
    /// `RuntimeError::SessionPoisoned` with a clear message.
    ///
    /// Gated on preflight so `open` can succeed.
    #[tokio::test]
    async fn step_with_agent_loop_without_worker_returns_session_poisoned_error() {
        if crate::preflight::check().is_err() {
            return;
        }
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();

        let session = TidepoolSession::open(
            persona,
            &sdk,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
            Arc::new(crate::port_registry::PortRegistryImpl::new(
                &tokio::runtime::Handle::current(),
            )),
        )
        .expect("open should succeed when preflight passes");

        let result = session.step_with_agent_loop(test_turn_input()).await;
        match result {
            Err(RuntimeError::SessionPoisoned { reason }) => {
                assert!(
                    reason.contains("open_with_agent_loop"),
                    "error should point at the correct constructor, got: {reason}"
                );
            }
            other => panic!("expected SessionPoisoned, got: {other:?}"),
        }
    }

    /// Integration test for `step_with_agent_loop` through the
    /// [`TidepoolSession::open_with_agent_loop`] constructor with the
    /// Phase 5 wire-turn-loop driver.
    ///
    /// Scripts two wire turns — tool_use then text — and asserts the
    /// resulting [`StepReply`] aggregates them correctly. Mirrors the
    /// `agent_loop::tests::drive_step_chains_tool_use_then_final_text_into_two_wire_turns`
    /// test but exercises the full session path instead of calling
    /// `drive_step` directly.
    ///
    /// # Environment requirements
    ///
    /// Gated on `preflight::check()` only — tidepool-extract bundles
    /// the prelude internally. Skips cleanly when unavailable.
    #[tokio::test]
    async fn open_with_agent_loop_and_step_drives_two_wire_turns() {
        if crate::preflight::check().is_err() {
            return;
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider = Arc::new(MockProviderClient::with_turns(vec![
            // Wire turn 1: tool_use
            MockProviderClient::tool_use_turn(
                "toolu_01",
                "code",
                serde_json::json!({"code": "pure (42 :: Int)"}),
            ),
            // Wire turn 2: final answer
            MockProviderClient::text_turn("I ran your code. The answer is 42."),
        ]));
        let provider_dyn: Arc<dyn ProviderClient> = provider.clone();
        let db = crate::testing::test_db().await;
        // Create the agent row so the FK on messages.agent_id is satisfied
        // when drive_step persists messages.
        {
            let agent = pattern_db::models::Agent {
                id: "agent-a".to_string(),
                name: "Test".to_string(),
                description: None,
                model_provider: "test".to_string(),
                model_name: "test-model".to_string(),
                system_prompt: "test".to_string(),
                config: pattern_db::Json(serde_json::json!({})),
                enabled_tools: pattern_db::Json(vec![]),
                tool_rules: None,
                status: pattern_db::models::AgentStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
                .expect("create test agent");
        }

        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider_dyn,
            db,
            tokio::runtime::Handle::current(),
            sink_dyn,
            None,
            None,
            None,
            None, // registries — test session, no inter-session routing needed.
        )
        .await
        .expect("open_with_agent_loop should succeed when preflight passes");

        let reply = session
            .step_with_agent_loop(test_turn_input())
            .await
            .expect("step_with_agent_loop should succeed with two scripted turns");

        // Two wire turns: tool_use then text.
        assert_eq!(provider.call_count(), 2, "two wire turns expected");
        assert_eq!(
            reply.turns.len(),
            2,
            "reply should aggregate two wire turns"
        );
        assert_eq!(reply.turns[0].stop_reason, StopReason::ToolUse);
        assert_eq!(reply.turns[1].stop_reason, StopReason::EndTurn);
        assert_eq!(reply.final_stop_reason, StopReason::EndTurn);

        // Batch id stable across wire turns.
        assert_eq!(
            reply.turns[0].messages[0].batch, reply.turns[1].messages[0].batch,
            "all wire turns in one step share batch_id"
        );

        // Aggregate usage sums both turns.
        let agg = reply
            .total_usage
            .expect("aggregated usage should be present");
        // tool_use_turn: prompt=50; text_turn: prompt=10 → 60 total
        assert_eq!(agg.prompt_tokens, Some(60));

        // The session's VecSink sees two Stop events (one per wire turn).
        let events = sink.snapshot();
        let stop_count = events
            .iter()
            .filter(|e| matches!(e, pattern_core::traits::TurnEvent::Stop(_)))
            .count();
        assert_eq!(stop_count, 2, "each wire turn emits one Stop event");

        // The sink should also capture the TurnEvent::Text for the final turn.
        let has_final_text = events
            .iter()
            .any(|e| matches!(e, pattern_core::traits::TurnEvent::Text(s) if s.contains("42")));
        assert!(
            has_final_text,
            "sink should contain text with '42' from final turn"
        );
    }

    /// The `NoOpSink` default is replaced by the caller's sink on sessions
    /// opened via `open_with_agent_loop`. We verify by checking that
    /// `ctx.turn_sink()` is NOT the default (NoOpSink) via pointer
    /// comparison — after open_with_agent_loop the sink should be the
    /// VecSink we passed in. The most direct assertion is that events
    /// actually appear in the VecSink (tested above), but this test
    /// checks the property directly without requiring a full eval.
    #[tokio::test]
    async fn open_with_agent_loop_wires_turn_sink_into_ctx() {
        if crate::preflight::check().is_err() {
            return;
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let sdk = SdkLocation::default();
        let sink = Arc::new(VecSink::new());
        let sink_dyn: Arc<dyn TurnSink> = sink.clone();

        let session = TidepoolSession::open_with_agent_loop(
            persona,
            &sdk,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
            sink_dyn,
            None,
            None,
            None,
            None, // registries — test session, no inter-session routing needed.
        )
        .await
        .expect("open_with_agent_loop should succeed");

        // The eval_worker and preamble should both be populated.
        assert!(
            session.eval_worker.is_some(),
            "eval_worker should be Some after open_with_agent_loop"
        );
        assert!(
            session.preamble.is_some(),
            "preamble should be Some after open_with_agent_loop"
        );
        let preamble = session.preamble.as_deref().unwrap();
        assert!(
            preamble.contains("module Expr where"),
            "preamble should contain the module header"
        );
        assert!(
            preamble.contains("paginateResult"),
            "preamble should contain pagination support"
        );
    }

    /// `seed_persona_memory_blocks` must thread the persona-declared
    /// `MemoryPermission` through to the underlying store. Without the fix,
    /// `BlockCreate` always defaulted to `ReadWrite`, silently upgrading any
    /// persona-declared `ReadOnly` block.
    ///
    /// Regression test for fix #2 (code-review finding: MemoryBlockSpec
    /// .permission not threaded through BlockCreate to MemoryCache).
    #[tokio::test]
    async fn seed_persona_memory_blocks_threads_permission_to_store() {
        use pattern_core::types::memory_types::MemoryPermission;
        use pattern_core::types::snapshot::MemoryBlockSpec;

        let store = Arc::new(InMemoryMemoryStore::new());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();

        let persona = PersonaSnapshot::new("agent-perm", "Permission test agent")
            .with_memory_block(
                "persona",
                MemoryBlockSpec::text("I am a read-only persona block.")
                    .with_permission(MemoryPermission::ReadOnly),
            )
            .with_memory_block(
                "scratchpad",
                MemoryBlockSpec::text("mutable notes").with_permission(MemoryPermission::ReadWrite),
            );

        seed_persona_memory_blocks(store_dyn.as_ref(), "agent-perm", &persona.memory_blocks)
            .expect("seed should succeed");

        // Check the read-only block — permission must be preserved.
        let agent_scope = pattern_core::types::memory_types::Scope::global("agent-perm");
        let doc = store_dyn
            .get_block(&agent_scope, "persona")
            .expect("get_block should succeed")
            .expect("persona block should exist");
        assert_eq!(
            doc.permission(),
            pattern_core::types::memory_types::MemoryPermission::ReadOnly,
            "persona block should be ReadOnly as declared in the spec"
        );

        // Check the read-write block — default must round-trip correctly.
        let doc2 = store_dyn
            .get_block(&agent_scope, "scratchpad")
            .expect("get_block should succeed")
            .expect("scratchpad block should exist");
        assert_eq!(
            doc2.permission(),
            pattern_core::types::memory_types::MemoryPermission::ReadWrite,
            "scratchpad block should be ReadWrite as declared in the spec"
        );
    }

    /// `seed_persona_memory_blocks` must reject any block that declares
    /// `shared_id`. Shared block references are not supported in the
    /// foundation runtime; silently ignoring the field would leave the agent
    /// with wrong memory configuration.
    ///
    /// Regression test for fix #3 (code-review finding: MemoryBlockSpec
    /// .shared_id is not validated at seed time).
    #[tokio::test]
    async fn seed_persona_memory_blocks_rejects_shared_id() {
        use pattern_core::error::RuntimeError;
        use pattern_core::types::snapshot::MemoryBlockSpec;
        use smol_str::SmolStr;

        let store = Arc::new(InMemoryMemoryStore::new());
        let store_dyn: Arc<dyn MemoryStore> = store.clone();

        // Build a spec with a shared_id set. We need to go through the
        // `Default` + field mutation path because `MemoryBlockSpec` is
        // `#[non_exhaustive]` so struct expressions are not usable outside
        // `pattern_core`. Use the `with_shared_id` builder if it exists;
        // otherwise mutate directly via the public field (it is `pub`).
        let mut spec_with_shared = MemoryBlockSpec::text("this content should never be used");
        spec_with_shared.shared_id = Some(SmolStr::new("mem_01HXYZ_shared"));

        let persona = PersonaSnapshot::new("agent-shared", "Shared block test agent")
            .with_memory_block("shared_notes", spec_with_shared);

        let result =
            seed_persona_memory_blocks(store_dyn.as_ref(), "agent-shared", &persona.memory_blocks);

        match result {
            Err(RuntimeError::SharedBlockRefNotSupported { label, shared_id }) => {
                assert_eq!(label, "shared_notes", "error should name the failing block");
                assert_eq!(
                    shared_id, "mem_01HXYZ_shared",
                    "error should include the shared_id value"
                );
            }
            Ok(()) => panic!("expected SharedBlockRefNotSupported error, got Ok"),
            Err(other) => panic!("expected SharedBlockRefNotSupported, got: {other:?}"),
        }
    }

    // -- Task 14: persona-level KDL rules merge into PolicySet ------------

    #[tokio::test]
    async fn merge_policies_layers_kdl_over_rust_defaults() {
        // Persona declares a KDL Allow rule for `git push*`. The
        // composed PolicySet should evaluate `git push origin main` as
        // Allow (KDL beats RustDefault), while `rm -rf /` still
        // RequireApproval (no KDL rule covers it).
        use pattern_core::{
            EffectCategory, PolicyAction, PolicyContext, PolicyMatcher, PolicyRule, Precedence,
        };

        let persona =
            PersonaSnapshot::new("agent-task14", "T14").with_policy_rules([PolicyRule::new(
                EffectCategory::Shell,
                PolicyMatcher::ShellCommand {
                    pattern: "git push*".into(),
                },
                PolicyAction::Allow,
                Precedence::KdlConfig,
            )]);

        let policies = merge_policies(&persona);

        // KDL Allow wins over the absence of a default for git push.
        assert_eq!(
            policies.evaluate(
                EffectCategory::Shell,
                &PolicyContext::Shell {
                    command: "git push origin main",
                },
            ),
            PolicyAction::Allow,
            "KDL Allow rule should reach the evaluator"
        );

        // Rust default still gates rm -rf — the KDL rule doesn't shadow it.
        match policies.evaluate(
            EffectCategory::Shell,
            &PolicyContext::Shell {
                command: "rm -rf /tmp/x",
            },
        ) {
            PolicyAction::RequireApproval { .. } => {}
            other => panic!("rm -rf should still RequireApproval, got {other:?}"),
        }
    }

    #[test]
    fn merge_policies_with_no_persona_rules_returns_just_defaults() {
        let persona = PersonaSnapshot::new("default-only", "D");
        let policies = merge_policies(&persona);
        // The defaults vec contains five shell rules + one spawn rule.
        assert_eq!(policies.rules().len(), crate::policy::rust_defaults().len());
    }

    /// AC2.2 end-to-end (review fix): a persona with a KDL `Allow`
    /// rule for `git push*` reaches the Shell handler, the policy
    /// evaluates to Allow, and the broker is NOT invoked. Exercises
    /// the full wire `persona.policy_rules → from_persona →
    /// SessionContext.policies → ShellHandler reads cx.user().policies()`.
    #[tokio::test]
    async fn ac2_2_persona_kdl_allow_reaches_shell_handler_and_skips_broker() {
        use crate::sdk::handlers::shell::ShellHandler;
        use crate::sdk::requests::ShellReq;
        use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};
        use tidepool_effect::EffectHandler;
        use tidepool_repr::DataConTable;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;

        let persona =
            PersonaSnapshot::new("agent-ac2-2", "AC22").with_policy_rules([PolicyRule::new(
                EffectCategory::Shell,
                PolicyMatcher::ShellCommand {
                    pattern: "git push*".into(),
                },
                PolicyAction::Allow,
                Precedence::KdlConfig,
            )]);

        // Wire a real broker + bridge, then watch for any traffic.
        let ctx_owned = SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        );
        let broker = ctx_owned.permission_broker().clone();
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker.clone()));
        let ctx = ctx_owned.with_permission_bridge(bridge);

        let mut rx = broker.subscribe();
        let saw_broker = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_broker.clone();
        let watcher = tokio::spawn(async move {
            if rx.recv().await.is_ok() {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        let result = tokio::task::spawn_blocking(move || {
            let mut h = ShellHandler;
            let table = DataConTable::new();
            let cx_eff = tidepool_effect::EffectContext::with_user(&table, &ctx);
            h.handle(
                ShellReq::Execute("git push origin main".into(), None),
                &cx_eff,
            )
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        // Allow path: post-gate error WITHOUT GateApproved marker (gate
        // did not fire because policy returned Allow before any broker
        // call). The exact post-gate error text is no longer "stub" since
        // v3-sandbox-io Phase 3 wired the real ShellHandler — the empty
        // DataConTable used here trips a Bridge decode error after the
        // handler progressed past the gate. Either shape proves AC2.2's
        // load-bearing claim: the gate routed Allow → handler without
        // consulting the broker. The PermissionDenied / GateApproved
        // markers are the discriminators we actually care about.
        assert!(
            !msg.contains("GateApproved:"),
            "Allow path must not carry GateApproved marker — gate should be skipped, got: {msg}"
        );
        assert!(
            !msg.contains(crate::policy::PERMISSION_DENIED_PREFIX),
            "Allow path must not carry PermissionDenied marker — gate said Allow, got: {msg}"
        );

        // Allow watcher a beat to record any broker traffic.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !saw_broker.load(std::sync::atomic::Ordering::SeqCst),
            "broker must NOT receive any request when KDL Allow rule matches"
        );
        watcher.abort();
    }

    /// AC2.3 end-to-end (review fix): a persona with a KDL
    /// `RequireApproval` rule for all file writes (`*` glob) escalates
    /// non-config writes through the broker. Exercises the same wire
    /// as AC2.2 but through the File handler.
    #[tokio::test]
    async fn ac2_3_persona_kdl_require_approval_reaches_file_handler_and_invokes_broker() {
        use crate::sdk::handlers::file::FileHandler;
        use crate::sdk::requests::FileReq;
        use pattern_core::permission::PermissionDecisionKind;
        use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};
        use tidepool_effect::EffectHandler;
        use tidepool_repr::DataConTable;

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let db = crate::testing::test_db().await;

        let persona =
            PersonaSnapshot::new("agent-ac2-3", "AC23").with_policy_rules([PolicyRule::new(
                EffectCategory::File,
                PolicyMatcher::FilePath {
                    pattern: "*".into(),
                },
                PolicyAction::RequireApproval {
                    reason: Some("all file writes gated for this persona".into()),
                },
                Precedence::KdlConfig,
            )]);

        let ctx_owned = SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        );
        let broker = ctx_owned.permission_broker().clone();
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker.clone()));
        let ctx = ctx_owned.with_permission_bridge(bridge);

        // Subscribe synchronously so the responder never misses.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                    .await;
            }
        });

        let result = tokio::task::spawn_blocking(move || {
            let mut h = FileHandler;
            let table = DataConTable::new();
            let cx_eff = tidepool_effect::EffectContext::with_user(&table, &ctx);
            h.handle(
                FileReq::Write("/tmp/notes.txt".into(), "hello".into()),
                &cx_eff,
            )
        })
        .await
        .expect("blocking task")
        .expect_err("Phase 1 stub always errors");
        let msg = result.to_string();
        assert!(
            msg.contains("GateApproved:"),
            "expected GateApproved marker — broker must have observed the prompt and approved, \
             got: {msg}"
        );
        responder.await.unwrap();
    }

    #[test]
    fn from_persona_threads_persona_capabilities_through() {
        use pattern_core::{CapabilitySet, EffectCategory};
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(MockProviderClient::with_turns(vec![]));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let db = rt.block_on(crate::testing::test_db());

        let persona = PersonaSnapshot::new("caps-thru", "C").with_capabilities(Some(
            CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message]),
        ));

        let ctx = SessionContext::from_persona(&persona, store, provider, db, rt.handle().clone());
        let caps = ctx.capabilities().expect("persona caps should propagate");
        assert!(caps.contains(EffectCategory::Memory));
        assert!(caps.contains(EffectCategory::Message));
        assert!(!caps.contains(EffectCategory::Shell));
    }
}
