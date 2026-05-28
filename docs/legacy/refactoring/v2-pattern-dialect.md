# Pattern Dialect - Action Language for Agents

## Overview

Pattern Dialect is a lightweight action language designed to replace structured tool calls (JSON/XML) with something that:

1. **LLMs can produce reliably** - even smaller/cheaper models
2. **Is easy to parse** - sigil-based structure with fuzzy argument matching
3. **Meets models where they are** - dense aliases for common operations
4. **Integrates with Pattern's permission system** - explicit markers for consent flows

The core insight: LLMs already have "intuitions" about how to express actions. Rather than forcing them into rigid schemas, we provide a flexible surface that maps many expressions to the same underlying operations.

## Design Principles

### Hard to Fail, Hard to Be Dangerous

- **Fuzzy matching** on verbs and arguments
- **Context inference** when targets are ambiguous
- **Sensible defaults** for omitted parameters
- **Permission gates invisible in syntax** - the system always checks, agents don't need to specify

### Meet Models Where They Are

- Dense alias tables for common operations
- Multiple syntactic forms for the same action
- Accept natural language fragments when unambiguous
- Intent inference from argument shape (e.g., `/recall "query text"` vs `/recall block_name`)

### Explicit Permission Requests

When an agent *knows* something might need approval:

| Marker | Meaning |
|--------|---------|
| (none) | Normal - system checks silently |
| `.` | "This is routine, don't bug them" |
| `?` | "This might need checking" |
| `?ask` | "Please confirm with partner" |
| `!` | "This is serious, definitely confirm" |

## Syntax

### Basic Structure

```
/VERB [ARGUMENTS] [PERMISSION_MARKER]
```

- `/` sigil marks an action (familiar from Discord/Slack/IRC)
- `VERB` is fuzzy-matched against known verbs + aliases
- `ARGUMENTS` are parsed based on verb signature
- Optional permission marker at end

### Chaining

```
/action1 -> /action2 -> /action3
```

`->` means "then prompt me again with the result". Not a pipe - the agent gets control back between each action.

### References

```
@last              # last message in context
@last.message      # same, more explicit  
@last.output       # output of last action
@thread            # current thread/conversation
@parent            # message being replied to
@[uri]             # specific resource by URI

that, this, it     # implicit references resolved from context
```

### Content Blocks

Any of these are recognized as "this is the content":

```
/post bluesky "single line content"
/post bluesky """
multi-line
content
"""
/post bluesky ```
code block style
```
/post bluesky
> block quote
> also works
```

Unquoted content is fine when unambiguous:

```
/reply sounds good to me
/recall user mentioned project deadlines
```

## Verb Reference

### Memory Operations

#### `/recall` - Long-term Memory

The most aliased verb. Intent is inferred from argument shape.

| Form | Interpretation |
|------|----------------|
| `/recall block_name` | Read block by exact name |
| `/recall "fuzzy query"` | Vector search |
| `/recall + "content"` | Insert new block |
| `/recall block_name + "content"` | Append to existing |
| `/recall -block_name` | Delete block |
| `/recall patch block_name \`\`\`diff...` | Structured edit |

**Aliases**: `remember`, `store`, `save`, `archive`, `forget` (→ delete)

**Subcommand forms** (equivalent to modifiers):
- `insert`, `add`, `save` → Insert
- `append`, `+` → Append  
- `read`, `get`, `check`, `show` → Read
- `delete`, `remove`, `-` → Delete
- `patch`, `edit` → Patch (for capable models)

#### `/context` - Working Memory

Operations on memory blocks that are always in context (persona, human, working notes).

| Form | Interpretation |
|------|----------------|
| `/context block_name + "content"` | Append to block |
| `/context block_name "old" -> "new"` | Replace text |
| `/context archive block_name` | Move to archival |
| `/context load archival_label` | Load from archival |
| `/context swap block archival_label` | Swap working ↔ archival |

**Aliases**: `note`, `update`, `remember` (context-aware disambiguation)

#### `/search` - Query Across Domains

| Form | Interpretation |
|------|----------------|
| `/search query terms` | Search all, return grouped |
| `/search in archival query` | Archival memory only |
| `/search in conversations query` | Conversation history |
| `/search in constellation query` | All agents' history |
| `/search query > 2 weeks` | Time-filtered |
| `/search query role:assistant` | Role-filtered |

**Aliases**: `find`, `query`, `look`, `lookup`

**Domain modifiers**: `in archival`, `in conversations`, `in constellation`, `in all`, `everywhere`

### Communication

#### `/send` - Explicit Target Form

```
/send @domain @target "message"
```

Domains: `@user`, `@agent`, `@channel`, `@bluesky`, `@discord`

Examples:
```
/send @agent @entropy "can you help break this down?"
/send @bluesky @at://did:plc:xxx/post/yyy "replying here"
/send @discord @general "hey everyone"
/send @user "here's what I found"
```

#### Shorthands

| Verb | Target | Use |
|------|--------|-----|
| `/reply` | Current thread | Reply to the message being responded to |
| `/post` | Platform | Post to Bluesky, Discord, etc. |
| `/dm` | User | Direct message |
| `/tell` | Agent | Agent-to-agent communication |

Examples:
```
/reply sounds good .                     # routine reply
/post bluesky "thoughts on this" ?public # flag as public
/dm @user "private info" ?               # might need checking
/tell @anchor "should I do this?" ?ask   # request approval
```

### External Operations

#### `/fetch` - Get Web Content

```
/fetch https://example.com           # as markdown
/fetch https://example.com html      # as raw html
/fetch continue                      # continue paginated read
```

**Aliases**: `get` (url context)

#### `/web` - Web Search

```
/web rust async patterns
/web site:docs.rs tokio runtime
```

**Aliases**: `search web`, `google`, `look up`

### Utilities

#### `/calc` - Calculation

```
/calc 5 feet to meters
/calc radius = 5; pi * radius^2
/calc 20% of 350
```

**Aliases**: `calculate`, `compute`, `math`

#### `/data` - Data Sources

```
/data list                           # show configured sources
/data read source_id                 # read from source
/data search source_id "query"       # search in source
/data monitor source_id              # start notifications
/data pause source_id                # pause notifications
```

### Authority Operations

Only available to agents with authority roles.

#### `/approve` - Grant Permission

```
/approve [request-id]
/approve [request-id] always         # create permanent rule
```

**Aliases**: `allow`, `permit`, `ok`, `yes`

#### `/deny` - Deny Permission

```
/deny [request-id]
/deny [request-id] "reason"
/deny [request-id] always            # create permanent block
```

**Aliases**: `reject`, `block`, `no`

#### `/escalate` - Punt to Higher Authority

```
/escalate [request-id]
/escalate [request-id] "this needs human review"
```

#### `/cancel` - Cancel Pending Action

```
/cancel                              # cancel current chain
/cancel [request-id]                 # cancel specific pending
```

**Aliases**: `abort`, `nevermind`, `stop`

### Emergency

#### `/halt` - Emergency Stop

```
/halt "reason for emergency stop" !
```

Only for system integrity agents. Terminates the process.

## Parser Architecture

### Intent Resolution Flow

```
1. Find sigil (/)
2. Extract verb token
3. Fuzzy match verb → canonical + aliases
4. Based on verb, parse arguments using signature
5. Extract permission markers
6. Resolve references (@last, that, etc.)
7. Infer intent from argument shape if needed
8. Return structured action
```

### Fuzzy Matching

```rust
fn match_verb(input: &str) -> Option<(Verb, f32)> {
    // 1. Exact match against canonical
    // 2. Exact match against aliases
    // 3. Levenshtein distance (weighted by verb strictness)
    // 4. Return best match above threshold
}
```

Each verb has a `strictness` level:
- **Strict**: `approve`, `deny`, `halt` - don't want accidental matches
- **Normal**: most verbs
- **Loose**: `recall`, `search` - maximize accessibility

### Argument Shape Inference

For `/recall`:
```rust
fn interpret_recall(args: &str) -> RecallIntent {
    if has_insert_modifier(args) {
        RecallIntent::Insert(extract_content(args))
    } else if has_delete_modifier(args) {
        RecallIntent::Delete(extract_label(args))
    } else if has_patch_modifier(args) {
        RecallIntent::Patch(extract_patch(args))
    } else if is_exact_block_match(args) {
        RecallIntent::Read(args.to_string())
    } else {
        // No exact match - treat as semantic search
        RecallIntent::Search(args.to_string())
    }
}
```

### Error Recovery

When parsing fails or is ambiguous, return natural language:

```
"I couldn't tell if you meant /post or /reply - which one?"
"'recall' found multiple blocks matching 'notes' - did you mean project_notes or meeting_notes?"
"No permission to post to bluesky DMs - want me to ask?"
```

## Permission Integration

### Implicit Checks (Always Run)

- Platform access
- Rate limits  
- Content policies
- DM vs public context
- Sensitivity heuristics

### Authority Resolution

```rust
enum Authority {
    Partner,                          // Human owner
    Agent(AgentId),                   // Supervisor agent
    Chain(Vec<Authority>),            // Try in order, escalate
}
```

Configured per action pattern:
- `/post bluesky` public → `Chain([Agent(anchor), Partner])`
- `/recall` sensitive → `Partner`
- `/send dm` → `Partner`

### Tiered Model Support

Routine permission checks can run on smaller/cheaper models:
- Local model handles 95% of traffic instantly
- Larger model handles edge cases
- Partner only sees truly novel situations

The small model can signal uncertainty:
```
/uncertain "this looks like sarcasm but I can't tell"
/escalate "content seems political, above my pay grade"
```

## Agent Instructions

The model-facing documentation is minimal:

```
Actions start with /

Common verbs:
  /recall - memory (read, search, store)
  /search - find things
  /reply - respond to messages
  /post - publish to platforms
  /tell - message other agents

Chain actions with ->
End with ? to request permission, ! if serious, . if routine

Examples:
  /recall project deadlines
  /reply sounds good .
  /post bluesky "hello" ?public
  /recall meeting notes -> /summarize -> /tell @entropy
```

## Implementation Notes

### Crate Location

`pattern_core::dialect` or separate `pattern_dialect` crate.

### Key Types

```rust
pub struct ParsedAction {
    pub verb: Verb,
    pub arguments: Arguments,
    pub permission_marker: Option<PermissionMarker>,
    pub chain: Option<Box<ParsedAction>>,
}

pub enum Arguments {
    Recall(RecallIntent),
    Send(SendTarget, Content),
    Search(SearchQuery),
    // ...
}

pub struct DialectParser {
    verb_specs: HashMap<String, VerbSpec>,
    block_registry: BlockRegistry,  // for exact match detection
}
```

### Integration with Existing Tools

The dialect parser produces structured actions that map directly to existing tool invocations. The `BuiltinTools` implementations remain unchanged - dialect is a new frontend, not a replacement of the execution layer.

```
Agent Output → Dialect Parser → ParsedAction → Tool Execution → Result
```

## Future Considerations

### Learning from Usage

Track which phrasings agents attempt:
- Add successful novel phrasings as aliases
- Identify common failure patterns
- Per-model alias tuning

### Visual/TUI Representation

The dialect is text-first but could render nicely:
- Syntax highlighting in logs
- Structured display in partner UI
- Action history with grouping

### Multi-Action Batching

Beyond chaining, explicit parallel execution:
```
/batch {
  /recall project_notes
  /search conversations about project
  /fetch https://project-docs.example.com
}
```
