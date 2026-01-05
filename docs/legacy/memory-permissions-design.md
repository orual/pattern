# Memory Permissions Design

**Note**: This document describes planned memory permission features that are not yet implemented. These concepts are being considered for future development to enhance agent memory protection and user control.

## Permission Levels (Most to Least Restrictive)

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPermission {
    ReadOnly,     // Can only read, no modifications
    Partner,      // Requires permission from partner (owner)
    Human,        // Requires permission from any human
    #[default]
    Append,       // Can add to existing content
    ReadWrite,    // Can modify content freely
    Admin,        // Total control, can delete
}
```

## Block Properties

```rust
pub struct MemoryBlock {
    // ... existing fields ...
    pub memory_type: MemoryType,  // Core, Working, Archival
    pub pinned: bool,             // Can't be swapped out of core
    pub permission: MemoryPermission, // Inherent block permission
}
```

## Relation Properties

```rust
pub struct AgentMemoryRelation {
    // ... existing fields ...
    pub access_level: MemoryPermission, // Relation-specific permission
}
```

## Permission Resolution

**Most restrictive permission wins** between:
- Block's inherent permission
- Relation's access level

Example: If block has `ReadWrite` but relation has `ReadOnly`, effective permission is `ReadOnly`.

## Default Core Blocks

1. **Persona Block**
   - Pinned: true
   - Default Permission: Partner (editable but requires confirmation)
   - Contains agent's identity/personality

2. **Human/Partner Block**
   - Pinned: true
   - Default Permission: ReadWrite
   - Contains information about the user

## Permission Checks for Operations

- **Modify Persona**: Check for Partner permission, prompt user
- **Swap Memory**: Check if block is pinned
- **Delete**: Requires Admin permission
- **Create Core**: Check against max_core_blocks constraint


## Current Tools (Following Letta/MemGPT Patterns)

The following tools are **already implemented** in Pattern:

- **context** tool - Core memory operations:
  - `append` - Add content to memory blocks
  - `replace` - Replace content in memory blocks
  - `archive` - Move core block to archival storage
  - `load_from_archival` - Load archival block to core
  - `swap` - Atomic swap between core and archival

- **recall** tool - Archival memory operations:
  - `insert` - Add new archival memories
  - `append` - Add content to existing archival memory
  - `read` - Read specific archival memory by label
  - `delete` - Remove archival memories

- **search** tool - Unified search interface:
  - `archival_memory` domain - Full-text search of archival storage
  - `conversations` domain - Search message history
  - `all` domain - Search everything

## What's Missing: Permission Checks

The permission system described above is not yet complete. Human and Partner permissions are currently treated as read-only.
