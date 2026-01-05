# Tool System Architecture

Pattern's tool system follows MemGPT/Letta patterns with multi-operation tools for agent capabilities.

## Core Design Principles

1. **Multi-operation tools**: Each tool has multiple operations under a single entry point
2. **Type safety**: Strong typing with JSON schema generation
3. **Usage rules**: Tools provide their own usage guidelines
4. **Thread-safe**: Registry uses DashMap for concurrent access
5. **Dynamic dispatch**: Runtime tool selection via trait objects
6. **MCP compatibility**: Schemas are inlined for MCP compliance

## Tool Trait System

### Base AiTool Trait
```rust
#[async_trait]
pub trait AiTool: Send + Sync + Debug {
    type Input: JsonSchema + DeserializeOwned + Serialize;
    type Output: JsonSchema + Serialize;
    
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn usage_rule(&self) -> Option<String> { None }
    
    async fn execute(&self, params: Self::Input) -> Result<Self::Output>;
    
    // Auto-generated schema with references inlined
    fn parameters_schema(&self) -> Value {
        let mut settings = SchemaSettings::default();
        settings.inline_subschemas = true;  // MCP compatibility
        let generator = SchemaGenerator::new(settings);
        let schema = generator.into_root_schema_for::<Self::Input>();
        serde_json::to_value(schema).unwrap()
    }
}
```

### Dynamic Dispatch Wrapper
```rust
pub trait DynamicTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    fn usage_rule(&self) -> Option<String>;
    
    async fn execute_dynamic(&self, params: Value) -> Result<Value>;
}
```

## Built-in Tools

### 1. context - Core Memory Management
```rust
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation")]
pub enum ContextOperation {
    Append { label: String, value: String },
    Replace { label: String, old: String, new: String },
    Archive { label: String },
    LoadFromArchival { label: String },
    Swap { archive: String, load: String },
}
```

**Purpose**: Manages core memory blocks that are always in agent context.

### 2. recall - Archival Memory
```rust
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation")]
pub enum RecallOperation {
    Insert { label: String, value: String },
    Append { label: String, value: String },
    Read { label: String },
    Delete { label: String },
}
```

**Purpose**: Long-term storage with full-text search via FTS5.

### 3. search - Unified Search
```rust
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SearchParams {
    domain: SearchDomain,  // archival_memory, conversations, all
    query: String,
    limit: Option<usize>,
    filters: Option<SearchFilters>,
}
```

**Purpose**: Search across different data domains with specific filters.

### 4. send_message - Agent Communication
```rust
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SendMessageParams {
    target: MessageTarget,
    content: String,
    metadata: Option<HashMap<String, Value>>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum MessageTarget {
    User,
    Agent { id: String },
    Group { name: String },
    Discord { channel_id: String },
    Bluesky { handle: String },
}
```

**Purpose**: Routes messages through appropriate endpoints.

## Tool Registry

### Registration
```rust
let registry = ToolRegistry::new();

// Register built-in tools automatically
let builtin = BuiltinTools::default_for_agent(handle);
builtin.register_all(&registry);

// Register custom tool
let custom_tool = Box::new(MyCustomTool);
registry.register(custom_tool);
```

### Tool Discovery
```rust
// List all available tools
let tools = registry.list_tools();

// Get specific tool
let tool = registry.get_tool("context")?;

// Get schemas for LLM
let schemas = registry.get_tool_schemas();
```

## Usage Rules System

Tools provide usage rules that are automatically included in agent context:

```rust
impl AiTool for SendMessageTool {
    fn usage_rule(&self) -> Option<String> {
        Some("When send_message is called, the conversation ends.".into())
    }
}
```

Context builder automatically formats rules:
```
## Tool Usage Rules
- send_message: When send_message is called, the conversation ends.
- context: Use append to add new information, replace to modify existing.
```

## Creating Custom Tools

### Simple Single-Operation Tool
```rust
#[derive(Debug, Clone)]
struct WordCountTool;

#[derive(Serialize, Deserialize, JsonSchema)]
struct WordCountInput {
    text: String,
}

#[derive(Serialize, JsonSchema)]
struct WordCountOutput {
    count: usize,
}

#[async_trait]
impl AiTool for WordCountTool {
    type Input = WordCountInput;
    type Output = WordCountOutput;
    
    fn name(&self) -> &str { "word_count" }
    fn description(&self) -> &str { "Counts words in text" }
    
    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        let count = params.text.split_whitespace().count();
        Ok(WordCountOutput { count })
    }
}
```

### Multi-Operation Tool
```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation")]
pub enum FileOperation {
    Read { path: String },
    Write { path: String, content: String },
    Delete { path: String },
}

#[derive(Serialize, JsonSchema)]
#[serde(tag = "status")]
pub enum FileResult {
    Success { data: String },
    Error { message: String },
}

#[async_trait]
impl AiTool for FileTool {
    type Input = FileOperation;
    type Output = FileResult;
    
    fn name(&self) -> &str { "file" }
    fn description(&self) -> &str { "File system operations" }
    
    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        match params {
            FileOperation::Read { path } => {
                // Read implementation
            },
            FileOperation::Write { path, content } => {
                // Write implementation
            },
            FileOperation::Delete { path } => {
                // Delete implementation
            },
        }
    }
}
```

## Thread Safety

ToolRegistry uses Arc<DashMap> for thread-safe concurrent access:

```rust
pub struct ToolRegistry {
    tools: Arc<DashMap<String, Box<dyn DynamicTool>>>,
}

// Cheap cloning for sharing across threads
let registry_clone = registry.clone();
tokio::spawn(async move {
    let tool = registry_clone.get_tool("context");
});
```

## Agent Integration

Tools are provided during agent creation:

```rust
let agent = DatabaseAgent::new(
    agent_id,
    user_id,
    // ... other params
    tool_registry,
    // ...
).await?;

// Agent executes tools via the registry
let result = agent.execute_tool("context", json!({
    "operation": "append",
    "label": "human",
    "value": "User prefers dark mode"
})).await?;
```

## MCP Compatibility

### Schema Requirements
- **NO $ref references**: All types inlined via `inline_subschemas = true`
- **NO nested enums**: Use tagged enums with `#[serde(tag = "type")]`
- **NO unsigned integers**: Use i32/i64 instead of u32/u64
- **Simple types only**: Avoid complex generics

### Example MCP-Compatible Schema
```rust
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct McpToolInput {
    #[schemars(description = "Operation to perform")]
    operation: String,  // Not an enum to avoid nesting
    
    #[schemars(description = "Target of operation")]
    target: String,
    
    #[serde(default)]
    #[schemars(description = "Optional parameters")]
    params: Option<HashMap<String, Value>>,
}
```

## Future Enhancements

1. **MCP Client Integration**: Consume external MCP tools (high priority)
2. **Tool Composition**: Chain multiple tools together
3. **Conditional Execution**: Tools that run based on conditions
4. **Rate Limiting**: Per-tool usage limits
5. **Tool Versioning**: Support multiple versions of the same tool