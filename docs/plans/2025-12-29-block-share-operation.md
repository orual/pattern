# Block Share Operation Design

Add `share` and `unshare` operations to the block tool, allowing agents to share blocks with other agents by name.

## Overview

Agents need to share memory blocks with each other for collaboration. This design adds share/unshare operations to the existing block tool, using the existing `SharedBlockManager` infrastructure.

## Changes Required

### 1. ToolContext Trait (tool_context.rs)

Add method to expose SharedBlockManager:

```rust
fn shared_blocks(&self) -> Option<Arc<SharedBlockManager>>;
```

### 2. AgentRuntime (runtime/mod.rs)

- Add field: `shared_blocks: Arc<SharedBlockManager>`
- Initialize in `RuntimeBuilder::build()` using existing `dbs`
- Implement trait method returning `Some(self.shared_blocks.clone())`

### 3. BlockOp Enum (tool/builtin/types.rs)

Add variants:

```rust
/// Share block with another agent
Share,
/// Remove sharing from another agent
Unshare,
```

### 4. BlockInput Struct (tool/builtin/types.rs)

Add fields:

```rust
/// Target agent name for share/unshare operations
#[serde(default, skip_serializing_if = "Option::is_none")]
pub target_agent: Option<String>,

/// Permission level for share operation (default: Append)
#[serde(default, skip_serializing_if = "Option::is_none")]
pub permission: Option<MemoryPermission>,
```

### 5. BlockTool Implementation (tool/builtin/block.rs)

Add Share handler:
1. Validate `target_agent` is provided
2. Get `SharedBlockManager` from context (error if None)
3. Look up target agent by name via `pattern_db::queries::get_agent_by_name`
4. Get block metadata to find block ID
5. Call `shared_blocks.share_block(block_id, target_agent_id, permission)`
6. Default permission to `MemoryPermission::Append` if not specified
7. Return success with sharing details

Add Unshare handler:
1. Validate `target_agent` is provided
2. Get `SharedBlockManager` from context
3. Look up target agent by name
4. Get block metadata to find block ID
5. Call `shared_blocks.unshare_block(block_id, target_agent_id)`
6. Return success message

### 6. Update Tool Description (tool/builtin/block.rs)

Update `description()` to include:
```
- 'share': Share block with another agent by name (optional 'permission', default: Append)
- 'unshare': Remove sharing from another agent by name
```

Update `operations()` to include `"share"` and `"unshare"`.

### 7. Mock Implementation Updates

Update `MockMemoryStore` and test utilities to handle the new `shared_blocks()` method (return `None` for basic mocks).

### 8. Tests

Add tests in block.rs:
- `test_block_tool_share_operation` - share with default permission
- `test_block_tool_share_with_explicit_permission` - share with ReadWrite
- `test_block_tool_unshare_operation` - remove sharing
- `test_block_tool_share_agent_not_found` - error when target agent doesn't exist
- `test_block_tool_share_block_not_found` - error when block doesn't exist

## Implementation Order

1. Add `shared_blocks()` to ToolContext trait
2. Add SharedBlockManager field to AgentRuntime and implement trait method
3. Add Share/Unshare to BlockOp enum
4. Add target_agent/permission fields to BlockInput
5. Implement Share handler in BlockTool
6. Implement Unshare handler in BlockTool
7. Update tool description and operations list
8. Add tests
9. Update any mock implementations as needed

## Notes

- Uses existing `SharedBlockManager` from `crates/pattern_core/src/memory/sharing.rs`
- Uses existing `get_agent_by_name` query from `crates/pattern_db/src/queries/agent.rs`
- Permission defaults to `Append` (can read and append, but not overwrite)
- Agents identify each other by name, not ID (more natural for agent-to-agent interaction)
