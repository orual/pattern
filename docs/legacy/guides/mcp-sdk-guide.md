# MCP Rust SDK Implementation Guide

## Official SDK (modelcontextprotocol/rust-sdk)

Based on research of the official SDK from git main branch.

### Important Import Notes

The git version has different module structure than documented:
- Use `rmcp::model::CallToolResult` (not `rmcp::server::CallToolResult`)
- Use `rmcp::Error as McpError` (not `ServiceError`)
- Use `schemars::JsonSchema` in derive macros (matching version 0.8.x)
- Use `rmcp::model::InitializeResult` as ServerInfo
- Use `rmcp::handler::server::tool::Parameters`
- Use `rmcp::model::Content` for tool responses

### Core Components

1. **ServerHandler Trait**: Core trait that services implement
   - Provides default implementations for most methods
   - Must be `Sized + Send + Sync + 'static`
   - Methods return futures with Result types

2. **Tool Definition**: Uses `#[tool]` attribute macro
   ```rust
   #[tool_router]
   impl Counter {
       #[tool(description = "Increment the counter by 1")]
       async fn increment(&self) -> Result<CallToolResult, McpError> {
           // implementation
       }
   }
   ```

3. **Transport Setup**:
   ```rust
   use tokio::io::{stdin, stdout};
   let transport = (stdin(), stdout());
   let server = service.serve(transport).await?;
   ```

### Key Patterns

1. **Service Structure**:
   ```rust
   #[derive(Clone)]
   pub struct MyService {
       // fields
   }
   
   #[tool_router]
   impl MyService {
       // tool methods with #[tool] attribute
   }
   
   impl ServerHandler for MyService {
       async fn get_info(&self) -> ServerInfo {
           // server metadata
       }
   }
   ```

2. **Tool Method Signatures**:
   - Return `Result<CallToolResult, McpError>`
   - Can be async or sync
   - Parameters can be:
     - Individual params with `#[schemars(description = "...")]`
     - Aggregated with `Parameters<T>` where T is a struct or JsonObject

3. **Required Imports**:
   ```rust
   use rmcp::{
       handler::{ServerHandler, Parameters},
       types::{CallToolResult, McpError, ServerInfo},
       ServiceExt,
   };
   ```

### Tool Response Format

Tools return `CallToolResult` which must be constructed using:
```rust
CallToolResult::success(vec![Content::text("response text")])
```

For JSON responses, serialize first:
```rust
CallToolResult::success(vec![Content::text(
    serde_json::to_string_pretty(&json_value).unwrap()
)])
```

### Example Tool Implementation

```rust
#[rmcp::tool(description = "Add two numbers")]
async fn add(
    &self,
    params: Parameters<AddRequest>,
) -> Result<CallToolResult, McpError> {
    let AddRequest { a, b } = params.0;
    let result = a + b;
    Ok(CallToolResult::success(vec![Content::text(
        result.to_string()
    )]))
}
```

### Error Handling

Use specific error constructors:
- `McpError::invalid_params("message", Some(json!({...})))`
- `McpError::internal_error("message", Some(json!({...})))`
- `McpError::resource_not_found("message", Some(json!({...})))`

### Differences from crates.io rmcp

The crates.io version uses different macros:
- `#[tool(tool_box)]` on impl block
- `#[tool(aggr)]` for aggregated params
- `#[tool(param)]` for individual params

The git version is cleaner and more streamlined.

### Transport Support

- **stdio**: Fully implemented and stable
- **SSE**: Deprecated but still supported
- **Streamable HTTP**: New recommended transport (replacing SSE)
- Examples exist for all transports in examples/servers/src/

### Running a Server

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize service
    let service = MyService::new();
    
    // Set up transport (stdio example)
    use tokio::io::{stdin, stdout};
    let transport = (stdin(), stdout());
    
    // Serve
    let server = service.serve(transport).await?;
    
    // Wait for completion
    let quit_reason = server.waiting().await?;
    println!("Server quit: {:?}", quit_reason);
    
    Ok(())
}
```