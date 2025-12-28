//! Helper functions for setting up data sources with agents

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    context::{AgentHandle, message_router::AgentMessageRouter},
    data_source::{
        StreamBuffer,
        bluesky::{BlueskyFilter, BlueskyFirehoseSource},
        coordinator::DataIngestionCoordinator,
        file::{FileDataSource, FileStorageMode},
    },
    embeddings::EmbeddingProvider,
    error::Result,
};
use surrealdb::Surreal;

/// Create a DataIngestionCoordinator with an agent's ID and name
pub fn create_coordinator_with_agent_info<E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    embedding_provider: Option<Arc<E>>,
) -> DataIngestionCoordinator<E>
where
    E: EmbeddingProvider + Clone + 'static,
{
    let router = AgentMessageRouter::new(agent_id, agent_name, db);

    DataIngestionCoordinator::new(router, embedding_provider)
}

/// Create a DataIngestionCoordinator that routes to a specific target
pub async fn create_coordinator_with_target<E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    embedding_provider: Option<Arc<E>>,
    target: crate::tool::builtin::MessageTarget,
) -> DataIngestionCoordinator<E>
where
    E: EmbeddingProvider + Clone + 'static,
{
    let router = AgentMessageRouter::new(agent_id, agent_name, db);
    let coordinator = DataIngestionCoordinator::new(router, embedding_provider);
    coordinator.set_default_target(target).await;
    coordinator
}

/// Add a file data source to an agent's coordinator
pub async fn add_file_source<P: AsRef<Path>, E>(
    coordinator: &mut DataIngestionCoordinator<E>,
    path: P,
    watch: bool,
    indexed: bool,
    template_path: Option<PathBuf>,
) -> Result<()>
where
    E: EmbeddingProvider + Clone + 'static,
{
    let storage_mode = if indexed {
        if let Some(embedding_provider) = coordinator.embedding_provider() {
            FileStorageMode::Indexed {
                embedding_provider,
                chunk_size: 1000, // Default chunk size
            }
        } else {
            tracing::warn!(
                "Indexed mode requested but no embedding provider available, falling back to ephemeral"
            );
            FileStorageMode::Ephemeral
        }
    } else {
        FileStorageMode::Ephemeral
    };

    let mut source = FileDataSource::new(path, storage_mode);

    if watch {
        source = source.with_watch();
    }

    // Note: Template path would need to be handled via prompt templates in coordinator
    // FileDataSource doesn't have built-in template support
    if template_path.is_some() {
        tracing::warn!(
            "Template path specified but FileDataSource doesn't support templates directly. Use prompt templates in coordinator instead."
        );
    }

    coordinator.add_source(source).await
}

/// Add a Bluesky firehose source to an agent's coordinator
pub async fn add_bluesky_source<E>(
    coordinator: &mut DataIngestionCoordinator<E>,
    source_id: String,
    endpoint: Option<String>,
    filter: BlueskyFilter,
    agent_handle: Option<AgentHandle>,
    bsky_agent: Option<(crate::atproto_identity::AtprotoAuthCredentials, String)>,
) -> Result<()>
where
    E: EmbeddingProvider + Clone + 'static,
{
    let endpoint =
        endpoint.unwrap_or_else(|| "wss://jetstream1.us-east.fire.hose.cam/subscribe".to_string());

    // Create a buffer with processing queue for rate limiting
    let buffer = StreamBuffer::new(1000, std::time::Duration::from_secs(3600)) // 1000 items, 1 hour
        .with_processing_queue(1800); // 30 minutes worth at 1 post/second

    // BlueskyFirehoseSource::new is async and takes 3 params
    let mut source = BlueskyFirehoseSource::new(source_id, endpoint, agent_handle)
        .await
        .with_cursor_file("./firehose_cursor.db")
        .with_filter(filter)
        .with_buffer(buffer)
        .with_rate_limit(1.0); // 1 post per second max

    if let Some((auth, handle)) = bsky_agent {
        source = source.with_auth(auth, handle).await?;
    }

    coordinator.add_source(source).await
}

/// Builder for setting up data sources on an agent
pub struct DataSourceBuilder<E: EmbeddingProvider + Clone> {
    #[allow(dead_code)]
    coordinator: Option<DataIngestionCoordinator<E>>,
    file_sources: Vec<FileSourceConfig>,
    bluesky_sources: Vec<BlueskySourceConfig>,
}

struct FileSourceConfig {
    path: PathBuf,
    watch: bool,
    indexed: bool,
    template_path: Option<PathBuf>,
}

struct BlueskySourceConfig {
    source_id: String,
    endpoint: Option<String>,
    filter: BlueskyFilter,
    use_agent_handle: bool,
}

impl<E: EmbeddingProvider + Clone> DataSourceBuilder<E> {
    pub fn new() -> Self {
        Self {
            coordinator: None,
            file_sources: Vec::new(),
            bluesky_sources: Vec::new(),
        }
    }

    /// Add a file source configuration
    pub fn with_file_source<P: AsRef<Path>>(mut self, path: P, watch: bool, indexed: bool) -> Self {
        self.file_sources.push(FileSourceConfig {
            path: path.as_ref().to_path_buf(),
            watch,
            indexed,
            template_path: None,
        });
        self
    }

    /// Add a file source with custom template
    pub fn with_templated_file_source<P: AsRef<Path>, T: AsRef<Path>>(
        mut self,
        path: P,
        template_path: T,
        watch: bool,
        indexed: bool,
    ) -> Self {
        self.file_sources.push(FileSourceConfig {
            path: path.as_ref().to_path_buf(),
            watch,
            indexed,
            template_path: Some(template_path.as_ref().to_path_buf()),
        });
        self
    }

    /// Add a Bluesky firehose source
    pub fn with_bluesky_source(
        mut self,
        source_id: String,
        filter: BlueskyFilter,
        use_agent_handle: bool,
    ) -> Self {
        self.bluesky_sources.push(BlueskySourceConfig {
            source_id,
            endpoint: None,
            filter,
            use_agent_handle,
        });
        self
    }

    /// Add a Bluesky source with custom endpoint
    pub fn with_custom_bluesky_source(
        mut self,
        source_id: String,
        endpoint: String,
        filter: BlueskyFilter,
        use_agent_handle: bool,
    ) -> Self {
        self.bluesky_sources.push(BlueskySourceConfig {
            source_id,
            endpoint: Some(endpoint),
            filter,
            use_agent_handle,
        });
        self
    }

    /// Build and attach all configured sources
    pub async fn build(
        self,
        agent_id: crate::id::AgentId,
        agent_name: String,
        db: Surreal<surrealdb::engine::any::Any>,
        embedding_provider: Option<Arc<E>>,
        agent_handle: Option<AgentHandle>,
        bsky_agent: Option<(crate::atproto_identity::AtprotoAuthCredentials, String)>,
    ) -> Result<DataIngestionCoordinator<E>>
    where
        E: EmbeddingProvider + Clone + 'static,
    {
        let mut coordinator =
            create_coordinator_with_agent_info(agent_id, agent_name, db, embedding_provider);

        // Add file sources
        for config in self.file_sources {
            add_file_source(
                &mut coordinator,
                config.path,
                config.watch,
                config.indexed,
                config.template_path,
            )
            .await?;
        }

        // Add Bluesky sources
        for config in self.bluesky_sources {
            let handle = if config.use_agent_handle {
                agent_handle.clone()
            } else {
                None
            };

            add_bluesky_source(
                &mut coordinator,
                config.source_id,
                config.endpoint,
                config.filter,
                handle,
                bsky_agent.clone(),
            )
            .await?;
        }

        Ok(coordinator)
    }

    /// Build and attach all configured sources with a custom target
    pub async fn build_with_target(
        self,
        agent_id: crate::id::AgentId,
        agent_name: String,
        db: Surreal<surrealdb::engine::any::Any>,
        embedding_provider: Option<Arc<E>>,
        agent_handle: Option<AgentHandle>,
        bsky_agent: Option<(crate::atproto_identity::AtprotoAuthCredentials, String)>,
        target: crate::tool::builtin::MessageTarget,
    ) -> Result<DataIngestionCoordinator<E>>
    where
        E: EmbeddingProvider + Clone + 'static,
    {
        let mut coordinator =
            create_coordinator_with_target(agent_id, agent_name, db, embedding_provider, target)
                .await;

        // Add file sources
        for config in self.file_sources {
            add_file_source(
                &mut coordinator,
                config.path,
                config.watch,
                config.indexed,
                config.template_path,
            )
            .await?;
        }

        // Add Bluesky sources
        for config in self.bluesky_sources {
            let handle = if config.use_agent_handle {
                agent_handle.clone()
            } else {
                None
            };

            add_bluesky_source(
                &mut coordinator,
                config.source_id,
                config.endpoint,
                config.filter,
                handle,
                bsky_agent.clone(),
            )
            .await?;
        }

        Ok(coordinator)
    }
}

/// Quick setup functions for common scenarios

/// Monitor a directory for changes and notify the agent
pub async fn monitor_directory<P, E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    path: P,
) -> Result<DataIngestionCoordinator<E>>
where
    P: AsRef<Path>,
    E: EmbeddingProvider + Clone + 'static,
{
    DataSourceBuilder::new()
        .with_file_source(path, true, false)
        .build(agent_id, agent_name, db, None, None, None)
        .await
}

/// Create an indexed knowledge base from a directory
pub async fn create_knowledge_base<P, E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    path: P,
    embedding_provider: Arc<E>,
) -> Result<DataIngestionCoordinator<E>>
where
    P: AsRef<Path>,
    E: EmbeddingProvider + Clone + 'static,
{
    DataSourceBuilder::new()
        .with_file_source(path, false, true)
        .build(
            agent_id,
            agent_name,
            db,
            Some(embedding_provider),
            None,
            None,
        )
        .await
}

/// Monitor Bluesky for mentions
pub async fn monitor_bluesky_mentions<E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    handle: &str,
    agent_handle: Option<AgentHandle>,
) -> Result<DataIngestionCoordinator<E>>
where
    E: EmbeddingProvider + Clone + 'static,
{
    let filter = BlueskyFilter {
        mentions: vec![handle.to_string()],
        ..Default::default()
    };

    DataSourceBuilder::new()
        .with_bluesky_source(
            format!("bluesky_mentions_{}", handle),
            filter,
            agent_handle.is_some(),
        )
        .build(agent_id, agent_name, db, None, agent_handle, None)
        .await
}

/// Create a multi-source setup for a fully-featured agent
pub async fn create_full_data_pipeline<E>(
    agent_id: crate::id::AgentId,
    agent_name: String,
    db: Surreal<surrealdb::engine::any::Any>,
    knowledge_dir: PathBuf,
    watch_dir: PathBuf,
    bluesky_filter: BlueskyFilter,
    embedding_provider: Arc<E>,
    agent_handle: Option<AgentHandle>,
) -> Result<DataIngestionCoordinator<E>>
where
    E: EmbeddingProvider + Clone + 'static,
{
    DataSourceBuilder::new()
        // Indexed knowledge base
        .with_file_source(&knowledge_dir, false, true)
        // Watched directory for new content
        .with_file_source(&watch_dir, true, false)
        // Bluesky monitoring with agent handle for enhanced notifications
        .with_bluesky_source(
            "bluesky_firehose".to_string(),
            bluesky_filter,
            agent_handle.is_some(),
        )
        .build(
            agent_id,
            agent_name,
            db,
            Some(embedding_provider),
            agent_handle,
            None,
        )
        .await
}
