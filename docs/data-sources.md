# Data Sources for Pattern Agents

This document describes the data source abstraction system that allows Pattern agents to consume data from various sources including files, streams, and APIs.

## Overview

The data source system provides a flexible way to pipe inputs from various data sources to prompt agents. It supports both pull-based (polling) and push-based (streaming) data consumption patterns with proper cursor management for resumption.

## Architecture

### Core Components

1. **DataSource Trait** (`pattern_core/src/data_source/traits.rs`)
   - Generic trait with associated types for Item, Filter, and Cursor
   - Supports both `pull()` for polling and `subscribe()` for streaming
   - Metadata tracking for source status and statistics

2. **DataIngestionCoordinator** (`pattern_core/src/data_source/coordinator.rs`)
   - Manages multiple data sources
   - Routes data to agents via prompt templates
   - Type-erased wrapper pattern for generic handling
   - Integrates with agent's embedding provider

3. **FileDataSource** (`pattern_core/src/data_source/file.rs`)
   - Concrete implementation for file-based data
   - Two storage modes:
     - **Ephemeral**: Simple file monitoring without persistence
     - **Indexed**: Semantic search with embeddings
   - Watch support for file change notifications
   - Multiple cursor types (ModTime, LineNumber, ByteOffset)

4. **StreamBuffer** (`pattern_core/src/data_source/buffer.rs`)
   - Caches stream data with configurable limits
   - Time-based and count-based retention
   - Optional database persistence for historical search

5. **PromptTemplate** (`pattern_core/src/prompt_template.rs`)
   - Jinja2-style templates using minijinja
   - Default templates for common data sources
   - Variable extraction and validation

6. **DataSourceTool** (`pattern_core/src/tool/builtin/data_source.rs`)
   - Agent-accessible tool for data source operations
   - Operations: ReadFile, IndexFile, WatchFile, ListSources, etc.

## Usage Guide

### Setting Up Data Sources for an Agent

```rust
use pattern_core::{
    agent::DatabaseAgent,
    data_source::{DataIngestionCoordinator, FileDataSource, FileStorageMode},
    runtime::router::AgentMessageRouter,
};

// 1. Create your agent with an embedding provider
let agent = DatabaseAgent::new(
    agent_id,
    user_id,
    agent_type,
    name,
    system_prompt,
    memory,
    db.clone(),
    model_provider,
    tool_registry,
    Some(embedding_provider), // Important for indexed sources
    heartbeat_sender,
);

// 2. Create the coordinator with the agent's embedding provider
let message_router = AgentMessageRouter::new(agent_id.clone(), db.clone());
let coordinator = DataIngestionCoordinator::new(
    message_router,
    agent.embedding_provider(), // Reuse agent's provider
)?;

// 3. Register the DataSourceTool
pattern_core::tool::builtin::register_data_source_tool(
    &tool_registry,
    Arc::new(RwLock::new(coordinator)),
);
```

### File Data Source Examples

#### Simple File Reading
```rust
// Ephemeral mode - no indexing, just read
let source = FileDataSource::new(
    "/path/to/file.txt",
    FileStorageMode::Ephemeral,
);

// Pull latest content
let items = source.pull(10, None).await?;
```

#### Indexed File with Semantic Search
```rust
// Indexed mode - requires embedding provider
let source = FileDataSource::new(
    "/path/to/docs.md",
    FileStorageMode::Indexed {
        embedding_provider: agent.embedding_provider().unwrap(),
        chunk_size: 1000,
    },
);

// Content will be chunked and embedded for search
```

#### Watch File for Changes
```rust
let source = FileDataSource::new(path, FileStorageMode::Ephemeral)
    .with_watch();

// Subscribe to changes
let stream = source.subscribe(None).await?;

// Process events as they come
while let Some(event) = stream.next().await {
    match event {
        Ok(StreamEvent { item, cursor, timestamp }) => {
            // Process new file content
        }
        Err(e) => {
            // Handle error
        }
    }
}
```

### Prompt Templates

The system uses prompt templates to format data for agents:

```rust
use pattern_core::prompt_template::{PromptTemplate, TemplateRegistry};

// Create a custom template
let template = PromptTemplate::new(
    "github_issue",
    "New issue #{{ number }} by @{{ author }}: {{ title }}\n{{ body }}",
)?;

// Register it
let mut registry = TemplateRegistry::new();
registry.register(template);

// Use it when adding a source
coordinator.add_source(
    github_source,
    buffer_config,
    "github_issue".to_string(), // Template name
).await?;
```

### Default Templates

The system includes default templates:
- `file_changed` - File modification notifications
- `stream_item` - Generic stream items
- `bluesky_post` - Bluesky social posts
- `scheduled_task` - Scheduled task triggers
- `data_ingestion` - Generic data ingestion

### Agent Tool Operations

Agents can interact with data sources through the DataSourceTool:

```yaml
# Read a file
operation: ReadFile
path: "/home/user/notes.txt"
lines: # Optional range
  start: 0
  end: 100

# Index a file for semantic search
operation: IndexFile
path: "/home/user/docs/manual.pdf"
chunk_size: 2000

# Watch a file for changes
operation: WatchFile
path: "/home/user/config.json"
notify: true
template_name: "config_changed"

# List all data sources
operation: ListSources

# Get buffer statistics
operation: GetBufferStats
source_id: "file_/home/user/data.log"
```

## Implementing Custom Data Sources

### Bluesky Firehose Implementation

The Bluesky firehose data source is fully implemented and uses the rocketman crate for Jetstream consumption:

```rust
use rocketman::{
    connection::JetstreamConnection,
    handler,
    ingestion::LexiconIngestor,
    options::JetstreamOptions,
    types::event::{Commit, Event, Operation},
};

pub struct BlueskyFirehoseSource {
    source_id: String,
    endpoint: String,
    filter: BlueskyFilter,
    current_cursor: Option<BlueskyFirehoseCursor>,
    stats: SourceStats,
    buffer: Option<Arc<parking_lot::Mutex<StreamBuffer<BlueskyPost, BlueskyFirehoseCursor>>>>,
}

// Rich post structure with facets for mentions/links
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueskyPost {
    pub uri: String,
    pub did: String,
    pub handle: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub reply_to: Option<String>,
    pub embed: Option<serde_json::Value>,
    pub langs: Vec<String>,
    pub labels: Vec<String>,
    pub facets: Vec<Facet>, // Rich text annotations
}

// Custom ingestor implementing rocketman's LexiconIngestor trait
struct PostIngestor {
    tx: tokio::sync::mpsc::UnboundedSender<Result<StreamEvent<BlueskyPost, BlueskyFirehoseCursor>>>,
    filter: BlueskyFilter,
    buffer: Option<Arc<parking_lot::Mutex<StreamBuffer<BlueskyPost, BlueskyFirehoseCursor>>>>,
}

#[async_trait]
impl LexiconIngestor for PostIngestor {
    async fn ingest(&self, event: Event<serde_json::Value>) -> anyhow::Result<()> {
        // Process commit events for posts
        if let Some(ref commit) = event.commit {
            if commit.collection == "app.bsky.feed.post" 
                && matches!(commit.operation, Operation::Create) {
                // Parse and filter post
                // Send to channel and buffer
            }
        }
        Ok(())
    }
}

// Usage with rocketman's handle_message
let msg_rx = connection.get_msg_rx();
let reconnect_tx = connection.get_reconnect_tx();
let mut ingestors: HashMap<String, Box<dyn LexiconIngestor + Send + Sync>> = HashMap::new();
ingestors.insert("app.bsky.feed.post".to_string(), Box::new(post_ingestor));

while let Ok(message) = msg_rx.recv_async().await {
    handler::handle_message(message, &ingestors, reconnect_tx.clone(), cursor_arc.clone()).await?;
}
```

## Type Erasure Pattern

The coordinator uses a type erasure pattern to handle concrete data sources generically:

```rust
// Your source has concrete types
let file_source: FileDataSource; // Item=FileItem, Cursor=FileCursor

// Coordinator wraps it in TypeErasedSource
coordinator.add_source(file_source, config, template).await?;

// Internally converts to Item=Value, Cursor=Value
// This allows the coordinator to handle any data source type
```

## Cursor Management

Cursors enable resumption after interruption:

- **Time-based**: For sources ordered by time (e.g., ModTime for files)
- **Sequence-based**: For sources with sequence numbers (e.g., Bluesky firehose)
- **Position-based**: For sources with byte/line positions (e.g., log files)

The system automatically serializes/deserializes cursors for persistence.

## Best Practices

1. **Use the agent's embedding provider** - Don't create duplicate providers
2. **Choose appropriate storage mode** - Indexed only when semantic search is needed
3. **Set reasonable buffer limits** - Balance between memory usage and data availability
4. **Use descriptive source IDs** - Makes debugging and monitoring easier
5. **Handle errors gracefully** - Streams can fail, have retry logic
6. **Update cursors atomically** - Ensure exactly-once processing

## Future Extensions

The data source abstraction is designed to support:
- Database change streams
- Message queue consumers (Kafka, RabbitMQ)
- WebSocket streams
- RSS/Atom feeds
- Email monitoring
- Calendar event streams
- IoT sensor data

Each just needs to implement the `DataSource` trait with appropriate Item, Filter, and Cursor types.
