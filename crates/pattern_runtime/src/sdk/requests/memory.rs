//! Mirror of `Pattern.Memory` (`haskell/Pattern/Memory.hs`).
//!
//! Variant names mirror the Haskell GADT constructors byte-for-byte via
//! the `#[core(name = "...")]` attribute. `FromCore` dispatches by
//! unqualified DataCon name, so the `Block*` / `Schema*` prefixes on
//! the nested enums are load-bearing — they avoid collisions with
//! other SDK modules' constructor namespaces (e.g. a bare `Log` would
//! clash with `Pattern.Log`'s module namespace in future).

use tidepool_bridge_derive::FromCore;

/// Block classification. Mirrors Haskell `Pattern.Memory.BlockType`.
/// The `Block` prefix is deliberate — see module docs.
#[derive(Debug, FromCore)]
pub enum BlockTypeReq {
    #[core(name = "BlockCore")]
    Core,
    #[core(name = "BlockWorking")]
    Working,
    #[core(name = "BlockArchival")]
    Archival,
    #[core(name = "BlockLog")]
    Log,
}

impl From<BlockTypeReq> for pattern_core::memory::BlockType {
    fn from(req: BlockTypeReq) -> Self {
        use pattern_core::memory::BlockType;
        match req {
            BlockTypeReq::Core => BlockType::Core,
            BlockTypeReq::Working => BlockType::Working,
            BlockTypeReq::Archival => BlockType::Archival,
            BlockTypeReq::Log => BlockType::Log,
        }
    }
}

/// Schema kind tag. Mirrors Haskell `Pattern.Memory.SchemaKind`.
/// The handler fills in nested defaults (e.g. empty `fields` for Map).
#[derive(Debug, FromCore)]
pub enum SchemaKindReq {
    #[core(name = "SchemaText")]
    Text,
    #[core(name = "SchemaMap")]
    Map,
    #[core(name = "SchemaList")]
    List,
    #[core(name = "SchemaLog")]
    Log,
}

impl From<SchemaKindReq> for pattern_core::memory::BlockSchema {
    fn from(req: SchemaKindReq) -> Self {
        use pattern_core::memory::{BlockSchema, LogEntrySchema};
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
#[derive(Debug, FromCore)]
pub enum MemoryReq {
    #[core(name = "Read")]
    Read(String),

    /// `Write label content description`.
    ///
    /// - `description = None`: leave existing metadata untouched (or
    ///   fall through to a default when auto-creating a missing block).
    /// - `description = Some(d)`: set/update the block's description.
    #[core(name = "Write")]
    Write(String, String, Option<String>),

    /// `Create label description block_type schema_kind char_limit initial_content`.
    ///
    /// Explicit block creation with full metadata control. `char_limit = None`
    /// falls back to the runtime's default (`DEFAULT_CHAR_LIMIT`).
    #[core(name = "Create")]
    Create(
        String,
        String,
        BlockTypeReq,
        SchemaKindReq,
        Option<i64>,
        String,
    ),

    #[core(name = "Append")]
    Append(String, String),

    /// `Replace label old new` — string-replace within the block's
    /// rendered text. Errors if the block does not exist.
    #[core(name = "Replace")]
    Replace(String, String, String),

    #[core(name = "Search")]
    Search(String),

    #[core(name = "Recall")]
    Recall(String),

    #[core(name = "Archive")]
    Archive(String),
}
