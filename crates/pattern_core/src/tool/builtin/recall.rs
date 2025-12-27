//! Recall storage management tool following Letta/MemGPT patterns

use std::fmt::Debug;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    memory::MemoryPermission,
    memory_acl::{MemoryGate, MemoryOp, check as acl_check, consent_reason},
    permission::PermissionScope,
    runtime::ToolContext,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

/// Operation types for recall storage management
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ArchivalMemoryOperationType {
    Insert,
    Append,
    Read,
    Delete,
}

/// Input for managing recall storage
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RecallInput {
    /// The operation to perform
    pub operation: ArchivalMemoryOperationType,

    /// For insert/read/delete: label for the memory (insert defaults to "archival_<timestamp>")
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// For insert: content to store in recall storage
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    // request_heartbeat handled via ExecutionMeta injection; field removed
}

/// Output from recall storage operations
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecallOutput {
    /// Whether the operation was successful
    pub success: bool,

    /// Message about the operation
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// For search operations, the matching entries
    #[schemars(default)]
    pub results: Vec<ArchivalSearchResult>,
}

/// A single search result from recall storage
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ArchivalSearchResult {
    /// Label of the memory block
    pub label: String,
    /// Content of the memory
    pub content: String,
    /// When the memory was created
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the memory was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ============================================================================
// Recall Tool using ToolContext
// ============================================================================
use std::sync::Arc;

/// Tool for managing recall storage using ToolContext
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
}

#[async_trait]
impl AiTool for RecallTool {
    type Input = RecallInput;
    type Output = RecallOutput;

    fn name(&self) -> &str {
        "recall"
    }

    fn description(&self) -> &str {
        "Manage long-term recall storage. Recall memories are not always visible in context. Operations: insert, append, read (by label), delete.
 - 'insert' creates a new recall memory with the provided content
 - 'append' appends the provided content to the recall memory with the specified label
 - 'read' reads out the contents of the recall block with the specified label
 - 'delete' removes the recall memory with the specified label"
    }

    async fn execute(
        &self,
        params: Self::Input,
        meta: &crate::tool::ExecutionMeta,
    ) -> Result<Self::Output> {
        match params.operation {
            ArchivalMemoryOperationType::Insert => {
                let content = params.content.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "recall",
                        serde_json::json!({"operation":"insert"}),
                        "insert operation requires 'content' field",
                    )
                })?;
                self.execute_insert(content, params.label).await
            }
            ArchivalMemoryOperationType::Append => {
                let label = params.label.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "recall",
                        serde_json::json!({"operation":"append"}),
                        "append operation requires 'label' field",
                    )
                })?;
                let content = params.content.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "recall",
                        serde_json::json!({"operation":"append"}),
                        "append operation requires 'content' field",
                    )
                })?;
                self.execute_append(label, content, meta).await
            }
            ArchivalMemoryOperationType::Read => {
                let label = params.label.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "recall",
                        serde_json::json!({"operation":"read"}),
                        "read operation requires 'label' field",
                    )
                })?;
                self.execute_read(label).await
            }
            ArchivalMemoryOperationType::Delete => {
                let label = params.label.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "recall",
                        serde_json::json!({"operation":"delete"}),
                        "delete operation requires 'label' field",
                    )
                })?;
                self.execute_delete(label, meta).await
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
                description: "Store important information for later".to_string(),
                parameters: RecallInput {
                    operation: ArchivalMemoryOperationType::Insert,
                    content: Some(
                        "User mentioned they have a dog named Max who likes to play fetch."
                            .to_string(),
                    ),
                    label: None,
                },
                expected_output: Some(RecallOutput {
                    success: true,
                    message: Some("Created recall memory 'archival_1234567890'".to_string()),
                    results: vec![],
                }),
            },
            crate::tool::ToolExample {
                description: "Add more information to existing recall memory".to_string(),
                parameters: RecallInput {
                    operation: ArchivalMemoryOperationType::Append,
                    content: Some("Max is a golden retriever.".to_string()),
                    label: Some("archival_1234567890".to_string()),
                },
                expected_output: Some(RecallOutput {
                    success: true,
                    message: Some("Appended to recall memory 'archival_1234567890'".to_string()),
                    results: vec![],
                }),
            },
        ]
    }
}

impl RecallTool {
    /// Helper to check if we can bypass permission checks
    fn can_bypass(&self, meta: &ExecutionMeta, key: &str) -> bool {
        if let Some(grant) = &meta.permission_grant {
            match &grant.scope {
                PermissionScope::MemoryEdit { key: gk } => gk == key,
                PermissionScope::MemoryBatch { prefix } => key.starts_with(prefix),
                _ => false,
            }
        } else {
            false
        }
    }

    /// Convert pattern_db MemoryPermission to core MemoryPermission
    fn convert_permission(&self, perm: pattern_db::models::MemoryPermission) -> MemoryPermission {
        use crate::memory::MemoryPermission as CorePerm;
        match perm {
            pattern_db::models::MemoryPermission::ReadOnly => CorePerm::ReadOnly,
            pattern_db::models::MemoryPermission::Partner => CorePerm::Partner,
            pattern_db::models::MemoryPermission::Human => CorePerm::Human,
            pattern_db::models::MemoryPermission::Append => CorePerm::Append,
            pattern_db::models::MemoryPermission::ReadWrite => CorePerm::ReadWrite,
            pattern_db::models::MemoryPermission::Admin => CorePerm::Admin,
        }
    }

    /// Helper to check permission and request consent if needed
    async fn check_permission(
        &self,
        block_name: &str,
        op: MemoryOp,
        meta: &ExecutionMeta,
    ) -> Result<Option<String>> {
        let agent_id = self.ctx.agent_id();

        // Get block metadata to check permission
        let current_perm = match self
            .ctx
            .memory()
            .get_block_metadata(agent_id, block_name)
            .await
        {
            Ok(Some(metadata)) => self.convert_permission(metadata.permission),
            Ok(None) => {
                return Ok(Some(format!("Memory block '{}' not found", block_name)));
            }
            Err(e) => {
                return Ok(Some(format!("Failed to get block metadata: {:?}", e)));
            }
        };

        // Gate by permission, offering consent path when applicable
        if !self.can_bypass(meta, block_name) {
            match acl_check(op, current_perm) {
                MemoryGate::Allow => {}
                MemoryGate::Deny { reason } => {
                    return Ok(Some(format!(
                        "{} — cannot {:?} '{}'",
                        reason, op, block_name
                    )));
                }
                MemoryGate::RequireConsent { .. } => {
                    let agent_id_parsed = match agent_id.parse::<crate::AgentId>() {
                        Ok(id) => id,
                        Err(_) => {
                            return Ok(Some(format!("Invalid agent ID format: {}", agent_id)));
                        }
                    };

                    let grant = self
                        .ctx
                        .permission_broker()
                        .request(
                            agent_id_parsed,
                            "recall".to_string(),
                            PermissionScope::MemoryEdit {
                                key: block_name.to_string(),
                            },
                            Some(consent_reason(block_name, op, current_perm)),
                            meta.route_metadata.clone(),
                            std::time::Duration::from_secs(90),
                        )
                        .await;
                    if grant.is_none() {
                        return Ok(Some(format!(
                            "{:?} on '{}' requires approval; request timed out or was denied",
                            op, block_name
                        )));
                    }
                }
            }
        }

        Ok(None) // No error
    }

    async fn execute_insert(&self, content: String, label: Option<String>) -> Result<RecallOutput> {
        let label = label.unwrap_or_else(|| format!("archival_{}", chrono::Utc::now().timestamp()));

        // Create archival entry with label in metadata for future reference
        let metadata = serde_json::json!({
            "label": label,
        });

        match self
            .ctx
            .memory()
            .insert_archival(self.ctx.agent_id(), &content, Some(metadata))
            .await
        {
            Ok(_id) => Ok(RecallOutput {
                success: true,
                message: Some(format!("Created recall memory '{}'", label)),
                results: vec![],
            }),
            Err(e) => Ok(RecallOutput {
                success: false,
                message: Some(format!("Failed to create recall memory: {:?}", e)),
                results: vec![],
            }),
        }
    }

    async fn execute_read(&self, label: String) -> Result<RecallOutput> {
        let agent_id = self.ctx.agent_id();

        // First: Check if an archival BLOCK exists with that label
        match self.ctx.memory().get_block(agent_id, &label).await {
            Ok(Some(doc)) => {
                // Check if it's actually an archival block
                match self.ctx.memory().get_block_metadata(agent_id, &label).await {
                    Ok(Some(metadata)) => {
                        if metadata.block_type == crate::memory::BlockType::Archival {
                            // Found archival block
                            let content = doc.text_content();
                            return Ok(RecallOutput {
                                success: true,
                                message: Some(format!("Found recall memory block '{}'", label)),
                                results: vec![ArchivalSearchResult {
                                    label: label.clone(),
                                    content,
                                    created_at: metadata.created_at,
                                    updated_at: metadata.updated_at,
                                }],
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        // If no archival block found, search archival ENTRIES
        match self
            .ctx
            .memory()
            .search_archival(agent_id, &label, 10)
            .await
        {
            Ok(entries) => {
                let results = entries
                    .into_iter()
                    .map(|entry| {
                        // Extract label from metadata if present, otherwise use entry id
                        let label = entry
                            .metadata
                            .as_ref()
                            .and_then(|m| m.get("label"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| entry.id.clone());
                        ArchivalSearchResult {
                            label,
                            content: entry.content,
                            created_at: entry.created_at,
                            updated_at: entry.created_at, // ArchivalEntry doesn't have updated_at
                        }
                    })
                    .collect::<Vec<_>>();

                if results.is_empty() {
                    Ok(RecallOutput {
                        success: false,
                        message: Some(format!("Couldn't find recall memory '{}'", label)),
                        results: vec![],
                    })
                } else {
                    Ok(RecallOutput {
                        success: true,
                        message: Some(format!(
                            "Found {} recall entries matching '{}'",
                            results.len(),
                            label
                        )),
                        results,
                    })
                }
            }
            Err(e) => Ok(RecallOutput {
                success: false,
                message: Some(format!("Failed to read recall memory: {:?}", e)),
                results: vec![],
            }),
        }
    }

    async fn execute_delete(&self, label: String, meta: &ExecutionMeta) -> Result<RecallOutput> {
        let agent_id = self.ctx.agent_id();

        // Only delete archival BLOCKS, not entries
        // First check if a block exists with that label
        match self.ctx.memory().get_block_metadata(agent_id, &label).await {
            Ok(Some(metadata)) => {
                // Check if it's an archival block
                if metadata.block_type != crate::memory::BlockType::Archival {
                    return Ok(RecallOutput {
                        success: false,
                        message: Some(format!(
                            "Block '{}' is not recall memory (type: {:?})",
                            label, metadata.block_type
                        )),
                        results: vec![],
                    });
                }

                // Check permissions (requires Admin or consent)
                if let Some(error_msg) = self
                    .check_permission(&label, MemoryOp::Delete, meta)
                    .await?
                {
                    return Ok(RecallOutput {
                        success: false,
                        message: Some(error_msg),
                        results: vec![],
                    });
                }

                // Delete the block
                match self.ctx.memory().delete_block(agent_id, &label).await {
                    Ok(()) => Ok(RecallOutput {
                        success: true,
                        message: Some(format!("Deleted recall memory block '{}'", label)),
                        results: vec![],
                    }),
                    Err(e) => Ok(RecallOutput {
                        success: false,
                        message: Some(format!("Failed to delete recall memory block: {:?}", e)),
                        results: vec![],
                    }),
                }
            }
            Ok(None) => {
                // No block found - can't delete archival entries
                Ok(RecallOutput {
                    success: false,
                    message: Some(format!(
                        "Cannot delete recall memory '{}' - only archival blocks can be deleted. Archival entries are immutable.",
                        label
                    )),
                    results: vec![],
                })
            }
            Err(e) => Ok(RecallOutput {
                success: false,
                message: Some(format!("Failed to check for recall memory block: {:?}", e)),
                results: vec![],
            }),
        }
    }

    async fn execute_append(
        &self,
        label: String,
        content: String,
        meta: &ExecutionMeta,
    ) -> Result<RecallOutput> {
        let agent_id = self.ctx.agent_id();

        // First: Check if an archival BLOCK exists with that label
        match self.ctx.memory().get_block_metadata(agent_id, &label).await {
            Ok(Some(metadata)) => {
                // Check if it's an archival block
                if metadata.block_type != crate::memory::BlockType::Archival {
                    return Ok(RecallOutput {
                        success: false,
                        message: Some(format!(
                            "Block '{}' is not recall memory (type: {:?})",
                            label, metadata.block_type
                        )),
                        results: vec![],
                    });
                }

                // Check permissions (requires Append permission or consent)
                if let Some(error_msg) = self
                    .check_permission(&label, MemoryOp::Append, meta)
                    .await?
                {
                    return Ok(RecallOutput {
                        success: false,
                        message: Some(error_msg),
                        results: vec![],
                    });
                }

                // Append to the block
                match self
                    .ctx
                    .memory()
                    .append_to_block(agent_id, &label, &content)
                    .await
                {
                    Ok(()) => {
                        // Get updated preview
                        let preview = if let Ok(Some(doc)) =
                            self.ctx.memory().get_block(agent_id, &label).await
                        {
                            let text = doc.text_content();
                            let char_count = text.chars().count();
                            let preview_chars = 200;
                            let content_preview = if text.len() > preview_chars {
                                format!("...{}", &text[text.len().saturating_sub(preview_chars)..])
                            } else {
                                text
                            };
                            format!(
                                "Successfully appended {} characters to recall memory '{}'. The memory now contains {} total characters. Preview: {}",
                                content.len(),
                                label,
                                char_count,
                                content_preview
                            )
                        } else {
                            format!(
                                "Successfully appended {} characters to recall memory '{}'",
                                content.len(),
                                label
                            )
                        };

                        Ok(RecallOutput {
                            success: true,
                            message: Some(preview),
                            results: vec![],
                        })
                    }
                    Err(e) => Ok(RecallOutput {
                        success: false,
                        message: Some(format!("Failed to append to recall memory: {:?}", e)),
                        results: vec![],
                    }),
                }
            }
            Ok(None) => {
                // No block exists - create a new archival entry with this content
                let metadata = Some(serde_json::json!({ "label": label }));
                match self
                    .ctx
                    .memory()
                    .insert_archival(agent_id, &content, metadata)
                    .await
                {
                    Ok(id) => Ok(RecallOutput {
                        success: true,
                        message: Some(format!(
                            "Created new recall memory '{}' (entry ID: {}) with {} characters",
                            label,
                            id,
                            content.len()
                        )),
                        results: vec![],
                    }),
                    Err(e) => Ok(RecallOutput {
                        success: false,
                        message: Some(format!("Failed to create recall memory: {:?}", e)),
                        results: vec![],
                    }),
                }
            }
            Err(e) => Ok(RecallOutput {
                success: false,
                message: Some(format!("Failed to check for recall memory block: {:?}", e)),
                results: vec![],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::builtin::test_utils::{MockToolContext, create_test_context_with_agent};
    use std::sync::Arc;

    async fn create_test_context() -> Arc<MockToolContext> {
        let (_db, _memory, ctx) = create_test_context_with_agent("test-agent").await;
        ctx
    }

    #[tokio::test]
    async fn test_archival_insert() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        let result = tool
            .execute(
                RecallInput {
                    operation: ArchivalMemoryOperationType::Insert,
                    content: Some("Test content".to_string()),
                    label: Some("test_label".to_string()),
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("test_label"));
    }

    #[tokio::test]
    async fn test_archival_read() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        // First insert some data
        let insert_result = tool
            .execute(
                RecallInput {
                    operation: ArchivalMemoryOperationType::Insert,
                    content: Some("Content to read back".to_string()),
                    label: Some("read_test".to_string()),
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(
            insert_result.success,
            "Insert failed: {:?}",
            insert_result.message
        );

        // Then read it back by label (FTS searches metadata too)
        let result = tool
            .execute(
                RecallInput {
                    operation: ArchivalMemoryOperationType::Read,
                    content: None,
                    label: Some("read_test".to_string()),
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success, "Read failed: {:?}", result.message);
        assert!(!result.results.is_empty());
        // Verify the label is extracted from metadata, not the entry ID
        assert_eq!(result.results[0].label, "read_test");
    }

    #[tokio::test]
    async fn test_archival_insert_without_label() {
        let ctx = create_test_context().await;
        let tool = RecallTool::new(ctx);

        // Insert without providing a label - should auto-generate one
        let result = tool
            .execute(
                RecallInput {
                    operation: ArchivalMemoryOperationType::Insert,
                    content: Some("Auto-labeled content".to_string()),
                    label: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.message.is_some());
        // Should contain "archival_" prefix in the auto-generated label
        assert!(result.message.unwrap().contains("archival_"));
    }
}
