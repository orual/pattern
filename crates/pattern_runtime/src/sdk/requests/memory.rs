//! Mirror of `Pattern.Memory` (`haskell/Pattern/Memory.hs`).
//!
//! Every variant carries `#[core(module = "Pattern.Memory", name = "...")]`
//! so `FromCore` dispatches via `get_by_qualified_name` — fully
//! disambiguating against other SDK modules even if a future rename
//! reintroduced a name+arity collision. Pattern's current SDK already
//! uses distinct unqualified names across modules (Memory uses
//! `Get`/`Put`, File uses `Read`/`Write`), but the module-qualified
//! derive attribute is kept as defense in depth. The `Block*` /
//! `Schema*` prefixes on the nested enums remain only for source-level
//! clarity; disambiguation is name-qualification rather than
//! name-prefixing.

use tidepool_bridge_derive::FromCore;

/// Block classification. Mirrors Haskell `Pattern.Memory.MemoryBlockType`.
/// The `Block` prefix is deliberate — see module docs.
///
/// `BlockArchival` and `BlockLog` are kept in the Haskell SDK for
/// backwards compatibility but map to `Working` on the Rust side.
/// Archival storage uses the `archival_entries` table; log-type blocks
/// use `Working` tier with `BlockSchema::Log`. Full SDK-side removal
/// is tracked for v3-memory-rework Phase 3.
#[derive(Debug, FromCore)]
pub enum BlockTypeReq {
    #[core(module = "Pattern.Memory", name = "BlockCore")]
    Core,
    #[core(module = "Pattern.Memory", name = "BlockWorking")]
    Working,
    /// Legacy: maps to Working. Archival storage uses archival_entries.
    #[core(module = "Pattern.Memory", name = "BlockArchival")]
    Archival,
    /// Legacy: maps to Working with log-schema metadata.
    #[core(module = "Pattern.Memory", name = "BlockLog")]
    Log,
}

impl From<BlockTypeReq> for pattern_core::types::memory_types::MemoryBlockType {
    fn from(req: BlockTypeReq) -> Self {
        use pattern_core::types::memory_types::MemoryBlockType;
        match req {
            BlockTypeReq::Core => MemoryBlockType::Core,
            // Archival and Log map to Working; archival storage uses the
            // archival_entries table, log blocks use Working + log-schema.
            BlockTypeReq::Working | BlockTypeReq::Archival | BlockTypeReq::Log => {
                MemoryBlockType::Working
            }
        }
    }
}

/// Schema kind tag. Mirrors Haskell `Pattern.Memory.SchemaKind`.
/// The handler fills in nested defaults (e.g. empty `fields` for Map).
#[derive(Debug, FromCore)]
pub enum SchemaKindReq {
    #[core(module = "Pattern.Memory", name = "SchemaText")]
    Text,
    #[core(module = "Pattern.Memory", name = "SchemaMap")]
    Map,
    #[core(module = "Pattern.Memory", name = "SchemaList")]
    List,
    #[core(module = "Pattern.Memory", name = "SchemaLog")]
    Log,
}

impl From<SchemaKindReq> for pattern_core::types::memory_types::BlockSchema {
    fn from(req: SchemaKindReq) -> Self {
        use pattern_core::types::memory_types::{BlockSchema, LogEntrySchema};
        match req {
            SchemaKindReq::Text => BlockSchema::text(),
            SchemaKindReq::Map => BlockSchema::Map { fields: vec![] },
            SchemaKindReq::List => BlockSchema::List {
                item_schema: None,
                max_items: None,
            },
            // Minimum-viable Log: display the last 10 entries, no custom
            // fields, no timestamp/agent_id auto-fields. Agents that need
            // a richer Log schema should construct the block via a
            // different code path (there is no effect yet for
            // fine-grained schema tuning).
            SchemaKindReq::Log => BlockSchema::Log {
                display_limit: 10,
                entry_schema: LogEntrySchema {
                    timestamp: false,
                    agent_id: false,
                    fields: vec![],
                },
            },
        }
    }
}

/// Rust mirror of the Haskell `Memory` GADT.
///
/// Uses `Get`/`Put` rather than `Read`/`Write` so the module composes
/// cleanly with `Pattern.File` (which owns `Read`/`Write` semantically).
/// Agents can import `Pattern.Prelude` unqualified or mix Memory + File
/// via qualified imports without Haskell-level collisions either way.
#[derive(Debug, FromCore)]
pub enum MemoryReq {
    #[core(module = "Pattern.Memory", name = "Get")]
    Get(String),

    /// `Put label content description`.
    ///
    /// - `description = None`: leave existing metadata untouched (or
    ///   fall through to a default when auto-creating a missing block).
    /// - `description = Some(d)`: set/update the block's description.
    #[core(module = "Pattern.Memory", name = "Put")]
    Put(String, String, Option<String>),

    /// `Create label description block_type schema_kind char_limit initial_content`.
    ///
    /// Explicit block creation with full metadata control. `char_limit = None`
    /// falls back to the runtime's default (`DEFAULT_CHAR_LIMIT`).
    #[core(module = "Pattern.Memory", name = "Create")]
    Create(
        String,
        String,
        BlockTypeReq,
        SchemaKindReq,
        Option<i64>,
        String,
    ),

    #[core(module = "Pattern.Memory", name = "Append")]
    Append(String, String),

    /// `Replace label old new` — string-replace within the block's
    /// rendered text. Errors if the block does not exist.
    #[core(module = "Pattern.Memory", name = "Replace")]
    Replace(String, String, String),

    #[core(module = "Pattern.Memory", name = "Search")]
    Search(String),

    #[core(module = "Pattern.Memory", name = "Recall")]
    Recall(String),

    /// `GetShared owner label` — fetch a block owned by another agent
    /// that has been shared with the caller.
    #[core(module = "Pattern.Memory", name = "GetShared")]
    GetShared(String, String),

    #[core(module = "Pattern.Memory", name = "Pin")]
    Pin(String),

    #[core(module = "Pattern.Memory", name = "Unpin")]
    Unpin(String),

    #[core(module = "Pattern.Memory", name = "GetSchema")]
    GetSchema(String),

    #[core(module = "Pattern.Memory", name = "GetField")]
    GetField(String, String),

    #[core(module = "Pattern.Memory", name = "SetField")]
    SetField(String, String, String),

    #[core(module = "Pattern.Memory", name = "UpdateDesc")]
    UpdateDesc(String, String),
}
