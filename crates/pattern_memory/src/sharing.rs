//! Shared memory block support.
//!
//! Enables explicit sharing of blocks between agents with controlled access levels.

use crate::db_bridge::{DbResultExt, core_perm_to_db, db_perm_to_core};
use pattern_core::types::memory_types::{MemoryError, MemoryPermission, MemoryResult};
use pattern_db::ConstellationDb;
use pattern_db::queries;
use std::sync::Arc;

// Re-export the constant from pattern_core for backward compatibility.
pub use pattern_core::types::memory_types::CONSTELLATION_OWNER;

/// Manager for shared memory blocks.
#[derive(Debug)]
pub struct SharedBlockManager {
    db: Arc<ConstellationDb>,
}

impl SharedBlockManager {
    /// Create a new shared block manager.
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self { db }
    }

    /// Share a block with another agent.
    ///
    /// Permission levels available:
    /// - `ReadOnly`: Can only read the block.
    /// - `Partner`: Requires partner approval to write.
    /// - `Human`: Requires human approval to write.
    /// - `Append`: Can append but not overwrite.
    /// - `ReadWrite`: Full read/write access.
    /// - `Admin`: Full access including delete.
    pub async fn share_block(
        &self,
        block_id: &str,
        agent_id: &str,
        permission: MemoryPermission,
    ) -> MemoryResult<()> {
        // Check that the block exists.
        let block = queries::get_block(&*self.db.get().mem()?, block_id).mem()?;
        if block.is_none() {
            return Err(MemoryError::Other(format!("Block not found: {}", block_id)));
        }

        // Create shared attachment.
        queries::create_shared_block_attachment(
            &*self.db.get().mem()?,
            block_id,
            agent_id,
            core_perm_to_db(permission),
        )
        .mem()?;

        Ok(())
    }

    /// Remove sharing for a block.
    pub async fn unshare_block(&self, block_id: &str, agent_id: &str) -> MemoryResult<()> {
        queries::delete_shared_block_attachment(&*self.db.get().mem()?, block_id, agent_id)
            .mem()?;
        Ok(())
    }

    /// Share a block with another agent by name.
    ///
    /// Looks up the target agent by name, then shares the block.
    /// Returns the target agent's ID on success.
    pub async fn share_block_by_name(
        &self,
        owner_agent_id: &str,
        block_label: &str,
        target_agent_name: &str,
        permission: MemoryPermission,
    ) -> MemoryResult<String> {
        // Look up target agent by name.
        let target_agent = queries::get_agent_by_name(&*self.db.get().mem()?, target_agent_name)
            .mem()?
            .ok_or_else(|| MemoryError::Other(format!("Agent not found: {}", target_agent_name)))?;

        // Get the block by label to find its ID.
        let block =
            queries::get_block_by_label(&*self.db.get().mem()?, owner_agent_id, block_label)
                .mem()?
                .ok_or_else(|| MemoryError::Other(format!("Block not found: {}", block_label)))?;

        // Share the block.
        self.share_block(&block.id, &target_agent.id, permission)
            .await?;

        Ok(target_agent.id)
    }

    /// Remove sharing from another agent by name.
    ///
    /// Looks up the target agent by name, then removes sharing.
    /// Returns the target agent's ID on success.
    pub async fn unshare_block_by_name(
        &self,
        owner_agent_id: &str,
        block_label: &str,
        target_agent_name: &str,
    ) -> MemoryResult<String> {
        // Look up target agent by name.
        let target_agent = queries::get_agent_by_name(&*self.db.get().mem()?, target_agent_name)
            .mem()?
            .ok_or_else(|| MemoryError::Other(format!("Agent not found: {}", target_agent_name)))?;

        // Get the block by label to find its ID.
        let block =
            queries::get_block_by_label(&*self.db.get().mem()?, owner_agent_id, block_label)
                .mem()?
                .ok_or_else(|| MemoryError::Other(format!("Block not found: {}", block_label)))?;

        // Unshare the block.
        self.unshare_block(&block.id, &target_agent.id).await?;

        Ok(target_agent.id)
    }

    /// Get all agents a block is shared with.
    pub async fn get_shared_agents(
        &self,
        block_id: &str,
    ) -> MemoryResult<Vec<(String, MemoryPermission)>> {
        let attachments =
            queries::list_block_shared_agents(&*self.db.get().mem()?, block_id).mem()?;

        Ok(attachments
            .into_iter()
            .map(|att| (att.agent_id, db_perm_to_core(att.permission)))
            .collect())
    }

    /// Get all blocks shared with an agent.
    pub async fn get_blocks_shared_with(
        &self,
        agent_id: &str,
    ) -> MemoryResult<Vec<(String, MemoryPermission)>> {
        let attachments =
            queries::list_agent_shared_blocks(&*self.db.get().mem()?, agent_id).mem()?;

        Ok(attachments
            .into_iter()
            .map(|att| (att.block_id, db_perm_to_core(att.permission)))
            .collect())
    }

    /// Check if agent has access to block (owner or shared).
    ///
    /// Returns:
    /// - Some(Admin) if agent owns the block.
    /// - Some(ReadOnly) if block owner is CONSTELLATION_OWNER (readable by all).
    /// - Some(permission) if block is explicitly shared with agent.
    /// - None if agent has no access.
    pub async fn check_access(
        &self,
        block_id: &str,
        agent_id: &str,
    ) -> MemoryResult<Option<MemoryPermission>> {
        // 1. Get block, check if agent is owner -> Admin access.
        let block = queries::get_block(&*self.db.get().mem()?, block_id).mem()?;
        if let Some(block) = block {
            if block.agent_id == agent_id {
                return Ok(Some(MemoryPermission::Admin));
            }

            // 2. Check if constellation owner -> dictated by the permission on the block.
            if block.agent_id == CONSTELLATION_OWNER {
                return Ok(Some(db_perm_to_core(block.permission)));
            }
        } else {
            // Block doesn't exist.
            return Ok(None);
        }

        // 3. Check shared attachments.
        let attachment =
            queries::get_shared_block_attachment(&*self.db.get().mem()?, block_id, agent_id)
                .mem()?;

        Ok(attachment.map(|att| db_perm_to_core(att.permission)))
    }

    /// Check if the given permission allows write operations.
    pub fn can_write(permission: MemoryPermission) -> bool {
        matches!(
            permission,
            MemoryPermission::Append | MemoryPermission::ReadWrite | MemoryPermission::Admin
        )
    }

    /// Check if the given permission allows delete operations.
    pub fn can_delete(permission: MemoryPermission) -> bool {
        matches!(permission, MemoryPermission::Admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pattern_db::models::MemoryPermission as DbMemoryPermission;
    use pattern_db::models::{MemoryBlock, MemoryBlockType};

    async fn setup_test_dbs() -> Arc<ConstellationDb> {
        Arc::new(ConstellationDb::open_in_memory().unwrap())
    }

    async fn create_test_agent(dbs: &ConstellationDb, id: &str, name: &str) {
        use pattern_db::Json;
        use pattern_db::models::{Agent, AgentStatus};
        let agent = Agent {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "Test prompt".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec![]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        queries::create_agent(&dbs.get().unwrap(), &agent).unwrap();
    }

    async fn create_test_block(dbs: &ConstellationDb, id: &str, agent_id: &str) -> MemoryBlock {
        let block = MemoryBlock {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            label: "test".to_string(),
            description: "Test block".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 1000,
            permission: DbMemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        queries::create_block(&dbs.get().unwrap(), &block).unwrap();
        block
    }

    #[tokio::test]
    async fn test_share_with_readonly_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;
        create_test_block(&dbs, "block1", "agent1").await;

        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::ReadOnly));
        assert!(!SharedBlockManager::can_write(access.unwrap()));
    }

    #[tokio::test]
    async fn test_share_with_append_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;
        create_test_block(&dbs, "block1", "agent1").await;

        manager
            .share_block("block1", "agent2", MemoryPermission::Append)
            .await
            .unwrap();

        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::Append));
        assert!(SharedBlockManager::can_write(access.unwrap()));
        assert!(!SharedBlockManager::can_delete(access.unwrap()));
    }

    #[tokio::test]
    async fn test_unshare_removes_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;
        create_test_block(&dbs, "block1", "agent1").await;
        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        manager.unshare_block("block1", "agent2").await.unwrap();

        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, None);
    }

    #[tokio::test]
    async fn test_owner_always_has_admin_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_block(&dbs, "block1", "agent1").await;

        let access = manager.check_access("block1", "agent1").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::Admin));
        assert!(SharedBlockManager::can_write(access.unwrap()));
        assert!(SharedBlockManager::can_delete(access.unwrap()));
    }

    #[tokio::test]
    async fn test_list_shared_agents_with_different_permissions() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;
        create_test_agent(&dbs, "agent3", "Agent 3").await;
        create_test_block(&dbs, "block1", "agent1").await;
        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();
        manager
            .share_block("block1", "agent3", MemoryPermission::ReadWrite)
            .await
            .unwrap();

        let mut shared = manager.get_shared_agents("block1").await.unwrap();
        shared.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(shared.len(), 2);
        assert_eq!(shared[0].0, "agent2");
        assert_eq!(shared[0].1, MemoryPermission::ReadOnly);
        assert_eq!(shared[1].0, "agent3");
        assert_eq!(shared[1].1, MemoryPermission::ReadWrite);
    }

    #[tokio::test]
    async fn test_constellation_owner_accessible_by_all() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        create_test_agent(&dbs, CONSTELLATION_OWNER, "Constellation").await;
        create_test_block(&dbs, "block1", CONSTELLATION_OWNER).await;

        let access = manager.check_access("block1", "any_agent").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::ReadWrite));
    }
}
