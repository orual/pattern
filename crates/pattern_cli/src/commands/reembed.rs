//! `pattern reembed` — backfill embeddings for existing rows.
//!
//! Walks the messages, archival_entries, and memory_blocks tables and
//! computes embeddings for rows that don't yet have one (or all rows when
//! `--all` is passed). Designed to be run while the daemon is stopped —
//! it opens its own mount + embedding provider, drives the backfill
//! synchronously, and exits with stats.
//!
//! Agent IDs are scope-encoded as `local:<id>` or `global:<id>` (see
//! [`pattern_core::types::memory_types::Scope::to_db_key`]). We discover
//! the actual set of agent_ids present by SELECT DISTINCT on each table
//! rather than reconstructing from the persona registry — that handles
//! both scopes uniformly and catches orphaned data from old test agents.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use miette::Result as MietteResult;

use pattern_core::traits::EmbeddingProvider;
use pattern_db::vector::{ContentType, embedding_is_current, update_embedding};
use pattern_runtime::embedding::render_chat_message_for_embedding;

#[derive(Args, Debug)]
pub struct ReembedCmd {
    /// Comma-separated list of content types to backfill.
    /// Valid values: messages, archival, blocks. If empty/unrecognized, defaults to all.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "messages,archival,blocks"
    )]
    pub types: Vec<String>,

    /// Re-embed every row, including those already current.
    #[arg(long)]
    pub all: bool,

    /// Don't actually compute embeddings; just report what would be done.
    #[arg(long)]
    pub dry_run: bool,

    /// Mount path (defaults to current dir / nearest ancestor).
    #[arg(long)]
    pub path: Option<PathBuf>,
}

#[derive(Default, Debug)]
struct Stats {
    embedded: usize,
    skipped: usize,
    failed: usize,
}

impl Stats {
    fn line(&self, label: &str) -> String {
        format!(
            "  {label:<10}: {} embedded, {} skipped (current), {} failed",
            self.embedded, self.skipped, self.failed
        )
    }
}

pub async fn cmd_reembed(cmd: ReembedCmd) -> MietteResult<()> {
    let mut do_blocks = cmd.types.iter().any(|t| t == "blocks");
    let mut do_archival = cmd.types.iter().any(|t| t == "archival");
    let mut do_messages = cmd.types.iter().any(|t| t == "messages");
    if !(do_blocks || do_archival || do_messages) {
        println!("no recognized types in --types; defaulting to all (messages, archival, blocks)");
        do_blocks = true;
        do_archival = true;
        do_messages = true;
    }

    let start = cmd
        .path
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let paths =
        pattern_memory::paths::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let model_path = paths.data_root().join("embeddinggemma-300m-qat-Q8_0.gguf");
    if !model_path.is_file() {
        return Err(miette::miette!(
            "no embedding model found at {}; backfill requires the local model",
            model_path.display()
        ));
    }
    let provider_config = pattern_provider::embedding::LlamaEmbeddingConfig {
        model_path: model_path.clone(),
        ..Default::default()
    };
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(
        pattern_provider::embedding::LlamaEmbeddingProvider::new(provider_config)
            .map_err(|e| miette::miette!("failed to load embedding provider: {e}"))?,
    );

    let store = pattern_memory::mount::attach(&start, None, Some(provider.clone()))
        .map_err(miette::Report::new)?;

    println!(
        "backfill starting (mount={}, project={}, force_all={}, dry_run={})",
        store.mount_path.display(),
        store.config.project.name,
        cmd.all,
        cmd.dry_run
    );
    println!("  memory.db:   {}", store.db.memory_path().display());
    println!("  messages.db: {}", store.db.messages_path().display());

    let mut archival_stats = Stats::default();
    let mut messages_stats = Stats::default();
    let mut blocks_stats = Stats::default();

    if do_archival {
        backfill_archival(
            &store,
            &*provider,
            cmd.all,
            cmd.dry_run,
            &mut archival_stats,
        )
        .await?;
    }
    if do_messages {
        backfill_messages(
            &store,
            &*provider,
            cmd.all,
            cmd.dry_run,
            &mut messages_stats,
        )
        .await?;
    }
    if do_blocks {
        backfill_blocks(&store, &*provider, cmd.all, cmd.dry_run, &mut blocks_stats).await?;
    }

    store.detach();

    println!("reembed complete:");
    println!("{}", archival_stats.line("archival"));
    println!("{}", messages_stats.line("messages"));
    println!("{}", blocks_stats.line("blocks"));
    Ok(())
}

fn distinct_agent_ids(
    conn: &pattern_db::rusqlite::Connection,
    table_sql: &str,
) -> MietteResult<Vec<String>> {
    let sql = format!("SELECT DISTINCT agent_id FROM {table_sql}");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| miette::miette!("prepare distinct ({table_sql}): {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| miette::miette!("query distinct ({table_sql}): {e}"))?;
    let mut ids = Vec::new();
    for r in rows {
        ids.push(r.map_err(|e| miette::miette!("row ({table_sql}): {e}"))?);
    }
    Ok(ids)
}

async fn backfill_archival(
    store: &pattern_memory::mount::MountedStore,
    provider: &dyn EmbeddingProvider,
    force_all: bool,
    dry_run: bool,
    stats: &mut Stats,
) -> MietteResult<()> {
    let conn = store.db.get().map_err(|e| miette::miette!("db get: {e}"))?;
    let scoped_ids = distinct_agent_ids(&conn, "archival_entries")?;
    let mut entries: Vec<pattern_db::ArchivalEntry> = Vec::new();
    for id in &scoped_ids {
        let mut chunk = pattern_db::queries::list_archival_entries(&conn, id, i64::MAX, 0)
            .map_err(|e| miette::miette!("list archival for {}: {e}", id))?;
        entries.append(&mut chunk);
    }
    drop(conn);

    println!(
        "  archival: {} entries across {} agent_ids ({:?})",
        entries.len(),
        scoped_ids.len(),
        scoped_ids
    );

    for entry in entries {
        embed_one(
            store,
            provider,
            ContentType::ArchivalEntry,
            &entry.id,
            &entry.content,
            force_all,
            dry_run,
            stats,
        )
        .await;
    }
    Ok(())
}

async fn backfill_messages(
    store: &pattern_memory::mount::MountedStore,
    provider: &dyn EmbeddingProvider,
    force_all: bool,
    dry_run: bool,
    stats: &mut Stats,
) -> MietteResult<()> {
    let conn = store.db.get().map_err(|e| miette::miette!("db get: {e}"))?;
    let scoped_ids = distinct_agent_ids(&conn, "msg.messages")?;
    let mut messages: Vec<pattern_db::models::Message> = Vec::new();
    for id in &scoped_ids {
        let mut chunk = pattern_db::queries::get_messages_with_archived(&conn, id, i64::MAX)
            .map_err(|e| miette::miette!("list messages for {}: {e}", id))?;
        messages.append(&mut chunk);
    }
    drop(conn);

    println!(
        "  messages: {} rows across {} agent_ids ({:?})",
        messages.len(),
        scoped_ids.len(),
        scoped_ids
    );

    for msg in messages {
        // content_json is Json<serde_json::Value>; deserialize the inner Value into ChatMessage.
        let chat: pattern_core::types::provider::ChatMessage =
            match serde_json::from_value(msg.content_json.0.clone()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(id = %msg.id, error = %e, "content_json deserialize failed");
                    stats.failed += 1;
                    continue;
                }
            };
        let text = render_chat_message_for_embedding(&chat);
        if text.is_empty() {
            stats.skipped += 1;
            continue;
        }
        embed_one(
            store,
            provider,
            ContentType::Message,
            &msg.id.clone(),
            &text,
            force_all,
            dry_run,
            stats,
        )
        .await;
    }
    Ok(())
}

async fn backfill_blocks(
    store: &pattern_memory::mount::MountedStore,
    provider: &dyn EmbeddingProvider,
    force_all: bool,
    dry_run: bool,
    stats: &mut Stats,
) -> MietteResult<()> {
    let conn = store.db.get().map_err(|e| miette::miette!("db get: {e}"))?;
    let scoped_ids = distinct_agent_ids(&conn, "memory_blocks")?;
    let mut blocks: Vec<pattern_db::MemoryBlock> = Vec::new();
    for id in &scoped_ids {
        let mut chunk = pattern_db::queries::list_blocks(&conn, id)
            .map_err(|e| miette::miette!("list blocks for {}: {e}", id))?;
        blocks.append(&mut chunk);
    }
    drop(conn);

    println!(
        "  blocks: {} rows across {} agent_ids ({:?})",
        blocks.len(),
        scoped_ids.len(),
        scoped_ids
    );

    for block in blocks {
        // content_preview is the canonical render, populated by persist_block via
        // StructuredDocument::render(). For rows where the preview was never
        // populated (e.g. very old data or a path that bypasses persist_block),
        // fall back to fetching the live doc through the cache and rendering.
        let text = match block.content_preview.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => match store.cache.get(&block.agent_id, &block.label) {
                Ok(Some(doc)) => doc.render(),
                _ => {
                    stats.skipped += 1;
                    continue;
                }
            },
        };
        if text.is_empty() {
            stats.skipped += 1;
            continue;
        }
        embed_one(
            store,
            provider,
            ContentType::MemoryBlock,
            &block.id.clone(),
            &text,
            force_all,
            dry_run,
            stats,
        )
        .await;
    }
    Ok(())
}

/// Embed one row given its canonical text. Handles the missing-check,
/// dry-run, embedding compute, and persist steps uniformly across content types.
#[allow(clippy::too_many_arguments)]
async fn embed_one(
    store: &pattern_memory::mount::MountedStore,
    provider: &dyn EmbeddingProvider,
    content_type: ContentType,
    id: &str,
    text: &str,
    force_all: bool,
    dry_run: bool,
    stats: &mut Stats,
) {
    let canonical_bytes = text.as_bytes().to_vec();
    let hash_hex = blake3::hash(&canonical_bytes).to_hex().to_string();

    if !force_all {
        match store.db.get() {
            Ok(conn) => {
                if embedding_is_current(&conn, content_type, id, &hash_hex).unwrap_or(false) {
                    stats.skipped += 1;
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(id = %id, error = %e, "db get failed during is_current check");
                stats.failed += 1;
                return;
            }
        }
    }

    if dry_run {
        stats.embedded += 1;
        return;
    }

    match provider.embed_query(text).await {
        Ok(embedding) => {
            let db = store.db.clone();
            let id_owned = id.to_string();
            let hash_owned = hash_hex.clone();
            let res = tokio::task::spawn_blocking(move || {
                let conn = db.get()?;
                update_embedding(
                    &conn,
                    content_type,
                    &id_owned,
                    &embedding,
                    None,
                    Some(&hash_owned),
                )
            })
            .await;
            match res {
                Ok(Ok(_)) => stats.embedded += 1,
                Ok(Err(e)) => {
                    tracing::warn!(id = %id, error = %e, "update_embedding failed");
                    stats.failed += 1;
                }
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "spawn_blocking join failed");
                    stats.failed += 1;
                }
            }
        }
        Err(e) => {
            tracing::warn!(id = %id, error = %e, "embed_query failed");
            stats.failed += 1;
        }
    }
}
