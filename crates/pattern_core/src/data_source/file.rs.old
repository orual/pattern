use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use compact_str::CompactString;
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::embeddings::EmbeddingProvider;
use crate::error::Result;
use crate::memory::MemoryBlock;

use super::traits::{DataSource, DataSourceMetadata, DataSourceStatus, Searchable, StreamEvent};

/// File-specific implementation
pub struct FileDataSource {
    pub path: PathBuf,
    pub storage_mode: FileStorageMode,
    pub watch: bool,
    source_id: String,
    current_cursor: Option<FileCursor>,
    filter: FileFilter,
    metadata: DataSourceMetadata,
    notifications_enabled: bool,
}

#[derive(Debug, Clone)]
pub enum FileStorageMode {
    Ephemeral,
    Indexed {
        embedding_provider: Arc<dyn EmbeddingProvider>,
        chunk_size: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileCursor {
    ModTime(SystemTime),
    LineNumber(usize),
    ByteOffset(i64),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileFilter {
    pub extensions: Option<Vec<String>>,
    pub pattern: Option<String>,
    pub max_size_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileItem {
    pub path: PathBuf,
    pub content: FileContent,
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileContent {
    Text(String),
    Lines(Vec<String>),
    Chunk {
        text: String,
        start_line: i64,
        end_line: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub size_bytes: i64,
    pub modified: SystemTime,
    pub created: Option<SystemTime>,
    pub is_dir: bool,
}

impl FileDataSource {
    pub fn new(path: impl AsRef<Path>, storage_mode: FileStorageMode) -> Self {
        let path = path.as_ref().to_path_buf();
        let source_id = format!("file:{}", path.display());

        let metadata = DataSourceMetadata {
            source_type: "file".to_string(),
            status: DataSourceStatus::Active,
            items_processed: 0,
            last_item_time: None,
            error_count: 0,
            custom: Default::default(),
        };

        Self {
            path,
            storage_mode,
            watch: false,
            source_id,
            current_cursor: None,
            filter: FileFilter::default(),
            metadata,
            notifications_enabled: true,
        }
    }

    pub fn with_watch(mut self) -> Self {
        self.watch = true;
        self
    }

    async fn read_file(&self, path: &Path) -> Result<FileItem> {
        let metadata = fs::metadata(path).await.map_err(|e| {
            crate::CoreError::tool_exec_error(
                "file_data_source",
                serde_json::json!({ "path": path }),
                e,
            )
        })?;

        let file_metadata = FileMetadata {
            size_bytes: metadata.len() as i64,
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            created: metadata.created().ok(),
            is_dir: metadata.is_dir(),
        };

        if metadata.is_dir() {
            return Ok(FileItem {
                path: path.to_path_buf(),
                content: FileContent::Text(String::new()),
                metadata: file_metadata,
            });
        }

        // Check filter
        if let Some(max_size) = self.filter.max_size_bytes {
            if metadata.len() as i64 > max_size {
                return Err(crate::CoreError::tool_exec_msg(
                    "file_data_source",
                    serde_json::json!({ "path": path, "size": metadata.len() }),
                    format!("File too large: {} bytes", metadata.len()),
                ));
            }
        }

        // Read content
        let content = match &self.storage_mode {
            FileStorageMode::Ephemeral => {
                let text = fs::read_to_string(path).await.map_err(|e| {
                    crate::CoreError::tool_exec_error(
                        "file_data_source",
                        serde_json::json!({ "path": path }),
                        e,
                    )
                })?;
                FileContent::Text(text)
            }
            FileStorageMode::Indexed { chunk_size, .. } => {
                // Read in chunks for indexing
                let file = fs::File::open(path).await.map_err(|e| {
                    crate::CoreError::tool_exec_error(
                        "file_data_source",
                        serde_json::json!({ "path": path }),
                        e,
                    )
                })?;

                let reader = BufReader::new(file);
                let mut lines = reader.lines();
                let mut chunks = Vec::new();
                let mut current_chunk = Vec::new();
                let mut line_num = 0;

                while let Some(line) = lines.next_line().await.map_err(|e| {
                    crate::CoreError::tool_exec_error(
                        "file_data_source",
                        serde_json::json!({ "path": path, "line": line_num }),
                        e,
                    )
                })? {
                    current_chunk.push(line);
                    line_num += 1;

                    if current_chunk.len() as i64 >= *chunk_size {
                        let text = current_chunk.join("\n");
                        chunks.push(FileContent::Chunk {
                            text,
                            start_line: line_num - current_chunk.len() as i64,
                            end_line: line_num - 1,
                        });
                        current_chunk.clear();
                    }
                }

                // Last chunk
                if !current_chunk.is_empty() {
                    let text = current_chunk.join("\n");
                    chunks.push(FileContent::Chunk {
                        text,
                        start_line: line_num - current_chunk.len() as i64,
                        end_line: line_num - 1,
                    });
                }

                // For now, return as lines
                FileContent::Lines(
                    chunks
                        .into_iter()
                        .map(|c| match c {
                            FileContent::Chunk { text, .. } => text,
                            _ => String::new(),
                        })
                        .collect(),
                )
            }
        };

        Ok(FileItem {
            path: path.to_path_buf(),
            content,
            metadata: file_metadata,
        })
    }
}

#[async_trait]
impl DataSource for FileDataSource {
    type Item = FileItem;
    type Filter = FileFilter;
    type Cursor = FileCursor;

    fn source_id(&self) -> &str {
        &self.source_id
    }

    async fn pull(&mut self, limit: usize, after: Option<Self::Cursor>) -> Result<Vec<Self::Item>> {
        let mut items = Vec::new();

        // For single file
        if self.path.is_file() {
            let item = self.read_file(&self.path).await?;

            // Check cursor
            if let Some(cursor) = after {
                match cursor {
                    FileCursor::ModTime(mod_time) => {
                        if item.metadata.modified <= mod_time {
                            return Ok(vec![]);
                        }
                    }
                    _ => {} // Other cursor types not applicable for single file
                }
            }

            items.push(item);
        } else {
            // Directory listing
            let mut entries = fs::read_dir(&self.path).await.map_err(|e| {
                crate::CoreError::ToolExecutionFailed {
                    tool_name: "file_data_source".to_string(),
                    cause: format!("Failed to read directory: {}", e),
                    parameters: serde_json::json!({ "path": &self.path }),
                }
            })?;

            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|e| crate::CoreError::ToolExecutionFailed {
                        tool_name: "file_data_source".to_string(),
                        cause: format!("Failed to read entry: {}", e),
                        parameters: serde_json::json!({ "path": &self.path }),
                    })?
            {
                if items.len() >= limit {
                    break;
                }

                let path = entry.path();

                // Apply filter
                if let Some(extensions) = &self.filter.extensions {
                    if let Some(ext) = path.extension() {
                        if !extensions.contains(&ext.to_string_lossy().to_string()) {
                            continue;
                        }
                    }
                }

                match self.read_file(&path).await {
                    Ok(item) => items.push(item),
                    Err(e) => {
                        tracing::warn!("Failed to read file {:?}: {}", path, e);
                        self.metadata.error_count += 1;
                    }
                }
            }
        }

        self.metadata.items_processed += items.len() as u64;
        if !items.is_empty() {
            self.metadata.last_item_time = Some(chrono::Utc::now());
            self.current_cursor = Some(FileCursor::ModTime(SystemTime::now()));
        }

        Ok(items)
    }

    async fn subscribe(
        &mut self,
        _from: Option<Self::Cursor>,
    ) -> Result<Box<dyn Stream<Item = Result<StreamEvent<Self::Item, Self::Cursor>>> + Send + Unpin>>
    {
        if !self.watch {
            return Err(crate::CoreError::ToolExecutionFailed {
                tool_name: "file_data_source".to_string(),
                cause: "File watching not enabled".to_string(),
                parameters: serde_json::json!({ "path": &self.path }),
            });
        }

        // TODO: Implement file watching with notify crate
        // For now, return empty stream
        let stream = futures::stream::empty();
        Ok(Box::new(stream))
    }

    fn set_filter(&mut self, filter: Self::Filter) {
        self.filter = filter;
    }

    fn current_cursor(&self) -> Option<Self::Cursor> {
        self.current_cursor.clone()
    }

    fn metadata(&self) -> DataSourceMetadata {
        self.metadata.clone()
    }

    fn buffer_config(&self) -> super::BufferConfig {
        // Files typically have lower volume and benefit from persistence
        super::BufferConfig {
            max_items: 1000,
            max_age: std::time::Duration::from_secs(86400), // 24 hours
            notify_changes: self.watch,                     // Only notify if watching
            persist_to_db: true,
            index_content: matches!(self.storage_mode, FileStorageMode::Indexed { .. }),
        }
    }

    async fn format_notification(
        &self,
        item: &Self::Item,
    ) -> Option<(String, Vec<(CompactString, MemoryBlock)>)> {
        let path_str = item.path.display();
        let size_str = if item.metadata.size_bytes > 1024 * 1024 {
            format!(
                "{:.1}MB",
                item.metadata.size_bytes as f64 / (1024.0 * 1024.0)
            )
        } else if item.metadata.size_bytes > 1024 {
            format!("{:.1}KB", item.metadata.size_bytes as f64 / 1024.0)
        } else {
            format!("{} bytes", item.metadata.size_bytes)
        };

        let message = match &item.content {
            FileContent::Text(text) => {
                let preview = if text.lines().count() > 5 {
                    let lines: Vec<&str> = text.lines().take(5).collect();
                    format!(
                        "\n\nPreview:\n{}\n... ({} more lines)",
                        lines.join("\n"),
                        text.lines().count() - 5
                    )
                } else {
                    format!("\n\nContent:\n{}", text)
                };
                format!("📄 File: {} ({}){}", path_str, size_str, preview)
            }
            FileContent::Lines(lines) => {
                let preview = if lines.len() > 5 {
                    format!(
                        "\n\nFirst 5 lines:\n{}\n... ({} more lines)",
                        lines[..5].join("\n"),
                        lines.len() - 5
                    )
                } else {
                    format!("\n\nLines:\n{}", lines.join("\n"))
                };
                format!("📋 File: {} ({}){}", path_str, size_str, preview)
            }
            FileContent::Chunk {
                text,
                start_line,
                end_line,
            } => format!(
                "📄 File: {} (lines {}-{})\n\n{}",
                path_str, start_line, end_line, text
            ),
        };

        // No memory blocks from file source
        Some((message, vec![]))
    }

    fn set_notifications_enabled(&mut self, enabled: bool) {
        self.notifications_enabled = enabled;
    }

    fn notifications_enabled(&self) -> bool {
        self.notifications_enabled
    }

    async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<Self::Item>> {
        // For files, we could implement a simple search by reading the file
        // and searching within it, but for now we'll use the default
        // TODO: Implement file content search
        Ok(vec![])
    }
}

impl Searchable for FileItem {
    fn matches(&self, query: &str) -> bool {
        let query_lower = query.to_lowercase();

        // Search in file path
        if self
            .path
            .to_string_lossy()
            .to_lowercase()
            .contains(&query_lower)
        {
            return true;
        }

        // Search in content
        match &self.content {
            FileContent::Text(text) => text.to_lowercase().contains(&query_lower),
            FileContent::Lines(lines) => lines
                .iter()
                .any(|line| line.to_lowercase().contains(&query_lower)),
            FileContent::Chunk { text, .. } => text.to_lowercase().contains(&query_lower),
        }
    }

    fn relevance(&self, query: &str) -> f32 {
        if !self.matches(query) {
            return 0.0;
        }

        let query_lower = query.to_lowercase();
        let mut score = 0.0;

        // Filename match is highly relevant
        if let Some(filename) = self.path.file_name() {
            let filename_str = filename.to_string_lossy().to_lowercase();
            if filename_str == query_lower {
                score += 5.0; // Exact filename match
            } else if filename_str.contains(&query_lower) {
                score += 2.0;
            }
        }

        // Path match
        let path_str = self.path.to_string_lossy().to_lowercase();
        if path_str.contains(&query_lower) {
            score += 0.5;
        }

        // Content matches
        match &self.content {
            FileContent::Text(text) => {
                let text_lower = text.to_lowercase();
                if text_lower == query_lower {
                    score += 3.0; // Exact content match (rare)
                } else {
                    let count = text_lower.matches(&query_lower).count() as f32;
                    score += (count * 0.3).min(2.0);
                }
            }
            FileContent::Lines(lines) => {
                let mut line_matches = 0;
                for line in lines {
                    if line.to_lowercase().contains(&query_lower) {
                        line_matches += 1;
                    }
                }
                score += (line_matches as f32 * 0.5).min(2.0);
            }
            FileContent::Chunk { text, .. } => {
                let count = text.to_lowercase().matches(&query_lower).count() as f32;
                score += (count * 0.3).min(2.0);
            }
        }

        // Normalize to 0.0-1.0 range
        (score / 10.0).min(1.0)
    }
}
