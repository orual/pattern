# WASM Extension System Design

**Status**: Future work - saved for later
**Date**: 2025-12-29

## Overview

Design for a WASM-based plugin system for Pattern, enabling sandboxed extensions for data sources and tools. Based on Zed's extension API approach using wit-bindgen and the WebAssembly Component Model.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  pattern_extension_api  (guest crate - plugins depend on)  │
│  ├── Extension trait                                        │
│  ├── register_extension! macro                              │
│  ├── DataSource, Tool traits (guest-side)                   │
│  └── Host bindings (memory, router, etc.)                   │
└─────────────────────────────────────────────────────────────┘
                            │
                      wit-bindgen
                            │
┌─────────────────────────────────────────────────────────────┐
│  pattern.wit  (interface definition)                        │
│  ├── types: BlockRef, Notification, ToolInput, etc.         │
│  ├── data-source interface                                  │
│  ├── tool interface                                         │
│  └── host functions: memory-read, memory-write, log, etc.   │
└─────────────────────────────────────────────────────────────┘
                            │
                      wit-bindgen
                            │
┌─────────────────────────────────────────────────────────────┐
│  pattern_core  (host - loads and runs plugins)              │
│  ├── WasmRuntime (wasmtime + generated bindings)            │
│  ├── WasmDataBlock / WasmDataStream (bridge impls)          │
│  └── WasmTool (bridge impl)                                 │
└─────────────────────────────────────────────────────────────┘
```

## WIT Definition (pattern.wit)

```wit
package pattern:extension;

// Shared types
record block-ref {
    block-id: string,
    label: string,
    agent-id: string,
}

record notification {
    text: string,
    blocks: list<block-ref>,
}

record permission-rule {
    pattern: string,
    permission: permission-level,
}

enum permission-level {
    read-only,
    read-write,
    admin,
}

variant source-status {
    stopped,
    running,
    paused,
}

record block-schema-spec {
    name: string,
    description: string,
    schema-json: string,
}

record version-info {
    version-id: string,
    timestamp: string,
    description: option<string>,
}

variant reconcile-result {
    resolved(tuple<string, string>),  // path, resolution
    needs-resolution(tuple<string, string, string>),  // path, disk, agent
    no-change(string),  // path
}

// What extensions can implement - DataBlock interface
interface data-block {
    source-id: func() -> string;
    name: func() -> string;
    block-schema: func() -> block-schema-spec;
    permission-rules: func() -> list<permission-rule>;

    load: func(path: string, owner: string) -> result<block-ref, string>;
    create: func(path: string, content: option<string>, owner: string) -> result<block-ref, string>;
    save: func(block: block-ref) -> result<_, string>;
    delete: func(path: string) -> result<_, string>;

    history: func(block: block-ref) -> result<list<version-info>, string>;
    rollback: func(block: block-ref, version: string) -> result<_, string>;
    diff: func(block: block-ref, from: option<string>, to: option<string>) -> result<string, string>;

    reconcile: func(paths: list<string>) -> result<list<reconcile-result>, string>;
}

// What extensions can implement - DataStream interface
interface data-stream {
    source-id: func() -> string;
    name: func() -> string;
    block-schemas: func() -> list<block-schema-spec>;

    start: func(owner: string) -> result<_, string>;
    stop: func() -> result<_, string>;
    pause: func();
    resume: func();
    status: func() -> source-status;

    supports-pull: func() -> bool;
    pull: func(limit: u32, cursor: option<string>) -> result<list<notification>, string>;
}

// What extensions can implement - Tool interface
interface tool {
    name: func() -> string;
    description: func() -> string;
    parameters-schema: func() -> string;  // JSON schema
    output-schema: func() -> string;  // JSON schema
    usage-rule: func() -> option<string>;

    execute: func(params-json: string, meta-json: string) -> result<string, string>;
}

// What extensions can call (host provides)
interface host {
    // Memory operations
    memory-create-block: func(owner: string, label: string, description: string, content: string) -> result<string, string>;
    memory-read-block: func(owner: string, label: string) -> result<string, string>;
    memory-update-block: func(owner: string, label: string, content: string) -> result<_, string>;
    memory-delete-block: func(owner: string, label: string) -> result<_, string>;

    // Logging
    log: func(level: u8, message: string);

    // HTTP (if WASI HTTP not available)
    http-get: func(url: string, headers-json: string) -> result<string, string>;
    http-post: func(url: string, headers-json: string, body: string) -> result<string, string>;

    // Config access
    get-config: func(key: string) -> option<string>;

    // Notification emission (for streams)
    emit-notification: func(notification: notification);
}

world pattern-extension {
    import host;

    // Extensions can export any combination
    export data-block;
    export data-stream;
    export tool;
}
```

## Guest Crate (pattern_extension_api)

```rust
// pattern_extension_api/src/lib.rs
wit_bindgen::generate!({
    world: "pattern-extension",
    exports: {
        "pattern:extension/data-block": DataBlockImpl,
        "pattern:extension/data-stream": DataStreamImpl,
        "pattern:extension/tool": ToolImpl,
    },
});

pub use pattern::extension::*;

/// Trait for data block extensions
pub trait DataBlockExtension {
    fn source_id(&self) -> String;
    fn name(&self) -> String;
    fn block_schema(&self) -> BlockSchemaSpec;
    fn permission_rules(&self) -> Vec<PermissionRule>;

    fn load(&self, path: &str, owner: &str) -> Result<BlockRef, String>;
    fn create(&self, path: &str, content: Option<&str>, owner: &str) -> Result<BlockRef, String>;
    fn save(&self, block: &BlockRef) -> Result<(), String>;
    fn delete(&self, path: &str) -> Result<(), String>;

    fn history(&self, block: &BlockRef) -> Result<Vec<VersionInfo>, String>;
    fn rollback(&self, block: &BlockRef, version: &str) -> Result<(), String>;
    fn diff(&self, block: &BlockRef, from: Option<&str>, to: Option<&str>) -> Result<String, String>;

    fn reconcile(&self, paths: &[String]) -> Result<Vec<ReconcileResult>, String>;
}

/// Trait for data stream extensions
pub trait DataStreamExtension {
    fn source_id(&self) -> String;
    fn name(&self) -> String;
    fn block_schemas(&self) -> Vec<BlockSchemaSpec>;

    fn start(&mut self, owner: &str) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn status(&self) -> SourceStatus;

    fn supports_pull(&self) -> bool { false }
    fn pull(&self, _limit: u32, _cursor: Option<&str>) -> Result<Vec<Notification>, String> {
        Ok(vec![])
    }
}

/// Trait for tool extensions
pub trait ToolExtension {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn parameters_schema(&self) -> serde_json::Value;
    fn output_schema(&self) -> serde_json::Value;
    fn usage_rule(&self) -> Option<String> { None }

    fn execute(&self, params: serde_json::Value, meta: ExecutionMeta) -> Result<serde_json::Value, String>;
}

/// Register your extension - call once with your extension type
#[macro_export]
macro_rules! register_extension {
    ($type:ty) => {
        static EXTENSION: std::sync::OnceLock<std::sync::Mutex<$type>> = std::sync::OnceLock::new();

        fn get_extension() -> &'static std::sync::Mutex<$type> {
            EXTENSION.get_or_init(|| std::sync::Mutex::new(<$type>::new()))
        }

        // wit-bindgen will call these exports
        // Implementation bridges to the extension trait methods
    };
}

// Host function wrappers (nice API for extension authors)
pub mod host {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    pub enum LogLevel {
        Trace = 0,
        Debug = 1,
        Info = 2,
        Warn = 3,
        Error = 4,
    }

    pub fn log(level: LogLevel, message: &str) {
        pattern::extension::host::log(level as u8, message);
    }

    pub fn memory_create_block(owner: &str, label: &str, description: &str, content: &str) -> Result<String, String> {
        pattern::extension::host::memory_create_block(owner, label, description, content)
    }

    pub fn memory_read_block(owner: &str, label: &str) -> Result<String, String> {
        pattern::extension::host::memory_read_block(owner, label)
    }

    pub fn memory_update_block(owner: &str, label: &str, content: &str) -> Result<(), String> {
        pattern::extension::host::memory_update_block(owner, label, content)
    }

    pub fn get_config(key: &str) -> Option<String> {
        pattern::extension::host::get_config(key)
    }

    pub fn emit_notification(notification: Notification) {
        pattern::extension::host::emit_notification(notification);
    }
}
```

## Example Extension: S3 Data Source

```rust
// my_s3_source/src/lib.rs
use pattern_extension_api::{
    DataBlockExtension, BlockRef, BlockSchemaSpec, PermissionRule, VersionInfo, ReconcileResult,
    register_extension,
    host::{self, LogLevel},
};

struct S3DataBlock {
    bucket: String,
    region: String,
}

impl S3DataBlock {
    fn new() -> Self {
        Self {
            bucket: host::get_config("bucket").unwrap_or_default(),
            region: host::get_config("region").unwrap_or_else(|| "us-east-1".into()),
        }
    }
}

impl DataBlockExtension for S3DataBlock {
    fn source_id(&self) -> String {
        format!("s3:{}", self.bucket)
    }

    fn name(&self) -> String {
        format!("S3 Bucket: {}", self.bucket)
    }

    fn block_schema(&self) -> BlockSchemaSpec {
        BlockSchemaSpec {
            name: "s3_object".into(),
            description: "S3 object contents".into(),
            schema_json: r#"{"type": "string"}"#.into(),
        }
    }

    fn permission_rules(&self) -> Vec<PermissionRule> {
        vec![
            PermissionRule {
                pattern: "**".into(),
                permission: PermissionLevel::ReadWrite,
            }
        ]
    }

    fn load(&self, path: &str, owner: &str) -> Result<BlockRef, String> {
        host::log(LogLevel::Info, &format!("Loading s3://{}/{}", self.bucket, path));

        // Fetch from S3 using host HTTP or WASI HTTP
        let url = format!("https://{}.s3.{}.amazonaws.com/{}", self.bucket, self.region, path);
        let content = host::http_get(&url, "{}")?;

        // Create block via host
        let label = format!("s3:{}", path);
        let block_id = host::memory_create_block(owner, &label, "S3 object", &content)?;

        Ok(BlockRef {
            block_id,
            label,
            agent_id: owner.to_string(),
        })
    }

    fn save(&self, block: &BlockRef) -> Result<(), String> {
        let content = host::memory_read_block(&block.agent_id, &block.label)?;
        let path = block.label.strip_prefix("s3:").unwrap_or(&block.label);
        let url = format!("https://{}.s3.{}.amazonaws.com/{}", self.bucket, self.region, path);
        host::http_post(&url, "{}", &content)?;
        Ok(())
    }

    // ... other methods
}

register_extension!(S3DataBlock);
```

## Host Side Integration

```rust
// pattern_core/src/wasm/mod.rs
use wasmtime::component::*;

wasmtime::component::bindgen!({
    world: "pattern-extension",
    async: true,
});

pub struct WasmExtensionHost {
    engine: Engine,
    linker: Linker<HostState>,
}

struct HostState {
    memory: Arc<MemoryCache>,
    ctx: Arc<dyn ToolContext>,
}

impl pattern::extension::host::Host for HostState {
    fn memory_create_block(&mut self, owner: &str, label: &str, description: &str, content: &str) -> Result<String, String> {
        // Bridge to actual memory operations
        futures::executor::block_on(async {
            self.memory.create_block(owner, label, description, BlockType::Working, BlockSchema::text(), 0)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn log(&mut self, level: u8, message: &str) {
        match level {
            0 => tracing::trace!("[wasm] {}", message),
            1 => tracing::debug!("[wasm] {}", message),
            2 => tracing::info!("[wasm] {}", message),
            3 => tracing::warn!("[wasm] {}", message),
            _ => tracing::error!("[wasm] {}", message),
        }
    }

    // ... other host function implementations
}

impl WasmExtensionHost {
    pub fn new(memory: Arc<MemoryCache>) -> Result<Self> {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);

        pattern::extension::host::add_to_linker(&mut linker, |state| state)?;

        Ok(Self { engine, linker })
    }

    pub async fn load_data_block(
        &self,
        wasm_path: &Path,
        config: &serde_json::Value,
        ctx: Arc<dyn ToolContext>,
    ) -> Result<Arc<dyn DataBlock>> {
        let component = Component::from_file(&self.engine, wasm_path)?;
        let mut store = Store::new(&self.engine, HostState {
            memory: ctx.memory().clone(),
            ctx: ctx.clone(),
        });

        let instance = self.linker.instantiate_async(&mut store, &component).await?;

        Ok(Arc::new(WasmDataBlockBridge {
            instance,
            store: Mutex::new(store),
            source_id: OnceCell::new(),
        }))
    }
}

/// Bridge that implements DataBlock by calling into WASM
struct WasmDataBlockBridge {
    instance: PatternExtension,
    store: Mutex<Store<HostState>>,
    source_id: OnceCell<String>,
}

#[async_trait]
impl DataBlock for WasmDataBlockBridge {
    fn source_id(&self) -> &str {
        self.source_id.get_or_init(|| {
            let mut store = self.store.lock().unwrap();
            self.instance.data_block().source_id(&mut store).unwrap()
        })
    }

    async fn load(&self, path: &Path, ctx: Arc<dyn ToolContext>, owner: AgentId) -> Result<BlockRef> {
        let path_str = path.to_string_lossy();
        let owner_str = owner.as_str();

        tokio::task::spawn_blocking(move || {
            let mut store = self.store.lock().unwrap();
            self.instance.data_block()
                .load(&mut store, &path_str, owner_str)
                .map_err(|e| CoreError::WasmError { message: e })
        }).await?
    }

    // ... other methods
}
```

## Config Integration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataSourceConfig {
    Bluesky(BlueskySourceConfig),
    Discord(DiscordSourceConfig),
    File(FileSourceConfig),
    Wasm(WasmSourceConfig),       // WASM plugins
    Custom(CustomSourceConfig),    // Native inventory plugins
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmSourceConfig {
    pub name: String,
    pub path: PathBuf,
    pub source_kind: WasmSourceKind,
    #[serde(default)]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WasmSourceKind {
    Stream,
    Block,
    Both,
}
```

Example config:
```toml
[agent.data_sources.s3_docs]
type = "wasm"
name = "S3 Documentation"
path = "./plugins/s3_source.wasm"
source_kind = "block"
config = { bucket = "my-docs", region = "us-west-2" }
```

## Benefits

| Aspect | Raw JSON/Memory | WIT + wit-bindgen |
|--------|-----------------|-------------------|
| Type safety | Runtime errors | Compile-time checks |
| Serialization | Manual JSON | Automatic, efficient |
| API evolution | Breaking changes | Versioned interfaces |
| Documentation | Comments only | WIT is self-documenting |
| Multi-language | Rust-only practical | Any WASM language |

## Dependencies

- `wasmtime` - WASM runtime with Component Model support
- `wit-bindgen` - Code generation from WIT definitions
- Possibly `wasi-http` for HTTP access in plugins

## Open Questions

1. How to handle async in WASM? (WASM is sync, may need spawn_blocking or async cooperation)
2. Plugin discovery - scan directory for .wasm files?
3. Hot reloading - watch for plugin changes?
4. Sandboxing limits - memory, CPU, filesystem access?
5. Plugin signing/verification for security?

## References

- [Zed Extension API](https://docs.rs/zed_extension_api)
- [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)
- [WebAssembly Component Model](https://component-model.bytecodealliance.org/)
- [wasmtime](https://wasmtime.dev/)
