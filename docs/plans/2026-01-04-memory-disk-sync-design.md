# Memory disk sync

Sync memory block and archival entry contents to disk for auditability, version control, and debugging.

## Goals

- **Auditability**: Human-readable view of what's in agent memory
- **Version control**: Files can be checked into git for history tracking
- **Roundtrip**: Can import from disk to restore or debug memory state
- **Compatibility**: Export format matches existing `MemoryBlockConfig` TOML structure

## File format and structure

### Directory layout

```
memory/
└── {agent_name}/
    ├── {label}.toml           # block metadata
    ├── {label}.content.{ext}  # content file (md/json/jsonl)
    └── archival/
        └── {YYYY-MM}.md       # batched archival entries
```

### Block metadata (`.toml`)

Matches existing `MemoryBlockConfig` format:

```toml
label = "persona"
description = "Core identity and personality traits"
memory_type = "core"
permission = "read_write"
pinned = true
char_limit = 5000
schema = "map"
content_path = "persona.content.json"

# Export-only fields (ignored on import)
id = "block_abc123"
agent_id = "aria"
created_at = "2026-01-04T00:00:00Z"
updated_at = "2026-01-04T12:00:00Z"
```

### Content file extensions by schema

| Schema | Extension | Format |
|--------|-----------|--------|
| `text` | `.content.md` | Markdown |
| `map` | `.content.json` | JSONC (comments allowed) |
| `log` | `.content.jsonl` | JSON Lines (JSONC per line) |
| `list` | `.content.json` | JSONC array |
| `composite` | `.content.json` | JSONC with nested sections |

### Archival entries

Standalone markdown files batched by month. Entries separated by `---` with JSON metadata in code blocks:

```markdown
```json
{"id": "entry_123", "created_at": "2026-01-03T10:00:00Z", "metadata": {"topic": "preferences"}}
```

User prefers morning check-ins. They find them helpful for setting daily priorities and are more receptive to suggestions early in the day.

---

```json
{"id": "entry_124", "created_at": "2026-01-04T14:30:00Z", "metadata": {"topic": "patterns"}}
```

Deadline anxiety peaks 2 days before due date. This is when support should increase and task breakdown becomes more important.

---
```

## Export (sync to disk)

### CLI commands

```bash
# Export single agent
pattern agent sync <name>
pattern agent sync <name> --include-archival

# Export all agents in group
pattern group sync <name>
pattern group sync <name> --include-archival

# Common options
--output-dir ./memory    # default: ./memory
--dry-run                # show what would be written
```

### Export logic

1. Load agent's memory blocks from DB
2. For each block:
   - Write `{label}.toml` with metadata
   - Extract content from Loro doc according to schema
   - Write `{label}.content.{ext}` in appropriate format
3. If `--include-archival`:
   - Query archival entries, group by month
   - Write `archival/{YYYY-MM}.md` files

### Runtime watch mode

Watch mode is a runtime config option, not a CLI flag:

```toml
# In agent config or pattern config
[memory_sync]
enabled = true
output_dir = "./memory"
include_archival = false
```

When agent runs with sync enabled:
- Initial full sync on startup
- Memory operations (create, update, delete) trigger file writes/deletes
- Archival entry creation appends to current month's file
- Hooks into memory layer, not Loro specifically

## Import (load from disk)

Extends existing config import machinery.

### CLI commands

```bash
# Import single block
pattern agent config import <agent> --block ./memory/aria/persona.toml

# Import all blocks from directory
pattern agent config import <agent> --memory-dir ./memory/aria/

# Import archival entries
pattern agent config import <agent> --archival ./memory/aria/archival/2026-01.md
```

### Import behavior

- Schema-aware content parsing (JSONC for maps/logs, markdown for text)
- Uses existing `ConfigPriority` to handle conflicts:
  - `Merge` (default): content from file, preserve existing CRDT history
  - `TomlWins`: replace block entirely (fresh Loro doc)
  - `DbWins`: skip import if block exists
- For archival: parse `---`-separated entries, insert into DB
- Regenerates `id`, `agent_id`, timestamps (label is the semantic key)

### Schema-aware content loading

Extend existing config loading to handle non-text blocks:

```rust
match (schema, content_path.extension()) {
    (Text, "md") => doc.set_text(&content),

    (Map, "json") => {
        let map: Value = parse_jsonc(content);
        for (key, value) in map.as_object() {
            doc.set_field(key, value);
        }
    }

    (Log, "jsonl") => {
        for line in content.lines() {
            let entry: LogEntry = parse_jsonc(line);
            doc.append_log_entry(entry);
        }
    }

    (List, "json") => {
        let items: Vec<Value> = parse_jsonc(content);
        doc.set_list(items);
    }

    (Composite, "json") => {
        // Handle nested sections per schema definition
    }
}
```

JSONC parsing (JSON with comments) keeps files human-editable.

## Implementation phases

### Phase 1: Schema-aware import (prerequisite)

Enables testability for everything else.

- Extend `MemoryBlockConfig` loading to handle non-text schemas
- Add JSONC parsing (strip `//` and `/* */` comments before serde)
- Parse content files according to schema field
- Apply to Loro doc correctly (set_text vs set_field vs append_log_entry)

### Phase 2: Export (one-shot sync)

- Add `pattern agent sync` command
- Implement content extraction from Loro docs by schema type
- Write `.toml` metadata + `.content.{ext}` files
- Add `--include-archival` for archival entry export
- Archival batched by month in markdown format

### Phase 3: Import extensions

- Add `--block`, `--memory-dir`, `--archival` flags to config import
- Parse archival markdown format (JSON code blocks + content + `---` separators)
- Handle `ConfigPriority` for conflict resolution

### Phase 4: Runtime watch mode

- Add `[memory_sync]` config section
- Hook into memory layer for create/update/delete events
- Initial sync on agent startup
- File writes/deletes on memory operations
