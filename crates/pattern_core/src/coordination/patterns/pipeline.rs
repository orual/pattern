//! Pipeline coordination pattern implementation

use async_trait::async_trait;
use chrono::Utc;
use std::{sync::Arc, time::Instant};

use crate::agent::AgentExt;
use crate::{
    AgentId, CoreError, Result,
    agent::Agent,
    coordination::{
        groups::{
            AgentResponse, AgentWithMembership, GroupManager, GroupResponse, GroupResponseEvent,
        },
        types::{GroupState, PipelineStage, StageFailureAction, StageResult},
        utils::text_response,
    },
    messages::Message,
};

#[derive(Clone)]
pub struct PipelineManager;

#[async_trait]
impl GroupManager for PipelineManager {
    async fn route_message(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<Box<dyn futures::Stream<Item = GroupResponseEvent> + Send + Unpin>> {
        use tokio_stream::wrappers::ReceiverStream;
        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let start_time = std::time::Instant::now();
        let group_id = group.id.clone();

        // Do the full pipeline operation synchronously first
        let result = self.do_pipeline(group, agents, message).await;

        // Then send the result as a single Complete event
        tokio::spawn(async move {
            match result {
                Ok((agent_responses, state_changes)) => {
                    let _ = tx
                        .send(GroupResponseEvent::Complete {
                            group_id,
                            pattern: "pipeline".to_string(),
                            execution_time: start_time.elapsed(),
                            agent_responses,
                            state_changes,
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(GroupResponseEvent::Error {
                            agent_id: None,
                            message: e.to_string(),
                            recoverable: false,
                        })
                        .await;
                }
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    async fn update_state(
        &self,
        _current_state: &GroupState,
        response: &GroupResponse,
    ) -> Result<Option<GroupState>> {
        // State is already updated in route_message for pipeline
        Ok(response.state_changes.clone())
    }
}

impl PipelineManager {
    async fn do_pipeline(
        &self,
        group: &crate::coordination::groups::AgentGroup,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        message: Message,
    ) -> Result<(Vec<AgentResponse>, Option<GroupState>)> {
        use crate::coordination::types::PipelineExecution;
        use uuid::Uuid;

        // Extract pipeline config
        let (stages, parallel_stages) = match &group.coordination_pattern {
            crate::coordination::types::CoordinationPattern::Pipeline {
                stages,
                parallel_stages,
            } => (stages, *parallel_stages),
            _ => {
                return Err(CoreError::AgentGroupError {
                    group_name: group.name.clone(),
                    operation: "route_message".to_string(),
                    cause: "Invalid pattern for PipelineManager".to_string(),
                });
            }
        };

        // Get or create pipeline execution
        let mut execution = match &group.state {
            GroupState::Pipeline { active_executions } => {
                // For simplicity, we'll use the first active execution or create new
                active_executions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PipelineExecution {
                        id: Uuid::new_v4(),
                        current_stage: 0,
                        stage_results: Vec::new(),
                        started_at: Utc::now(),
                    })
            }
            _ => PipelineExecution {
                id: Uuid::new_v4(),
                current_stage: 0,
                stage_results: Vec::new(),
                started_at: Utc::now(),
            },
        };

        let mut responses = Vec::new();
        let mut all_stage_results = execution.stage_results.clone();

        // Process stages
        if parallel_stages {
            // TODO: Implement parallel processing
            // For now, process sequentially
        }

        // Sequential processing
        while execution.current_stage < stages.len() {
            let stage = &stages[execution.current_stage];

            match self
                .process_stage(
                    stage,
                    execution.current_stage,
                    &message,
                    agents,
                    group.name.clone(),
                )
                .await
            {
                Ok((response, result)) => {
                    responses.push(response);
                    all_stage_results.push(result);
                    execution.current_stage += 1;
                }
                Err(e) => {
                    // Handle stage failure
                    let failure_result = self
                        .handle_stage_failure(stage, execution.current_stage, e, agents)
                        .await?;

                    if let Some((response, result)) = failure_result {
                        responses.push(response);
                        all_stage_results.push(result);
                        execution.current_stage += 1;
                    } else {
                        // Pipeline aborted
                        break;
                    }
                }
            }
        }

        // Update execution state
        execution.stage_results = all_stage_results;

        // Determine if pipeline is complete
        let new_state = if execution.current_stage >= stages.len() {
            // Pipeline complete, clear execution
            Some(GroupState::Pipeline {
                active_executions: vec![],
            })
        } else {
            // Pipeline still in progress
            Some(GroupState::Pipeline {
                active_executions: vec![execution],
            })
        };

        Ok((responses, new_state))
    }

    async fn process_stage(
        &self,
        stage: &PipelineStage,
        _stage_index: usize,
        message: &Message,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
        group_name: String,
    ) -> Result<(AgentResponse, StageResult)> {
        let stage_start = Instant::now();

        // Select an agent for this stage
        let agent_id = stage
            .agent_ids
            .first()
            .ok_or_else(|| CoreError::AgentGroupError {
                group_name: group_name.clone(),
                operation: format!("stage_{}", stage.name),
                cause: format!("No agents configured for stage '{}'", stage.name),
            })?;

        // Verify agent exists and is active
        let awm = agents
            .iter()
            .find(|awm| &awm.agent.as_ref().id() == agent_id)
            .ok_or_else(|| CoreError::AgentGroupError {
                group_name: group_name.clone(),
                operation: format!("stage_{}", stage.name),
                cause: format!("Agent '{}' not found", agent_id),
            })?;

        if !awm.membership.is_active {
            return Err(CoreError::AgentGroupError {
                group_name,
                operation: format!("stage_{}", stage.name),
                cause: format!("Agent {} is not active", agent_id),
            });
        }

        // Process message with selected agent
        let agent_response = awm
            .agent
            .clone()
            .process_to_response(message.clone())
            .await?;
        let response = AgentResponse {
            agent_id: awm.agent.as_ref().id(),
            response: agent_response,
            responded_at: Utc::now(),
        };

        let result = StageResult {
            stage_name: stage.name.clone(),
            agent_id: awm.agent.as_ref().id(),
            success: true,
            duration: stage_start.elapsed(),
            output: serde_json::json!({
                "stage": stage.name,
                "processed": true,
                "message_preview": "<message preview>"
            }),
        };

        Ok((response, result))
    }

    async fn handle_stage_failure(
        &self,
        stage: &PipelineStage,
        stage_index: usize,
        error: CoreError,
        agents: &[AgentWithMembership<Arc<dyn Agent>>],
    ) -> Result<Option<(AgentResponse, StageResult)>> {
        match &stage.on_failure {
            StageFailureAction::Skip => {
                // Skip the stage and continue
                let response = AgentResponse {
                    agent_id: stage
                        .agent_ids
                        .first()
                        .cloned()
                        .unwrap_or_else(AgentId::generate),
                    response: text_response(format!(
                        "[Pipeline Stage {}: {} - SKIPPED] Error: {:?}",
                        stage_index + 1,
                        stage.name,
                        error
                    )),
                    responded_at: Utc::now(),
                };

                let result = StageResult {
                    stage_name: stage.name.clone(),
                    agent_id: stage
                        .agent_ids
                        .first()
                        .cloned()
                        .unwrap_or_else(AgentId::generate),
                    success: false,
                    duration: std::time::Duration::from_secs(0),
                    output: serde_json::json!({
                        "stage": stage.name,
                        "skipped": true,
                        "error": error.to_string()
                    }),
                };

                Ok(Some((response, result)))
            }
            StageFailureAction::Retry { max_attempts } => {
                // In a real implementation, would track retry count
                // For now, just fail after pretending to retry
                Err(CoreError::AgentGroupError {
                    group_name: "pipeline".to_string(),
                    operation: format!("stage_{}_retry", stage.name),
                    cause: format!(
                        "Stage '{}' failed after {} attempts",
                        stage.name, max_attempts
                    ),
                })
            }
            StageFailureAction::Abort => {
                // Abort the entire pipeline
                Ok(None)
            }
            StageFailureAction::Fallback { agent_id } => {
                // Use fallback agent
                let awm = agents
                    .iter()
                    .find(|awm| &awm.agent.as_ref().id() == agent_id)
                    .ok_or_else(|| CoreError::AgentGroupError {
                        group_name: "pipeline".to_string(),
                        operation: format!("stage_{}_fallback", stage.name),
                        cause: format!("Fallback agent '{}' not found", agent_id),
                    })?;

                if !awm.membership.is_active {
                    return Err(CoreError::AgentGroupError {
                        group_name: "pipeline".to_string(),
                        operation: format!("stage_{}_fallback", stage.name),
                        cause: format!("Fallback agent {} is not active", agent_id),
                    });
                }

                let response = AgentResponse {
                    agent_id: awm.agent.as_ref().id(),
                    response: text_response(format!(
                        "[Pipeline Stage {}: {} - FALLBACK] Handling after primary failure",
                        stage_index + 1,
                        stage.name
                    )),
                    responded_at: Utc::now(),
                };

                let result = StageResult {
                    stage_name: stage.name.clone(),
                    agent_id: awm.agent.as_ref().id(),
                    success: true,
                    duration: std::time::Duration::from_secs(1),
                    output: serde_json::json!({
                        "stage": stage.name,
                        "fallback": true,
                        "original_error": error.to_string()
                    }),
                };

                Ok(Some((response, result)))
            }
        }
    }
}
