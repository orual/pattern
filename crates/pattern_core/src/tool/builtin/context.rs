//! Context management tool following Letta/MemGPT patterns

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    Result,
    memory::MemoryPermission,
    memory_acl::{MemoryGate, MemoryOp, check as acl_check, consent_reason},
    permission::PermissionScope,
    tool::{AiTool, ExecutionMeta, ToolRule, ToolRuleType},
};

/// Operation types for context management
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoreMemoryOperationType {
    Append,
    Replace,
    Archive,
    Load,
    Swap,
}

/// Input for managing context
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContextInput {
    /// The operation to perform
    pub operation: CoreMemoryOperationType,

    /// The name/label of the context section (required for append/replace)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Content to append or new content for replace
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// For replace: text to search for (must match exactly)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_content: Option<String>,

    /// For replace: replacement text (use empty string to delete)
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_content: Option<String>,

    /// For archive/load/swap: label of the recall memory
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archival_label: Option<String>,

    /// For swap: name of the context to archive
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_name: Option<String>,
}

/// Output from context operations
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ContextOutput {
    /// Whether the operation was successful
    pub success: bool,

    /// Message about the operation
    #[schemars(default, with = "String")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// For read operations, the memory content
    #[serde(default)]
    pub content: serde_json::Value,
}

// ============================================================================
// Implementation using ToolContext
// ============================================================================

use crate::runtime::ToolContext;
use std::sync::Arc;

/// Tool for managing context using ToolContext
#[derive(Clone)]
pub struct ContextTool {
    ctx: Arc<dyn ToolContext>,
}

impl std::fmt::Debug for ContextTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextTool")
            .field("agent_id", &self.ctx.agent_id())
            .finish()
    }
}

impl ContextTool {
    pub fn new(ctx: Arc<dyn ToolContext>) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl AiTool for ContextTool {
    type Input = ContextInput;
    type Output = ContextOutput;

    fn name(&self) -> &str {
        "context"
    }

    fn description(&self) -> &str {
        "Manage context sections (persona, human, etc). Context is always visible and shapes agent behavior. No need to read - it's already in your messages. Operations: append, replace, archive, load, swap.
 - 'append' adds a new chunk of text to the block. avoid duplicate append operations.
 - 'replace' replaces a section of text (old_content is matched and replaced with new content) within a block. this can be used to delete sections.
 - 'archive' swaps an entire block to recall memory (only works on 'working' memory, not 'core', requires permissions)
 - 'load' pulls a block from recall memory into working memory (destination name optional - defaults to same label)
 - 'swap' replaces a working memory with the requested recall memory, by label"
    }

    async fn execute(&self, params: Self::Input, meta: &ExecutionMeta) -> Result<Self::Output> {
        match params.operation {
            CoreMemoryOperationType::Append => {
                let name = params.name.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"append"}),
                        "append operation requires 'name' field",
                    )
                })?;
                let content = params.content.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"append"}),
                        "append operation requires 'content' field",
                    )
                })?;
                self.execute_append(name, content, meta).await
            }
            CoreMemoryOperationType::Replace => {
                let name = params.name.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"replace"}),
                        "replace operation requires 'name' field",
                    )
                })?;
                let old_content = params.old_content.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"replace"}),
                        "replace operation requires 'old_content' field",
                    )
                })?;
                let new_content = params.new_content.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"replace"}),
                        "replace operation requires 'new_content' field",
                    )
                })?;
                self.execute_replace(name, old_content, new_content, meta)
                    .await
            }
            CoreMemoryOperationType::Archive => {
                let name = params.name.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"archive"}),
                        "archive operation requires 'name' field",
                    )
                })?;
                self.execute_archive(name, params.archival_label, meta)
                    .await
            }
            CoreMemoryOperationType::Load => {
                let archival_label = params.archival_label.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"load"}),
                        "load operation requires 'archival_label' field",
                    )
                })?;
                let name = params.name;
                self.execute_load(archival_label, name, meta).await
            }
            CoreMemoryOperationType::Swap => {
                let archive_name = params.archive_name.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"swap"}),
                        "swap operation requires 'archive_name' field",
                    )
                })?;
                let archival_label = params.archival_label.ok_or_else(|| {
                    crate::CoreError::tool_exec_msg(
                        "context",
                        serde_json::json!({"operation":"swap"}),
                        "swap operation requires 'archival_label' field",
                    )
                })?;
                self.execute_swap(archive_name, archival_label, meta).await
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
                description: "Remember the user's name".to_string(),
                parameters: ContextInput {
                    operation: CoreMemoryOperationType::Append,
                    name: Some("human".to_string()),
                    content: Some("User's name is Alice, prefers to be called Ali.".to_string()),
                    old_content: None,
                    new_content: None,
                    archival_label: None,
                    archive_name: None,
                },
                expected_output: Some(ContextOutput {
                    success: true,
                    message: Some("Appended 44 characters to context section 'human'".to_string()),
                    content: json!({}),
                }),
            },
            crate::tool::ToolExample {
                description: "Update agent personality".to_string(),
                parameters: ContextInput {
                    operation: CoreMemoryOperationType::Replace,
                    name: Some("persona".to_string()),
                    content: None,
                    old_content: Some("helpful AI assistant".to_string()),
                    new_content: Some("knowledgeable AI companion".to_string()),
                    archival_label: None,
                    archive_name: None,
                },
                expected_output: Some(ContextOutput {
                    success: true,
                    message: Some("Replaced content in context section 'persona'".to_string()),
                    content: json!({}),
                }),
            },
        ]
    }
}

impl ContextTool {
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
                            "context".to_string(),
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

    async fn execute_append(
        &self,
        name: String,
        content: String,
        meta: &ExecutionMeta,
    ) -> Result<ContextOutput> {
        let agent_id = self.ctx.agent_id();

        // Check permission before appending
        if let Some(error_msg) = self.check_permission(&name, MemoryOp::Append, meta).await? {
            return Ok(ContextOutput {
                success: false,
                message: Some(error_msg),
                content: json!({}),
            });
        }

        // Use MemoryStore::append_to_block
        // Prepend newlines to separate from existing content
        let content_with_separator = format!("\n\n{}", content);
        match self
            .ctx
            .memory()
            .append_to_block(agent_id, &name, &content_with_separator)
            .await
        {
            Ok(()) => {
                // Get the updated block to show preview
                let preview =
                    if let Ok(Some(doc)) = self.ctx.memory().get_block(agent_id, &name).await {
                        let text = doc.text_content();
                        let char_count = text.chars().count();
                        let preview_chars = 200;
                        let content_preview = if text.len() > preview_chars {
                            format!("...{}", &text[text.len().saturating_sub(preview_chars)..])
                        } else {
                            text
                        };
                        json!({
                            "content_preview": content_preview,
                            "total_chars": char_count,
                        })
                    } else {
                        json!({})
                    };

                Ok(ContextOutput {
                    success: true,
                    message: Some(format!(
                        "Successfully appended {} characters to context section '{}'",
                        content.len(),
                        name
                    )),
                    content: preview,
                })
            }
            Err(e) => Ok(ContextOutput {
                success: false,
                message: Some(format!("Failed to append to context '{}': {:?}", name, e)),
                content: json!({}),
            }),
        }
    }

    async fn execute_replace(
        &self,
        name: String,
        old_content: String,
        new_content: String,
        meta: &ExecutionMeta,
    ) -> Result<ContextOutput> {
        let agent_id = self.ctx.agent_id();

        // Check permission before replacing
        if let Some(error_msg) = self
            .check_permission(&name, MemoryOp::Overwrite, meta)
            .await?
        {
            return Ok(ContextOutput {
                success: false,
                message: Some(error_msg),
                content: json!({}),
            });
        }

        // Use MemoryStore::replace_in_block
        match self
            .ctx
            .memory()
            .replace_in_block(agent_id, &name, &old_content, &new_content)
            .await
        {
            Ok(true) => {
                // Get the updated block to show preview
                let preview =
                    if let Ok(Some(doc)) = self.ctx.memory().get_block(agent_id, &name).await {
                        let text = doc.text_content();
                        let char_count = text.chars().count();

                        // Find where the replacement happened and show context
                        let preview_chars = 100;
                        let content_preview = if let Some(pos) = text.find(&new_content) {
                            let start = pos.saturating_sub(preview_chars);
                            let end = (pos + new_content.len() + preview_chars).min(text.len());

                            let prefix = if start > 0 { "..." } else { "" };
                            let suffix = if end < text.len() { "..." } else { "" };

                            format!("{}{}{}", prefix, &text[start..end], suffix)
                        } else {
                            if text.len() > preview_chars * 2 {
                                format!(
                                    "...{}",
                                    &text[text.len().saturating_sub(preview_chars * 2)..]
                                )
                            } else {
                                text
                            }
                        };

                        json!({
                            "content_preview": content_preview,
                            "total_chars": char_count,
                        })
                    } else {
                        json!({})
                    };

                Ok(ContextOutput {
                    success: true,
                    message: Some(format!(
                        "Successfully replaced content in context section '{}'",
                        name
                    )),
                    content: preview,
                })
            }
            Ok(false) => Ok(ContextOutput {
                success: false,
                message: Some(format!(
                    "Content '{}' not found in context section '{}'",
                    old_content, name
                )),
                content: json!({}),
            }),
            Err(e) => Ok(ContextOutput {
                success: false,
                message: Some(format!("Failed to replace in context '{}': {:?}", name, e)),
                content: json!({}),
            }),
        }
    }

    async fn execute_archive(
        &self,
        name: String,
        archival_label: Option<String>,
        meta: &ExecutionMeta,
    ) -> Result<ContextOutput> {
        let agent_id = self.ctx.agent_id();

        // Check permission before archiving (requires delete permission on source block)
        if let Some(error_msg) = self.check_permission(&name, MemoryOp::Delete, meta).await? {
            return Ok(ContextOutput {
                success: false,
                message: Some(error_msg),
                content: json!({}),
            });
        }

        // Get the block content first
        let block_content = match self.ctx.memory().get_block(agent_id, &name).await {
            Ok(Some(doc)) => doc.text_content(),
            Ok(None) => {
                return Ok(ContextOutput {
                    success: false,
                    message: Some(format!("Memory '{}' not found", name)),
                    content: json!({}),
                });
            }
            Err(e) => {
                return Ok(ContextOutput {
                    success: false,
                    message: Some(format!("Failed to get block '{}': {:?}", name, e)),
                    content: json!({}),
                });
            }
        };

        // Generate archival label if not provided
        let archival_label = archival_label
            .unwrap_or_else(|| format!("{}_archived_{}", name, chrono::Utc::now().timestamp()));

        // Insert to archival memory
        match self
            .ctx
            .memory()
            .insert_archival(agent_id, &block_content, None)
            .await
        {
            Ok(_id) => {
                // Delete the original block
                match self.ctx.memory().delete_block(agent_id, &name).await {
                    Ok(()) => Ok(ContextOutput {
                        success: true,
                        message: Some(format!(
                            "Archived context '{}' to recall memory '{}'",
                            name, archival_label
                        )),
                        content: json!({}),
                    }),
                    Err(e) => Ok(ContextOutput {
                        success: false,
                        message: Some(format!(
                            "Archived to recall but failed to delete original block '{}': {:?}",
                            name, e
                        )),
                        content: json!({}),
                    }),
                }
            }
            Err(e) => Ok(ContextOutput {
                success: false,
                message: Some(format!("Failed to archive to recall memory: {:?}", e)),
                content: json!({}),
            }),
        }
    }

    async fn execute_load(
        &self,
        archival_label: String,
        name: Option<String>,
        _meta: &ExecutionMeta,
    ) -> Result<ContextOutput> {
        use crate::memory::{BlockSchema, BlockType};

        let agent_id = self.ctx.agent_id();
        let destination_name = name.unwrap_or_else(|| archival_label.clone());

        // Search archival to find the content
        match self
            .ctx
            .memory()
            .search_archival(agent_id, &archival_label, 1)
            .await
        {
            Ok(entries) => {
                if let Some(entry) = entries.first() {
                    // Check if destination block exists, create if not
                    let block_exists = self
                        .ctx
                        .memory()
                        .get_block(agent_id, &destination_name)
                        .await
                        .map(|b| b.is_some())
                        .unwrap_or(false);

                    if !block_exists {
                        // Create the destination block as Working type
                        if let Err(e) = self
                            .ctx
                            .memory()
                            .create_block(
                                agent_id,
                                &destination_name,
                                &format!("Loaded from archival: {}", archival_label),
                                BlockType::Working,
                                BlockSchema::Text,
                                10000, // reasonable default char limit
                            )
                            .await
                        {
                            return Ok(ContextOutput {
                                success: false,
                                message: Some(format!(
                                    "Failed to create destination block '{}': {:?}",
                                    destination_name, e
                                )),
                                content: json!({}),
                            });
                        }
                    }

                    // Update the block with archival content
                    match self
                        .ctx
                        .memory()
                        .update_block_text(agent_id, &destination_name, &entry.content)
                        .await
                    {
                        Ok(()) => Ok(ContextOutput {
                            success: true,
                            message: Some(format!(
                                "Loaded recall memory '{}' into working memory '{}'",
                                archival_label, destination_name
                            )),
                            content: json!({}),
                        }),
                        Err(e) => Ok(ContextOutput {
                            success: false,
                            message: Some(format!(
                                "Found recall memory but failed to load into '{}': {:?}",
                                destination_name, e
                            )),
                            content: json!({}),
                        }),
                    }
                } else {
                    Ok(ContextOutput {
                        success: false,
                        message: Some(format!("Archival memory '{}' not found", archival_label)),
                        content: json!({}),
                    })
                }
            }
            Err(e) => Ok(ContextOutput {
                success: false,
                message: Some(format!("Failed to search for archival memory: {:?}", e)),
                content: json!({}),
            }),
        }
    }

    async fn execute_swap(
        &self,
        archive_name: String,
        archival_label: String,
        meta: &ExecutionMeta,
    ) -> Result<ContextOutput> {
        let agent_id = self.ctx.agent_id();

        // Check permission before swapping (requires overwrite permission on the block being replaced)
        if let Some(error_msg) = self
            .check_permission(&archive_name, MemoryOp::Overwrite, meta)
            .await?
        {
            return Ok(ContextOutput {
                success: false,
                message: Some(error_msg),
                content: json!({}),
            });
        }

        // Get both blocks' contents
        let core_block_content = match self.ctx.memory().get_block(agent_id, &archive_name).await {
            Ok(Some(doc)) => doc.text_content(),
            Ok(None) => {
                return Ok(ContextOutput {
                    success: false,
                    message: Some(format!("Memory '{}' not found", archive_name)),
                    content: json!({}),
                });
            }
            Err(e) => {
                return Ok(ContextOutput {
                    success: false,
                    message: Some(format!("Failed to get block '{}': {:?}", archive_name, e)),
                    content: json!({}),
                });
            }
        };

        // Search for archival block
        let archival_content = match self
            .ctx
            .memory()
            .search_archival(agent_id, &archival_label, 1)
            .await
        {
            Ok(entries) => {
                if let Some(entry) = entries.first() {
                    entry.content.clone()
                } else {
                    return Ok(ContextOutput {
                        success: false,
                        message: Some(format!("Archival memory '{}' not found", archival_label)),
                        content: json!({}),
                    });
                }
            }
            Err(e) => {
                return Ok(ContextOutput {
                    success: false,
                    message: Some(format!("Failed to search for archival memory: {:?}", e)),
                    content: json!({}),
                });
            }
        };

        // Swap: update core block with archival content
        match self
            .ctx
            .memory()
            .update_block_text(agent_id, &archive_name, &archival_content)
            .await
        {
            Ok(()) => {
                // Archive the original core block content
                match self
                    .ctx
                    .memory()
                    .insert_archival(agent_id, &core_block_content, None)
                    .await
                {
                    Ok(_) => Ok(ContextOutput {
                        success: true,
                        message: Some(format!(
                            "Swapped context '{}' with recall memory '{}'",
                            archive_name, archival_label
                        )),
                        content: json!({}),
                    }),
                    Err(e) => Ok(ContextOutput {
                        success: false,
                        message: Some(format!(
                            "Updated core block but failed to archive original: {:?}",
                            e
                        )),
                        content: json!({}),
                    }),
                }
            }
            Err(e) => Ok(ContextOutput {
                success: false,
                message: Some(format!(
                    "Failed to update block '{}': {:?}",
                    archive_name, e
                )),
                content: json!({}),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{BlockSchema, BlockType, MemoryStore};
    use crate::tool::builtin::test_utils::create_test_context_with_agent;

    #[tokio::test]
    async fn test_context_append() {
        let (_db, memory, ctx) = create_test_context_with_agent("test-agent").await;

        // Create a block to append to
        memory
            .create_block(
                "test-agent",
                "human",
                "Initial content.",
                BlockType::Core,
                BlockSchema::Text,
                2000,
            )
            .await
            .unwrap();

        let tool = ContextTool::new(ctx);

        // Test appending
        let result = tool
            .execute(
                ContextInput {
                    operation: CoreMemoryOperationType::Append,
                    name: Some("human".to_string()),
                    content: Some("They work in healthcare.".to_string()),
                    old_content: None,
                    new_content: None,
                    archival_label: None,
                    archive_name: None,
                },
                &crate::tool::ExecutionMeta::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
    }
}
