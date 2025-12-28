use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use compact_str::CompactString;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{BufferConfig, BufferStats};
use crate::{error::Result, memory::MemoryBlock};

/// Core trait for data sources that agents can consume from
#[async_trait]
pub trait DataSource: Send + Sync {
    type Item: Serialize + for<'de> Deserialize<'de> + Send;
    type Filter: Send;
    type Cursor: Serialize + for<'de> Deserialize<'de> + Send + Clone;

    /// Unique identifier for this source
    fn source_id(&self) -> &str;

    /// Pull next items with optional cursor
    async fn pull(&mut self, limit: usize, after: Option<Self::Cursor>) -> Result<Vec<Self::Item>>;

    /// Subscribe from a specific cursor position
    async fn subscribe(
        &mut self,
        from: Option<Self::Cursor>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin>>;

    /// Apply filtering/rate limiting
    fn set_filter(&mut self, filter: Self::Filter);

    /// Get current cursor position
    fn current_cursor(&self) -> Option<Self::Cursor>;

    /// Get source metadata (type, status, stats)
    fn metadata(&self) -> DataSourceMetadata;

    /// Get buffer configuration for this source
    fn buffer_config(&self) -> BufferConfig;

    /// Format an item for notification to agents (returns None if item shouldn't notify)
    /// Also returns any memory blocks that should be attached to the receiving agent
    async fn format_notification(
        &self,
        item: &Self::Item,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)>;

    /// Get buffer statistics if source maintains a buffer
    fn get_buffer_stats(&self) -> Option<BufferStats> {
        None // Default: no buffer
    }

    /// Enable or disable notifications
    fn set_notifications_enabled(&mut self, enabled: bool);

    /// Check if notifications are enabled
    fn notifications_enabled(&self) -> bool;

    /// Search within buffered items (if source maintains a searchable buffer)
    async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<Self::Item>> {
        // Default: not implemented
        Ok(vec![])
    }
}

/// Event from a streaming data source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent<T, C> {
    pub item: T,
    pub cursor: C,
    pub timestamp: DateTime<Utc>,
}

/// Metadata about a data source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceMetadata {
    pub source_type: String,
    pub status: DataSourceStatus,
    pub items_processed: u64,
    pub last_item_time: Option<DateTime<Utc>>,
    pub error_count: u64,
    pub custom: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataSourceStatus {
    Active,
    Paused,
    Error(String),
    Disconnected,
}

/// Trait for items that can be searched
pub trait Searchable {
    /// Check if the item matches a search query
    fn matches(&self, query: &str) -> bool;

    /// Get a relevance score for the query (0.0 to 1.0)
    fn relevance(&self, query: &str) -> f32 {
        if self.matches(query) { 1.0 } else { 0.0 }
    }
}
