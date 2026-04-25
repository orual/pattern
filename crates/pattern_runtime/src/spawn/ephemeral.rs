//! Ephemeral child sessions: fork-for-ephemeral construction + the
//! `run_ephemeral` driver that owns the child's wire-turn loop.
//!
//! Phase 2 Task 4 of the v3-multi-agent plan. The handler arms in
//! `crate::sdk::handlers::spawn` produce `SpawnId` / `SpawnResult` /
//! `SpawnError` values via the helpers below.
//!
//! Ephemeral semantics:
//!
//! - The child runs a full LLM-driven turn loop via [`drive_step`], with
//!   its own [`EvalWorker`] (256 MiB OS thread; new per ephemeral).
//! - `EphemeralConfig.program` becomes a synthesized lib module on the
//!   child's GHC include path so the child's eval-tool snippets can
//!   `import Pattern.SpawnHelpers` and call helpers defined there.
//! - `EphemeralConfig.prompt`, when `Some`, becomes the initial
//!   human-role message; when `None`, the child opens with no human
//!   turn and runs on `costume`/system-prompt alone.
//! - The whole thing is wrapped in [`tokio::time::timeout`] for hard
//!   bound on wall-time.

use std::path::PathBuf;
use std::sync::Arc;

use jiff::{Span, Timestamp};
use smol_str::SmolStr;
use tempfile::TempDir;

use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
use pattern_core::types::turn::TurnInput;
use pattern_core::{CapabilitySet, spawn::EphemeralConfig};

use crate::agent_loop::{EvalWorker, drive_step};
use crate::memory::TurnHistory;
use crate::session::SessionContext;
use crate::spawn::{SpawnError, SpawnResult, TerminationReason};
use crate::timeout::CancelState;

/// Maximum number of wire turns an ephemeral may take before the runner
/// terminates with [`TerminationReason::MaxTurns`]. Conservative safety
/// net; agents that need more should split their work across multiple
/// spawns or refine this when a real workload demands it.
pub const MAX_EPHEMERAL_TURNS: u32 = 32;

/// Default ephemeral timeout when the caller leaves
/// `EphemeralConfig.timeout` unset.
fn default_timeout() -> Span {
    // 60 seconds — enough for a few-turn LLM loop, conservative against
    // runaway. Calibrate when the spawn surface gets real workloads.
    Span::new().seconds(60)
}

/// Compute the child's effective capability set from the parent's caps
/// and the ephemeral's request.
///
/// `None` on the request means "inherit parent's full set." `Some(set)`
/// is intersected against the parent via `restrict_to`; any escalation
/// surfaces as [`SpawnError::CapabilityEscalation`].
pub fn compute_child_caps(
    parent: &SessionContext,
    cfg: &EphemeralConfig,
) -> Result<CapabilitySet, SpawnError> {
    let parent_caps = parent
        .capabilities()
        .cloned()
        .unwrap_or_else(CapabilitySet::all);
    match &cfg.capabilities {
        Some(set) => {
            set.clone()
                .restrict_to(&parent_caps)
                .map_err(|e| SpawnError::CapabilityEscalation {
                    reason: e.to_string(),
                })
        }
        None => Ok(parent_caps),
    }
}

/// Synthesize a `lib/Pattern/SpawnHelpers.hs` module from `program` in
/// a fresh temp directory, returning the directory handle (whose path
/// is added to the child's include paths) on success.
///
/// Returns `Ok(None)` when `program` is empty or whitespace — no lib
/// module is synthesized; the child runs on the parent's include paths
/// only.
///
/// The returned [`TempDir`] must outlive the child's eval worker; the
/// child's `SessionContext` holds it (via the embedded handle on the
/// `SpawnRegistry`'s `ChildSessionHandle`'s payload, or via a session
/// drop hook — see the runner for details).
pub fn synthesize_program_lib(program: &str) -> Result<Option<TempDir>, SpawnError> {
    if program.trim().is_empty() {
        return Ok(None);
    }
    let dir = tempfile::Builder::new()
        .prefix("pattern-spawn-lib-")
        .tempdir()
        .map_err(|e| SpawnError::ProgramCompileFailed {
            message: format!("could not create temp dir for spawn lib: {e}"),
        })?;
    let module_dir = dir.path().join("Pattern");
    std::fs::create_dir_all(&module_dir).map_err(|e| SpawnError::ProgramCompileFailed {
        message: format!("could not create Pattern/ subdir: {e}"),
    })?;
    let module_path = module_dir.join("SpawnHelpers.hs");
    let header = "{-# LANGUAGE OverloadedStrings #-}\n\
                  module Pattern.SpawnHelpers where\n\n\
                  import Data.Text (Text)\n\
                  import qualified Data.Text as T\n\n";
    let source = format!("{header}{program}\n");
    std::fs::write(&module_path, &source).map_err(|e| SpawnError::ProgramCompileFailed {
        message: format!("could not write {}: {e}", module_path.display()),
    })?;
    Ok(Some(dir))
}

/// Build the include-path list for the child session from the parent's
/// include paths plus, optionally, the synthesized spawn-lib temp dir.
pub fn child_include_paths(parent: &SessionContext, lib_dir: Option<&TempDir>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = parent.include_paths().as_ref().clone();
    if let Some(d) = lib_dir {
        paths.push(d.path().to_path_buf());
    }
    paths
}

/// Construct the initial [`TurnInput`] for the child from the optional
/// caller prompt.
///
/// When `prompt = Some(text)`, synthesizes a single user-role message
/// in a fresh batch. When `prompt = None`, builds an empty turn that
/// opens on the system prompt alone — the LLM proceeds with whatever
/// `costume` or default system-prompt directs.
fn initial_turn_input(prompt: Option<&str>, agent_id: &str) -> TurnInput {
    let batch_id: BatchId = new_snowflake_id();
    let agent_id_owned: AgentId = agent_id.into();
    match prompt {
        Some(text) => {
            let chat_msg = genai::chat::ChatMessage::new(genai::chat::ChatRole::User, text);
            let msg = Message {
                chat_message: chat_msg,
                id: MessageId::from(new_id()),
                position: new_snowflake_id(),
                owner_id: agent_id_owned,
                created_at: Timestamp::now(),
                batch: batch_id.clone(),
                response_meta: None,
                block_refs: vec![],
                attachments: vec![],
            };
            TurnInput {
                turn_id: new_snowflake_id(),
                batch_id,
                origin: MessageOrigin::new(
                    Author::System {
                        reason: SystemReason::Wakeup,
                    },
                    Sphere::System,
                ),
                messages: vec![msg],
            }
        }
        None => {
            // Synthesize a System-authored continuation when no prompt
            // is supplied; the LLM opens against the system prompt /
            // costume alone.
            TurnInput::continuation(batch_id, agent_id_owned)
        }
    }
}

/// Drive the child's wire-turn loop. Wraps [`drive_step`] in a
/// [`tokio::time::timeout`] honouring `cfg.timeout` (or the
/// runtime default). Returns a [`SpawnResult`] that the parent's
/// `awaitSpawn` (or `awaitAll`) surfaces verbatim.
///
/// Cancellation: if the timeout fires, the child's `cancel_state` is
/// flipped before returning so any in-flight handler observes the
/// cancellation at its next effect boundary.
pub async fn run_ephemeral(
    child_ctx: Arc<SessionContext>,
    cfg: EphemeralConfig,
    child_id: SmolStr,
    progress_log_label: SmolStr,
    child_include: Vec<PathBuf>,
    preamble: String,
    _lib_dir_owned: Option<TempDir>,
) -> Result<SpawnResult, SpawnError> {
    let timeout_dur = cfg.timeout.unwrap_or_else(default_timeout);
    // Convert jiff::Span -> std::time::Duration for tokio::time.
    let std_timeout = match timeout_dur.total(jiff::Unit::Millisecond) {
        Ok(ms) => std::time::Duration::from_millis(ms.max(0.0) as u64),
        Err(_) => std::time::Duration::from_secs(60),
    };

    let agent_id = child_ctx.agent_id().to_string();
    let initial_input = initial_turn_input(cfg.prompt.as_deref(), &agent_id);

    // Fresh history for the child — completely independent of parent.
    let history = Arc::new(std::sync::Mutex::new(TurnHistory::empty()));

    // Spawn the child's eval worker. Each ephemeral gets its own
    // 256 MiB OS thread; semaphore-bounded count keeps fan-out sane.
    let session_id_for_worker = child_id.to_string();
    let worker =
        EvalWorker::spawn_with_includes(child_ctx.clone(), child_include, session_id_for_worker);

    // Default cache profile — same as the parent session's
    // step_with_agent_loop fallback. CacheProfile lives in
    // pattern_provider's compose surface.
    let cache_profile = pattern_provider::compose::CacheProfile::default_anthropic_subscriber();

    let drive_fut = drive_step(
        initial_input,
        child_ctx.clone(),
        history,
        cache_profile,
        &worker,
        &preamble,
    );

    let outcome = tokio::time::timeout(std_timeout, drive_fut).await;

    // Drop the worker before returning so its OS thread can wind down.
    drop(worker);

    let cancel_state: Arc<CancelState> = child_ctx.cancel_state();

    let progress_log_some: Option<String> = Some(progress_log_label.to_string());

    match outcome {
        Ok(Ok(reply)) => {
            // Inspect the StepReply for terminal info. Prefer the last
            // turn's stop_reason for the termination reason.
            let turns = reply.turns.len() as u32;
            let (final_text, terminated) = reply
                .turns
                .last()
                .map(|t| {
                    let text = t.messages.iter().rev().find_map(|m| {
                        // Extract joined-text from the chat message's
                        // content parts, if any.
                        m.chat_message.content.joined_texts()
                    });
                    let term = if t.stop_reason.is_terminal() {
                        TerminationReason::EndTurn
                    } else {
                        TerminationReason::ToolUse
                    };
                    (text, term)
                })
                .unwrap_or((None, TerminationReason::EndTurn));
            // Cap at MAX_EPHEMERAL_TURNS — drive_step already runs to
            // terminal so this is just a sentinel.
            let terminated = if turns >= MAX_EPHEMERAL_TURNS {
                TerminationReason::MaxTurns
            } else {
                terminated
            };
            Ok(SpawnResult {
                child_id,
                final_text,
                turns,
                terminated,
                progress_log_label: progress_log_some,
            })
        }
        Ok(Err(rt_err)) => Err(SpawnError::Runtime(rt_err.to_string())),
        Err(_elapsed) => {
            // Timeout fired. Flip the cancel atomic so any straggling
            // handler sees the cancellation.
            cancel_state
                .cancellation
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Err(SpawnError::Timeout {
                timeout: timeout_dur,
            })
        }
    }
}
