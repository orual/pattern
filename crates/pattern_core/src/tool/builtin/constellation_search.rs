//! Constellation-wide search tool for Archive agents with expanded scope
//!
//! # Known Regressions
//!
//! This tool was ported from AgentHandle-based implementation to ToolContext in commit 61a6093.
//! Several features were lost during this refactoring. See full documentation at:
//! `/docs/regressions/constellation-search-toolcontext-port.md`
//!
//! ## Summary of Major Regressions:
//!
//! 1. **Score adjustment logic lost** - No longer downranks reasoning/tool responses (up to 50% penalty)
//! 2. **Metadata lost** - Results missing: label, agent_name, role, created_at, updated_at timestamps
//! 3. **Fuzzy parameter ignored** - Always uses FTS mode, fuzzy_level conversion removed
//! 4. **Role/time filtering lost** - Parameters accepted but prefixed with `_` (see TODO at line 431)
//! 5. **search_all limit changed** - Now returns up to `limit` total instead of `limit` per domain
//! 6. **Progressive truncation limits changed** - Constellation search lost longer snippet limits
//! 7. **search_archival_in_memory() removed** - No fallback when database search fails
//!
//! ## Needed SearchOptions Extensions:
//!
//! To restore full functionality, SearchOptions needs these additions:
//! ```rust,ignore
//! pub struct SearchOptions {
//!     pub mode: SearchMode,
//!     pub content_types: Vec<SearchContentType>,
//!     pub limit: usize,
//!     // NEEDED:
//!     pub fuzzy_level: Option<i32>,           // For fuzzy search support
//!     pub role_filter: Option<ChatRole>,      // Filter messages by role
//!     pub start_time: Option<DateTime<Utc>>,  // Time range filtering
//!     pub end_time: Option<DateTime<Utc>>,
//!     pub limit_per_type: bool,               // Apply limit to each content type separately
//! }
//! ```
//!
//! ## Needed MemorySearchResult Extensions:
//!
//! To restore metadata in output:
//! ```rust,ignore
//! pub struct MemorySearchResult {
//!     pub id: String,
//!     pub content_type: SearchContentType,
//!     pub content: Option<String>,
//!     pub score: f64,
//!     // NEEDED:
//!     pub label: Option<String>,              // For blocks/archival
//!     pub agent_id: Option<String>,           // Which agent owns this
//!     pub agent_name: Option<String>,         // Display name
//!     pub role: Option<String>,               // For messages: user/assistant/tool
//!     pub created_at: Option<DateTime<Utc>>,
//!     pub updated_at: Option<DateTime<Utc>>,
//! }
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use super::search_utils::extract_snippet;
use crate::{
    Result,
    memory::{SearchContentType, SearchMode, SearchOptions},
    messages::ChatRole,
    runtime::{SearchScope, ToolContext},
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

/// Default search domain for constellation search
fn default_domain() -> ConstellationSearchDomain {
    ConstellationSearchDomain::GroupArchival
}

/// Default limit for constellation search (higher than normal)
fn default_limit() -> i64 {
    30
}

/// Search domains for constellation-wide access
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConstellationSearchDomain {
    LocalArchival,        // Just this agent's archival memory
    GroupArchival,        // Archival memory across all group members
    ConstellationHistory, // Conversation history across entire constellation
    All,                  // Search everything at constellation level
}

/// Input for constellation-wide search
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ConstellationSearchInput {
    /// Where to search (defaults to group_archival)
    #[serde(default = "default_domain")]
    pub domain: ConstellationSearchDomain,

    /// Search query
    pub query: String,

    /// Maximum number of results per agent (default: 30 for comprehensive results)
    #[schemars(default, with = "i64")]
    #[serde(default = "default_limit")]
    pub limit: i64,

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

    /// Enable fuzzy search (Note: Currently a placeholder - fuzzy search not yet implemented)
    /// This will enable typo-tolerant search once SurrealDB fuzzy functions are integrated
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

/// Constellation-wide search tool for Archive agents
#[derive(Clone)]
pub struct ConstellationSearchTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for ConstellationSearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstellationSearchTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

#[async_trait]
impl AiTool for ConstellationSearchTool {
    type Input = ConstellationSearchInput;
    type Output = SearchOutput;

    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Unified search across different domains:
            - local_archival (your own recall memory)
            - group_archival (recall memory for yourself and other entities in your constellation)
            - constellation_history (message history for the entire constellation)
            - all (all of the above)
        Returns relevant results ranked by BM25 relevance score. Make regular use of this to ground yourself in past events.
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
        let limit = params.limit.max(1).min(100) as usize;

        match params.domain {
            ConstellationSearchDomain::LocalArchival => {
                // Search just this agent's archival
                self.search_local_archival(&params.query, limit, params.fuzzy)
                    .await
            }
            ConstellationSearchDomain::GroupArchival => {
                // Search archival across all group members
                self.search_group_archival(&params.query, limit, params.fuzzy)
                    .await
            }
            ConstellationSearchDomain::ConstellationHistory => {
                let role = params
                    .role
                    .as_ref()
                    .and_then(|r| match r.to_lowercase().as_str() {
                        "user" => Some(ChatRole::User),
                        "assistant" => Some(ChatRole::Assistant),
                        "tool" => Some(ChatRole::Tool),
                        _ => None,
                    });

                let start_time = params
                    .start_time
                    .as_ref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                let end_time = params
                    .end_time
                    .as_ref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                self.search_constellation_messages(
                    &params.query,
                    role,
                    start_time,
                    end_time,
                    limit,
                    params.fuzzy,
                )
                .await
            }
            ConstellationSearchDomain::All => {
                // Search everything - both group archival and constellation history
                self.search_all(&params.query, limit, params.fuzzy).await
            }
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
        vec![
            crate::tool::ToolExample {
                description: "Search archival memory for user preferences".to_string(),
                parameters: ConstellationSearchInput {
                    domain: ConstellationSearchDomain::LocalArchival,
                    query: "favorite color".to_string(),
                    limit: 40,
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
            },
            crate::tool::ToolExample {
                description: "Search conversation history for technical discussions".to_string(),
                parameters: ConstellationSearchInput {
                    domain: ConstellationSearchDomain::ConstellationHistory,
                    query: "database design".to_string(),
                    limit: 10,
                    role: Some("assistant".to_string()),
                    start_time: None,
                    end_time: None,
                    fuzzy: false,
                },
                expected_output: Some(SearchOutput {
                    success: true,
                    message: Some("Found 3 messages matching 'database design'".to_string()),
                    results: json!([{
                        "id": "msg_123",
                        "role": "assistant",
                        "content": "For the database design, I recommend using...",
                        "created_at": "2024-01-01T00:00:00Z"
                    }]),
                }),
            },
        ]
    }
}

impl ConstellationSearchTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }

    async fn search_local_archival(
        &self,
        query: &str,
        limit: usize,
        fuzzy: bool,
    ) -> Result<SearchOutput> {
        let options = SearchOptions {
            mode: if fuzzy {
                SearchMode::Hybrid
            } else {
                SearchMode::Fts
            },
            content_types: vec![SearchContentType::Blocks, SearchContentType::Archival],
            limit,
        };

        match self
            .ctx
            .search(query, SearchScope::CurrentAgent, options)
            .await
        {
            Ok(results) => {
                let formatted: Vec<_> = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        // Progressive truncation: show less content for lower-ranked results
                        let content = r.content.as_ref().map(|c| {
                            if i < 2 {
                                c.clone()
                            } else if i < 5 {
                                extract_snippet(c, query, 1000)
                            } else {
                                extract_snippet(c, query, 400)
                            }
                        });

                        json!({
                            "id": &r.id,
                            "content": content,
                            "relevance_score": r.score,
                        })
                    })
                    .collect();

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!(
                        "Found {} archival memories matching '{}'",
                        formatted.len(),
                        query
                    )),
                    results: json!(formatted),
                })
            }
            Err(e) => Ok(SearchOutput {
                success: false,
                message: Some(format!("Search failed: {}", e)),
                results: json!([]),
            }),
        }
    }

    async fn search_group_archival(
        &self,
        query: &str,
        limit: usize,
        fuzzy: bool,
    ) -> Result<SearchOutput> {
        let options = SearchOptions {
            mode: if fuzzy {
                SearchMode::Hybrid
            } else {
                SearchMode::Fts
            },
            content_types: vec![SearchContentType::Blocks, SearchContentType::Archival],
            limit,
        };

        match self
            .ctx
            .search(query, SearchScope::Constellation, options)
            .await
        {
            Ok(results) => {
                let formatted: Vec<_> = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        // Progressive truncation for constellation search - longer content since this is for Archive
                        let content = r.content.as_ref().map(|c| {
                            if i < 5 {
                                // Show more content for top results (Archive is designed for this)
                                c.clone()
                            } else if i < 15 {
                                extract_snippet(c, query, 1500)
                            } else {
                                extract_snippet(c, query, 800)
                            }
                        });

                        json!({
                            "id": &r.id,
                            "content": content,
                            "relevance_score": r.score,
                        })
                    })
                    .collect();

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!(
                        "Found {} group archival memories matching '{}'",
                        formatted.len(),
                        query
                    )),
                    results: json!(formatted),
                })
            }
            Err(e) => {
                tracing::warn!("Group archival search failed: {}", e);
                Ok(SearchOutput {
                    success: false,
                    message: Some(format!("Group archival search failed: {}", e)),
                    results: json!([]),
                })
            }
        }
    }

    async fn search_constellation_messages(
        &self,
        query: &str,
        _role: Option<ChatRole>,
        _start_time: Option<DateTime<Utc>>,
        _end_time: Option<DateTime<Utc>>,
        limit: usize,
        fuzzy: bool,
    ) -> Result<SearchOutput> {
        // TODO: ToolContext doesn't currently expose role/time filtering for message search
        // Need to add these parameters to SearchOptions once message search is fully integrated
        let options = SearchOptions {
            mode: if fuzzy {
                SearchMode::Hybrid
            } else {
                SearchMode::Fts
            },
            content_types: vec![SearchContentType::Messages],
            limit,
        };

        match self
            .ctx
            .search(query, SearchScope::Constellation, options)
            .await
        {
            Ok(results) => {
                let formatted: Vec<_> = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        // Progressive content display
                        let content = r.content.as_ref().map(|c| {
                            if i < 2 {
                                c.clone()
                            } else if i < 5 {
                                extract_snippet(c, query, 400)
                            } else {
                                extract_snippet(c, query, 200)
                            }
                        });

                        json!({
                            "id": &r.id,
                            "content": content,
                            "relevance_score": r.score,
                        })
                    })
                    .collect();

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!(
                        "Found {} constellation messages matching '{}' (ranked by relevance)",
                        formatted.len(),
                        query
                    )),
                    results: json!(formatted),
                })
            }
            Err(e) => Ok(SearchOutput {
                success: false,
                message: Some(format!("Constellation message search failed: {}", e)),
                results: json!([]),
            }),
        }
    }

    async fn search_all(&self, query: &str, limit: usize, fuzzy: bool) -> Result<SearchOutput> {
        // Search both archival and messages across constellation
        let options = SearchOptions {
            mode: if fuzzy {
                SearchMode::Hybrid
            } else {
                SearchMode::Fts
            },
            content_types: vec![
                SearchContentType::Archival,
                SearchContentType::Blocks,
                SearchContentType::Messages,
            ],
            limit,
        };

        match self
            .ctx
            .search(query, SearchScope::Constellation, options)
            .await
        {
            Ok(results) => {
                // Separate by content type
                let mut archival = Vec::new();
                let mut messages = Vec::new();

                for (i, r) in results.iter().enumerate() {
                    let content = r.content.as_ref().map(|c| {
                        if i < 2 {
                            c.clone()
                        } else if i < 5 {
                            extract_snippet(c, query, 1000)
                        } else {
                            extract_snippet(c, query, 400)
                        }
                    });

                    let item = json!({
                        "id": &r.id,
                        "content": content,
                        "relevance_score": r.score,
                    });

                    match r.content_type {
                        SearchContentType::Archival => archival.push(item),
                        SearchContentType::Blocks => archival.push(item),
                        SearchContentType::Messages => messages.push(item),
                    }
                }

                let all_results = json!({
                    "archival_memory": archival,
                    "conversations": messages
                });

                Ok(SearchOutput {
                    success: true,
                    message: Some(format!("Searched all domains for '{}'", query)),
                    results: all_results,
                })
            }
            Err(e) => Ok(SearchOutput {
                success: false,
                message: Some(format!("Search all failed: {}", e)),
                results: json!({"archival_memory": [], "conversations": []}),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ConstellationDatabases;
    use crate::memory::{BlockSchema, BlockType};
    use crate::runtime::ToolContext;
    use crate::tool::builtin::test_utils::MockToolContext;
    use std::sync::Arc;

    async fn create_test_context() -> Arc<MockToolContext> {
        let dbs = Arc::new(
            ConstellationDatabases::open_in_memory()
                .await
                .expect("Failed to create test dbs"),
        );

        // Create a test agent in the database
        let agent = pattern_db::models::Agent {
            id: "test-agent".to_string(),
            name: "Test Agent".to_string(),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: Default::default(),
            enabled_tools: Default::default(),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .expect("Failed to create test agent");

        let memory = Arc::new(crate::memory::MemoryCache::new(Arc::clone(&dbs)));
        Arc::new(MockToolContext::new("test-agent", memory, dbs))
    }

    #[tokio::test]
    async fn test_archival_search_returns_blocks_and_archival() {
        let ctx = create_test_context().await;

        // Insert a memory block with searchable content
        ctx.memory()
            .create_block(
                "test-agent",
                "preferences",
                "User preferences",
                BlockType::Core,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = ctx
            .memory()
            .get_block("test-agent", "preferences")
            .await
            .unwrap()
            .unwrap();
        doc.set_text("I love rust programming and system design", true)
            .unwrap();
        ctx.memory().mark_dirty("test-agent", "preferences");
        ctx.memory()
            .persist_block("test-agent", "preferences")
            .await
            .unwrap();

        // Insert an archival entry with searchable content
        ctx.memory()
            .insert_archival(
                "test-agent",
                "Rust is great for systems programming and has excellent tooling",
                None,
            )
            .await
            .unwrap();

        // Create tool and search for "rust"
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool.search_local_archival("rust", 10, false).await.unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert!(
            results.len() >= 2,
            "Should find both block and archival entry, found {}",
            results.len()
        );

        // Verify result format
        for r in results {
            assert!(r.get("id").is_some(), "Result should have id field");
            assert!(
                r.get("content").is_some(),
                "Result should have content field"
            );
            assert!(
                r.get("relevance_score").is_some(),
                "Result should have relevance_score field"
            );

            // Verify content contains "rust"
            let content = r.get("content").unwrap().as_str().unwrap();
            assert!(
                content.to_lowercase().contains("rust"),
                "Content should contain 'rust': {}",
                content
            );
        }
    }

    #[tokio::test]
    async fn test_archival_search_fts_mode() {
        let ctx = create_test_context().await;

        // Insert test data
        ctx.memory()
            .insert_archival("test-agent", "Python is a dynamic language", None)
            .await
            .unwrap();
        ctx.memory()
            .insert_archival("test-agent", "JavaScript is used for web development", None)
            .await
            .unwrap();
        ctx.memory()
            .insert_archival("test-agent", "Rust provides memory safety", None)
            .await
            .unwrap();

        // Search with fuzzy=false (should use FTS mode)
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("memory", 10, false)
            .await
            .unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert_eq!(
            results.len(),
            1,
            "Should find exactly one result with 'memory'"
        );

        let content = results[0].get("content").unwrap().as_str().unwrap();
        assert!(
            content.contains("memory safety"),
            "Should find the Rust entry"
        );
    }

    #[tokio::test]
    async fn test_archival_search_hybrid_mode_fallback() {
        let ctx = create_test_context().await;

        // Insert test data
        ctx.memory()
            .insert_archival("test-agent", "Testing hybrid search fallback to FTS", None)
            .await
            .unwrap();

        // Search with fuzzy=true (should request Hybrid but fall back to FTS with warning)
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("hybrid", 10, true)
            .await
            .unwrap();

        assert!(
            result.success,
            "Should succeed even without embedding provider"
        );
        let results = result.results.as_array().unwrap();
        assert_eq!(results.len(), 1, "Should find result using FTS fallback");

        let content = results[0].get("content").unwrap().as_str().unwrap();
        assert!(content.contains("hybrid search"));
    }

    #[tokio::test]
    async fn test_search_respects_limit() {
        let ctx = create_test_context().await;

        // Insert many archival entries
        for i in 0..20 {
            ctx.memory()
                .insert_archival(
                    "test-agent",
                    &format!("Test entry {} about searching", i),
                    None,
                )
                .await
                .unwrap();
        }

        // Search with limit of 5
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("searching", 5, false)
            .await
            .unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert!(
            results.len() <= 5,
            "Should respect limit of 5, got {}",
            results.len()
        );
    }

    #[tokio::test]
    async fn test_search_blocks_only() {
        let ctx = create_test_context().await;

        // Create a memory block
        ctx.memory()
            .create_block(
                "test-agent",
                "notes",
                "Working notes",
                BlockType::Working,
                BlockSchema::text(),
                1000,
            )
            .await
            .unwrap();

        let doc = ctx
            .memory()
            .get_block("test-agent", "notes")
            .await
            .unwrap()
            .unwrap();
        doc.set_text("Important meeting scheduled for tomorrow", true)
            .unwrap();
        ctx.memory().mark_dirty("test-agent", "notes");
        ctx.memory()
            .persist_block("test-agent", "notes")
            .await
            .unwrap();

        // Search for content only in block (not in any archival)
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("meeting", 10, false)
            .await
            .unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert_eq!(results.len(), 1, "Should find the block");

        let content = results[0].get("content").unwrap().as_str().unwrap();
        assert!(content.contains("meeting"));
    }

    #[tokio::test]
    async fn test_search_archival_only() {
        let ctx = create_test_context().await;

        // Insert archival entries only (no blocks)
        ctx.memory()
            .insert_archival("test-agent", "Database schema design notes", None)
            .await
            .unwrap();
        ctx.memory()
            .insert_archival("test-agent", "API endpoint implementation details", None)
            .await
            .unwrap();

        // Search for archival content
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("database", 10, false)
            .await
            .unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert_eq!(results.len(), 1, "Should find exactly one archival entry");

        let content = results[0].get("content").unwrap().as_str().unwrap();
        assert!(content.contains("Database"));
    }

    #[tokio::test]
    async fn test_search_returns_empty_when_no_matches() {
        let ctx = create_test_context().await;

        // Insert some data that won't match
        ctx.memory()
            .insert_archival("test-agent", "Python programming guide", None)
            .await
            .unwrap();

        // Search for something that doesn't exist
        let tool = ConstellationSearchTool::new(Arc::clone(&ctx) as Arc<dyn ToolContext>);
        let result = tool
            .search_local_archival("xyznonexistent", 10, false)
            .await
            .unwrap();

        assert!(result.success);
        let results = result.results.as_array().unwrap();
        assert_eq!(results.len(), 0, "Should return empty results");
    }
}
