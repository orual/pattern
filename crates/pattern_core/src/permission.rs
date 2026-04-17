use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, oneshot};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionScope {
    MemoryEdit {
        key: String,
    },
    MemoryBatch {
        prefix: String,
    },
    ToolExecution {
        tool: String,
        args_digest: Option<String>,
    },
    DataSourceAction {
        source_id: String,
        action: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionGrant {
    pub id: String,
    pub scope: PermissionScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub agent_id: crate::AgentId,
    pub tool_name: String,
    pub scope: PermissionScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecisionKind {
    Deny,
    ApproveOnce,
    ApproveForDuration(std::time::Duration),
    ApproveForScope,
}

#[derive(Clone)]
pub struct PermissionBroker {
    tx: broadcast::Sender<PermissionRequest>,
    pending: Arc<RwLock<HashMap<String, oneshot::Sender<PermissionDecisionKind>>>>,
    pending_info: Arc<RwLock<HashMap<String, PermissionRequest>>>,
}

impl PermissionBroker {
    fn new() -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            tx,
            pending: Arc::new(RwLock::new(HashMap::new())),
            pending_info: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PermissionRequest> {
        self.tx.subscribe()
    }

    pub async fn request(
        &self,
        agent_id: crate::AgentId,
        tool_name: String,
        scope: PermissionScope,
        reason: Option<String>,
        metadata: Option<serde_json::Value>,
        timeout: std::time::Duration,
    ) -> Option<PermissionGrant> {
        tracing::debug!("permission.request tool={} scope={:?}", tool_name, scope);
        let id = Uuid::new_v4().to_string();
        let (tx_decision, rx_decision) = oneshot::channel();
        {
            let mut p = self.pending.write().await;
            p.insert(id.clone(), tx_decision);
        }
        let req = PermissionRequest {
            id: id.clone(),
            agent_id: agent_id.clone(),
            tool_name: tool_name.clone(),
            scope: scope.clone(),
            reason,
            metadata,
        };
        {
            let mut pi = self.pending_info.write().await;
            pi.insert(id.clone(), req.clone());
        }
        let _ = self.tx.send(req);

        match tokio::time::timeout(timeout, rx_decision).await {
            Ok(Ok(decision)) => match decision {
                PermissionDecisionKind::Deny => None,
                PermissionDecisionKind::ApproveOnce => Some(PermissionGrant {
                    id,
                    scope,
                    expires_at: None,
                }),
                PermissionDecisionKind::ApproveForScope => Some(PermissionGrant {
                    id,
                    scope,
                    expires_at: None,
                }),
                PermissionDecisionKind::ApproveForDuration(dur) => Some(PermissionGrant {
                    id,
                    scope,
                    expires_at: Some(
                        chrono::Utc::now() + chrono::Duration::from_std(dur).unwrap_or_default(),
                    ),
                }),
            },
            _ => {
                tracing::warn!(
                    "permission.request timeout or channel closed: tool={} scope={:?}",
                    tool_name,
                    scope
                );
                None
            }
        }
    }

    pub async fn resolve(&self, request_id: &str, decision: PermissionDecisionKind) -> bool {
        let tx_opt = { self.pending.write().await.remove(request_id) };
        {
            let mut pi = self.pending_info.write().await;
            pi.remove(request_id);
        }
        if let Some(tx) = tx_opt {
            tracing::debug!(
                "permission.resolve id={} decision={:?}",
                request_id,
                decision
            );
            let _ = tx.send(decision);
            true
        } else {
            false
        }
    }

    pub async fn list_pending(&self) -> Vec<PermissionRequest> {
        let pi = self.pending_info.read().await;
        pi.values().cloned().collect()
    }
}

use std::sync::OnceLock;
static BROKER: OnceLock<PermissionBroker> = OnceLock::new();

pub fn broker() -> &'static PermissionBroker {
    BROKER.get_or_init(PermissionBroker::new)
}
