use std::collections::HashMap;
use std::sync::Arc;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingProvider;
use crate::error::Result;
use crate::memory::MemoryBlock;
use crate::runtime::router::AgentMessageRouter;

use super::buffer::{BufferConfig, BufferStats};
use super::traits::{DataSource, StreamEvent};

use async_trait::async_trait;
use futures::Stream;

/// Event that prompts an agent with new data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataIngestionEvent {
    pub source_id: String,
    pub items: Vec<Value>,
    pub cursor: Option<Value>,
    pub metadata: HashMap<String, Value>,
    pub template_name: String,
}

/// Manages multiple data sources and routes their output to agents
#[derive(Clone)]
pub struct DataIngestionCoordinator<E: EmbeddingProvider + Clone> {
    sources: Arc<RwLock<HashMap<String, SourceHandle>>>,
    agent_router: AgentMessageRouter,
    embedding_provider: Option<Arc<E>>,
    /// Default target for data source notifications
    default_target: Arc<RwLock<crate::tool::builtin::MessageTarget>>,
}

/// Type-erased wrapper for concrete data sources
struct TypeErasedSource<S>
where
    S: DataSource + 'static,
{
    inner: S,
}

impl<S> TypeErasedSource<S>
where
    S: DataSource + 'static,
    S::Item: Serialize + for<'de> Deserialize<'de> + Send,
    S::Filter: Serialize + for<'de> Deserialize<'de> + Send,
    S::Cursor: Serialize + for<'de> Deserialize<'de> + Send,
{
    fn new(source: S) -> Self {
        Self { inner: source }
    }
}

#[async_trait]
impl<S> DataSource for TypeErasedSource<S>
where
    S: DataSource + 'static,
    S::Item: Serialize + for<'de> Deserialize<'de> + Send,
    S::Filter: Serialize + for<'de> Deserialize<'de> + Send,
    S::Cursor: Serialize + for<'de> Deserialize<'de> + Send,
{
    type Item = Value;
    type Filter = Value;
    type Cursor = Value;

    fn source_id(&self) -> &str {
        self.inner.source_id()
    }

    fn metadata(&self) -> super::traits::DataSourceMetadata {
        self.inner.metadata()
    }

    async fn pull(&mut self, limit: usize, after: Option<Self::Cursor>) -> Result<Vec<Self::Item>> {
        // Convert Value cursor to concrete type
        let cursor = if let Some(v) = after {
            Some(
                serde_json::from_value(v).map_err(|e| crate::CoreError::SerializationError {
                    data_type: "cursor".to_string(),
                    cause: e,
                })?,
            )
        } else {
            None
        };

        // Pull from inner source
        let items = self.inner.pull(limit, cursor).await?;

        // Convert concrete items to Values
        items
            .into_iter()
            .map(|item| {
                serde_json::to_value(item).map_err(|e| crate::CoreError::SerializationError {
                    data_type: "item".to_string(),
                    cause: e,
                })
            })
            .collect()
    }

    async fn subscribe(
        &mut self,
        from: Option<Self::Cursor>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin>>
    {
        use futures::stream::StreamExt;

        // Convert Value cursor to concrete type
        let cursor = if let Some(v) = from {
            Some(
                serde_json::from_value(v).map_err(|e| crate::CoreError::SerializationError {
                    data_type: "cursor".to_string(),
                    cause: e,
                })?,
            )
        } else {
            None
        };

        // Get stream from inner source
        let inner_stream = self.inner.subscribe(cursor).await?;

        // Map concrete events to Value events
        let mapped_stream = inner_stream.map(|event_result| {
            event_result.and_then(|event| {
                let value_item = serde_json::to_value(event.item).map_err(|e| {
                    crate::CoreError::SerializationError {
                        data_type: "item".to_string(),
                        cause: e,
                    }
                })?;
                let value_cursor = serde_json::to_value(event.cursor).map_err(|e| {
                    crate::CoreError::SerializationError {
                        data_type: "cursor".to_string(),
                        cause: e,
                    }
                })?;
                Ok(StreamEvent {
                    item: value_item,
                    cursor: value_cursor,
                    timestamp: event.timestamp,
                })
            })
        });

        Ok(Box::new(mapped_stream))
    }

    fn set_filter(&mut self, filter: Self::Filter) {
        // Convert Value filter to concrete type if possible
        if let Ok(concrete_filter) = serde_json::from_value::<S::Filter>(filter) {
            self.inner.set_filter(concrete_filter);
        }
        // Otherwise ignore - best effort
    }

    fn current_cursor(&self) -> Option<Self::Cursor> {
        self.inner
            .current_cursor()
            .map(|cursor| serde_json::to_value(cursor).unwrap_or(Value::Null))
    }

    fn buffer_config(&self) -> BufferConfig {
        self.inner.buffer_config()
    }

    async fn format_notification(
        &self,
        item: &Self::Item,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        // Deserialize Value back to concrete type and delegate
        if let Ok(typed_item) = serde_json::from_value::<S::Item>(item.clone()) {
            self.inner.format_notification(&typed_item).await
        } else {
            None
        }
    }

    fn get_buffer_stats(&self) -> Option<BufferStats> {
        self.inner.get_buffer_stats()
    }

    fn set_notifications_enabled(&mut self, enabled: bool) {
        self.inner.set_notifications_enabled(enabled)
    }

    fn notifications_enabled(&self) -> bool {
        self.inner.notifications_enabled()
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Self::Item>> {
        let results = self.inner.search(query, limit).await?;
        // Convert concrete items to Values
        results
            .into_iter()
            .map(|item| {
                serde_json::to_value(item).map_err(|e| crate::CoreError::SerializationError {
                    data_type: "item".to_string(),
                    cause: e,
                })
            })
            .collect()
    }
}

struct SourceHandle {
    source: Box<dyn DataSource<Item = Value, Filter = Value, Cursor = Value>>,
    monitoring_handle: Option<tokio::task::JoinHandle<()>>,
    /// Optional custom target for this specific source
    custom_target: Option<crate::tool::builtin::MessageTarget>,
}

impl std::fmt::Debug for SourceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceHandle")
            .field("source_id", &self.source.source_id())
            .field("has_monitor", &self.monitoring_handle.is_some())
            .finish()
    }
}

impl<E: EmbeddingProvider + Clone> std::fmt::Debug for DataIngestionCoordinator<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataIngestionCoordinator")
            .field(
                "sources_count",
                &self.sources.try_read().map(|s| s.len()).unwrap_or(0),
            )
            .field("has_embedding_provider", &self.embedding_provider.is_some())
            .finish()
    }
}

impl<E: EmbeddingProvider + Clone + 'static> DataIngestionCoordinator<E> {
    pub fn new(agent_router: AgentMessageRouter, embedding_provider: Option<Arc<E>>) -> Self {
        // Default to sending to the agent that owns this coordinator
        let default_target = crate::tool::builtin::MessageTarget {
            target_type: crate::tool::builtin::TargetType::Agent,
            target_id: Some(agent_router.agent_id().to_record_id()),
        };

        Self {
            sources: Arc::new(RwLock::new(HashMap::new())),
            agent_router,
            embedding_provider,
            default_target: Arc::new(RwLock::new(default_target)),
        }
    }

    /// Get a reference to the agent router
    pub fn router(&self) -> &AgentMessageRouter {
        &self.agent_router
    }

    /// Get the embedding provider if available
    pub fn embedding_provider(&self) -> Option<Arc<E>> {
        self.embedding_provider.clone()
    }

    /// Set the default target for data source notifications
    pub async fn set_default_target(&self, target: crate::tool::builtin::MessageTarget) {
        let mut default_target = self.default_target.write().await;
        *default_target = target;
    }

    /// Register a source and start monitoring
    pub async fn add_source<S>(&mut self, source: S) -> Result<()>
    where
        S: DataSource + 'static,
        S::Item: Serialize + for<'de> Deserialize<'de> + Send,
        S::Filter: Serialize + for<'de> Deserialize<'de> + Send,
        S::Cursor: Serialize + for<'de> Deserialize<'de> + Send,
    {
        self.add_source_with_target(source, None).await
    }

    /// Register a source with a custom target and start monitoring
    pub async fn add_source_with_target<S>(
        &mut self,
        source: S,
        target: Option<crate::tool::builtin::MessageTarget>,
    ) -> Result<()>
    where
        S: DataSource + 'static,
        S::Item: Serialize + for<'de> Deserialize<'de> + Send,
        S::Filter: Serialize + for<'de> Deserialize<'de> + Send,
        S::Cursor: Serialize + for<'de> Deserialize<'de> + Send,
    {
        let source_id = source.source_id().to_string();
        let buffer_config = source.buffer_config();

        tracing::info!(
            "DataIngestionCoordinator::add_source_with_target called for source: {}, notify_changes: {}",
            source_id,
            buffer_config.notify_changes
        );

        // Create a type-erased wrapper
        let mut erased_source = Box::new(TypeErasedSource::new(source));

        // Start monitoring if needed
        let monitoring_handle = if buffer_config.notify_changes {
            let stream = erased_source.subscribe(None).await?;
            let source_id_clone = source_id.clone();
            let coordinator = self.clone();

            Some(tokio::spawn(async move {
                coordinator.monitor_source(source_id_clone, stream).await;
            }))
        } else {
            None
        };

        let handle = SourceHandle {
            source: erased_source,
            monitoring_handle,
            custom_target: target,
        };

        let mut sources = self.sources.write().await;
        sources.insert(source_id.clone(), handle);

        Ok(())
    }

    /// Monitor a source stream and send notifications
    async fn monitor_source(
        &self,
        source_id: String,
        mut stream: Box<dyn Stream<Item = Result<StreamEvent<Value, Value>>> + Send + Unpin>,
    ) {
        tracing::info!("🔍 monitor_source started for source: {}", source_id);

        // Simplified error handling - just catch the main monitoring loop errors
        let result = self.run_monitoring_loop(&source_id, &mut stream).await;

        match result {
            Ok(()) => {
                tracing::info!("Source {} monitoring completed successfully", source_id);
            }
            Err(e) => {
                tracing::error!("💀 Source {} monitoring failed: {}", source_id, e);
            }
        }

        // Always log when the monitoring task is about to exit
        tracing::error!("🚨 Monitoring task for source {} is exiting!", source_id);
    }

    /// Run the actual monitoring loop with proper error handling
    async fn run_monitoring_loop(
        &self,
        source_id: &str,
        stream: &mut Box<dyn Stream<Item = Result<StreamEvent<Value, Value>>> + Send + Unpin>,
    ) -> Result<()> {
        use futures::StreamExt;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    // Wrap individual event processing in error handling
                    if let Err(e) = self.process_stream_event(source_id, event).await {
                        tracing::error!("Failed to process event for source {}: {}", source_id, e);
                        // Continue processing other events despite individual failures
                    }
                }
                Err(e) => {
                    tracing::error!("Stream error from source {}: {}", source_id, e);
                    // Stream errors might indicate connection issues - continue for now
                    // TODO: Consider reconnection strategy based on error type
                }
            }
        }

        tracing::info!("✅ Source {} stream ended normally", source_id);
        Ok(())
    }

    /// Process a single stream event (extracted for better error handling)
    async fn process_stream_event(
        &self,
        source_id: &str,
        event: StreamEvent<Value, Value>,
    ) -> Result<()> {
        // Use read_owned() to get an owned guard that we can hold across await points
        // This eliminates the double lock issue!
        let sources_guard = self.sources.clone().read_owned().await;

        let Some(handle) = sources_guard.get(source_id) else {
            return Err(crate::CoreError::DataSourceError {
                source_name: source_id.to_string(),
                operation: "format_notification".to_string(),
                cause: "Source not found in coordinator".to_string(),
            });
        };

        // Check if notifications are enabled before formatting
        if !handle.source.notifications_enabled() {
            return Ok(());
        }

        let custom_target = handle.custom_target.clone();
        let source_type = handle.source.metadata().source_type;

        tracing::debug!("📞 Calling format_notification for source: {}", source_id);

        // Now we can call format_notification without needing a second lock!
        // The owned guard lets us keep the reference across the await
        let start = std::time::Instant::now();
        let notification_result = handle.source.format_notification(&event.item).await;

        // Drop the guard after we're done with it
        drop(sources_guard);

        if start.elapsed() > std::time::Duration::from_secs(1) {
            tracing::warn!(
                "⚠️ format_notification took {:?} for source: {}",
                start.elapsed(),
                source_id
            );
        }

        let Some((message, memory_blocks)) = notification_result else {
            // No notification needed for this event
            return Ok(());
        };

        // Use custom target if available, otherwise use default
        let target = if let Some(custom_target) = custom_target {
            custom_target
        } else {
            self.default_target.read().await.clone()
        };

        // Create origin for this data source
        let origin = crate::runtime::router::MessageOrigin::DataSource {
            source_id: source_id.to_string(),
            source_type,
            item_id: None, // Could extract from item if needed
            cursor: Some(event.cursor.clone()),
        };

        // Include memory blocks in metadata for now
        let mut metadata = serde_json::json!({
            "source": "data_ingestion",
            "source_id": source_id,
            "item": event.item,
            "cursor": event.cursor,
        });

        // Add memory blocks if any
        if !memory_blocks.is_empty() {
            metadata["memory_blocks"] =
                serde_json::to_value(&memory_blocks).unwrap_or(serde_json::Value::Null);
        }

        tracing::info!(
            "📮 Sending notification to agent router for source: {}",
            source_id
        );
        let send_start = std::time::Instant::now();

        let send_result = self
            .agent_router
            .send_message(target, message, Some(metadata), Some(origin))
            .await;

        match send_result {
            Ok(_) => {
                tracing::info!(
                    "✅ Message sent successfully in {:?} for source: {}",
                    send_start.elapsed(),
                    source_id
                );
            }
            Err(e) => {
                tracing::error!(
                    "❌ Failed to send message after {:?} for source: {}: {}",
                    send_start.elapsed(),
                    source_id,
                    e
                );
                return Err(crate::CoreError::DataSourceError {
                    source_name: source_id.to_string(),
                    operation: "send_message".to_string(),
                    cause: format!("Failed to send notification: {}", e),
                });
            }
        }

        Ok(())
    }

    /// List all registered sources
    pub async fn list_sources(&self) -> Vec<(String, String)> {
        let sources = self.sources.read().await;
        sources
            .iter()
            .map(|(id, handle)| (id.clone(), handle.source.metadata().source_type))
            .collect()
    }

    /// Pause a source
    pub async fn pause_source(&self, source_id: &str) -> Result<()> {
        let mut sources = self.sources.write().await;

        if let Some(handle) = sources.get_mut(source_id) {
            handle.source.set_notifications_enabled(false);
            Ok(())
        } else {
            Err(crate::CoreError::tool_exec_msg(
                "data_ingestion",
                serde_json::json!({ "source_id": source_id }),
                format!("Source '{}' not found", source_id),
            ))
        }
    }

    /// Resume a source
    pub async fn resume_source(&self, source_id: &str) -> Result<()> {
        let mut sources = self.sources.write().await;

        if let Some(handle) = sources.get_mut(source_id) {
            handle.source.set_notifications_enabled(true);
            Ok(())
        } else {
            Err(crate::CoreError::tool_exec_msg(
                "data_ingestion",
                serde_json::json!({ "source_id": source_id }),
                format!("Source '{}' not found", source_id),
            ))
        }
    }

    /// Get buffer stats for a source
    pub async fn get_buffer_stats(&self, source_id: &str) -> Result<Value> {
        let sources = self.sources.read().await;

        if let Some(handle) = sources.get(source_id) {
            if let Some(stats) = handle.source.get_buffer_stats() {
                Ok(serde_json::to_value(stats).map_err(|e| {
                    crate::CoreError::SerializationError {
                        data_type: "buffer stats".to_string(),
                        cause: e,
                    }
                })?)
            } else {
                Ok(serde_json::json!({
                    "message": "Source has no buffer"
                }))
            }
        } else {
            Err(crate::CoreError::tool_exec_msg(
                "data_ingestion",
                serde_json::json!({ "source_id": source_id }),
                format!("Source '{}' not found", source_id),
            ))
        }
    }

    /// Read items from a source
    pub async fn read_source(
        &self,
        source_id: &str,
        limit: usize,
        cursor: Option<Value>,
    ) -> Result<Vec<Value>> {
        let mut sources = self.sources.write().await;

        if let Some(handle) = sources.get_mut(source_id) {
            handle.source.pull(limit, cursor).await
        } else {
            Err(crate::CoreError::tool_exec_msg(
                "data_ingestion",
                serde_json::json!({ "source_id": source_id }),
                format!("Source '{}' not found", source_id),
            ))
        }
    }

    /// Search within a source (if supported)
    pub async fn search_source(
        &self,
        source_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let sources = self.sources.read().await;

        if let Some(handle) = sources.get(source_id) {
            // Use the source's search method
            handle.source.search(query, limit).await
        } else {
            Err(crate::CoreError::ToolExecutionFailed {
                tool_name: "data_ingestion".to_string(),
                cause: format!("Source '{}' not found", source_id),
                parameters: serde_json::json!({ "source_id": source_id }),
            })
        }
    }

    /// Start monitoring a source
    pub async fn start_monitoring(&self, source_id: &str) -> Result<()> {
        tracing::info!(
            "DataIngestionCoordinator::start_monitoring called for source: {}",
            source_id
        );

        // First check if already monitoring (and task is still alive)
        {
            let sources = self.sources.read().await;
            if let Some(handle) = sources.get(source_id) {
                if let Some(ref task_handle) = handle.monitoring_handle {
                    if !task_handle.is_finished() {
                        tracing::info!("Source {} is already being monitored", source_id);
                        return Ok(()); // Already monitoring
                    } else {
                        tracing::warn!("Source {} monitoring task died, will restart", source_id);
                    }
                }
            } else {
                tracing::error!("Source '{}' not found in sources map", source_id);
                return Err(crate::CoreError::tool_exec_msg(
                    "data_ingestion",
                    serde_json::json!({ "source_id": source_id }),
                    format!("Source '{}' not found", source_id),
                ));
            }
        }

        // Start monitoring
        let mut sources = self.sources.write().await;
        if let Some(handle) = sources.get_mut(source_id) {
            // Enable notifications
            handle.source.set_notifications_enabled(true);

            // Start monitoring task
            let stream = handle.source.subscribe(None).await?;
            let source_id_clone = source_id.to_string();
            let coordinator = self.clone();

            handle.monitoring_handle = Some(tokio::spawn(async move {
                coordinator.monitor_source(source_id_clone, stream).await;
            }));

            Ok(())
        } else {
            Err(crate::CoreError::tool_exec_msg(
                "data_ingestion",
                serde_json::json!({ "source_id": source_id }),
                format!("Source '{}' not found", source_id),
            ))
        }
    }
}
