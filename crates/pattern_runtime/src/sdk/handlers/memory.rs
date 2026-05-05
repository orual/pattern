//! Fully-wired handler for `Pattern.Memory`.
//!
//! All memory operations go through the session context's adapter,
//! which wraps the scoped store (`MemoryScope`). The handler itself
//! is stateless — it does not hold a store reference.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockWrite, BlockWriteKind};
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
use pattern_core::types::origin::{AgentAuthor, Author};
use smol_str::SmolStr;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::MemoryReq;
use crate::session::{SessionContext, record_exchange};
use crate::timeout::{CANCELLED_SENTINEL, HandlerGuard};

const MEMORY_HANDLER_TAG: u32 = 0;

#[derive(Clone)]
pub struct MemoryHandler;

impl std::fmt::Debug for MemoryHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryHandler").finish_non_exhaustive()
    }
}

impl Default for MemoryHandler {
    fn default() -> Self {
        Self
    }
}

impl MemoryHandler {
    pub fn new() -> Self {
        Self
    }
}

impl DescribeEffect for MemoryHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Memory",
            description: "Persistent memory-block operations (Get/Put/Create/Append/Replace/Search/Recall/GetShared/Pin/Unpin/GetSchema/GetField/SetField/UpdateDesc)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Get            :: BlockHandle -> Memory Content",
                "Put            :: BlockHandle -> Content -> Maybe Text -> Memory ()",
                "Create         :: BlockHandle -> Text -> MemoryBlockType -> SchemaKind -> Maybe Int -> Content -> Memory ()",
                "Append         :: BlockHandle -> Content -> Memory ()",
                "Replace        :: BlockHandle -> Text -> Text -> Memory ()",
                "Search         :: Query -> Memory [BlockHandle]",
                "Recall         :: BlockHandle -> Memory Content",
                "GetShared      :: Owner -> BlockHandle -> Memory Content",
                "Pin            :: BlockHandle -> Memory ()",
                "Unpin          :: BlockHandle -> Memory ()",
                "GetSchema      :: BlockHandle -> Memory Text",
                "GetField       :: BlockHandle -> Text -> Memory (Maybe Text)",
                "SetField       :: BlockHandle -> Text -> Text -> Memory ()",
                "UpdateDesc     :: BlockHandle -> Text -> Memory ()",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type BlockHandle = Text",
                "type Content = Text",
                "type Query = Text",
                "type Owner = Text",
                "data MemoryBlockType = BlockCore | BlockWorking | BlockArchival | BlockLog",
                "data SchemaKind = SchemaText | SchemaMap | SchemaList | SchemaLog",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "get :: Member Memory effs => BlockHandle -> Eff effs Content\nget h = send (Get h)",
                "put :: Member Memory effs => BlockHandle -> Content -> Eff effs ()\nput h c = send (Put h c Nothing)",
                "putWithDesc :: Member Memory effs => BlockHandle -> Content -> Text -> Eff effs ()\nputWithDesc h c d = send (Put h c (Just d))",
                "create :: Member Memory effs => BlockHandle -> Text -> MemoryBlockType -> SchemaKind -> Maybe Int -> Content -> Eff effs ()\ncreate h d bt sk cl ic = send (Create h d bt sk cl ic)",
                "append :: Member Memory effs => BlockHandle -> Content -> Eff effs ()\nappend h c = send (Append h c)",
                "replace :: Member Memory effs => BlockHandle -> Text -> Text -> Eff effs ()\nreplace h old new = send (Replace h old new)",
                "search :: Member Memory effs => Query -> Eff effs [BlockHandle]\nsearch q = send (Search q)",
                "recall :: Member Memory effs => BlockHandle -> Eff effs Content\nrecall h = send (Recall h)",
                "getShared :: Member Memory effs => Owner -> BlockHandle -> Eff effs Content\ngetShared o h = send (GetShared o h)",
                "pin :: Member Memory effs => BlockHandle -> Eff effs ()\npin h = send (Pin h)",
                "unpin :: Member Memory effs => BlockHandle -> Eff effs ()\nunpin h = send (Unpin h)",
                "getSchema :: Member Memory effs => BlockHandle -> Eff effs Text\ngetSchema h = send (GetSchema h)",
                "getField :: Member Memory effs => BlockHandle -> Text -> Eff effs (Maybe Text)\ngetField h f = send (GetField h f)",
                "setField :: Member Memory effs => BlockHandle -> Text -> Text -> Eff effs ()\nsetField h f v = send (SetField h f v)",
                "updateDesc :: Member Memory effs => BlockHandle -> Text -> Eff effs ()\nupdateDesc h d = send (UpdateDesc h d)",
            ]),
        }
    }
}

impl EffectHandler<SessionContext> for MemoryHandler {
    type Request = MemoryReq;

    fn handle(
        &mut self,
        req: MemoryReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let state = cx.user().cancel_state();
        if state.cancellation.load(Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{CANCELLED_SENTINEL}: memory handler cancelled at entry"
            )));
        }

        let _guard = HandlerGuard::enter(&state.gate);

        let constructor_name = match &req {
            MemoryReq::Get(_) => "Get",
            MemoryReq::Put(_, _, _) => "Put",
            MemoryReq::Create(_, _, _, _, _, _) => "Create",
            MemoryReq::Append(_, _) => "Append",
            MemoryReq::Replace(_, _, _) => "Replace",
            MemoryReq::Search(_) => "Search",
            MemoryReq::Recall(_) => "Recall",
            MemoryReq::GetShared(_, _) => "GetShared",
            MemoryReq::Pin(_) => "Pin",
            MemoryReq::Unpin(_) => "Unpin",
            MemoryReq::GetSchema(_) => "GetSchema",
            MemoryReq::GetField(_, _) => "GetField",
            MemoryReq::SetField(_, _, _) => "SetField",
            MemoryReq::UpdateDesc(_, _) => "UpdateDesc",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Memory",
            constructor_name,
        )?;

        let agent_id = cx.user().agent_id().to_string();
        // Default routing scope for this session: project-bound sessions
        // route to `Scope::Local(project_id)`; passthrough sessions route
        // to `Scope::Global(persona_id)`. Phase 2 adds an explicit
        // scope arg to the wire and resolves Maybe Scope here.
        let scope = cx.user().default_scope().clone();

        let request_repr = format!("{req:?}");

        let adapter = cx.user().adapter().clone();

        let result = (|| match req {
            MemoryReq::Get(label) => {
                tracing::trace!(
                    agent_id = %agent_id,
                    scope = %scope,
                    label = %label,
                    "Memory.Get: looking up block"
                );
                let result = adapter.get_rendered_content(&scope, &label);
                let text = result
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Get: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Get: no block named {label:?} for scope {scope}"
                        ))
                    })?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_READ,
                    serde_json::json!({ "label": label, "scope": scope.to_string() }),
                ));
                cx.respond(text)
            }
            MemoryReq::Put(label, content, description) => {
                let pre = pre_write_state(&*adapter, &scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Put: {e}")))?;
                upsert_block_content(&*adapter, &scope, &label, &content, description.as_deref())
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Put: {e}")))?;

                let kind = if pre.existed {
                    BlockWriteKind::Replaced
                } else {
                    BlockWriteKind::Created
                };
                record_block_write(
                    RecordBlockWriteParams {
                        adapter: &adapter,
                        scope: &scope,
                        agent_id: &agent_id,
                        label: &label,
                        post_content: &content,
                        kind,
                        pre: &pre,
                    },
                    &*adapter,
                );
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "put" }),
                ));
                cx.respond(())
            }
            MemoryReq::Create(label, description, block_type, schema_kind, char_limit, initial) => {
                let bt: MemoryBlockType = block_type.into();
                let schema: BlockSchema = schema_kind.into();
                let limit = char_limit
                    .map(|n| n.max(0) as usize)
                    .unwrap_or(DEFAULT_CHAR_LIMIT);
                let create =
                    pattern_core::types::block::BlockCreate::new(label.clone(), bt, schema)
                        .with_description(description)
                        .with_char_limit(limit);
                let doc = adapter
                    .create_block(&scope, create)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;
                write_text_into(&doc, &initial)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;
                adapter
                    .mark_dirty(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;
                adapter
                    .persist_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Create: {e}")))?;

                let memory_id = SmolStr::new(doc.id());
                adapter.record_write(BlockWrite {
                    handle: SmolStr::new(&label),
                    memory_id,
                    block_type: doc.block_type(),
                    rendered_content: initial,
                    kind: BlockWriteKind::Created,
                    previous_content_hash: None,
                    previous_rendered_content: None,
                    at: jiff::Timestamp::now(),
                    author: Author::Agent(AgentAuthor {
                        agent_id: SmolStr::new(&agent_id),
                    }),
                });
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "create" }),
                ));
                cx.respond(())
            }
            MemoryReq::Append(label, content) => {
                let pre = pre_write_state(&*adapter, &scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;

                let doc = match adapter
                    .get_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?
                {
                    Some(doc) => doc,
                    None => {
                        let create = pattern_core::types::block::BlockCreate::new(
                            label.clone(),
                            MemoryBlockType::Working,
                            BlockSchema::text(),
                        )
                        .with_description(DEFAULT_AUTO_CREATE_DESCRIPTION)
                        .with_char_limit(DEFAULT_CHAR_LIMIT);
                        adapter.create_block(&scope, create).map_err(|e| {
                            EffectError::Handler(format!("Pattern.Memory.Append: {e}"))
                        })?
                    }
                };

                doc.append(&content, false)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;

                adapter
                    .mark_dirty(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;
                adapter
                    .persist_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Append: {e}")))?;

                let post_content = doc.text_content();
                record_block_write(
                    RecordBlockWriteParams {
                        adapter: &adapter,
                        scope: &scope,
                        agent_id: &agent_id,
                        label: &label,
                        post_content: &post_content,
                        kind: BlockWriteKind::Appended,
                        pre: &pre,
                    },
                    &*adapter,
                );
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "append" }),
                ));
                cx.respond(())
            }
            MemoryReq::Replace(label, old, new) => {
                let pre = pre_write_state(&*adapter, &scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Replace: {e}")))?;
                if !pre.existed {
                    return Err(EffectError::Handler(format!(
                        "Pattern.Memory.Replace: no block named {label:?} for scope {scope}"
                    )));
                }

                let doc = adapter
                    .get_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Replace: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Replace: block {label:?} disappeared between pre_write_state and get_block"
                        ))
                    })?;

                let found = doc
                    .replace_text(&old, &new, false)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Replace: {e}")))?;

                if found {
                    adapter.mark_dirty(&scope, &label).map_err(|e| {
                        EffectError::Handler(format!("Pattern.Memory.Replace: {e}"))
                    })?;
                    adapter.persist_block(&scope, &label).map_err(|e| {
                        EffectError::Handler(format!("Pattern.Memory.Replace: {e}"))
                    })?;

                    let post_content = doc.text_content();
                    record_block_write(
                        RecordBlockWriteParams {
                            adapter: &adapter,
                            scope: &scope,
                            agent_id: &agent_id,
                            label: &label,
                            post_content: &post_content,
                            kind: BlockWriteKind::Replaced,
                            pre: &pre,
                        },
                        &*adapter,
                    );
                }
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "replace" }),
                ));
                cx.respond(())
            }
            MemoryReq::Search(query) => {
                let options = pattern_core::types::memory_types::SearchOptions::new();
                let search_scope =
                    pattern_core::types::memory_types::MemorySearchScope::Scope(scope.clone());
                let results = adapter
                    .search(&query, options, search_scope)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Search: {e}")))?;
                let handles: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_READ,
                    serde_json::json!({ "label": query, "scope": scope.to_string(), "operation": "search" }),
                ));
                cx.respond(handles)
            }
            MemoryReq::Recall(handle) => {
                let entries = adapter
                    .search_archival(&scope, &handle, 1)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Recall: {e}")))?;
                let content = entries
                    .first()
                    .map(|e| e.content.clone())
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.Recall: no archival entry matching {handle:?} for scope {scope}"
                        ))
                    })?;
                cx.respond(content)
            }
            MemoryReq::GetShared(owner, label) => {
                // Cross-agent shared block access. Both requester and owner
                // are persona-scoped (Global) since shared blocks are
                // owned by personas, not projects.
                let requester = cx.user().persona_scope();
                let owner_scope = Scope::Global(owner.clone().into());
                let doc = adapter
                    .get_shared_block(&requester, &owner_scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.GetShared: {e}")))?
                    .ok_or_else(|| {
                        EffectError::Handler(format!(
                            "Pattern.Memory.GetShared: no shared block \
                             label={label:?} from owner={owner:?} accessible \
                             to scope={requester}"
                        ))
                    })?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_SHARED_READ,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "get_shared", "owner": owner }),
                ));
                cx.respond(doc.render())
            }
            MemoryReq::Pin(label) => {
                let patch = pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(true);
                adapter.update_block_metadata(&scope, &label, patch)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Pin: {e}")))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "pin" }),
                ));
                cx.respond(())
            }
            MemoryReq::Unpin(label) => {
                let patch = pattern_core::types::memory_types::BlockMetadataPatch::default().pinned(false);
                adapter.update_block_metadata(&scope, &label, patch)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.Unpin: {e}")))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "unpin" }),
                ));
                cx.respond(())
            }
            MemoryReq::GetSchema(label) => {
                let doc = adapter
                    .get_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.GetSchema: {e}")))?
                    .ok_or_else(|| EffectError::Handler(format!("Pattern.Memory.GetSchema: no block {label:?}")))?;
                let schema_name = match doc.schema() {
                    pattern_core::types::memory_types::BlockSchema::Text { .. } => "text",
                    pattern_core::types::memory_types::BlockSchema::Map { .. } => "map",
                    pattern_core::types::memory_types::BlockSchema::List { .. } => "list",
                    pattern_core::types::memory_types::BlockSchema::Log { .. } => "log",
                    pattern_core::types::memory_types::BlockSchema::Composite { .. } => "composite",
                    pattern_core::types::memory_types::BlockSchema::TaskList { .. } => "tasklist",
                    pattern_core::types::memory_types::BlockSchema::Skill { .. } => "skill",
                    _ => "unknown",
                };
                cx.respond(schema_name.to_string())
            }
            MemoryReq::GetField(label, field) => {
                let doc = adapter
                    .get_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.GetField: {e}")))?
                    .ok_or_else(|| EffectError::Handler(format!("Pattern.Memory.GetField: no block {label:?}")))?;
                let value = doc.get_field(&field)
                    .map(|v| serde_json::to_string(&v).unwrap_or_default());
                cx.respond(value)
            }
            MemoryReq::SetField(label, field, value_json) => {
                let json_val: serde_json::Value = serde_json::from_str(&value_json)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.SetField: invalid JSON: {e}")))?;
                let doc = adapter
                    .get_block(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.SetField: {e}")))?
                    .ok_or_else(|| EffectError::Handler(format!("Pattern.Memory.SetField: no block {label:?}")))?;
                doc.set_field(&field, json_val, false)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.SetField: {e}")))?;
                adapter.mark_dirty(&scope, &label)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.SetField: {e}")))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "set_field", "field": field }),
                ));
                cx.respond(())
            }
            MemoryReq::UpdateDesc(label, desc) => {
                let patch = pattern_core::types::memory_types::BlockMetadataPatch::default()
                    .description(desc);
                adapter.update_block_metadata(&scope, &label, patch)
                    .map_err(|e| EffectError::Handler(format!("Pattern.Memory.UpdateDesc: {e}")))?;
                cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
                    pattern_core::hooks::tags::MEMORY_WRITE,
                    serde_json::json!({ "label": label, "scope": scope.to_string(), "operation": "update_desc" }),
                ));
                cx.respond(())
            }
        })();

        if let Ok(ref value) = result {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            record_exchange(&log, MEMORY_HANDLER_TAG, request_repr, value, turn);
        }
        result
    }
}

/// Upsert a block's content. If the block does not exist, create it as a
/// Working block with a Text schema; otherwise replace its rendered text
/// and persist.
fn upsert_block_content(
    store: &dyn MemoryStore,
    scope: &Scope,
    label: &str,
    content: &str,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let existing = store.get_block(scope, label)?;
    let (doc, is_new) = match existing {
        Some(doc) => (doc, false),
        None => {
            let desc = description.unwrap_or(DEFAULT_AUTO_CREATE_DESCRIPTION);
            let create = pattern_core::types::block::BlockCreate::new(
                label.to_owned(),
                MemoryBlockType::Working,
                BlockSchema::text(),
            )
            .with_description(desc)
            .with_char_limit(DEFAULT_CHAR_LIMIT);
            let doc = store.create_block(scope, create)?;
            (doc, true)
        }
    };
    write_text_into(&doc, content)?;
    if let (false, Some(desc)) = (is_new, description) {
        store.update_block_metadata(
            scope,
            label,
            pattern_core::types::memory_types::BlockMetadataPatch::default().description(desc),
        )?;
    }
    store.mark_dirty(scope, label)?;
    store.persist_block(scope, label)?;
    Ok(())
}

const DEFAULT_AUTO_CREATE_DESCRIPTION: &str =
    "auto-created by Pattern.Memory.write (no description supplied)";

fn write_text_into(
    doc: &StructuredDocument,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    doc.set_text(content, false)?;
    Ok(())
}

const DEFAULT_CHAR_LIMIT: usize = 4096;

struct PreWriteState {
    existed: bool,
    rendered_content: Option<String>,
    content_hash: Option<u64>,
    memory_id: Option<SmolStr>,
    block_type: Option<MemoryBlockType>,
}

fn pre_write_state(
    store: &dyn MemoryStore,
    scope: &Scope,
    label: &str,
) -> Result<PreWriteState, Box<dyn std::error::Error + Send + Sync>> {
    match store.get_block(scope, label)? {
        Some(doc) => {
            let rendered = doc.text_content();
            let hash = content_hash(&rendered);
            Ok(PreWriteState {
                existed: true,
                rendered_content: Some(rendered),
                content_hash: Some(hash),
                memory_id: Some(SmolStr::new(doc.id())),
                block_type: Some(doc.block_type()),
            })
        }
        None => Ok(PreWriteState {
            existed: false,
            rendered_content: None,
            content_hash: None,
            memory_id: None,
            block_type: None,
        }),
    }
}

fn content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

struct RecordBlockWriteParams<'a> {
    adapter: &'a crate::memory::MemoryStoreAdapter,
    scope: &'a Scope,
    agent_id: &'a str,
    label: &'a str,
    post_content: &'a str,
    kind: BlockWriteKind,
    pre: &'a PreWriteState,
}

fn record_block_write(params: RecordBlockWriteParams<'_>, store: &dyn MemoryStore) {
    let RecordBlockWriteParams {
        adapter,
        scope,
        agent_id,
        label,
        post_content,
        kind,
        pre,
    } = params;

    let (memory_id, block_type) = match (&pre.memory_id, &pre.block_type) {
        (Some(mid), Some(bt)) => (mid.clone(), *bt),
        _ => match store.get_block(scope, label) {
            Ok(Some(doc)) => (SmolStr::new(doc.id()), doc.block_type()),
            _ => (SmolStr::new("unknown"), MemoryBlockType::Working),
        },
    };

    adapter.record_write(BlockWrite {
        handle: SmolStr::new(label),
        memory_id,
        block_type,
        rendered_content: post_content.to_string(),
        kind,
        previous_content_hash: pre.content_hash,
        previous_rendered_content: pre.rendered_content.clone(),
        at: jiff::Timestamp::now(),
        author: Author::Agent(AgentAuthor {
            agent_id: SmolStr::new(agent_id),
        }),
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::NopProviderClient;
    use crate::testing::standard_datacon_table;
    use crate::timeout::CancelState;
    use pattern_core::ProviderClient;
    use pattern_core::types::snapshot::PersonaSnapshot;

    #[derive(Debug)]
    struct NeverStore;

    impl MemoryStore for NeverStore {
        fn create_or_replace_block(&self, _scope: &Scope, _create: BlockCreate) -> MemoryResult<StructuredDocument> { unreachable!() }
        fn create_block(
            &self,
            _s: &Scope,
            _create: pattern_core::types::block::BlockCreate,
        ) -> pattern_core::types::memory_types::MemoryResult<pattern_core::memory::StructuredDocument>
        {
            panic!("NeverStore")
        }
        fn get_block(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::memory::StructuredDocument>,
        > {
            panic!("NeverStore")
        }
        fn get_block_metadata(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::types::memory_types::BlockMetadata>,
        > {
            panic!()
        }
        fn list_blocks(
            &self,
            _f: pattern_core::types::memory_types::BlockFilter,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::BlockMetadata>,
        > {
            panic!()
        }
        fn delete_block(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn get_rendered_content(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<Option<String>> {
            panic!()
        }
        fn persist_block(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn mark_dirty(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            Ok(())
        }
        fn insert_archival(
            &self,
            _s: &Scope,
            _c: &str,
            _m: Option<serde_json::Value>,
        ) -> pattern_core::types::memory_types::MemoryResult<String> {
            panic!()
        }
        fn search_archival(
            &self,
            _s: &Scope,
            _q: &str,
            _n: usize,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::ArchivalEntry>,
        > {
            panic!()
        }
        fn delete_archival(
            &self,
            _id: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn search(
            &self,
            _q: &str,
            _o: pattern_core::types::memory_types::SearchOptions,
            _s: pattern_core::types::memory_types::MemorySearchScope,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::MemorySearchResult>,
        > {
            panic!()
        }
        fn list_shared_blocks(
            &self,
            _s: &Scope,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Vec<pattern_core::types::memory_types::SharedBlockInfo>,
        > {
            panic!()
        }
        fn get_shared_block(
            &self,
            _r: &Scope,
            _o: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            Option<pattern_core::memory::StructuredDocument>,
        > {
            panic!()
        }
        fn update_block_metadata(
            &self,
            _s: &Scope,
            _l: &str,
            _p: pattern_core::types::memory_types::BlockMetadataPatch,
        ) -> pattern_core::types::memory_types::MemoryResult<()> {
            panic!()
        }
        fn undo_redo(
            &self,
            _s: &Scope,
            _l: &str,
            _op: pattern_core::types::memory_types::UndoRedoOp,
        ) -> pattern_core::types::memory_types::MemoryResult<bool> {
            panic!()
        }
        fn history_depth(
            &self,
            _s: &Scope,
            _l: &str,
        ) -> pattern_core::types::memory_types::MemoryResult<
            pattern_core::types::memory_types::UndoRedoDepth,
        > {
            panic!()
        }
    }

    async fn sctx() -> SessionContext {
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        SessionContext::from_persona(
            &persona,
            Arc::new(NeverStore),
            Arc::new(NopProviderClient),
            db,
            tokio::runtime::Handle::current(),
        )
    }

    async fn sctx_with_store() -> (SessionContext, Arc<dyn MemoryStore>) {
        use crate::testing::InMemoryMemoryStore;
        let db = crate::testing::test_db().await;
        let persona = PersonaSnapshot::new("agent-a", "A");
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let ctx = SessionContext::from_persona(
            &persona,
            store.clone(),
            Arc::new(NopProviderClient),
            db,
            tokio::runtime::Handle::current(),
        );
        (ctx, store)
    }

    #[tokio::test]
    async fn search_delegates_to_store_fts() {
        let table = standard_datacon_table();
        let (ctx, _store) = sctx_with_store().await;
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new();
        let result = h.handle(MemoryReq::Search("anything".into()), &cx);
        assert!(result.is_ok(), "search should succeed, got: {result:?}");
    }

    #[tokio::test]
    async fn recall_returns_not_found_when_no_archival() {
        let table = standard_datacon_table();
        let (ctx, _store) = sctx_with_store().await;
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new();
        let err = h
            .handle(MemoryReq::Recall("block".into()), &cx)
            .unwrap_err();
        assert!(
            err.to_string().contains("Pattern.Memory.Recall"),
            "error should identify op; got: {err}"
        );
        assert!(
            err.to_string().contains("block"),
            "error should identify handle; got: {err}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replace_on_missing_block_returns_handler_error() {
        use crate::testing::InMemoryMemoryStore;
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let db = crate::testing::test_db().await;
        let provider_for_ctx = provider.clone();
        let err_msg = tokio::task::spawn_blocking(move || {
            let table = standard_datacon_table();
            let persona = PersonaSnapshot::new("agent-a", "A");
            let ctx = SessionContext::from_persona(
                &persona,
                store,
                provider_for_ctx,
                db,
                tokio::runtime::Handle::current(),
            );
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MemoryHandler::new();
            let err = h
                .handle(
                    MemoryReq::Replace("ghost".into(), "a".into(), "b".into()),
                    &cx,
                )
                .unwrap_err();
            err.to_string()
        })
        .await
        .expect("spawn_blocking task panicked");
        assert!(
            err_msg.contains("Pattern.Memory.Replace"),
            "error should identify op; got: {err_msg}"
        );
        assert!(
            err_msg.contains("ghost"),
            "error should identify missing label; got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn cancelled_flag_short_circuits_at_entry() {
        let table = standard_datacon_table();
        let ctx = sctx().await;
        ctx.cancel_state()
            .cancellation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MemoryHandler::new();
        let err = h.handle(MemoryReq::Get("any".into()), &cx).unwrap_err();
        assert!(err.to_string().contains(CANCELLED_SENTINEL), "got: {err}");
        let _ = CancelState::new();
    }
}
