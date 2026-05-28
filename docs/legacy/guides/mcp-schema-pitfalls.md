# MCP Tool Parameter Schema Pitfalls

When implementing MCP tools with the `#[rmcp::tool]` attribute, there are critical limitations in what types can be used in tool parameters. These limitations stem from JSON Schema generation requirements.

## ❌ DO NOT USE in MCP Tool Parameters

### 1. References
- `&str`, `&[T]`, or any borrowed types
- `#[serde(flatten)]` - Not supported by JSON Schema
- Example of what NOT to do:
  ```rust
  #[rmcp::tool]
  async fn bad_tool(text: &str) -> Result<String> {} // ❌ Won't work
  ```

### 2. Enums
- MCP doesn't handle enum variants properly in parameters
- Even simple enums will cause schema generation errors
- Example of what NOT to do:
  ```rust
  enum DestinationType { Agent, Group, Discord }
  
  #[rmcp::tool]
  async fn bad_tool(dest_type: DestinationType) -> Result<String> {} // ❌ Won't work
  ```

### 3. Nested Structs
- Parameters must be flat at the top level
- No nested custom types in parameters
- Example of what NOT to do:
  ```rust
  struct Destination {
      dest_type: String,
      dest_id: String,
  }
  
  #[rmcp::tool]
  async fn bad_tool(destination: Destination) -> Result<String> {} // ❌ Won't work
  ```

### 4. Unsigned Integers
- `u32`, `u64`, `usize` are not properly supported
- Use signed integers or `number` type instead
- Example of what NOT to do:
  ```rust
  #[rmcp::tool]
  async fn bad_tool(count: u32) -> Result<String> {} // ❌ Won't work
  ```

## ✅ DO USE in MCP Tool Parameters

### Supported Types
- **Strings**: `String` (not `&str`)
- **Numbers**: `i32`, `i64`, `f64` (not unsigned)
- **Booleans**: `bool`
- **Arrays**: `Vec<String>`, `Vec<i32>`, etc.
- **Optional**: `Option<T>` where T is a supported type

### Example of Proper Tool Definition
```rust
#[rmcp::tool(
    description = "Send a message to an agent, group, or Discord",
    param_descriptions(
        "destination_type" = "Type of destination: 'agent', 'group', 'discord_channel', or 'discord_dm'",
        "destination" = "Name/ID of the destination (agent name, group name, channel ID, or user ID)",
        "message" = "The message content to send",
        "channel_name" = "Optional channel name in 'guild/channel' format (for discord_channel only)"
    )
)]
async fn send_message(
    destination_type: String,      // ✅ String instead of enum
    destination: String,           // ✅ Simple string
    message: String,              // ✅ Simple string
    channel_name: Option<String>, // ✅ Optional primitive
) -> Result<String> {
    // Implementation
}
```

## Real-World Fix Example

### Before (Broken)
```rust
#[derive(Serialize, Deserialize)]
struct EventParams {
    title: String,
    start_time: String,
    #[serde(flatten)]
    details: EventDetails,  // ❌ Nested struct with flatten
}

enum EventType {
    Meeting,
    Task,
    Reminder,
}

#[rmcp::tool]
async fn schedule_event(
    params: EventParams,     // ❌ Nested struct
    event_type: EventType,   // ❌ Enum
    duration_mins: u32,      // ❌ Unsigned int
) -> Result<String> {}
```

### After (Fixed)
```rust
#[rmcp::tool]
async fn schedule_event(
    title: String,                    // ✅ Flat parameter
    start_time: String,               // ✅ Simple string
    end_time: Option<String>,         // ✅ Optional string
    description: Option<String>,      // ✅ All fields flattened
    location: Option<String>,         // ✅ No nested structs
    adhd_buffer_mins: Option<i32>,    // ✅ Signed integer
) -> Result<String> {}
```

## Key Takeaways

1. **Keep it flat**: All parameters at the top level
2. **Keep it simple**: Use only primitive types
3. **No enums**: Use strings with validation in the implementation
4. **No unsigned**: Use i32/i64 or f64 for numbers
5. **No references**: Always use owned types like String

When in doubt, stick to `String`, `i32`, `i64`, `f64`, `bool`, and `Vec<T>` of these types.

---

## Non-MCP Tool Schema Issues (AiTool/JsonSchema)

For Pattern's internal `AiTool` trait using `schemars::JsonSchema`, there are additional pitfalls:

### Doc Comments on Enum Variants Cause `oneOf`

When you add doc comments to enum variants, `schemars` generates a `oneOf` schema with `const` values. Some LLM APIs (notably Gemini) don't handle `oneOf`/`const` patterns well.

**Problematic:**
```rust
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOp {
    /// Execute a command and wait for completion.
    Execute,
    /// Spawn a long-running process.
    Spawn,
}
```

This generates:
```json
{
  "oneOf": [
    { "const": "execute", "description": "Execute a command..." },
    { "const": "spawn", "description": "Spawn a long-running..." }
  ]
}
```

**Fixed - no doc comments on variants:**
```rust
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOp {
    Execute,
    Spawn,
}
```

This generates a simpler enum schema that works across all providers:
```json
{
  "type": "string",
  "enum": ["execute", "spawn"]
}
```

**Rule**: If you need multi-provider LLM compatibility, avoid doc comments on enum variants. Put documentation at the enum level or in the containing struct's field description instead.