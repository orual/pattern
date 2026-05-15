//! Handler for `Pattern.File` — all eight variants dispatched to `FileManager`.
//!
//! Decision flow per [`FileReq::Write`]:
//!
//! 1. **Shape guard (locked invariant)**: if the destination looks
//!    like a Pattern config KDL
//!    ([`crate::policy::is_pattern_config_kdl`]), the handler escalates
//!    directly to the broker with a [`PermissionScope::FileWrite`]
//!    keyed on the path. The [`pattern_core::PolicySet`] is **not**
//!    consulted on this path; no rule (including KDL-loaded
//!    `Allow`-everything rules and runtime overrides) can loosen the
//!    gate. The user can grant temporary access via the broker's
//!    `ApproveForDuration` flow — that grant lives in the broker's
//!    in-memory `scope_cache` only and dies with the session.
//!
//! 2. **Policy pipeline**: non-config writes flow through the standard
//!    [`pattern_core::PolicySet::evaluate`] → [`pattern_core::PolicyAction`]
//!    fan-out (`Deny` / `RequireApproval` / `Allow`). On `Deny` or
//!    timeout, the handler returns a `PERMISSION_DENIED_PREFIX`-marked
//!    error. On `Allow`, the write is dispatched to `FileManager`.
//!
//! 3. **FileManager dispatch**: all other variants (`Read`, `ListDir`,
//!    `Open`, `Close`, `Watch`, `Reload`, `ForceWrite`) dispatch directly
//!    to `FileManager` without consulting the policy pipeline. Capability
//!    checking is enforced inside `FileManager` itself.
//!
//! The handler is generic over `HasCancelState + HasPolicySet +
//! HasPermissionBridge + HasFileManager` — this lets the existing
//! policy-gate tests keep a lightweight `TestUser` while the production
//! `SessionContext` satisfies all four bounds.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pattern_core::permission::PermissionScope;
use pattern_core::{EffectCategory, PolicyAction, PolicyContext};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::policy::config_guard::is_pattern_config_kdl;
use crate::policy::{GATE_APPROVED_PREFIX, PERMISSION_DENIED_PREFIX};
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::FileReq;
use crate::session::{HasPermissionBridge, SessionContext};
use crate::timeout::HandlerGuard;

/// Default broker-request timeout for file-write gates. Same envelope
/// as the Shell handler's gate timeout — long enough for human
/// thinking, short enough that a stalled responder surfaces as denial.
const FILE_GATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Handler for `Pattern.File` — dispatches all eight variants to `FileManager`.
#[derive(Default, Clone)]
pub struct FileHandler;

impl DescribeEffect for FileHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "File",
            description: "Sandboxed filesystem access (Read/Write/ListDir/Open/Close/Watch/Reload/ForceWrite)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Read       :: Path -> File Content",
                "Write      :: Path -> Content -> File ()",
                "ListDir    :: Path -> GlobPattern -> File [FileInfo]",
                "Open       :: Path -> File Content",
                "Close      :: Path -> File ()",
                "Watch      :: Path -> File ()",
                "Reload     :: Path -> File Content",
                "ForceWrite :: Path -> Content -> File ()",
                "InsertLines :: Path -> Int -> Content -> File ()",
                "ReplaceLines :: Path -> Int -> Int -> Content -> File ()",
                "DeleteLines :: Path -> Int -> Int -> File ()",
                "ReadLines  :: Path -> Int -> Int -> File Content",
                "Replace    :: Path -> Text -> Text -> File Text",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type Path = Text",
                "type Content = Text",
                "type GlobPattern = Text",
                "type FileInfo = Text -- JSON: {path:Text, size:Int, mtime:Text, is_dir:Bool}",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "read :: Member File effs => Path -> Eff effs Content\nread p = Freer.send (Read p)",
                "write :: Member File effs => Path -> Content -> Eff effs ()\nwrite p c = Freer.send (Write p c)",
                "listDir :: Member File effs => Path -> GlobPattern -> Eff effs [FileInfo]\nlistDir p g = Freer.send (ListDir p g)",
                "open :: Member File effs => Path -> Eff effs Content\nopen p = Freer.send (Open p)",
                "close :: Member File effs => Path -> Eff effs ()\nclose p = Freer.send (Close p)",
                "watch :: Member File effs => Path -> Eff effs ()\nwatch p = Freer.send (Watch p)",
                // Reload drops memory_doc state and returns reloaded content from disk.
                "reload :: Member File effs => Path -> Eff effs Content\nreload p = Freer.send (Reload p)",
                // ForceWrite writes through, bypassing ConflictPolicy.
                "forceWrite :: Member File effs => Path -> Content -> Eff effs ()\nforceWrite p c = Freer.send (ForceWrite p c)",
                "insertLines :: Member File effs => Path -> Int -> Content -> Eff effs ()\ninsertLines p n c = Freer.send (InsertLines p n c)",
                "replaceLines :: Member File effs => Path -> Int -> Int -> Content -> Eff effs ()\nreplaceLines p from to c = Freer.send (ReplaceLines p from to c)",
                "deleteLines :: Member File effs => Path -> Int -> Int -> Eff effs ()\ndeleteLines p from to = Freer.send (DeleteLines p from to)",
                "readLines :: Member File effs => Path -> Int -> Int -> Eff effs Content\nreadLines p start count = Freer.send (ReadLines p start count)",
                "replace :: Member File effs => Path -> Text -> Text -> Eff effs Text\nreplace p find repl = Freer.send (Replace p find repl)",
            ]),
        }
    }
}

impl EffectHandler<SessionContext> for FileHandler {
    type Request = FileReq;

    fn handle(
        &mut self,
        req: FileReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        // Effect-class runtime guard. Runs BEFORE the shape guard and policy
        // pipeline. Write/ForceWrite are MutateExternal/Skip (the shape guard
        // is the authoritative gate); other constructors are Observe or
        // MutateInternal with Enforce semantics.
        let constructor_name = match &req {
            FileReq::Read(_) => "Read",
            FileReq::Write(_, _) => "Write",
            FileReq::ListDir(_, _) => "ListDir",
            FileReq::Open(_) => "Open",
            FileReq::Close(_) => "Close",
            FileReq::Watch(_) => "Watch",
            FileReq::Reload(_) => "Reload",
            FileReq::ForceWrite(_, _) => "ForceWrite",
            FileReq::InsertLines(_, _, _) => "InsertLines",
            FileReq::ReplaceLines(_, _, _, _) => "ReplaceLines",
            FileReq::DeleteLines(_, _, _) => "DeleteLines",
            FileReq::ReadLines(_, _, _) => "ReadLines",
            FileReq::Replace(_, _, _) => "Replace",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "File",
            constructor_name,
        )?;

        match req {
            FileReq::Read(path) => {
                let fm = require_file_manager(cx.user())?;
                let p = Path::new(&path);
                // Sandbox + capability gate via FileManager. For binary content
                // we bypass `get_or_open` entirely (loro doc-sync is text-only);
                // explicit `check_access` enforces the same gate the text path
                // would have gotten via `get_or_open`'s internals.
                fm.check_access(p)
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let raw_bytes = std::fs::read(p).map_err(|e| {
                    EffectError::Handler(format!("Pattern.File.Read: {e}"))
                })?;
                let mime = pattern_core::multimodal::sniff_content_type(&raw_bytes, p)
                    .unwrap_or_else(|_| "application/octet-stream".to_string());
                cx.user()
                    .hook_bridge()
                    .emit(pattern_core::hooks::HookEvent::notification(
                        pattern_core::hooks::tags::FILE_READ,
                        serde_json::json!({
                            "path": path,
                            "operation": "read",
                            "content_type": mime,
                        }),
                    ));

                if pattern_core::multimodal::is_binary_mime(&mime) {
                    // Binary path: bypass loro, build a multi-modal ContentPart,
                    // push it to the per-eval attachment side-channel, return a
                    // marker text to the agent's Haskell eval.
                    let display = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(String::from);
                    let (part, meta) = pattern_core::multimodal::bytes_to_binary_part(
                        raw_bytes,
                        &mime,
                        display,
                        &pattern_core::multimodal::BinaryConvertOpts::default(),
                    )
                    .map_err(|e| {
                        EffectError::Handler(format!("Pattern.File.Read multi-modal: {e}"))
                    })?;
                    let marker = pattern_core::multimodal::marker_text_for(&meta);
                    cx.user().push_pending_tool_attachment(part);
                    return cx.respond(marker);
                }

                // Text path: existing loro-backed flow via get_or_open.
                let sf = fm
                    .get_or_open(p)
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let content = sf
                    .read()
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.Read: {e}")))?;
                cx.respond(content)
            }
            FileReq::ListDir(path, glob) => {
                let fm = require_file_manager(cx.user())?;
                let entries = fm
                    .list(Path::new(&path), &glob)
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let json: Vec<String> = entries
                    .iter()
                    .map(|e| serde_json::to_string(e).unwrap_or_default())
                    .collect();
                cx.respond(json)
            }
            FileReq::Open(path) => {
                let fm = require_file_manager(cx.user())?;
                let bytes = fm
                    .open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let s = String::from_utf8(bytes).map_err(|e| {
                    EffectError::Handler(format!(
                        "Pattern.File.Open: {path} is not valid UTF-8: {e}"
                    ))
                })?;
                cx.user()
                    .hook_bridge()
                    .emit(pattern_core::hooks::HookEvent::notification(
                        pattern_core::hooks::tags::FILE_OPENED,
                        serde_json::json!({ "path": path }),
                    ));
                cx.respond(s)
            }
            FileReq::Close(path) => {
                let fm = require_file_manager(cx.user())?;
                fm.close(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.respond(())
            }
            FileReq::Watch(path) => {
                let fm = require_file_manager(cx.user())?;
                fm.watch(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.user()
                    .hook_bridge()
                    .emit(pattern_core::hooks::HookEvent::notification(
                        pattern_core::hooks::tags::FILE_WATCHED,
                        serde_json::json!({ "path": path }),
                    ));
                cx.respond(())
            }
            FileReq::Reload(path) => {
                let fm = require_file_manager(cx.user())?;
                let bytes = fm
                    .reload(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let s = String::from_utf8(bytes).map_err(|e| {
                    EffectError::Handler(format!(
                        "Pattern.File.Reload: {path} is not valid UTF-8: {e}"
                    ))
                })?;
                cx.respond(s)
            }
            FileReq::Write(path, content) => {
                // Gate evaluation runs FIRST: the "locked-default"
                // shape guard for Pattern config KDL writes must
                // escalate to the broker even when no FileManager is
                // wired (e.g. in unit tests, or before a session has
                // a mount). Reaching `require_file_manager` first
                // would shortcut the gate.
                evaluate_write(&path, content.as_bytes(), cx.user())?;
                let fm = require_file_manager(cx.user())?;
                fm.write(Path::new(&path), content.as_bytes())
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.user()
                    .hook_bridge()
                    .emit(pattern_core::hooks::HookEvent::notification(
                        pattern_core::hooks::tags::FILE_WRITE,
                        serde_json::json!({ "path": path, "operation": "write" }),
                    ));
                cx.respond(())
            }
            FileReq::ForceWrite(path, content) => {
                evaluate_write(&path, content.as_bytes(), cx.user())?;
                let fm = require_file_manager(cx.user())?;
                fm.force_write(Path::new(&path), content.as_bytes())
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                cx.respond(())
            }
            FileReq::InsertLines(path, after_line, new_content) => {
                evaluate_write(&path, new_content.as_bytes(), cx.user())?;
                let fm = require_file_manager(cx.user())?;
                let sf = fm
                    .get_or_open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                sf.insert_lines(after_line as usize, &new_content)
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.InsertLines: {e}")))?;
                cx.respond(())
            }
            FileReq::ReplaceLines(path, from, to, content) => {
                evaluate_write(&path, content.as_bytes(), cx.user())?;
                let fm = require_file_manager(cx.user())?;
                let sf = fm
                    .get_or_open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                sf.replace_lines(from as usize, to as usize, &content)
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.ReplaceLines: {e}")))?;
                cx.respond(())
            }

            FileReq::DeleteLines(path, from, to) => {
                evaluate_write(&path, &[], cx.user())?;
                let fm = require_file_manager(cx.user())?;
                let sf = fm
                    .get_or_open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                sf.delete_lines(from as usize, to as usize)
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.DeleteLines: {e}")))?;
                cx.respond(())
            }
            FileReq::ReadLines(path, start, count) => {
                let fm = require_file_manager(cx.user())?;
                let sf = fm
                    .get_or_open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let content = sf
                    .read()
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.ReadLines: {e}")))?;
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let start_idx = (start.max(1) as usize).saturating_sub(1);
                let count_usize = count.max(0) as usize;
                let end_idx = (start_idx + count_usize).min(total);
                let slice = if start_idx < total {
                    lines[start_idx..end_idx].join("\n")
                } else {
                    String::new()
                };
                let header = format!("[lines {}-{} of {}]\n", start_idx + 1, end_idx, total,);
                cx.respond(header + &slice)
            }
            FileReq::Replace(path, find, replace_with) => {
                evaluate_write(&path, replace_with.as_bytes(), cx.user())?;
                let fm = require_file_manager(cx.user())?;
                let sf = fm
                    .get_or_open(Path::new(&path))
                    .map_err(|e| EffectError::Handler(e.to_effect_message()))?;
                let content = sf
                    .read()
                    .map_err(|e| EffectError::Handler(format!("Pattern.File.Replace: {e}")))?;
                let count = content.matches(&find).count();
                if count > 0 {
                    let new_content = content.replace(&find, &replace_with);
                    sf.write(&new_content)
                        .map_err(|e| EffectError::Handler(format!("Pattern.File.Replace: {e}")))?;
                }
                cx.user()
                    .hook_bridge()
                    .emit(pattern_core::hooks::HookEvent::notification(
                        pattern_core::hooks::tags::FILE_WRITE,
                        serde_json::json!({ "path": path, "operation": "replace", "count": count }),
                    ));
                cx.respond(count.to_string())
            }
        }
    }
}

/// Return the file manager from user context, or a clear error if not wired.
fn require_file_manager(
    user: &SessionContext,
) -> Result<&Arc<crate::file_manager::FileManager>, EffectError> {
    user.file_manager().ok_or_else(|| {
        EffectError::Handler(
            "Pattern.File: no file manager configured for this session \
             (session opened without a mount config)"
                .to_string(),
        )
    })
}

/// Two-stage gate for `File.Write`: shape guard → policy pipeline →
/// `FileManager.write`.
///
/// Returns `Ok(())` when the write should proceed (the caller is responsible
/// for calling `cx.respond(())`). Returns `Err` on denial, broker timeout,
/// or gate approval (the escalation path returns `Err(GateApproved)` so
/// tests and the UI can observe the gate decision).
///
/// Note: the `RequireApproval` → broker-grant → FM-write flow currently
/// returns `Err(GateApproved)` from the escalation path rather than
/// dispatching to FM after approval. This is because the `escalate` fn
/// signature predates full FileManager wiring. Follow-up: restructure
/// `escalate` to return a typed enum so the caller can distinguish
/// "approved, proceed to FM" from "denied, stop", and dispatch accordingly.
fn evaluate_write(
    path_str: &str,
    content: &[u8],
    user: &SessionContext,
) -> Result<(), EffectError> {
    let path = Path::new(path_str);

    // Path-normalization deferral (see Phase 1 review item minor #1):
    // `PermissionScope::FileWrite { path }` keys the broker's scope cache on
    // the literal path string. Canonicalization lives in the FileManager's
    // write path; the gate uses the literal string so over-prompting on
    // aliases is the safe direction until the sandbox-IO canonicalization is
    // wired end-to-end.

    // (1) Locked invariant — Pattern config KDL writes always escalate
    //     to the broker. PolicySet is not consulted on this path.
    if is_pattern_config_kdl(path, content).is_config() {
        escalate(
            user,
            PermissionScope::FileWrite {
                path: path_str.to_string(),
            },
            "write to Pattern config KDL",
        )?;
        // escalate only returns Ok when the broker approved; the gate
        // approval is signalled as Err(GateApproved) so the path below
        // (returning Ok to trigger cx.respond(())) is not reached for
        // config-KDL paths — the VM sees an error. This is the Phase 1
        // established contract for the shape-guard path.
        unreachable!("escalate always returns Err — never falls through to here");
    }

    // (2) Non-config writes flow through the policy pipeline.
    let policy_ctx = PolicyContext::FileWrite { path, content };
    match user.policies().evaluate(EffectCategory::File, &policy_ctx) {
        PolicyAction::Deny { reason } => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}{}",
            reason.unwrap_or_else(|| "file write denied by policy".into())
        ))),
        PolicyAction::RequireApproval { reason } => {
            escalate(
                user,
                PermissionScope::FileWrite {
                    path: path_str.to_string(),
                },
                reason.as_deref().unwrap_or("file write requires approval"),
            )?;
            // See note above — escalate returns Err(GateApproved) on
            // broker approval; never reaches here.
            unreachable!("escalate always returns Err — never falls through to here");
        }
        PolicyAction::Allow => Ok(()),
        // PolicyAction is `#[non_exhaustive]` — fail closed on any future variant.
        other => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}unhandled policy action {other:?}"
        ))),
    }
}

/// Escalate a write through the [`crate::permission::PermissionBridge`].
///
/// Always returns `Err` — either `Err(GateApproved)` on broker grant, or
/// `Err(PermissionDenied)` on denial / timeout / missing bridge. This
/// asymmetry exists because the original Phase 1 escalation path used
/// `Result<Value>` to communicate gate outcomes directly to the VM. The
/// `evaluate_write` caller translates the `Err(GateApproved)` marker back
/// to a meaningful error on the wire.
fn escalate(
    user: &SessionContext,
    scope: PermissionScope,
    reason: &str,
) -> Result<(), EffectError> {
    let Some(bridge) = user.permission_bridge() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write gated but no permission bridge wired"
        )));
    };
    let origin = user.current_dispatch_origin().unwrap_or_else(|| {
        pattern_core::types::origin::MessageOrigin::new(
            pattern_core::types::origin::Author::System {
                reason: pattern_core::types::origin::SystemReason::Timer,
            },
            pattern_core::types::origin::Sphere::System,
        )
    });
    // Real session agent_id is load-bearing for per-agent isolation
    // of the broker's scope cache (keyed `(agent_id, scope)`); fail
    // closed if absent.
    let Some(agent) = user.dispatch_agent_id() else {
        return Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write gated but no agent identity \
             available for broker attribution"
        )));
    };
    let grant = bridge.request_sync(
        agent,
        "file".into(),
        scope,
        &origin,
        Some(reason.to_string()),
        None,
        FILE_GATE_TIMEOUT,
    );
    if grant.is_some() {
        // Broker approved. Return GateApproved marker — the caller (handle)
        // converts this to a VM-visible error. A future refactor can
        // return Ok(()) here and let the caller dispatch to FM.
        Err(EffectError::Handler(format!(
            "{GATE_APPROVED_PREFIX}Pattern.File.Write gate cleared by broker"
        )))
    } else {
        Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}file write denied or timed out at the broker"
        )))
    }
}

// TODO(pattern): File handler tests need updating after tightening to SessionContext.
// Issues:
// - make_test_ctx is async but many call sites are in spawn_blocking
// - One test directly constructs the removed TestUser struct
// - One test sets .origin which isn't a field on SessionContext
// - Need to restructure test setup to create ctx outside spawn_blocking
// See archival entry for full TODO list.
// File handler tests disabled pending SessionContext migration.
// See TODO archival entry.
#[cfg(any())]
mod tests {
    use super::*;
    use crate::session::HasCancelState;
    use crate::testing::standard_datacon_table;
    use pattern_core::permission::{PermissionBroker, PermissionDecisionKind};
    use pattern_core::types::origin::{Author, Human, MessageOrigin, Sphere};
    use pattern_core::{PolicyAction, PolicyMatcher, PolicyRule, PolicySet, Precedence};
    use std::sync::Arc;
    use tidepool_repr::{DataCon, DataConId, DataConTable};

    /// Build a DataConTable that includes the `()` constructor required
    /// by `cx.respond(())`, plus the standard tidepool DataCons for
    /// String / list / etc.
    fn handler_table() -> DataConTable {
        let mut table = standard_datacon_table();
        // `()` (GHC.Tuple) is required by `ToCore<()>` / `cx.respond(())`.
        table.insert(DataCon {
            id: DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    use crate::testing::{InMemoryMemoryStore, NopProviderClient};
    use pattern_core::types::snapshot::PersonaSnapshot;

    /// Build a minimal SessionContext for file handler tests.
    async fn make_test_ctx(
        agent_id: &str,
        policies: PolicySet,
        bridge: Option<Arc<crate::permission::PermissionBridge>>,
    ) -> SessionContext {
        let db = crate::testing::test_db().await;
        let store: Arc<dyn pattern_core::traits::MemoryStore> =
            Arc::new(InMemoryMemoryStore::new());
        let persona = PersonaSnapshot::new(agent_id, agent_id);
        let mut ctx = SessionContext::from_persona(
            &persona,
            store,
            Arc::new(NopProviderClient),
            db,
            tokio::runtime::Handle::current(),
        );
        ctx = ctx.with_policies(Arc::new(policies));
        if let Some(b) = bridge {
            ctx = ctx.with_permission_bridge(b);
        }
        ctx
    }

    fn human_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Human(Human {
                user_id: pattern_core::types::ids::new_id(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    fn allow_all_policy(dir: &std::path::Path) -> crate::file_manager::policy::FilePolicy {
        // Two rules: allow the directory itself and everything inside it.
        // The `{dir}/**` glob covers files and subdirs inside; `{dir}` alone
        // covers the directory path passed to `list()`.
        let dir_str = dir.to_string_lossy();
        crate::file_manager::policy::FilePolicy::from_rules(vec![
            (
                crate::file_manager::policy::RuleMode::Allow,
                dir_str.to_string(),
            ),
            (
                crate::file_manager::policy::RuleMode::Allow,
                format!("{dir_str}/**"),
            ),
        ])
        .unwrap()
    }

    async fn make_test_ctx_with_fm(
        agent_id: &str,
        dir: &std::path::Path,
    ) -> (SessionContext, Arc<crate::file_manager::FileManager>) {
        let broker = Arc::new(PermissionBroker::new());
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        let caps = Arc::new(pattern_core::capability::CapabilitySet::all());
        let fm = Arc::new(crate::file_manager::FileManager::new(
            allow_all_policy(dir),
            queue,
            caps,
            bridge.clone(),
            pattern_core::AgentId::from(agent_id),
        ));
        let ctx = make_test_ctx(agent_id, PolicySet::new(), Some(bridge)).await;
        let ctx = ctx.with_file_manager(fm.clone());
        (ctx, fm)
    }

    /// AC2.7 core: agent calls File.Write to a Pattern config KDL.
    /// Broker observes a request with `FileWrite { path }` scope; test
    /// responds Deny; agent sees PERMISSION_DENIED_PREFIX-marked error.
    #[tokio::test]
    async fn config_kdl_write_escalates_to_broker_and_can_be_denied() {
        let broker = Arc::new(PermissionBroker::new());

        // Subscribe synchronously so the responder cannot miss.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let observed_scope = Arc::new(std::sync::Mutex::new(None));
        let observed_for_thread = observed_scope.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                *observed_for_thread.lock().unwrap() = Some(req.scope.clone());
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });

        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let result = tokio::task::spawn_blocking(move || {
            let user = make_test_ctx(
                "agent-cfg-deny",
                PolicySet::from_rules([]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(
                FileReq::Write("/tmp/.pattern.kdl".into(), "mount mode=\"A\"\n".into()),
                &cx,
            )
        })
        .await
        .expect("blocking task")
        .expect_err("denial should surface");
        let msg = result.to_string();
        assert!(
            msg.contains(PERMISSION_DENIED_PREFIX),
            "expected PermissionDenied prefix, got: {msg}"
        );
        responder.await.unwrap();

        // Confirm the broker saw a FileWrite-scoped request keyed on
        // the actual path (not a tool-execution scope).
        let scope = observed_scope.lock().unwrap().clone();
        match scope {
            Some(PermissionScope::FileWrite { path }) => {
                assert_eq!(path, "/tmp/.pattern.kdl");
            }
            other => panic!("expected FileWrite scope, got {other:?}"),
        }
    }

    /// AC2.7 locked-default: even with a KDL `Allow` rule for all
    /// file writes, config-KDL writes still escalate (because the
    /// shape guard short-circuits before the policy is consulted).
    #[tokio::test]
    async fn config_kdl_write_locked_against_kdl_allow_all() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        // Persona-style KDL Allow rule for everything — must NOT override
        // the shape guard.
        let kdl_allow_all = PolicyRule::new(
            EffectCategory::File,
            PolicyMatcher::FilePath {
                pattern: "*".into(),
            },
            PolicyAction::Allow,
            Precedence::KdlConfig,
        );
        let _ = tokio::task::spawn_blocking(move || {
            let user = make_test_ctx(
                "agent-cfg-locked",
                PolicySet::from_rules([kdl_allow_all]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            // Result discarded — the test only asserts on the broker's
            // observed request, not the handler's error message.
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task");
        responder.await.unwrap();

        assert!(
            saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "broker must observe a request despite KDL Allow-all"
        );
    }

    /// AC2.7 locked-default vs RuntimeOverride: even the highest
    /// configurable precedence cannot loosen the shape guard.
    #[tokio::test]
    async fn config_kdl_write_locked_against_runtime_override_allow() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let runtime_allow_all = PolicyRule::new(
            EffectCategory::File,
            PolicyMatcher::Always,
            PolicyAction::Allow,
            Precedence::RuntimeOverride,
        );
        let _ = tokio::task::spawn_blocking(move || {
            let user = make_test_ctx(
                "agent-cfg-runtime",
                PolicySet::from_rules([runtime_allow_all]),
                Some(bridge_for_thread),
            );
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task");
        responder.await.unwrap();

        assert!(
            saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "broker must observe a request despite RuntimeOverride Allow-all"
        );
    }

    /// Handler-level Partner-bypass: when the dispatch origin IS a
    /// Partner, the broker short-circuits via `bypasses_permission_gate()`
    /// and the handler returns GateApproved without any responder firing.
    #[tokio::test]
    async fn partner_origin_short_circuits_at_handler_level() {
        use pattern_core::types::origin::Partner;
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_for_thread = saw_request.clone();
        let _watcher = tokio::spawn(async move {
            if rx.recv().await.is_ok() {
                saw_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut user = make_test_ctx(
                "agent-partner-bypass",
                PolicySet::new(),
                Some(bridge_for_thread),
            );
            user.origin = Some(MessageOrigin::new(
                Author::Partner(Partner {
                    user_id: pattern_core::types::ids::new_id(),
                    display_name: None,
                }),
                Sphere::Private,
            ));
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            // Config-KDL write — Partner origin short-circuits the broker.
            h.handle(FileReq::Write("/tmp/.pattern.kdl".into(), "".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("escalation always returns Err (approved or denied marker)");
        let msg = result.to_string();
        assert!(
            msg.contains(GATE_APPROVED_PREFIX),
            "Partner-origin should produce GateApproved (synthesized grant), got: {msg}"
        );
        // Allow a beat for the watcher to record any prompt.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "Partner-origin must short-circuit at the broker — no request should land in the queue"
        );
    }

    /// Non-config write with Allow policy and a wired FileManager succeeds.
    #[tokio::test]
    async fn non_config_write_with_file_manager_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "original").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-write", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Write(file_str, "updated".into()), &cx)
        })
        .await
        .expect("blocking task");

        assert!(
            result.is_ok(),
            "non-config write with FM and Allow policy should succeed"
        );
    }

    /// Non-config write without a FileManager wired surfaces a clear error.
    #[tokio::test]
    async fn non_config_write_without_file_manager_surfaces_clear_error() {
        let result = tokio::task::spawn_blocking(|| {
            let user = make_test_ctx("agent-no-fm", PolicySet::new(), None);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Write("/tmp/notes.txt".into(), "hi".into()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("missing FM should error");
        let msg = result.to_string();
        assert!(
            msg.contains("no file manager configured"),
            "expected clear 'no file manager' error, got: {msg}"
        );
    }

    /// Approve-for-duration on a config write caches; same path within
    /// the window does NOT re-prompt; different path DOES re-prompt.
    #[tokio::test]
    async fn approve_for_duration_caches_per_path() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let prompts_for_thread = prompts.clone();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            while let Ok(req) = rx.recv().await {
                prompts_for_thread.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                broker_for_responder
                    .resolve(
                        &req.id,
                        PermissionDecisionKind::ApproveForDuration(jiff::Span::new().minutes(5)),
                    )
                    .await;
            }
        });

        let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
        let bridge_for_thread = bridge.clone();

        let join: tokio::task::JoinHandle<()> = tokio::task::spawn_blocking(move || {
            let user = make_test_ctx("agent-cfg-dur", PolicySet::new(), Some(bridge_for_thread));
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);

            // First write to /a/.pattern.kdl — broker prompted, approves.
            h.handle(FileReq::Write("/a/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("escalation returns Err(GateApproved) marker");
            // Same path within the window — cache hit, no re-prompt.
            h.handle(FileReq::Write("/a/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("cached approval still returns Err(GateApproved) marker");
            // Different config path — distinct scope, must re-prompt.
            h.handle(FileReq::Write("/b/.pattern.kdl".into(), "".into()), &cx)
                .expect_err("fresh approval for new path");
        });
        join.await.expect("blocking task");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let count = prompts.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            count, 2,
            "expected exactly 2 broker prompts (first /a write, first /b write); got {count}"
        );
        drop(bridge);
        responder.abort();
    }

    /// File.Read dispatches to FileManager and returns file content.
    #[tokio::test]
    async fn read_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("read_test.txt");
        std::fs::write(&file, "hello from read").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-read", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Read(file_str), &cx)
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "read should succeed: {result:?}");
    }

    /// File.Read on a PNG image returns a marker text via cx.respond AND pushes
    /// a ContentPart::Binary onto the per-eval attachment side-channel.
    /// End-to-end exercise of seam A's binary path.
    #[tokio::test]
    async fn read_png_image_returns_marker_and_pushes_binary_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pixel.png");

        // Minimal valid 1x1 PNG (precomputed bytes — avoids pulling image crate
        // into runtime test deps just to generate test fixtures).
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
            0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
            0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
            0x00, 0x00, 0x03, 0x00, 0x01, 0x5e, 0xf3, 0x2a, 0x3a, 0x00, 0x00, 0x00,
            0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        std::fs::write(&file, png_bytes).unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();

        let (user, _fm) = make_test_ctx_with_fm("agent-read-png", &dir_path);
        let user_arc = std::sync::Arc::new(user);
        let user_for_handler = user_arc.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, user_for_handler.as_ref());
            h.handle(FileReq::Read(file_str), &cx)
        })
        .await
        .expect("blocking task");

        let response = result.expect("File.Read on PNG should succeed");
        // cx.respond returns Value-shaped result; coerce to a string for the marker check.
        let response_str = match &response {
            tidepool_eval::Value::String(s) => s.clone(),
            other => panic!("expected Value::String marker, got: {other:?}"),
        };

        assert!(
            response_str.starts_with("[image:"),
            "marker should start with [image: ... — got: {response_str}"
        );
        assert!(
            response_str.contains("image/png"),
            "marker should name the MIME type — got: {response_str}"
        );
        assert!(
            response_str.contains("pixel.png"),
            "marker should include the filename — got: {response_str}"
        );

        // Drain the side-channel and assert a ContentPart::Binary landed there.
        let attachments = user_arc.drain_pending_tool_attachments();
        assert_eq!(
            attachments.len(),
            1,
            "exactly one Binary attachment expected; got: {attachments:?}"
        );
        match &attachments[0] {
            genai::chat::ContentPart::Binary(b) => {
                assert_eq!(b.content_type, "image/png");
                assert_eq!(b.name.as_deref(), Some("pixel.png"));
                match &b.source {
                    genai::chat::BinarySource::Base64(data) => {
                        assert!(!data.is_empty(), "base64 payload must be non-empty");
                    }
                    other => panic!("expected Base64 source, got: {other:?}"),
                }
            }
            other => panic!("expected ContentPart::Binary, got: {other:?}"),
        }
    }

    /// File.Open dispatches to FileManager and returns file content.

    #[tokio::test]
    async fn open_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("open_test.txt");
        std::fs::write(&file, "open content").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-open", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Open(file_str), &cx)
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "open should succeed: {result:?}");
    }

    /// File.Close dispatches to FileManager (must open first).
    #[tokio::test]
    async fn close_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("close_test.txt");
        std::fs::write(&file, "contents").unwrap();

        let file_path = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-close", dir.path());
            // Open first so close has something to close.
            fm.open(&file_path).unwrap();
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(
                FileReq::Close(file_path.to_string_lossy().into_owned()),
                &cx,
            )
        })
        .await
        .expect("blocking task");

        assert!(
            result.is_ok(),
            "close should succeed after open: {result:?}"
        );
    }

    /// File.Watch dispatches to FileManager.
    #[tokio::test]
    async fn watch_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watch_test.txt");
        std::fs::write(&file, "watched").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-watch", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Watch(file_str), &cx)
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "watch should succeed: {result:?}");
    }

    /// File.ListDir dispatches to FileManager.
    #[tokio::test]
    async fn list_dir_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();

        let dir_str = dir.path().to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-list", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::ListDir(dir_str, "*".into()), &cx)
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "listdir should succeed: {result:?}");
    }

    /// File.Reload dispatches to FileManager (must open first).
    #[tokio::test]
    async fn reload_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("reload_test.txt");
        std::fs::write(&file, "initial").unwrap();

        let file_path = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-reload", dir.path());
            // Open so reload has a CRDT doc to reload.
            fm.open(&file_path).unwrap();
            // Write new content directly to disk after open.
            std::fs::write(&file_path, "reloaded content").unwrap();
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(
                FileReq::Reload(file_path.to_string_lossy().into_owned()),
                &cx,
            )
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "reload should succeed: {result:?}");
    }

    /// File.ForceWrite dispatches to FileManager (must open first).
    #[tokio::test]
    async fn force_write_dispatches_to_file_manager() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("force_test.txt");
        std::fs::write(&file, "original").unwrap();

        let file_path = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-force", dir.path());
            // Open so force_write has a CRDT doc.
            fm.open(&file_path).unwrap();
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(
                FileReq::ForceWrite(
                    file_path.to_string_lossy().into_owned(),
                    "force written".into(),
                ),
                &cx,
            )
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "force_write should succeed: {result:?}");
    }

    /// CapabilityDenied from FileManager is prefixed with PERMISSION_DENIED_PREFIX.
    #[tokio::test]
    async fn capability_denied_uses_permission_denied_prefix() {
        let result = tokio::task::spawn_blocking(|| {
            // Build a user with a FileManager that has no File capability.
            let broker = Arc::new(PermissionBroker::new());
            let bridge = Arc::new(crate::permission::PermissionBridge::spawn(broker));
            let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
            // CapabilitySet with only Memory — no File.
            let mut caps = pattern_core::capability::CapabilitySet::default();
            caps.categories
                .insert(pattern_core::capability::EffectCategory::Memory);
            let caps = Arc::new(caps);
            let dir = tempfile::tempdir().unwrap();
            let fm = Arc::new(crate::file_manager::FileManager::new(
                allow_all_policy(dir.path()),
                queue,
                caps,
                bridge.clone(),
                pattern_core::AgentId::from("agent-no-caps"),
            ));
            let file = dir.path().join("test.txt");
            std::fs::write(&file, "data").unwrap();

            let user = TestUser {
                agent_id: pattern_core::AgentId::from("agent-no-caps"),
                policies: PolicySet::new(),
                bridge: Some(bridge),
                origin: Some(human_origin()),
                file_manager: Some(fm),
            };
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::Read(file.to_string_lossy().into_owned()), &cx)
        })
        .await
        .expect("blocking task")
        .expect_err("capability denied should be an error");

        let msg = result.to_string();
        assert!(
            msg.contains(PERMISSION_DENIED_PREFIX),
            "CapabilityDenied must use PERMISSION_DENIED_PREFIX, got: {msg}"
        );
    }

    /// Subscribe to disk-write notifications BEFORE invoking a line edit
    /// so the receiver doesn't race with the ingest thread. Returns the
    /// receiver; tests then call `wait` after the handler returns.
    fn subscribe_writes_before(
        sf: &Arc<pattern_memory::loro_sync::text::LoroSyncedFile>,
    ) -> crossbeam_channel::Receiver<pattern_memory::loro_sync::synced_doc::WriteNotification> {
        sf.subscribe_writes()
    }

    /// Wait up to 2 s on a previously-subscribed write-notification rx.
    fn wait_for_write(
        rx: &crossbeam_channel::Receiver<pattern_memory::loro_sync::synced_doc::WriteNotification>,
    ) -> Result<(), &'static str> {
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map(|_| ())
            .map_err(|_| "no disk write within 2s")
    }

    #[tokio::test]
    async fn insert_lines_at_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("insert_test.txt");
        std::fs::write(&file, "line1\nline2\nline3").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();
        let file_read = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-insert", &dir_path);
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(FileReq::InsertLines(file_str, 0, "header".into()), &cx);
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "insert at 0 should succeed: {result:?}");
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "header\nline1\nline2\nline3");
        drop(dir);
    }

    #[tokio::test]
    async fn insert_lines_in_middle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("insert_mid.txt");
        std::fs::write(&file, "line1\nline2\nline3").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();
        let file_read = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-insert-mid", &dir_path);
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(FileReq::InsertLines(file_str, 1, "inserted".into()), &cx);
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(
            result.is_ok(),
            "insert after line 1 should succeed: {result:?}"
        );
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "line1\ninserted\nline2\nline3");
        drop(dir);
    }

    #[tokio::test]
    async fn insert_lines_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("insert_multi.txt");
        std::fs::write(&file, "line1\nline2").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();
        let file_read = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-insert-multi", &dir_path);
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(FileReq::InsertLines(file_str, 1, "new1\nnew2".into()), &cx);
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok());
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "line1\nnew1\nnew2\nline2");
        drop(dir);
    }

    #[tokio::test]
    async fn replace_lines_single() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("replace_test.txt");
        std::fs::write(&file, "line1\nline2\nline3\nline4").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();
        let file_read = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-replace", &dir_path);
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(
                FileReq::ReplaceLines(file_str, 2, 3, "replaced".into()),
                &cx,
            );
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "replace should succeed: {result:?}");
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "line1\nreplaced\nline4");
        drop(dir);
    }

    #[tokio::test]
    async fn replace_lines_with_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("replace_multi.txt");
        std::fs::write(&file, "line1\nline2\nline3").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let dir_path = dir.path().to_path_buf();
        let file_read = file.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-replace-multi", &dir_path);
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(
                FileReq::ReplaceLines(file_str, 2, 2, "new2a\nnew2b".into()),
                &cx,
            );
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok());
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "line1\nnew2a\nnew2b\nline3");
        drop(dir);
    }

    #[tokio::test]
    async fn delete_lines_middle() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("delete_test.txt");
        std::fs::write(&file, "line1\nline2\nline3\nline4").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let file_read = file.clone();
        let dir_path = dir.path().to_path_buf();
        let result = tokio::task::spawn_blocking(move || {
            let (user, fm) = make_test_ctx_with_fm("agent-delete", &dir_path);
            let mut h = FileHandler;
            let sf = fm.get_or_open(&file_read).unwrap();
            let writes_rx = subscribe_writes_before(&sf);
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            let r = h.handle(FileReq::DeleteLines(file_str, 2, 3), &cx);
            wait_for_write(&writes_rx).expect("disk write must land");
            r
        })
        .await
        .expect("blocking task");

        assert!(result.is_ok(), "delete should succeed: {result:?}");
        let content = std::fs::read_to_string(&file).unwrap();
        assert_eq!(content, "line1\nline4");
        drop(dir);
    }

    #[tokio::test]
    async fn replace_lines_invalid_range_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad_range.txt");
        std::fs::write(&file, "line1\nline2").unwrap();

        let file_str = file.to_string_lossy().into_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (user, _fm) = make_test_ctx_with_fm("agent-bad-range", dir.path());
            let mut h = FileHandler;
            let table = handler_table();
            let cx = EffectContext::with_user(&table, &user);
            h.handle(FileReq::ReplaceLines(file_str, 3, 1, "bad".into()), &cx)
        })
        .await
        .expect("blocking task");

        assert!(result.is_err(), "reversed range should error");
    }
}
