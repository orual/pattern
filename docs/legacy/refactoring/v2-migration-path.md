# Pattern v2: Migration Path

## Overview

Migration from v1 to v2 uses the existing CAR (Content Addressable aRchive) export/import system. The compatibility layer lives at this boundary - v1 exports, v2 imports and transforms.

## Existing CAR Export Structure

v1 already has a solid export format:

### Export Types
- `Agent` - Single agent with messages and memories
- `Group` - Agent group with member references
- `Constellation` - Full constellation with all groups and agents

### Data Structures

```rust
// Manifest (root of CAR file)
ExportManifest {
    version: u32,           // Currently 1
    exported_at: DateTime,
    export_type: ExportType,
    stats: ExportStats,
    data_cid: Cid,          // Points to actual export data
}

// Agent export
AgentRecordExport {
    id, name, agent_type,
    model_id, model_config,
    base_instructions,
    // ... config fields ...
    owner_id: UserId,       // v1 uses user ownership
    message_chunks: Vec<Cid>,
    memory_chunks: Vec<Cid>,
}

// Message chunk
MessageChunk {
    chunk_id: u32,
    start_position: String,  // Snowflake ID
    end_position: String,
    messages: Vec<(Message, AgentMessageRelation)>,
    next_chunk: Option<Cid>,
}

// Memory chunk
MemoryChunk {
    chunk_id: u32,
    memories: Vec<(MemoryBlock, AgentMemoryRelation)>,
    next_chunk: Option<Cid>,
}
```

## Migration Strategy

### Phase 1: Export from v1

Use existing `pattern-cli export` commands:

```bash
# Export single agent
pattern-cli export agent --name "MyAgent" -o agent.car

# Export group
pattern-cli export group --name "MyGroup" -o group.car

# Export full constellation
pattern-cli export constellation --name "MyConstellation" -o constellation.car
```

### Phase 2: Transform During Import

The v2 importer reads v1 CAR files and transforms:

```rust
pub struct V2Importer {
    db: ConstellationDb,
}

impl V2Importer {
    pub async fn import_v1_car(&self, car_path: &Path) -> Result<ImportResult> {
        // 1. Read CAR file
        let car_reader = CarReader::new(File::open(car_path)?).await?;
        
        // 2. Parse manifest
        let manifest = self.read_manifest(&car_reader).await?;
        
        // 3. Transform based on export type
        match manifest.export_type {
            ExportType::Agent => self.import_agent(&car_reader, &manifest).await,
            ExportType::Group => self.import_group(&car_reader, &manifest).await,
            ExportType::Constellation => self.import_constellation(&car_reader, &manifest).await,
        }
    }
}
```

### Key Transformations

#### 1. Memory Ownership Change

v1: Memories owned by User, accessed by Agent via relation
v2: Memories owned by Agent directly

```rust
fn transform_memory(
    v1_memory: &V1MemoryBlock,
    v1_relation: &AgentMemoryRelation,
    target_agent_id: &AgentId,
) -> V2MemoryBlock {
    // Create Loro document with content
    let doc = LoroDoc::new();
    doc.get_text("content").insert(0, &v1_memory.value);
    
    V2MemoryBlock {
        id: MemoryBlockId::generate(),
        agent_id: target_agent_id.clone(),  // Now agent-owned
        label: v1_memory.label.clone(),
        description: generate_description(&v1_memory.label),  // Add description
        block_type: map_memory_type(v1_memory.memory_type),
        char_limit: 5000,  // Default
        read_only: v1_relation.access_level < MemoryPermission::Append,
        loro_snapshot: doc.export(ExportMode::Snapshot),
        content_preview: truncate(&v1_memory.value, 200),
        created_at: v1_memory.created_at,
        updated_at: v1_memory.updated_at,
    }
}

fn generate_description(label: &str) -> String {
    match label {
        "persona" => "Stores details about your current persona, guiding how you behave and respond.".into(),
        "human" => "Stores key details about the person you are conversing with.".into(),
        label if label.starts_with("archival_") => format!("Archival memory entry: {}", label),
        _ => format!("Memory block: {}", label),
    }
}

fn map_memory_type(v1_type: V1MemoryType) -> V2MemoryBlockType {
    match v1_type {
        V1MemoryType::Core => V2MemoryBlockType::Core,
        V1MemoryType::Working => V2MemoryBlockType::Working,
        V1MemoryType::Archival => V2MemoryBlockType::Archival,
    }
}
```

#### 2. Message Migration

Messages stay largely the same, but move to SQLite:

```rust
fn transform_message(
    v1_msg: &V1Message,
    v1_relation: &AgentMessageRelation,
    target_agent_id: &AgentId,
) -> V2Message {
    V2Message {
        id: v1_msg.id.to_string(),
        agent_id: target_agent_id.clone(),
        position: v1_relation.position.to_string(),
        batch_id: v1_relation.batch.map(|b| b.to_string()),
        sequence_in_batch: v1_relation.sequence_num,
        role: v1_msg.role.to_string(),
        content: v1_msg.content.clone(),
        tool_call_id: v1_msg.tool_call_id.clone(),
        tool_name: v1_msg.tool_name.clone(),
        tool_args: v1_msg.tool_args.clone(),
        tool_result: v1_msg.tool_result.clone(),
        source: None,  // Lost in v1 export
        source_metadata: None,
        is_archived: v1_relation.message_type == MessageRelationType::Archived,
        created_at: v1_msg.created_at.to_rfc3339(),
    }
}
```

#### 3. Agent Configuration

Agent config maps mostly 1:1, but stored differently:

```rust
fn transform_agent(v1_agent: &AgentRecordExport) -> V2Agent {
    V2Agent {
        id: v1_agent.id.to_string(),
        name: v1_agent.name.clone(),
        description: None,  // New field, not in v1
        model_provider: extract_provider(&v1_agent.model_id),
        model_name: extract_model(&v1_agent.model_id),
        system_prompt: v1_agent.base_instructions.clone(),
        config: json!({
            "max_messages": v1_agent.max_messages,
            "max_message_age_hours": v1_agent.max_message_age_hours,
            "compression_threshold": v1_agent.compression_threshold,
            "memory_char_limit": v1_agent.memory_char_limit,
            "enable_thinking": v1_agent.enable_thinking,
        }),
        enabled_tools: vec!["context", "recall", "search", "send_message"],  // Defaults
        tool_rules: transform_tool_rules(&v1_agent.tool_rules),
        status: "active".into(),
        created_at: v1_agent.created_at.to_rfc3339(),
        updated_at: v1_agent.updated_at.to_rfc3339(),
    }
}
```

#### 4. Group/Constellation Structure

Groups and constellations map cleanly:

```rust
fn transform_constellation(
    v1_const: &V1Constellation,
    v1_groups: &[V1GroupExport],
) -> (V2Constellation, Vec<V2AgentGroup>) {
    // Constellation becomes the database directory
    let constellation = V2Constellation {
        id: v1_const.id.to_string(),
        owner_id: v1_const.owner_id.to_string(),
        name: v1_const.name.clone(),
        db_path: format!("constellations/{}", v1_const.id),
        created_at: v1_const.created_at.to_rfc3339(),
        last_accessed_at: Utc::now().to_rfc3339(),
    };
    
    let groups = v1_groups.iter().map(|g| transform_group(g)).collect();
    
    (constellation, groups)
}
```

### Phase 3: Verification

After import, verify data integrity:

```rust
pub struct ImportVerifier {
    db: ConstellationDb,
}

impl ImportVerifier {
    pub async fn verify(&self, result: &ImportResult) -> Result<VerificationReport> {
        let mut report = VerificationReport::default();
        
        // Check agent count matches
        let agents = sqlx::query!("SELECT COUNT(*) as count FROM agents")
            .fetch_one(self.db.pool())
            .await?;
        report.agents_imported = agents.count as usize;
        report.agents_expected = result.expected_agents;
        
        // Check message count
        let messages = sqlx::query!("SELECT COUNT(*) as count FROM messages")
            .fetch_one(self.db.pool())
            .await?;
        report.messages_imported = messages.count as usize;
        report.messages_expected = result.expected_messages;
        
        // Check memory blocks
        let blocks = sqlx::query!("SELECT COUNT(*) as count FROM memory_blocks")
            .fetch_one(self.db.pool())
            .await?;
        report.blocks_imported = blocks.count as usize;
        report.blocks_expected = result.expected_blocks;
        
        // Sample content verification
        report.sample_checks = self.verify_samples(result).await?;
        
        Ok(report)
    }
}
```

## CLI Commands

```bash
# v2 import command
pattern-cli import --from v1.car --constellation "MyConstellation"

# With verification
pattern-cli import --from v1.car --constellation "MyConstellation" --verify

# Dry run (parse and transform, don't write)
pattern-cli import --from v1.car --dry-run

# Import with explicit version
pattern-cli import --from v1.car --version 1
```

## Export Version Bumping

v2 exports will use version 2 format:

```rust
pub const EXPORT_VERSION: u32 = 2;

// v2 export changes:
// - Includes Loro snapshots instead of raw content
// - Agent-scoped memories (no separate relation)
// - SQLite-native types
```

v2 importer will handle both:

```rust
match manifest.version {
    1 => self.import_v1(&car_reader).await,
    2 => self.import_v2(&car_reader).await,
    v => Err(CoreError::UnsupportedExportVersion(v)),
}
```

## Rollback Strategy

If v2 import fails or has issues:

1. Original v1 CAR file is preserved (never modified)
2. v2 constellation DB can be deleted and re-imported
3. v1 system remains functional until migration verified

```bash
# Keep v1 running alongside v2 during transition
pattern-cli-v1 chat --agent "MyAgent"  # Uses SurrealDB
pattern-cli-v2 chat --agent "MyAgent"  # Uses SQLite

# Once verified, decommission v1
```

## Interactive Migration

v1 accumulated data quality issues that can't be automatically fixed:
- Memory cross-contamination between agents
- Attribution errors (wrong agent_id on memories)
- Duplicate/conflicting blocks with same label
- Orphaned data without valid agent references

The migration tool provides an **interactive review mode** to resolve these.

### Issue Detection

During import, the migrator scans for problems:

```rust
pub struct MigrationIssue {
    pub id: IssueId,
    pub severity: IssueSeverity,
    pub issue_type: IssueType,
    pub description: String,
    pub affected_items: Vec<AffectedItem>,
    pub suggested_actions: Vec<SuggestedAction>,
}

pub enum IssueSeverity {
    /// Blocks import until resolved
    Critical,
    /// Should review, has default resolution
    Warning,
    /// Informational, auto-resolved
    Info,
}

pub enum IssueType {
    /// Same label exists for multiple agents
    DuplicateLabel {
        label: String,
        agents: Vec<AgentId>,
        contents: Vec<String>,
    },
    
    /// Memory content suggests wrong attribution
    SuspiciousAttribution {
        memory_id: MemoryBlockId,
        current_agent: AgentId,
        likely_agent: AgentId,
        evidence: String,  // why we think it's wrong
    },
    
    /// Memory references agent that doesn't exist
    OrphanedMemory {
        memory_id: MemoryBlockId,
        referenced_agent: String,
    },
    
    /// Message has no valid agent reference
    OrphanedMessage {
        message_id: MessageId,
        context: String,
    },
    
    /// Persona/human block has content from wrong agent
    CrossContamination {
        block_label: String,
        owning_agent: AgentId,
        contaminating_agent: AgentId,
        contaminated_content: String,
    },
    
    /// Content looks corrupted or truncated
    CorruptContent {
        item_type: String,
        item_id: String,
        issue: String,
    },
    
    /// Timestamp ordering issues
    TimestampAnomaly {
        item_type: String,
        description: String,
    },
}

pub enum SuggestedAction {
    /// Keep as-is, assign to this agent
    AssignTo(AgentId),
    /// Delete this item
    Delete,
    /// Merge with another item
    MergeWith(String),
    /// Split into multiple items
    Split(Vec<String>),
    /// Keep both versions
    KeepBoth,
    /// Manual edit required
    ManualEdit,
    /// Skip/ignore
    Skip,
}
```

### Detection Heuristics

```rust
impl MigrationAnalyzer {
    /// Detect suspicious attribution based on content
    fn detect_attribution_issues(&self, memories: &[V1MemoryExport]) -> Vec<MigrationIssue> {
        let mut issues = Vec::new();
        
        for memory in memories {
            // Check if persona block mentions another agent's name
            if memory.label == "persona" {
                for agent in &self.all_agents {
                    if agent.id != memory.agent_id 
                        && memory.value.contains(&agent.name) 
                        && memory.value.contains("I am") 
                    {
                        issues.push(MigrationIssue {
                            severity: IssueSeverity::Warning,
                            issue_type: IssueType::SuspiciousAttribution {
                                memory_id: memory.id.clone(),
                                current_agent: memory.agent_id.clone(),
                                likely_agent: agent.id.clone(),
                                evidence: format!(
                                    "Persona block says 'I am' and mentions '{}' but belongs to '{}'",
                                    agent.name, 
                                    self.get_agent_name(&memory.agent_id)
                                ),
                            },
                            suggested_actions: vec![
                                SuggestedAction::AssignTo(agent.id.clone()),
                                SuggestedAction::Delete,
                                SuggestedAction::ManualEdit,
                            ],
                            // ...
                        });
                    }
                }
            }
            
            // Check for duplicate labels across agents
            let same_label: Vec<_> = memories.iter()
                .filter(|m| m.label == memory.label && m.id != memory.id)
                .collect();
            
            if !same_label.is_empty() && memory.label != "human" && memory.label != "persona" {
                // Duplicate archival labels are suspicious
                issues.push(MigrationIssue {
                    severity: IssueSeverity::Warning,
                    issue_type: IssueType::DuplicateLabel {
                        label: memory.label.clone(),
                        agents: same_label.iter().map(|m| m.agent_id.clone()).collect(),
                        contents: same_label.iter().map(|m| truncate(&m.value, 100)).collect(),
                    },
                    // ...
                });
            }
        }
        
        issues
    }
    
    /// Detect cross-contamination patterns
    fn detect_cross_contamination(&self, memories: &[V1MemoryExport]) -> Vec<MigrationIssue> {
        let mut issues = Vec::new();
        
        // Group by label
        let by_label: HashMap<String, Vec<_>> = memories.iter()
            .fold(HashMap::new(), |mut acc, m| {
                acc.entry(m.label.clone()).or_default().push(m);
                acc
            });
        
        // For core blocks, check if content matches the owning agent
        for (label, blocks) in &by_label {
            if label == "persona" || label == "human" {
                for block in blocks {
                    let agent = self.get_agent(&block.agent_id);
                    
                    // Persona should reference the agent's own name
                    if label == "persona" && !block.value.to_lowercase().contains(&agent.name.to_lowercase()) {
                        // Might be contaminated - check if it matches another agent
                        for other_agent in &self.all_agents {
                            if other_agent.id != agent.id 
                                && block.value.to_lowercase().contains(&other_agent.name.to_lowercase()) 
                            {
                                issues.push(MigrationIssue {
                                    severity: IssueSeverity::Critical,
                                    issue_type: IssueType::CrossContamination {
                                        block_label: label.clone(),
                                        owning_agent: agent.id.clone(),
                                        contaminating_agent: other_agent.id.clone(),
                                        contaminated_content: truncate(&block.value, 200),
                                    },
                                    // ...
                                });
                            }
                        }
                    }
                }
            }
        }
        
        issues
    }
}
```

### Interactive Review UI

The CLI provides an interactive review session:

```
$ pattern-cli import --from constellation.car --interactive

Analyzing export file...
Found: 5 agents, 2847 messages, 156 memory blocks

Detected 7 issues requiring review:

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[1/7] CRITICAL: Cross-contamination detected

Block: persona (owned by: Flux)
Content preview:
  "I am Entropy, a task-focused agent specializing in breaking down
   complex problems into manageable steps..."

This persona block belongs to Flux but contains Entropy's identity.

Options:
  [1] Reassign to Entropy
  [2] Delete this block (Flux will get default persona)
  [3] Edit content manually
  [4] Keep as-is (not recommended)
  [?] Show full content

Your choice: 1

✓ Will reassign to Entropy

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[2/7] WARNING: Duplicate label across agents

Label: "project_notes"
Found in:
  - Flux: "## Project Status\n- Working on bluesky integration..."
  - Entropy: "## Project Status\n- Task breakdown complete..."

These might be:
  - Intentionally separate (each agent's own notes)
  - Accidentally duplicated (should be one shared block)

Options:
  [1] Keep both as separate agent-owned blocks
  [2] Merge into shared block (pick primary owner)
  [3] Keep Flux's version, delete Entropy's
  [4] Keep Entropy's version, delete Flux's
  [5] View full content of each
  [?] Help

Your choice: 1

✓ Will keep as separate blocks

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[3/7] WARNING: Suspicious attribution

Block: archival_user_preferences (owned by: Anchor)
Content preview:
  "User prefers: morning standups, async communication, 
   detailed task breakdowns from Entropy..."

This archival block mentions Entropy's role but is owned by Anchor.
Possibly should belong to Entropy or be a shared block.

Options:
  [1] Keep with Anchor (constellation-wide info is reasonable)
  [2] Reassign to Entropy
  [3] Convert to shared block (Anchor owns, all can read)
  [4] Delete
  [?] Show full content

Your choice: 3

✓ Will convert to shared block

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

... (4 more issues)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Review complete. Summary of changes:
  - 2 blocks reassigned
  - 1 block converted to shared
  - 3 blocks kept as-is
  - 1 block deleted

Proceed with import? [y/N]: y

Importing...
✓ Created constellation database
✓ Imported 5 agents
✓ Imported 2847 messages
✓ Imported 155 memory blocks (1 deleted per review)
✓ Created 1 shared block attachment
✓ Generated embeddings for archival blocks

Import complete!
```

### Non-Interactive Mode

For scripted migrations, issues can be exported and resolved via config:

```bash
# Export issues to JSON
pattern-cli import --from constellation.car --analyze-only > issues.json

# Edit issues.json to add resolutions...

# Import with resolutions
pattern-cli import --from constellation.car --resolutions issues.json
```

Resolution file format:

```json
{
  "resolutions": [
    {
      "issue_id": "issue_001",
      "action": "assign_to",
      "target_agent": "entropy_abc123"
    },
    {
      "issue_id": "issue_002", 
      "action": "keep_both"
    },
    {
      "issue_id": "issue_003",
      "action": "delete"
    },
    {
      "issue_id": "issue_004",
      "action": "manual_edit",
      "new_content": "Corrected content here..."
    }
  ]
}
```

### Batch Operations

For constellations with many similar issues:

```
$ pattern-cli import --from constellation.car --interactive

Found 47 duplicate "archival_*" labels across agents.

Apply batch resolution?
  [1] Keep all as separate agent-owned blocks
  [2] Review each individually
  [3] Delete all duplicates (keep first occurrence)

Your choice: 1

✓ Applied to 47 items

Remaining issues: 3 (require individual review)
```

### Audit Log

All migration decisions are logged:

```rust
pub struct MigrationAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub issue_id: IssueId,
    pub issue_type: String,
    pub resolution: String,
    pub affected_items: Vec<String>,
    pub resolved_by: String,  // "user", "auto", "batch"
}
```

Stored in the constellation DB for future reference:

```sql
CREATE TABLE migration_audit (
    id TEXT PRIMARY KEY,
    imported_at TEXT NOT NULL,
    source_file TEXT NOT NULL,
    source_version INTEGER NOT NULL,
    issues_found INTEGER NOT NULL,
    issues_resolved INTEGER NOT NULL,
    audit_log JSON NOT NULL  -- Full decision log
);
```

### Post-Migration Cleanup Tools

After import, additional tools help clean up:

```bash
# Find remaining anomalies
pattern-cli db analyze --constellation "MyConstellation"

# Bulk reassign memories
pattern-cli db reassign-memories --from-agent flux --to-agent entropy --label "task_*"

# Merge duplicate blocks
pattern-cli db merge-blocks --keep block_id_1 --merge block_id_2

# Delete orphaned data
pattern-cli db cleanup --remove-orphans --dry-run
pattern-cli db cleanup --remove-orphans

# Regenerate embeddings for specific blocks
pattern-cli db reindex --agent flux --type archival
```

---

## Known Limitations

1. **Embeddings** - May need regeneration if model changed
2. **Live queries** - v1 subscriptions don't migrate (not persisted)
3. **Timestamps** - Some precision may be lost in conversion
4. **Custom metadata** - Unrecognized fields in v1 config will be dropped
5. **Semantic analysis limits** - Heuristics can't catch all attribution errors; user review is essential

## Migration Checklist

- [ ] Export all constellations from v1
- [ ] Backup v1 SurrealDB (just in case)
- [ ] Install v2 pattern-cli
- [ ] Run analysis on each export: `pattern-cli import --analyze-only`
- [ ] Review and resolve issues interactively or via resolution file
- [ ] Import each constellation with `--interactive` or `--resolutions`
- [ ] Verify import counts match
- [ ] Run `pattern-cli db analyze` on imported constellation
- [ ] Test agent interactions
- [ ] Regenerate embeddings if needed: `pattern-cli db reindex`
- [ ] Update any external integrations (Discord bot config, etc.)
- [ ] Keep v1 running in parallel during verification period
- [ ] Decommission v1 after verification complete
