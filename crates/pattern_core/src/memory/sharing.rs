//! Shared memory block support
//!
//! Enables explicit sharing of blocks between agents with controlled access levels.
//! Uses MemoryPermission from pattern_db for access control granularity.

use crate::db::ConstellationDatabases;
use crate::memory::{MemoryError, MemoryResult};
use pattern_db::models::MemoryPermission;
use pattern_db::queries;
use std::sync::Arc;

/// Special agent ID for constellation-level blocks (readable by all agents)
pub const CONSTELLATION_OWNER: &str = "_constellation_";

/// Manager for shared memory blocks
pub struct SharedBlockManager {
    dbs: Arc<ConstellationDatabases>,
}

impl SharedBlockManager {
    /// Create a new shared block manager
    pub fn new(dbs: Arc<ConstellationDatabases>) -> Self {
        Self { dbs }
    }

    /// Share a block with another agent
    ///
    /// Permission levels available:
    /// - `ReadOnly`: Can only read the block
    /// - `Partner`: Requires partner approval to write
    /// - `Human`: Requires human approval to write
    /// - `Append`: Can append but not overwrite
    /// - `ReadWrite`: Full read/write access
    /// - `Admin`: Full access including delete
    pub async fn share_block(
        &self,
        block_id: &str,
        agent_id: &str,
        permission: MemoryPermission,
    ) -> MemoryResult<()> {
        // Check that the block exists
        let block = queries::get_block(self.dbs.constellation.pool(), block_id).await?;
        if block.is_none() {
            return Err(MemoryError::Other(format!("Block not found: {}", block_id)));
        }

        // Create shared attachment
        queries::create_shared_block_attachment(
            self.dbs.constellation.pool(),
            block_id,
            agent_id,
            permission,
        )
        .await?;

        Ok(())
    }

    /// Remove sharing for a block
    pub async fn unshare_block(&self, block_id: &str, agent_id: &str) -> MemoryResult<()> {
        queries::delete_shared_block_attachment(self.dbs.constellation.pool(), block_id, agent_id)
            .await?;
        Ok(())
    }

    /// Get all agents a block is shared with
    pub async fn get_shared_agents(
        &self,
        block_id: &str,
    ) -> MemoryResult<Vec<(String, MemoryPermission)>> {
        let attachments =
            queries::list_block_shared_agents(self.dbs.constellation.pool(), block_id).await?;

        Ok(attachments
            .into_iter()
            .map(|att| (att.agent_id, att.permission))
            .collect())
    }

    /// Get all blocks shared with an agent
    pub async fn get_blocks_shared_with(
        &self,
        agent_id: &str,
    ) -> MemoryResult<Vec<(String, MemoryPermission)>> {
        let attachments =
            queries::list_agent_shared_blocks(self.dbs.constellation.pool(), agent_id).await?;

        Ok(attachments
            .into_iter()
            .map(|att| (att.block_id, att.permission))
            .collect())
    }

    /// Check if agent has access to block (owner or shared)
    ///
    /// Returns:
    /// - Some(Admin) if agent owns the block
    /// - Some(ReadOnly) if block owner is CONSTELLATION_OWNER (readable by all)
    /// - Some(permission) if block is explicitly shared with agent
    /// - None if agent has no access
    pub async fn check_access(
        &self,
        block_id: &str,
        agent_id: &str,
    ) -> MemoryResult<Option<MemoryPermission>> {
        // 1. Get block, check if agent is owner -> Admin access
        let block = queries::get_block(self.dbs.constellation.pool(), block_id).await?;
        if let Some(block) = block {
            if block.agent_id == agent_id {
                return Ok(Some(MemoryPermission::Admin));
            }

            // 2. Check if constellation owner -> dictated by the permission on the block
            if block.agent_id == CONSTELLATION_OWNER {
                return Ok(Some(block.permission));
            }
        } else {
            // Block doesn't exist
            return Ok(None);
        }

        // 3. Check shared attachments
        let attachment =
            queries::get_shared_block_attachment(self.dbs.constellation.pool(), block_id, agent_id)
                .await?;

        Ok(attachment.map(|att| att.permission))
    }

    /// Check if the given permission allows write operations
    pub fn can_write(permission: MemoryPermission) -> bool {
        matches!(
            permission,
            MemoryPermission::Append | MemoryPermission::ReadWrite | MemoryPermission::Admin
        )
    }

    /// Check if the given permission allows delete operations
    pub fn can_delete(permission: MemoryPermission) -> bool {
        matches!(permission, MemoryPermission::Admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pattern_db::models::{MemoryBlock, MemoryBlockType};

    async fn setup_test_dbs() -> Arc<ConstellationDatabases> {
        Arc::new(ConstellationDatabases::open_in_memory().await.unwrap())
    }

    async fn create_test_agent(dbs: &ConstellationDatabases, id: &str, name: &str) {
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json;
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
        queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .unwrap();
    }

    async fn create_test_block(
        dbs: &ConstellationDatabases,
        id: &str,
        agent_id: &str,
    ) -> MemoryBlock {
        let block = MemoryBlock {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            label: "test".to_string(),
            description: "Test block".to_string(),
            block_type: MemoryBlockType::Working,
            char_limit: 1000,
            permission: MemoryPermission::ReadWrite,
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
        queries::create_block(dbs.constellation.pool(), &block)
            .await
            .unwrap();
        block
    }

    #[tokio::test]
    async fn test_share_with_readonly_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        // Create test agents
        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;

        // Create a block owned by agent1
        create_test_block(&dbs, "block1", "agent1").await;

        // Share it with agent2 with ReadOnly access
        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        // Verify agent2 has ReadOnly access
        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::ReadOnly));
        assert!(!SharedBlockManager::can_write(access.unwrap()));
    }

    #[tokio::test]
    async fn test_share_with_append_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        // Create test agents
        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;

        // Create a block owned by agent1
        create_test_block(&dbs, "block1", "agent1").await;

        // Share it with agent2 with Append access
        manager
            .share_block("block1", "agent2", MemoryPermission::Append)
            .await
            .unwrap();

        // Verify agent2 has Append access
        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::Append));
        assert!(SharedBlockManager::can_write(access.unwrap()));
        assert!(!SharedBlockManager::can_delete(access.unwrap()));
    }

    #[tokio::test]
    async fn test_unshare_removes_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        // Create test agents
        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;

        // Create and share a block
        create_test_block(&dbs, "block1", "agent1").await;
        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();

        // Unshare it
        manager.unshare_block("block1", "agent2").await.unwrap();

        // Verify agent2 no longer has access
        let access = manager.check_access("block1", "agent2").await.unwrap();
        assert_eq!(access, None);
    }

    #[tokio::test]
    async fn test_owner_always_has_admin_access() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        // Create test agent
        create_test_agent(&dbs, "agent1", "Agent 1").await;

        // Create a block
        create_test_block(&dbs, "block1", "agent1").await;

        // Owner should have Admin access without explicit sharing
        let access = manager.check_access("block1", "agent1").await.unwrap();
        assert_eq!(access, Some(MemoryPermission::Admin));
        assert!(SharedBlockManager::can_write(access.unwrap()));
        assert!(SharedBlockManager::can_delete(access.unwrap()));
    }

    #[tokio::test]
    async fn test_list_shared_agents_with_different_permissions() {
        let dbs = setup_test_dbs().await;
        let manager = SharedBlockManager::new(dbs.clone());

        // Create test agents
        create_test_agent(&dbs, "agent1", "Agent 1").await;
        create_test_agent(&dbs, "agent2", "Agent 2").await;
        create_test_agent(&dbs, "agent3", "Agent 3").await;

        // Create a block and share with multiple agents with different permissions
        create_test_block(&dbs, "block1", "agent1").await;
        manager
            .share_block("block1", "agent2", MemoryPermission::ReadOnly)
            .await
            .unwrap();
        manager
            .share_block("block1", "agent3", MemoryPermission::ReadWrite)
            .await
            .unwrap();

        // List shared agents
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

        // Create constellation owner agent
        create_test_agent(&dbs, CONSTELLATION_OWNER, "Constellation").await;

        // Create a block owned by constellation (default permission is ReadWrite)
        create_test_block(&dbs, "block1", CONSTELLATION_OWNER).await;

        // Any agent should have access matching the block's permission
        let access = manager.check_access("block1", "any_agent").await.unwrap();
        // The block is created with ReadWrite permission, so that's what non-owners get
        assert_eq!(access, Some(MemoryPermission::ReadWrite));
    }
}
