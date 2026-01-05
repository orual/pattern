//! Unified search tool for querying across different domains

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    Result,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

/// Search domains available
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchDomain {
    ArchivalMemory,
    Conversations,
    ConstellationMessages,
    All,
}

/// Input for unified search
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchInput {
    /// Where to search
    pub domain: SearchDomain,

    /// Search query
    pub query: String,

    /// Maximum number of results (default: 10)
    #[schemars(default, with = "i64")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,

    /// For conversations: filter by role (user/assistant/tool)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// For time-based filtering: start time
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,

    /// For time-based filtering: end time
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,

    /// Enable fuzzy search for typo-tolerant matching
    #[serde(default)]
    pub fuzzy: bool,
    // request_heartbeat handled via ExecutionMeta injection; field removed
}

/// Output from search operations
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchOutput {
    /// Whether the search was successful
    pub success: bool,

    /// Message about the search
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Search results
    pub results: serde_json::Value,
}

// ============================================================================
// Implementation using ToolContext
// ============================================================================

use crate::memory::SearchOptions;
use crate::runtime::{SearchScope, ToolContext};
use std::sync::Arc;

/// Tool for searching across different domains using ToolContext
#[derive(Clone)]
pub struct SearchTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for SearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl SearchTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl AiTool for SearchTool {
    type Input = SearchInput;
    type Output = SearchOutput;

    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Unified search across different domains (archival_memory, conversations, constellation_messages, all). Returns relevant results ranked by BM25 relevance score. Make regular use of this to ground yourself in past events.
        - Use constellation_messages to search messages from all agents in your constellation.
        - archival_memory domain searches your recall memory.
        - To broaden your search, use a larger limit
        - To narrow your search, you can:
            - use explicit start_time and end_time parameters with rfc3339 datetime parsing
            - filter based on role (user, assistant, tool)
            - use time expressions after your query string
                - e.g. 'search term > 5 days', 'search term < 3 hours',
                       'search term 5 days old', 'search term 1-2 weeks'
                - supported units: hour/hours, day/days, week/weeks, month/months
                - IMPORTANT: time expression must come after query string, distinguishable by regular expression
                - if the only thing in the query is a time expression, it becomes a simple time-based filter
                - if you need to search for something that might otherwise be parsed as a time expression, quote it with \"5 days old\"
                "
    }

    async fn execute(&self, params: Self::Input, _meta: &ExecutionMeta) -> Result<Self::Output> {
        let limit = params
            .limit
            .map(|l| l.max(1).min(100) as usize)
            .unwrap_or(20);

        match params.domain {
            SearchDomain::ArchivalMemory => self.search_archival(&params.query, limit).await,
            SearchDomain::Conversations => {
                // Search current agent's messages
                let options = crate::memory::SearchOptions::new()
                    .limit(limit)
                    .messages_only();

                match self
                    .ctx
                    .search(
                        &params.query,
                        crate::runtime::SearchScope::CurrentAgent,
                        options,
                    )
                    .await
                {
                    Ok(results) => {
                        let formatted: Vec<_> = results
                            .iter()
                            .map(|r| {
                                json!({
                                    "id": r.id,
                                    "content": r.content,
                                    "content_type": format!("{:?}", r.content_type),
                                    "score": r.score,
                                })
                            })
                            .collect();

                        Ok(SearchOutput {
                            success: true,
                            message: Some(format!(
                                "Found {} conversation messages",
                                formatted.len()
                            )),
                            results: json!(formatted),
                        })
                    }
                    Err(e) => Ok(SearchOutput {
                        success: false,
                        message: Some(format!("Conversation search failed: {:?}", e)),
                        results: json!([]),
                    }),
                }
            }
            SearchDomain::ConstellationMessages => {
                // Use SearchScope::Constellation
                self.search_constellation(&params.query, limit).await
            }
            SearchDomain::All => self.search_all(&params.query, limit).await,
        }
    }

    fn usage_rule(&self) -> Option<&'static str> {
        Some("the conversation will be continued when called")
    }

    fn tool_rules(&self) -> Vec<ToolRule> {
        vec![ToolRule {
            tool_name: self.name().to_string(),
            rule_type: ToolRuleType::ContinueLoop,
            conditions: vec![],
            priority: 0,
            metadata: None,
        }]
    }

    fn examples(&self) -> Vec<crate::tool::ToolExample<Self::Input, Self::Output>> {
        vec![crate::tool::ToolExample {
            description: "Search archival memory for user preferences".to_string(),
            parameters: SearchInput {
                domain: SearchDomain::ArchivalMemory,
                query: "favorite color".to_string(),
                limit: Some(5),
                role: None,
                start_time: None,
                end_time: None,
                fuzzy: false,
            },
            expected_output: Some(SearchOutput {
                success: true,
                message: Some("Found 1 archival memory matching 'favorite color'".to_string()),
                results: json!([{
                    "label": "user_preferences",
                    "content": "User's favorite color is blue",
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-01T00:00:00Z"
                }]),
            }),
        }]
    }
}

impl SearchTool {
    async fn search_archival(&self, query: &str, limit: usize) -> Result<SearchOutput> {
        // Use MemoryStore::search_archival
        match self
            .ctx
            .memory()
            .search_archival(self.ctx.agent_id(), query, limit)
            .await
        {
            Ok(entries) => {
                let results: Vec<_> = entries
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "content": entry.content,
                            "created_at": entry.created_at,
                            "metadata": entry.metadata,
                        })
                    })
                    .collect();

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!(
                        "Found {} archival memories matching '{}'",
                        results.len(),
                        query
                    )),
                    results: json!(results),
                })
            }
            Err(e) => Ok(SearchOutput {
                success: false,
                message: Some(format!("Archival search failed: {:?}", e)),
                results: json!([]),
            }),
        }
    }

    async fn search_constellation(&self, query: &str, limit: usize) -> Result<SearchOutput> {
        // Use ToolContext::search with Constellation scope
        let options = SearchOptions::new().limit(limit).messages_only(); // Only search messages for constellation

        match self
            .ctx
            .search(query, SearchScope::Constellation, options)
            .await
        {
            Ok(results) => {
                let formatted: Vec<_> = results
                    .into_iter()
                    .map(|result| {
                        json!({
                            "id": result.id,
                            "content_type": format!("{:?}", result.content_type),
                            "content": result.content,
                            "score": result.score,
                        })
                    })
                    .collect();

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!(
                        "Found {} constellation messages matching '{}'",
                        formatted.len(),
                        query
                    )),
                    results: json!(formatted),
                })
            }
            Err(e) => Ok(SearchOutput {
                success: false,
                message: Some(format!("Constellation search failed: {:?}", e)),
                results: json!([]),
            }),
        }
    }

    async fn search_all(&self, query: &str, limit: usize) -> Result<SearchOutput> {
        // Search both archival and constellation
        let archival_result = self.search_archival(query, limit).await?;
        let constellation_result = self.search_constellation(query, limit).await?;

        let all_results = json!({
            "archival_memory": archival_result.results,
            "constellation_messages": constellation_result.results,
        });

        Ok(SearchOutput {
            success: true,
            message: Some(format!("Searched all domains for '{}'", query)),
            results: all_results,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    #[tokio::test]
    async fn test_search_archival() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Insert some archival memories
        memory
            .insert_archival("test-agent", "User's favorite color is blue", None)
            .await
            .expect("Failed to insert archival memory");

        let tool = SearchTool::new(ctx);

        // Test searching
        let result = tool
            .execute(
                SearchInput {
                    domain: SearchDomain::ArchivalMemory,
                    query: "color".to_string(),
                    limit: Some(5),
                    role: None,
                    start_time: None,
                    end_time: None,
                    fuzzy: false,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.as_ref().unwrap().contains("Found"));
    }
}
