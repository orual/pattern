# LSP Edit Tool Usage Guide

## Key Learnings from Using `mcp__language-server__edit_file`

### 1. Line Number Accuracy is Critical
- The tool uses **1-based line numbering** (not 0-based)
- Always verify line numbers match exactly what's in the file
- When in doubt, use `Read` or `mcp__language-server__hover` to confirm

### 2. Preserve Exact Formatting
- The tool is sensitive to whitespace and indentation
- Copy indentation exactly as it appears in the original
- Don't add or remove blank lines unless intentional

### 3. Be Careful with Multi-Edit Operations
When making multiple edits to the same file:
- Edits are applied **sequentially from top to bottom**
- Later edits must account for line number changes from earlier edits
- Consider using multiple separate edit calls for complex changes

### 4. Common Pitfalls I Encountered

#### Duplicate Content
- **Problem**: Accidentally included duplicate lines or sections
- **Solution**: Always check the exact range being replaced
- **Example**: If replacing lines 10-15, make sure not to include line 16

#### Missing Context
- **Problem**: Removed necessary closing braces or delimiters
- **Solution**: Always include complete logical blocks in edits
- **Example**: When editing a function, include both opening and closing braces

#### Line Offset Errors
- **Problem**: Edit ranges were off by one or more lines
- **Solution**: Use `Read` with line numbers to verify exact positions
- **Better**: Use `mcp__language-server__hover` for precise location info

### 5. Best Practices

#### For Simple Edits
```json
{
  "startLine": 10,
  "endLine": 10,
  "newText": "single line replacement"
}
```

#### For Block Replacements
```json
{
  "startLine": 20,
  "endLine": 30,
  "newText": "multi-line\nreplacement\ntext"
}
```

#### For Insertions
- To insert after line N: use startLine: N+1, endLine: N
- To insert at beginning: use startLine: 1, endLine: 0

### 6. When to Use Alternative Tools

Use **`Edit`** or **`MultiEdit`** instead when:
- Making string-based replacements
- Need to ensure unique matches
- Working with smaller, targeted changes

Use **`Write`** when:
- Rewriting entire files
- File structure is completely broken
- Starting fresh is cleaner than many edits

### 7. Debugging Failed Edits

When edits fail or produce unexpected results:
1. Read the specific section with line numbers
2. Verify the exact content and formatting
3. Check for:
   - Unclosed delimiters
   - Missing imports
   - Duplicate definitions
   - Syntax errors in the newText

### 8. Language Server Integration Benefits

The LSP tools provide:
- Real-time diagnostics to catch errors early
- Hover information for type checking
- Reference finding before refactoring
- Symbol renaming across files

But remember:
- LSP edits are line-based, not AST-based
- Syntax errors can break LSP functionality
- Always verify edits with `cargo check` or equivalent

## Specific Lessons from This Session

1. **Module definitions**: Be careful not to duplicate `pub mod` statements
2. **Struct fields**: Watch for duplicate field definitions when merging edits
3. **Import statements**: Use the correct import style (`use sqlx::types::chrono`)
4. **Feature gates**: Ensure optional dependencies are properly gated
5. **Error types**: Check that custom error types are properly defined before use

## Recovery Strategy

When files get badly mangled (like happened with config.rs and db.rs):
1. Start with `Write` to create a clean version
2. Use `cargo check` to identify all issues
3. Fix systematically from top to bottom
4. Group related fixes together
5. Test frequently with `cargo check`