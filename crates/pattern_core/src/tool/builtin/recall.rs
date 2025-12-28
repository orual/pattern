//! Archival entry management tool (simplified).
//!
//! This is the v2 recall tool with simplified Insert/Search operations.
//! It replaces the legacy recall tool which had Insert/Append/Read/Delete.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::CoreError;
use crate::runtime::ToolContext;
use crate::tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType};

use super::types::{RecallInput, RecallOp, ToolOutput};

/// Archival entry management tool (simplified).
///
/// Operations:
/// - `insert` - Create new immutable archival entry
/// - `search` - Full-text search over archival entries
///
/// Note: This operates on archival *entries*, not Archival-typed blocks.
/// Archival entries are immutable once created.
#[derive(Clone)]
pub struct RecallTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for RecallTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl RecallTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    async fn handle_insert(
        &self,
        content: Option<String>,
        metadata: Option<serde_json::Value>,
    ) -> crate::Result<ToolOutput> {
        let content = content.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "recall",
                json!({"op": "insert"}),
                "insert requires 'content' parameter",
            )
        })?;

        let memory = self.ctx.memory();
        let entry_id = memory
            .insert_archival(self.ctx.agent_id(), &content, metadata)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "recall",
                    json!({"op": "insert"}),
                    format!("Failed to insert archival entry: {}", e),
                )
            })?;

        Ok(ToolOutput::success_with_data(
            "Archival entry created",
            json!({ "entry_id": entry_id }),
        ))
    }

    async fn handle_search(
        &self,
        query: Option<String>,
        limit: Option<usize>,
    ) -> crate::Result<ToolOutput> {
        let query = query.ok_or_else(|| {
            CoreError::tool_exec_msg(
                "recall",
                json!({"op": "search"}),
                "search requires 'query' parameter",
            )
        })?;
        let limit = limit.unwrap_or(10);

        let memory = self.ctx.memory();
        let results = memory
            .search_archival(self.ctx.agent_id(), &query, limit)
            .await
            .map_err(|e| {
                CoreError::tool_exec_msg(
                    "recall",
                    json!({"op": "search", "query": query}),
                    format!("Search failed: {}", e),
                )
            })?;

        let entries: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                let mut entry = json!({
                    "id": r.id,
                    "content": r.content,
                    "created_at": r.created_at.to_rfc3339(),
                });
                if let Some(metadata) = r.metadata {
                    entry["metadata"] = metadata;
                }
                entry
            })
            .collect();

        Ok(ToolOutput::success_with_data(
            format!("Found {} archival entries", entries.len()),
            json!({ "entries": entries }),
        ))
    }
}

#[async_trait]
impl AiTool for RecallTool {
    type Input = RecallInput;
    type Output = ToolOutput;

    fn name(&self) -> &str {
        "recall"
    }

    fn description(&self) -> &str {
        "Manage archival memory: insert new entries for long-term storage or search existing entries. Entries are immutable once created."
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some(
            "Use to store important information for later retrieval. Search when you need to remember something from the past.",
        )
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule::new(
            self.name().to_string(),
            ToolRuleType::ContinueLoop,
        )]
    }

    fn operations(&self) -> &'static [&'static str] {
        &["insert", "search"]
    }

    async fn execute(
        &self,
        input: Self::Input,
        _meta: &ExecutionMeta,
    ) -> crate::Result<Self::Output> {
        match input.op {
            RecallOp::Insert => self.handle_insert(input.content, input.metadata).await,
            RecallOp::Search => self.handle_search(input.query, input.limit).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    async fn create_test_context() -> Arc<crate::tool::builtin::test_utils::MockToolContext> {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;
        ctx
    }

    #[tokio::test]
    async fn test_recall_insert() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        let result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Insert,
                    content: Some("Test archival content".to_string()),
                    metadata: Some(json!({"tag": "test"})),
                    query: None,
                    limit: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.message, "Archival entry created");
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        assert!(data.get("entry_id").is_some());
    }

    #[tokio::test]
    async fn test_recall_insert_requires_content() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        let result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Insert,
                    content: None,
                    metadata: None,
                    query: None,
                    limit: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err(), "Expected error but got: {:?}", result);
        let err = result.unwrap_err();
        // Check that we got a ToolExecutionFailed with cause containing "content"
        match err {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("content"),
                    "Expected cause to mention 'content', got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_recall_search() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx.clone());

        // First insert some data
        let insert_result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Insert,
                    content: Some("Important fact about golden retrievers".to_string()),
                    metadata: None,
                    query: None,
                    limit: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(
            insert_result.success,
            "Insert failed: {}",
            insert_result.message
        );

        // Now search for it
        let search_result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Search,
                    content: None,
                    metadata: None,
                    query: Some("golden retrievers".to_string()),
                    limit: Some(5),
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(search_result.success);
        assert!(search_result.data.is_some());
        let data = search_result.data.unwrap();
        let entries = data.get("entries").unwrap().as_array().unwrap();
        assert!(!entries.is_empty(), "Expected at least one search result");

        // Verify the found entry contains the expected content
        let first_entry = &entries[0];
        let content = first_entry.get("content").unwrap().as_str().unwrap();
        assert!(
            content.contains("golden retrievers"),
            "Expected content to contain 'golden retrievers', got: {}",
            content
        );
    }

    #[tokio::test]
    async fn test_recall_search_requires_query() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        let result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Search,
                    content: None,
                    metadata: None,
                    query: None,
                    limit: None,
                },
                &ExecutionMeta::default(),
            )
            .await;

        assert!(result.is_err(), "Expected error but got: {:?}", result);
        let err = result.unwrap_err();
        // Check that we got a ToolExecutionFailed with cause containing "query"
        match err {
            crate::CoreError::ToolExecutionFailed { cause, .. } => {
                assert!(
                    cause.contains("query"),
                    "Expected cause to mention 'query', got: {}",
                    cause
                );
            }
            other => panic!("Expected ToolExecutionFailed, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_recall_search_empty_results() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        // Search without inserting anything first
        let result = tool
            .execute(
                RecallInput {
                    op: RecallOp::Search,
                    content: None,
                    metadata: None,
                    query: Some("nonexistent topic xyz123".to_string()),
                    limit: None,
                },
                &ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        let entries = data.get("entries").unwrap().as_array().unwrap();
        assert!(entries.is_empty());
    }
}
