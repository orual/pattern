//! DatabaseAgent - V2 agent implementation with slim trait design

use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;
use tokio::sync::{RwLock, watch};
use tokio_stream::Stream;

use crate::agent::Agent;
use crate::agent::{AgentState, ResponseEvent};
use crate::context::heartbeat::HeartbeatSender;
use crate::error::CoreError;
use crate::id::AgentId;
use crate::message::Message;
use crate::model::ModelProvider;
use crate::runtime::AgentRuntime;

/// DatabaseAgent - A slim agent implementation backed by runtime services
///
/// This agent delegates all "doing" to the runtime and focuses only on:
/// - Identity (id, name)
/// - Processing loop
/// - State management
pub struct DatabaseAgent {
    // Identity
    id: AgentId,
    name: String,

    // Runtime (provides stores, tool execution, context building)
    // Wrapped in Arc for cheap cloning into spawned tasks
    runtime: Arc<AgentRuntime>,

    // Model provider for completions
    model: Arc<dyn ModelProvider>,

    // Model ID for looking up response options from runtime config
    // If None, uses runtime's default model configuration
    model_id: Option<String>,

    // Base instructions (system prompt) for context building
    // Passed to ContextBuilder when preparing requests
    base_instructions: Option<String>,

    // State (needs interior mutability for async state updates)
    state: Arc<RwLock<AgentState>>,

    /// Watch channel for state changes
    state_watch: Option<Arc<(watch::Sender<AgentState>, watch::Receiver<AgentState>)>>,

    // Heartbeat channel for continuation signaling
    heartbeat_sender: HeartbeatSender,
}

impl fmt::Debug for DatabaseAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseAgent")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("runtime", &self.runtime)
            .field("model", &"<dyn ModelProvider>")
            .field("state", &self.state)
            .field("heartbeat_sender", &"<HeartbeatSender>")
            .finish()
    }
}

#[async_trait]
impl Agent for DatabaseAgent {
    fn id(&self) -> AgentId {
        self.id.clone()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn runtime(&self) -> &AgentRuntime {
        &self.runtime
    }

    async fn process(
        self: Arc<Self>,
        message: Message,
    ) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError> {
        use std::collections::HashSet;
        use tokio::sync::mpsc;
        use tokio_stream::wrappers::ReceiverStream;

        // Determine batch ID and type from the incoming message
        let batch_id = message
            .batch
            .unwrap_or_else(|| crate::agent::get_next_message_position_sync());
        let batch_type = message
            .batch_type
            .unwrap_or(crate::message::BatchType::UserRequest);

        // Update state to Processing
        let mut active_batches = HashSet::new();
        active_batches.insert(batch_id);
        self.set_state(AgentState::Processing { active_batches })
            .await?;

        // Create channel for streaming events
        let (tx, rx) = mpsc::channel(100);

        // Clone what we need for the spawned task
        // AgentRuntime is wrapped in Arc for cheap cloning into spawned tasks
        let agent_self = self.clone();
        let model = self.model.clone();
        let agent_id = self.id.clone();

        // Spawn task to do the processing
        tokio::spawn(async move {
            // Helper to send events
            let send_event = |event: ResponseEvent| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(event).await;
                }
            };

            // Create fresh process state for this process() call
            let mut process_state = agent_self.runtime.new_process_state();

            // Retry attempt tracking (per process)
            let mut start_constraint_attempts = 0u8;
            let mut exit_requirement_attempts = 0u8;

            // Extract initial sequence number from incoming message
            // The next message in the batch should have sequence_num + 1
            // If incoming message has no sequence_num, it will be assigned 0, so next is 1
            let initial_sequence_num = message.sequence_num.map(|n| n + 1).unwrap_or(1);

            // 1. Build the initial request (this stores the incoming message)
            let request_result = agent_self
                .runtime
                .prepare_request(
                    vec![message],
                    None, // model_id - use default
                    Some(batch_id),
                    Some(batch_type),
                    agent_self.base_instructions.as_deref(),
                )
                .await;

            let mut request = match request_result {
                Ok(req) => req,
                Err(e) => {
                    let error_msg = format!("Failed to prepare request: {}", e);
                    let error_kind = crate::agent::RecoverableErrorKind::from_error_str(&error_msg);

                    send_event(ResponseEvent::Error {
                        message: error_msg.clone(),
                        recoverable: true,
                    })
                    .await;

                    // Set error state with recovery information
                    let _ = agent_self
                        .set_state(AgentState::Error {
                            kind: error_kind.clone(),
                            message: error_msg.clone(),
                        })
                        .await;

                    // Run error recovery
                    agent_self
                        .run_error_recovery(&error_kind, &error_msg, Some(batch_id))
                        .await;

                    // After recovery, reset to Ready
                    let _ = agent_self.set_state(AgentState::Ready).await;
                    return;
                }
            };

            // 2. Get model options (try agent's model_id, then fall back to default)
            let response_options = if let Some(model_id) = agent_self.model_id.as_deref() {
                // Try specific model first
                agent_self
                    .runtime
                    .config()
                    .get_model_options(model_id)
                    .or_else(|| agent_self.runtime.config().get_default_options())
                    .cloned()
            } else {
                // No model_id specified, use default
                agent_self.runtime.config().get_default_options().cloned()
            };

            let response_options = match response_options {
                Some(opts) => opts,
                None => {
                    send_event(ResponseEvent::Error {
                        message: format!(
                            "No model options configured for '{}' and no default options available",
                            agent_self.model_id.as_deref().unwrap_or("(none)")
                        ),
                        recoverable: false,
                    })
                    .await;

                    // Reset state to Ready
                    let _ = agent_self.set_state(AgentState::Ready).await;
                    return;
                }
            };

            // Variable to hold metadata from the last response for final Complete event
            let mut last_response_metadata = None;

            // Sequence number tracking within the batch
            // Initialized from incoming message's sequence_num + 1
            let mut current_sequence_num: u32 = initial_sequence_num;

            // Get model vendor from response_options for heartbeat requests
            let model_vendor = crate::model::ModelVendor::from_provider_string(
                &response_options.model_info.provider,
            );

            // Track which tool requested external heartbeat (tool_name, call_id)
            let mut heartbeat_tool_info: Option<(String, String)> = None;

            // Track if heartbeat was sent (to skip batch completion)
            let mut heartbeat_sent = false;

            // Main processing loop
            loop {
                // 3. Call the model
                let response_result = model.complete(&response_options, request.clone()).await;

                let response = match response_result {
                    Ok(resp) => resp,
                    Err(e) => {
                        let error_msg = format!("Model completion failed: {}", e);
                        let error_kind =
                            crate::agent::RecoverableErrorKind::from_error_str(&error_msg);

                        // Determine if this is truly recoverable based on error kind
                        let is_recoverable =
                            !matches!(error_kind, crate::agent::RecoverableErrorKind::Unknown);

                        send_event(ResponseEvent::Error {
                            message: error_msg.clone(),
                            recoverable: is_recoverable,
                        })
                        .await;

                        // Set error state with recovery information
                        let _ = agent_self
                            .set_state(AgentState::Error {
                                kind: error_kind.clone(),
                                message: error_msg.clone(),
                            })
                            .await;

                        // Run error recovery
                        agent_self
                            .run_error_recovery(&error_kind, &error_msg, Some(batch_id))
                            .await;

                        // After recovery, reset to Ready
                        let _ = agent_self.set_state(AgentState::Ready).await;
                        return;
                    }
                };

                // Save metadata for final Complete event
                last_response_metadata = Some(response.metadata.clone());

                // 4. Stream response events (text, reasoning)
                for content in &response.content {
                    match content {
                        crate::message::MessageContent::Text(text) => {
                            send_event(ResponseEvent::TextChunk {
                                text: text.clone(),
                                is_final: true,
                            })
                            .await;
                        }
                        crate::message::MessageContent::ToolCalls(calls) => {
                            send_event(ResponseEvent::ToolCalls {
                                calls: calls.clone(),
                            })
                            .await;
                        }
                        _ => {}
                    }
                }

                // Emit reasoning if present
                if let Some(reasoning) = &response.reasoning {
                    send_event(ResponseEvent::ReasoningChunk {
                        text: reasoning.clone(),
                        is_final: true,
                    })
                    .await;
                }

                // 5. Store the response message(s)
                let response_messages = crate::message::Message::from_response(
                    &response,
                    &agent_id,
                    Some(batch_id),
                    Some(batch_type),
                );

                for msg in &response_messages {
                    if let Err(e) = agent_self.runtime.store_message(msg).await {
                        let error_msg = format!("Failed to store message: {}", e);
                        // Message storage failures are typically context/build related
                        let error_kind = crate::agent::RecoverableErrorKind::ContextBuildFailed;

                        send_event(ResponseEvent::Error {
                            message: error_msg.clone(),
                            recoverable: true,
                        })
                        .await;

                        // Set error state with recovery information
                        let _ = agent_self
                            .set_state(AgentState::Error {
                                kind: error_kind.clone(),
                                message: error_msg.clone(),
                            })
                            .await;

                        // Run error recovery
                        agent_self
                            .run_error_recovery(&error_kind, &error_msg, Some(batch_id))
                            .await;

                        // After recovery, reset to Ready
                        let _ = agent_self.set_state(AgentState::Ready).await;
                        return;
                    }
                }

                // 6. Extract and execute tool calls
                let tool_calls: Vec<crate::message::ToolCall> = response
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let crate::message::MessageContent::ToolCalls(calls) = c {
                            Some(calls.clone())
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .collect();

                let mut needs_continuation = false;

                if !tool_calls.is_empty() {
                    let mut tool_responses = Vec::new();

                    // Execute each tool call
                    for call in &tool_calls {
                        // Extract heartbeat request from tool arguments
                        let explicit_heartbeat =
                            crate::context::heartbeat::check_heartbeat_request(&call.fn_arguments);

                        let meta = crate::tool::ExecutionMeta {
                            permission_grant: None,
                            request_heartbeat: explicit_heartbeat,
                            caller_user: None,
                            call_id: Some(crate::ToolCallId(call.call_id.clone())),
                            route_metadata: None,
                        };

                        match agent_self
                            .runtime
                            .execute_tool(call, batch_id, &mut process_state, &meta)
                            .await
                        {
                            Ok(result) => {
                                if result.requests_continuation {
                                    needs_continuation = true;
                                }
                                // Track external heartbeat: only if explicit request AND no ContinueLoop rule
                                // Tools with ContinueLoop continue internally, no external heartbeat needed
                                if explicit_heartbeat && !result.has_continue_rule {
                                    heartbeat_tool_info =
                                        Some((call.fn_name.clone(), call.call_id.clone()));
                                }
                                tool_responses.push(result.response);
                            }
                            Err(e) => {
                                // Handle StartConstraintsNotMet with retry logic
                                if let crate::runtime::ToolExecutionError::RuleViolation(
                                    crate::agent::tool_rules::ToolRuleViolation::StartConstraintsNotMet {
                                        ref required_start_tools,
                                        ..
                                    },
                                ) = e
                                {
                                    start_constraint_attempts += 1;

                                    if start_constraint_attempts >= 3 {
                                        // Attempt 3: Force execute required tools with {} args
                                        for tool_name in required_start_tools {
                                            let synthetic_call = crate::message::ToolCall {
                                                call_id: format!("force_{}", crate::MessageId::generate()),
                                                fn_name: tool_name.clone(),
                                                fn_arguments: serde_json::json!({}),
                                            };

                                            let force_meta = crate::tool::ExecutionMeta {
                                                permission_grant: None,
                                                request_heartbeat: false,
                                                caller_user: None,
                                                call_id: Some(crate::ToolCallId(synthetic_call.call_id.clone())),
                                                route_metadata: None,
                                            };

                                            // Try to execute, but don't error if it fails
                                            match agent_self
                                                .runtime
                                                .execute_tool(&synthetic_call, batch_id, &mut process_state, &force_meta)
                                                .await
                                            {
                                                Ok(result) => {
                                                    tool_responses.push(result.response);
                                                }
                                                Err(_) => {
                                                    // Tool failed with {} args - that's okay, we tried
                                                    tool_responses.push(crate::message::ToolResponse {
                                                        call_id: synthetic_call.call_id.clone(),
                                                        content: format!(
                                                            "Force-executed {} with empty args (failed)",
                                                            tool_name
                                                        ),
                                                        is_error: Some(true),
                                                    });
                                                }
                                            }
                                        }

                                        // Mark constraints as done and add original error as response
                                        agent_self.runtime.mark_start_constraints_done(&mut process_state);
                                        tool_responses.push(crate::message::ToolResponse {
                                            call_id: call.call_id.clone(),
                                            content: format!("Error: {}", e),
                                            is_error: Some(true),
                                        });
                                        needs_continuation = true;
                                    } else {
                                        // Attempt 1 or 2: Return error as tool response
                                        tool_responses.push(crate::message::ToolResponse {
                                            call_id: call.call_id.clone(),
                                            content: format!("Error: {}", e),
                                            is_error: Some(true),
                                        });
                                        needs_continuation = true;

                                        // Attempt 2: Add system reminder for next iteration
                                        if start_constraint_attempts == 2 {
                                            let reminder_text = format!(
                                                "[System Reminder] You must call these tools first before any others: {}",
                                                required_start_tools.join(", ")
                                            );
                                            let reminder_msg = crate::message::Message::user_in_batch_typed(
                                                batch_id,
                                                current_sequence_num,
                                                batch_type,
                                                reminder_text,
                                            );
                                            current_sequence_num += 1;
                                            if let Err(e) = agent_self.runtime.store_message(&reminder_msg).await {
                                                send_event(ResponseEvent::Error {
                                                    message: format!("Failed to store reminder: {}", e),
                                                    recoverable: false,
                                                })
                                                .await;
                                            }
                                        }
                                    }
                                } else {
                                    // Other execution errors: convert to error response
                                    tool_responses.push(crate::message::ToolResponse {
                                        call_id: call.call_id.clone(),
                                        content: format!("Execution error: {}", e),
                                        is_error: Some(true),
                                    });
                                }
                            }
                        }
                    }

                    // Emit and store tool responses
                    if !tool_responses.is_empty() {
                        send_event(ResponseEvent::ToolResponses {
                            responses: tool_responses.clone(),
                        })
                        .await;

                        // Store tool responses as messages
                        for response in &tool_responses {
                            let msg = crate::message::Message::tool_in_batch_typed(
                                batch_id,
                                current_sequence_num,
                                batch_type,
                                vec![response.clone()],
                            );
                            current_sequence_num += 1;

                            if let Err(e) = agent_self.runtime.store_message(&msg).await {
                                send_event(ResponseEvent::Error {
                                    message: format!("Failed to store tool response: {}", e),
                                    recoverable: false,
                                })
                                .await;
                            }
                        }
                    }
                }

                // 7. Check if we should exit the loop
                let should_exit = agent_self.runtime.should_exit_loop(&process_state);

                // Check exit requirements before exiting
                if should_exit || (!needs_continuation && tool_calls.is_empty()) {
                    let pending_exit = agent_self
                        .runtime
                        .get_pending_exit_requirements(&process_state);

                    if !pending_exit.is_empty() {
                        exit_requirement_attempts += 1;

                        if exit_requirement_attempts >= 3 {
                            // Attempt 3: Force execute required exit tools
                            let mut exit_tool_responses = Vec::new();

                            for tool_name in &pending_exit {
                                let synthetic_call = crate::message::ToolCall {
                                    call_id: format!("exit_force_{}", crate::MessageId::generate()),
                                    fn_name: tool_name.clone(),
                                    fn_arguments: serde_json::json!({}),
                                };

                                let force_meta = crate::tool::ExecutionMeta {
                                    permission_grant: None,
                                    request_heartbeat: false,
                                    caller_user: None,
                                    call_id: Some(crate::ToolCallId(
                                        synthetic_call.call_id.clone(),
                                    )),
                                    route_metadata: None,
                                };

                                // Try to execute, but collect responses
                                match agent_self
                                    .runtime
                                    .execute_tool(
                                        &synthetic_call,
                                        batch_id,
                                        &mut process_state,
                                        &force_meta,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        exit_tool_responses.push(result.response);
                                    }
                                    Err(_) => {
                                        // Tool failed with {} args - that's okay, we tried
                                        exit_tool_responses.push(crate::message::ToolResponse {
                                            call_id: synthetic_call.call_id.clone(),
                                            content: format!(
                                                "Force-executed {} with empty args (failed)",
                                                tool_name
                                            ),
                                            is_error: Some(true),
                                        });
                                    }
                                }
                            }

                            // Emit and store exit tool responses
                            if !exit_tool_responses.is_empty() {
                                send_event(ResponseEvent::ToolResponses {
                                    responses: exit_tool_responses.clone(),
                                })
                                .await;

                                // Store tool responses as messages
                                for response in &exit_tool_responses {
                                    let msg = crate::message::Message::tool_in_batch_typed(
                                        batch_id,
                                        current_sequence_num,
                                        batch_type,
                                        vec![response.clone()],
                                    );
                                    current_sequence_num += 1;

                                    if let Err(e) = agent_self.runtime.store_message(&msg).await {
                                        send_event(ResponseEvent::Error {
                                            message: format!(
                                                "Failed to store exit tool response: {}",
                                                e
                                            ),
                                            recoverable: false,
                                        })
                                        .await;
                                    }
                                }
                            }

                            agent_self.runtime.mark_complete(&mut process_state);
                            break;
                        } else {
                            // Attempt 1 or 2: Add reminder and continue
                            let reminder_intensity = if exit_requirement_attempts == 1 {
                                "Reminder"
                            } else {
                                "IMPORTANT REMINDER"
                            };

                            let reminder_text = format!(
                                "[System {}] You must call these tools before ending the conversation: {}",
                                reminder_intensity,
                                pending_exit.join(", ")
                            );
                            let reminder_msg = crate::message::Message::user_in_batch_typed(
                                batch_id,
                                current_sequence_num,
                                batch_type,
                                reminder_text,
                            );
                            current_sequence_num += 1;
                            if let Err(e) = agent_self.runtime.store_message(&reminder_msg).await {
                                send_event(ResponseEvent::Error {
                                    message: format!("Failed to store exit reminder: {}", e),
                                    recoverable: false,
                                })
                                .await;
                            }

                            needs_continuation = true;
                        }
                    } else {
                        // No pending exit requirements
                        // Check if we should send heartbeat instead of completing
                        if let Some((tool_name, tool_call_id)) = heartbeat_tool_info.take() {
                            // Send heartbeat request for external continuation
                            let heartbeat_req = crate::context::heartbeat::HeartbeatRequest {
                                agent_id: agent_id.clone(),
                                tool_name,
                                tool_call_id,
                                batch_id: Some(batch_id),
                                next_sequence_num: Some(current_sequence_num),
                                model_vendor: Some(model_vendor),
                            };

                            if let Err(e) = agent_self.heartbeat_sender.try_send(heartbeat_req) {
                                tracing::warn!("Failed to send heartbeat: {:?}", e);
                            } else {
                                heartbeat_sent = true;
                            }

                            // Exit the loop WITHOUT completing the batch
                            // The heartbeat processor will continue the batch
                            break;
                        }

                        // No heartbeat needed, safe to exit
                        break;
                    }
                }

                // 8. Continue loop if needed
                if needs_continuation {
                    // Check if continuation is due to heartbeat request with ExitLoop
                    // In this case, send external heartbeat instead of internal loop
                    let should_exit = agent_self.runtime.should_exit_loop(&process_state);
                    if should_exit {
                        if let Some((tool_name, tool_call_id)) = heartbeat_tool_info.take() {
                            // Send heartbeat request for external continuation
                            let heartbeat_req = crate::context::heartbeat::HeartbeatRequest {
                                agent_id: agent_id.clone(),
                                tool_name,
                                tool_call_id,
                                batch_id: Some(batch_id),
                                next_sequence_num: Some(current_sequence_num),
                                model_vendor: Some(model_vendor),
                            };

                            if let Err(e) = agent_self.heartbeat_sender.try_send(heartbeat_req) {
                                tracing::warn!("Failed to send heartbeat: {:?}", e);
                            } else {
                                heartbeat_sent = true;
                            }

                            // Exit the loop WITHOUT completing the batch
                            break;
                        }
                    }

                    // Prepare next request with updated context (internal continuation)
                    let next_request = agent_self
                        .runtime
                        .prepare_request(
                            Vec::<crate::message::Message>::new(),
                            None,
                            Some(batch_id),
                            Some(batch_type),
                            agent_self.base_instructions.as_deref(),
                        )
                        .await;

                    request = match next_request {
                        Ok(req) => req,
                        Err(e) => {
                            send_event(ResponseEvent::Error {
                                message: format!("Failed to prepare continuation request: {}", e),
                                recoverable: false,
                            })
                            .await;
                            let _ = agent_self.set_state(AgentState::Ready).await;
                            return;
                        }
                    };
                } else {
                    // No continuation needed - exit handled above
                    break;
                }
            }

            // 9. Complete the batch ONLY if heartbeat was not sent
            // If heartbeat was sent, the batch continues via the heartbeat processor
            if !heartbeat_sent {
                agent_self.runtime.complete_batch(batch_id);
            }

            // 10. Emit completion event - always emit, even with heartbeat
            // This processing loop is complete; async continuation is pending if heartbeat sent
            send_event(ResponseEvent::Complete {
                message_id: crate::MessageId::generate(),
                metadata: last_response_metadata.unwrap_or_else(|| {
                    crate::message::ResponseMetadata {
                        processing_time: None,
                        tokens_used: None,
                        model_used: None,
                        confidence: None,
                        model_iden: genai::ModelIden::new(
                            genai::adapter::AdapterKind::Anthropic,
                            "unknown",
                        ),
                        custom: serde_json::json!({}),
                    }
                }),
            })
            .await;

            // 11. Update state back to Ready
            let _ = agent_self.set_state(AgentState::Ready).await;
        });

        // Return the receiver as a stream
        Ok(Box::new(ReceiverStream::new(rx)))
    }

    /// Get the agent's current state and a watch receiver for changes
    async fn state(&self) -> (AgentState, Option<tokio::sync::watch::Receiver<AgentState>>) {
        let state = self.state.read().await.clone();
        let rx = self.state_watch.as_ref().map(|watch| watch.1.clone());
        (state, rx)
    }

    async fn set_state(&self, state: AgentState) -> Result<(), CoreError> {
        *self.state.write().await = state.clone();
        if let Some(arc) = &self.state_watch {
            let _ = arc.0.send(state);
        }
        Ok(())
    }
}

impl DatabaseAgent {
    /// Create a new builder for constructing a DatabaseAgent
    pub fn builder() -> DatabaseAgentBuilder {
        DatabaseAgentBuilder::default()
    }

    /// Run error recovery based on the error kind.
    ///
    /// This method performs cleanup and recovery actions based on the type of error
    /// encountered, making the agent more resilient to API quirks and transient issues.
    ///
    /// The recovery actions are based on production experience with Anthropic, Gemini,
    /// and other model providers.
    async fn run_error_recovery(
        &self,
        error_kind: &crate::agent::RecoverableErrorKind,
        error_msg: &str,
        batch_id: Option<crate::agent::SnowflakePosition>,
    ) {
        use crate::agent::RecoverableErrorKind;

        tracing::warn!(
            agent_id = %self.id,
            error_kind = ?error_kind,
            batch_id = ?batch_id,
            "Running error recovery: {}",
            error_msg
        );

        match error_kind {
            RecoverableErrorKind::AnthropicThinkingOrder => {
                // Anthropic thinking mode requires specific message ordering.
                // The error often includes an index indicating the problematic message.
                // Recovery: Clean up the batch to remove unpaired tool calls which
                // can cause ordering violations with thinking blocks.
                tracing::info!(
                    agent_id = %self.id,
                    "Anthropic thinking order error - cleaning up batch"
                );

                if let Some(batch) = batch_id {
                    match self.runtime.messages().cleanup_batch(&batch).await {
                        Ok(removed) => {
                            tracing::info!(
                                agent_id = %self.id,
                                batch = %batch,
                                removed_count = removed,
                                "Cleaned up batch for Anthropic thinking order fix"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                agent_id = %self.id,
                                batch = %batch,
                                error = %e,
                                "Failed to clean up batch for Anthropic thinking order"
                            );
                        }
                    }
                }
            }

            RecoverableErrorKind::GeminiEmptyContents => {
                // Gemini fails when contents array is empty or contains only empty entries.
                // This often happens during heartbeat continuations.
                // Recovery: First try to clean up empty messages in the batch.
                // If batch is truly empty, add a synthetic message to ensure non-empty context.
                tracing::info!(
                    agent_id = %self.id,
                    "Gemini empty contents error - cleaning up empty messages"
                );

                if let Some(batch) = batch_id {
                    // First, try cleaning up the batch (removes empty/unpaired messages)
                    match self.runtime.messages().cleanup_batch(&batch).await {
                        Ok(removed) => {
                            tracing::info!(
                                agent_id = %self.id,
                                batch = %batch,
                                removed_count = removed,
                                "Cleaned up empty messages for Gemini"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %self.id,
                                error = %e,
                                "Failed to cleanup batch, will add synthetic message"
                            );
                        }
                    }

                    // Check if batch is now empty and add synthetic message if needed
                    match self.runtime.messages().get_batch(&batch.to_string()).await {
                        Ok(messages) => {
                            if messages.is_empty() {
                                // Add a synthetic message to prevent empty context
                                match self
                                    .runtime
                                    .messages()
                                    .add_synthetic_message(
                                        batch,
                                        "[System: Continuing conversation]",
                                    )
                                    .await
                                {
                                    Ok(msg_id) => {
                                        tracing::info!(
                                            agent_id = %self.id,
                                            batch = %batch,
                                            message_id = %msg_id.0,
                                            "Added synthetic message to prevent empty Gemini context"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            agent_id = %self.id,
                                            error = %e,
                                            "Failed to add synthetic message for Gemini"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %self.id,
                                error = %e,
                                "Failed to check batch contents"
                            );
                        }
                    }
                }
            }

            RecoverableErrorKind::UnpairedToolCalls
            | RecoverableErrorKind::UnpairedToolResponses => {
                // Tool call/response pairs must match. This can happen when:
                // - Compression removes one half of a pair
                // - Timeouts cause partial tool execution
                // Recovery: Remove unpaired tool calls/responses from the batch.
                tracing::info!(
                    agent_id = %self.id,
                    "Unpaired tool call/response error - cleaning up batch"
                );

                if let Some(batch) = batch_id {
                    match self.runtime.messages().cleanup_batch(&batch).await {
                        Ok(removed) => {
                            tracing::info!(
                                agent_id = %self.id,
                                batch = %batch,
                                removed_count = removed,
                                "Removed unpaired tool calls/responses from batch"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                agent_id = %self.id,
                                batch = %batch,
                                error = %e,
                                "Failed to clean up unpaired tool calls/responses"
                            );
                        }
                    }
                }
            }

            RecoverableErrorKind::PromptTooLong => {
                // The prompt exceeds the model's token limit.
                // Recovery: Force aggressive compression of message history.
                tracing::info!(
                    agent_id = %self.id,
                    "Prompt too long - forcing context compression"
                );

                // Use a conservative limit - keep only recent messages
                // The context builder will handle proper compression on retry
                const EMERGENCY_KEEP_RECENT: usize = 20;

                match self
                    .runtime
                    .messages()
                    .force_compression(EMERGENCY_KEEP_RECENT)
                    .await
                {
                    Ok(archived) => {
                        tracing::info!(
                            agent_id = %self.id,
                            archived_count = archived,
                            keep_recent = EMERGENCY_KEEP_RECENT,
                            "Force compression complete - archived {} messages",
                            archived
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            agent_id = %self.id,
                            error = %e,
                            "Failed to force compression"
                        );
                    }
                }
            }

            RecoverableErrorKind::MessageCompressionFailed => {
                // Compression itself failed, possibly due to invalid message structure.
                // Recovery: Clean up any problematic batches.
                tracing::info!(
                    agent_id = %self.id,
                    "Message compression failed - cleaning up current batch"
                );

                if let Some(batch) = batch_id {
                    match self.runtime.messages().cleanup_batch(&batch).await {
                        Ok(removed) => {
                            if removed > 0 {
                                tracing::info!(
                                    agent_id = %self.id,
                                    batch = %batch,
                                    removed_count = removed,
                                    "Cleaned up batch after compression failure"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %self.id,
                                error = %e,
                                "Failed to clean up batch after compression failure"
                            );
                        }
                    }
                }
            }

            RecoverableErrorKind::ContextBuildFailed => {
                // Context building failed, possibly due to message store issues.
                // Recovery: Clean up current batch and try to recover.
                tracing::info!(
                    agent_id = %self.id,
                    "Context build failed - cleaning up for rebuild"
                );

                if let Some(batch) = batch_id {
                    match self.runtime.messages().cleanup_batch(&batch).await {
                        Ok(removed) => {
                            tracing::info!(
                                agent_id = %self.id,
                                batch = %batch,
                                removed_count = removed,
                                "Cleaned up batch for context rebuild"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent_id = %self.id,
                                error = %e,
                                "Failed to clean up batch for context rebuild"
                            );
                        }
                    }
                }
            }

            RecoverableErrorKind::ModelApiError => {
                // Generic model API error (rate limit, server error, etc.)
                // Check if it's a rate limit error and extract wait time.
                let is_rate_limit = error_msg.contains("429")
                    || error_msg.to_lowercase().contains("rate limit")
                    || error_msg.to_lowercase().contains("too many requests");

                if is_rate_limit {
                    // Extract wait time from error message if possible
                    let wait_seconds = Self::extract_rate_limit_wait_time(error_msg);

                    tracing::info!(
                        agent_id = %self.id,
                        wait_seconds = wait_seconds,
                        "Rate limit hit - waiting before retry"
                    );

                    // Actually wait for the backoff period
                    tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds)).await;

                    tracing::info!(
                        agent_id = %self.id,
                        "Rate limit wait complete, ready for retry"
                    );
                } else {
                    tracing::info!(
                        agent_id = %self.id,
                        "Model API error (non-rate-limit) - will retry"
                    );
                }
            }

            RecoverableErrorKind::Unknown => {
                // Unknown error type - do generic cleanup.
                tracing::warn!(
                    agent_id = %self.id,
                    "Unknown error type - performing generic cleanup"
                );

                // Clean up current batch if available
                if let Some(batch) = batch_id {
                    if let Err(e) = self.runtime.messages().cleanup_batch(&batch).await {
                        tracing::warn!(
                            agent_id = %self.id,
                            error = %e,
                            "Failed generic batch cleanup"
                        );
                    }
                }
            }
        }

        // Prune any expired state from the tool executor
        self.runtime.prune_expired();

        tracing::info!(
            agent_id = %self.id,
            "Error recovery complete"
        );
    }

    /// Extract wait time from rate limit error messages.
    ///
    /// Attempts to parse common rate limit response formats:
    /// - "retry-after: 30" header value
    /// - "wait 30 seconds" in message
    /// - "reset in 30s" in message
    ///
    /// Returns a default backoff if parsing fails.
    fn extract_rate_limit_wait_time(error_msg: &str) -> u64 {
        let error_lower = error_msg.to_lowercase();

        // Try to find "retry-after: N" or "retry after N"
        if let Some(idx) = error_lower.find("retry") {
            let after_retry = &error_msg[idx..];
            if let Some(num_start) = after_retry.find(|c: char| c.is_ascii_digit()) {
                let num_str: String = after_retry[num_start..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(seconds) = num_str.parse::<u64>() {
                    return seconds.min(300); // Cap at 5 minutes
                }
            }
        }

        // Try to find "wait N seconds" or "N seconds"
        if let Some(idx) = error_lower.find("second") {
            // Look backwards for a number
            let before_seconds = &error_msg[..idx];
            let num_str: String = before_seconds
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ' ')
                .collect::<String>()
                .chars()
                .rev()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if let Ok(seconds) = num_str.parse::<u64>() {
                return seconds.min(300);
            }
        }

        // Try to find "reset in Ns" pattern
        if let Some(idx) = error_lower.find("reset") {
            let after_reset = &error_msg[idx..];
            if let Some(num_start) = after_reset.find(|c: char| c.is_ascii_digit()) {
                let num_str: String = after_reset[num_start..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(seconds) = num_str.parse::<u64>() {
                    return seconds.min(300);
                }
            }
        }

        // Default: exponential backoff starting at 30 seconds
        30
    }
}

/// Builder for constructing a DatabaseAgent
#[derive(Default)]
pub struct DatabaseAgentBuilder {
    id: Option<AgentId>,
    name: Option<String>,
    runtime: Option<Arc<AgentRuntime>>,
    model: Option<Arc<dyn ModelProvider>>,
    model_id: Option<String>,
    base_instructions: Option<String>,
    heartbeat_sender: Option<HeartbeatSender>,
}

impl DatabaseAgentBuilder {
    /// Set the agent ID
    pub fn id(mut self, id: AgentId) -> Self {
        self.id = Some(id);
        self
    }

    /// Set the agent name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the runtime
    /// Accepts Arc<AgentRuntime> for sharing across spawned tasks
    pub fn runtime(mut self, runtime: Arc<AgentRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Set the model provider
    pub fn model(mut self, model: Arc<dyn ModelProvider>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the model ID for looking up response options from runtime config
    /// If not set, uses runtime's default model configuration
    pub fn model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Set base instructions (system prompt) for context building
    ///
    /// These instructions are passed to ContextBuilder when preparing requests
    /// and become the foundation of the agent's system prompt.
    ///
    /// Empty strings are treated as None (use default instructions).
    pub fn base_instructions(mut self, instructions: impl Into<String>) -> Self {
        let instructions = instructions.into();
        // Empty string should be treated as None (use default)
        if !instructions.is_empty() {
            self.base_instructions = Some(instructions);
        }
        self
    }

    /// Set the heartbeat sender
    pub fn heartbeat_sender(mut self, sender: HeartbeatSender) -> Self {
        self.heartbeat_sender = Some(sender);
        self
    }

    /// Build the DatabaseAgent, validating that all required fields are present
    pub fn build(self) -> Result<DatabaseAgent, CoreError> {
        let id = self.id.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "id is required".to_string(),
        })?;

        let name = self.name.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "name is required".to_string(),
        })?;

        let runtime = self.runtime.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "runtime is required".to_string(),
        })?;

        let model = self.model.ok_or_else(|| CoreError::InvalidFormat {
            data_type: "DatabaseAgent".to_string(),
            details: "model is required".to_string(),
        })?;

        let heartbeat_sender = self
            .heartbeat_sender
            .ok_or_else(|| CoreError::InvalidFormat {
                data_type: "DatabaseAgent".to_string(),
                details: "heartbeat_sender is required".to_string(),
            })?;

        let state = AgentState::Ready;
        let (tx, rx) = watch::channel(state.clone());
        Ok(DatabaseAgent {
            id,
            name,
            runtime,
            model,
            model_id: self.model_id,
            base_instructions: self.base_instructions,
            state: Arc::new(RwLock::new(state)),
            state_watch: Some(Arc::new((tx, rx))),
            heartbeat_sender,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::heartbeat::heartbeat_channel;
    use crate::memory::{
        ArchivalEntry, BlockMetadata, BlockSchema, BlockType, MemoryResult, MemorySearchResult,
        MemoryStore, SearchOptions, StructuredDocument,
    };
    use crate::messages::MessageStore;
    use async_trait::async_trait;
    use serde_json::Value as JsonValue;

    // Mock ModelProvider for testing
    #[derive(Debug)]
    struct MockModelProvider;

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(
            &self,
            _options: &crate::model::ResponseOptions,
            _request: crate::message::Request,
        ) -> crate::Result<crate::message::Response> {
            unimplemented!("Mock provider")
        }

        async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
            Ok(Vec::new())
        }

        async fn supports_capability(
            &self,
            _model: &str,
            _capability: crate::model::ModelCapability,
        ) -> bool {
            false
        }

        async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
            Ok(0)
        }
    }

    // Mock MemoryStore for testing
    #[derive(Debug)]
    struct MockMemoryStore;

    #[async_trait]
    impl MemoryStore for MockMemoryStore {
        async fn create_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _description: &str,
            _block_type: BlockType,
            _schema: BlockSchema,
            _char_limit: usize,
        ) -> MemoryResult<String> {
            Ok("test-block-id".to_string())
        }

        async fn get_block(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }

        async fn get_block_metadata(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<BlockMetadata>> {
            Ok(None)
        }

        async fn list_blocks(&self, _agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
            Ok(Vec::new())
        }

        async fn list_blocks_by_type(
            &self,
            _agent_id: &str,
            _block_type: BlockType,
        ) -> MemoryResult<Vec<BlockMetadata>> {
            Ok(Vec::new())
        }

        async fn delete_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn get_rendered_content(
            &self,
            _agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<String>> {
            Ok(None)
        }

        async fn persist_block(&self, _agent_id: &str, _label: &str) -> MemoryResult<()> {
            Ok(())
        }

        fn mark_dirty(&self, _agent_id: &str, _label: &str) {}

        async fn insert_archival(
            &self,
            _agent_id: &str,
            _content: &str,
            _metadata: Option<JsonValue>,
        ) -> MemoryResult<String> {
            Ok("archival-id".to_string())
        }

        async fn search_archival(
            &self,
            _agent_id: &str,
            _query: &str,
            _limit: usize,
        ) -> MemoryResult<Vec<ArchivalEntry>> {
            Ok(Vec::new())
        }

        async fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
            Ok(())
        }

        async fn search(
            &self,
            _agent_id: &str,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn search_all(
            &self,
            _query: &str,
            _options: SearchOptions,
        ) -> MemoryResult<Vec<MemorySearchResult>> {
            Ok(Vec::new())
        }

        async fn update_block_text(
            &self,
            _agent_id: &str,
            _label: &str,
            _new_content: &str,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn append_to_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _content: &str,
        ) -> MemoryResult<()> {
            Ok(())
        }

        async fn replace_in_block(
            &self,
            _agent_id: &str,
            _label: &str,
            _old: &str,
            _new: &str,
        ) -> MemoryResult<bool> {
            Ok(false)
        }

        async fn list_shared_blocks(
            &self,
            _agent_id: &str,
        ) -> MemoryResult<Vec<crate::memory::SharedBlockInfo>> {
            Ok(Vec::new())
        }

        async fn get_shared_block(
            &self,
            _requester_agent_id: &str,
            _owner_agent_id: &str,
            _label: &str,
        ) -> MemoryResult<Option<StructuredDocument>> {
            Ok(None)
        }
    }

    async fn test_dbs() -> crate::db::ConstellationDatabases {
        crate::db::ConstellationDatabases::open_in_memory()
            .await
            .unwrap()
    }

    /// Helper to create a test agent in the database
    async fn create_test_agent(dbs: &crate::db::ConstellationDatabases, id: &str) {
        use pattern_db::models::{Agent, AgentStatus};
        use sqlx::types::Json as SqlxJson;

        let agent = Agent {
            id: id.to_string(),
            name: format!("Test Agent {}", id),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: SqlxJson(serde_json::json!({})),
            enabled_tools: SqlxJson(vec![]),
            tool_rules: None,
            status: AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(dbs.constellation.pool(), &agent)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_builder_requires_id() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("id"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_name() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("name"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_runtime() {
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("runtime"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_model() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .heartbeat_sender(heartbeat_tx)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("model"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_requires_heartbeat_sender() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let result = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .build();

        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::InvalidFormat { data_type, details } => {
                assert_eq!(data_type, "DatabaseAgent");
                assert!(details.contains("heartbeat"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[tokio::test]
    async fn test_builder_success() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        assert_eq!(agent.id().as_str(), "test_agent");
        assert_eq!(agent.name(), "Test Agent");
    }

    #[tokio::test]
    async fn test_initial_state_is_ready() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    #[tokio::test]
    async fn test_state_update() {
        let dbs = test_dbs().await;
        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(MockModelProvider) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .build()
            .unwrap();

        let agent = DatabaseAgent::builder()
            .id(AgentId::new("test_agent"))
            .name("Test Agent")
            .runtime(Arc::new(runtime))
            .model(model)
            .heartbeat_sender(heartbeat_tx)
            .build()
            .unwrap();

        agent.set_state(AgentState::Suspended).await.unwrap();
        assert_eq!(agent.state().await.0, AgentState::Suspended);
    }

    #[tokio::test]
    async fn test_process_basic_flow() {
        use crate::agent::Agent;
        use crate::message::{Message, MessageContent, Response, ResponseMetadata};
        use futures::StreamExt;

        // Mock model that returns a simple text response
        #[derive(Debug)]
        struct SimpleTestModel;

        #[async_trait]
        impl ModelProvider for SimpleTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::message::Request,
            ) -> crate::Result<crate::message::Response> {
                Ok(Response {
                    content: vec![MessageContent::Text("Hello from model!".to_string())],
                    reasoning: None,
                    metadata: ResponseMetadata {
                        processing_time: None,
                        tokens_used: None,
                        model_used: Some("test".to_string()),
                        confidence: None,
                        model_iden: genai::ModelIden::new(
                            genai::adapter::AdapterKind::Anthropic,
                            "test",
                        ),
                        custom: serde_json::json!({}),
                    },
                })
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(SimpleTestModel) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with model options configured
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .dbs(dbs.clone())
            .config(runtime_config)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Create a test message
        let test_message = Message::user("Hello agent!");

        // Process the message
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Debug: print all events
        for (i, event) in events.iter().enumerate() {
            eprintln!("Event {}: {:?}", i, event);
        }

        // Verify we got the expected events
        assert!(!events.is_empty(), "Should have received events");

        // Should have at least one TextChunk and one Complete event
        let has_text = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextChunk { .. }));
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(has_text, "Should have received TextChunk event");
        assert!(has_complete, "Should have received Complete event");

        // Verify state is back to Ready
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    #[tokio::test]
    async fn test_tool_execution_flow() {
        use crate::agent::Agent;
        use crate::message::{Message, MessageContent, Response, ResponseMetadata, ToolCall};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that returns tool calls
        #[derive(Debug)]
        struct ToolCallModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ToolCallModel {
            fn name(&self) -> &str {
                "test_tool"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::message::Request,
            ) -> crate::Result<crate::message::Response> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                if count == 0 {
                    // First call: return a tool call
                    Ok(Response {
                        content: vec![MessageContent::ToolCalls(vec![ToolCall {
                            call_id: "test_call_1".to_string(),
                            fn_name: "test_tool".to_string(),
                            fn_arguments: serde_json::json!({ "message": "hello" }),
                        }])],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                } else {
                    // Second call: return text response
                    Ok(Response {
                        content: vec![MessageContent::Text(
                            "Tool executed successfully".to_string(),
                        )],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                }
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        // Create a simple test tool
        #[derive(Debug, Clone)]
        struct TestTool;

        #[async_trait]
        impl AiTool for TestTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "test_tool"
            }

            fn description(&self) -> &str {
                "A test tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Tool executed".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ToolCallModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with model options and register test tool
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let mut tools = crate::tool::ToolRegistry::new();
        tools.register(TestTool);

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test tool execution");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Verify we got tool calls and tool responses
        let has_tool_calls = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCalls { .. }));
        let has_tool_responses = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolResponses { .. }));
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(has_tool_calls, "Should have emitted ToolCalls event");
        assert!(
            has_tool_responses,
            "Should have emitted ToolResponses event"
        );
        assert!(has_complete, "Should have emitted Complete event");
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    // TODO: Re-enable these tests once runtime.prepare_request() properly supports continuation
    // Currently fails with "Invalid data format: SnowflakePosition" during continuation
    #[tokio::test]
    async fn test_start_constraint_retry() {
        use crate::agent::Agent;
        use crate::agent::tool_rules::{ToolRule, ToolRuleType};
        use crate::message::{Message, MessageContent, Response, ResponseMetadata, ToolCall};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that tries to call regular tool before start constraint tool
        #[derive(Debug)]
        struct ConstraintTestModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ConstraintTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::message::Request,
            ) -> crate::Result<crate::message::Response> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                if count < 3 {
                    // First 3 attempts: try to call wrong tool
                    Ok(Response {
                        content: vec![MessageContent::ToolCalls(vec![ToolCall {
                            call_id: format!("bad_call_{}", count),
                            fn_name: "regular_tool".to_string(),
                            fn_arguments: serde_json::json!({}),
                        }])],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                } else {
                    // After retries: just return text
                    Ok(Response {
                        content: vec![MessageContent::Text("Done".to_string())],
                        reasoning: None,
                        metadata: ResponseMetadata {
                            processing_time: None,
                            tokens_used: None,
                            model_used: Some("test".to_string()),
                            confidence: None,
                            model_iden: genai::ModelIden::new(
                                genai::adapter::AdapterKind::Anthropic,
                                "test",
                            ),
                            custom: serde_json::json!({}),
                        },
                    })
                }
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        #[derive(Debug, Clone)]
        struct StartTool;

        #[async_trait]
        impl AiTool for StartTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "start_tool"
            }

            fn description(&self) -> &str {
                "Start constraint tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Started".to_string())
            }
        }

        #[derive(Debug, Clone)]
        struct RegularTool;

        #[async_trait]
        impl AiTool for RegularTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "regular_tool"
            }

            fn description(&self) -> &str {
                "Regular tool"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Regular".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ConstraintTestModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with start constraint rule
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let mut tools = crate::tool::ToolRegistry::new();
        tools.register(StartTool);
        tools.register(RegularTool);

        let start_rule = ToolRule {
            tool_name: "start_tool".to_string(),
            rule_type: ToolRuleType::StartConstraint,
            conditions: vec![],
            priority: 100,
            metadata: None,
        };

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .add_tool_rule(start_rule)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test start constraint");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Debug: print all events
        eprintln!(
            "=== Start Constraint Test Events ({} total) ===",
            events.len()
        );
        for (i, event) in events.iter().enumerate() {
            eprintln!("Event {}: {:?}", i, event);
        }

        // Should have tool responses (including errors and forced execution)
        let has_tool_responses = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolResponses { .. }));

        // Should eventually complete (retry logic worked)
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(
            has_tool_responses,
            "Should have emitted tool responses during retry attempts. Got {} events",
            events.len()
        );
        assert!(
            has_complete,
            "Should eventually complete after retries. Got {} events",
            events.len()
        );
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }

    // TODO: Re-enable once runtime.prepare_request() properly supports continuation
    #[tokio::test]
    async fn test_exit_requirement_retry() {
        use crate::agent::Agent;
        use crate::agent::tool_rules::{ToolRule, ToolRuleType};
        use crate::message::{Message, MessageContent, Response, ResponseMetadata};
        use crate::tool::{AiTool, ExecutionMeta};
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Mock model that tries to exit without calling required exit tool
        #[derive(Debug)]
        struct ExitTestModel {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl ModelProvider for ExitTestModel {
            fn name(&self) -> &str {
                "test"
            }

            async fn complete(
                &self,
                _options: &crate::model::ResponseOptions,
                _request: crate::message::Request,
            ) -> crate::Result<crate::message::Response> {
                let _count = self.call_count.fetch_add(1, Ordering::SeqCst);

                // Always return text (no tool calls = wants to exit)
                Ok(Response {
                    content: vec![MessageContent::Text("I'm done".to_string())],
                    reasoning: None,
                    metadata: ResponseMetadata {
                        processing_time: None,
                        tokens_used: None,
                        model_used: Some("test".to_string()),
                        confidence: None,
                        model_iden: genai::ModelIden::new(
                            genai::adapter::AdapterKind::Anthropic,
                            "test",
                        ),
                        custom: serde_json::json!({}),
                    },
                })
            }

            async fn list_models(&self) -> crate::Result<Vec<crate::model::ModelInfo>> {
                Ok(Vec::new())
            }

            async fn supports_capability(
                &self,
                _model: &str,
                _capability: crate::model::ModelCapability,
            ) -> bool {
                false
            }

            async fn count_tokens(&self, _model: &str, _content: &str) -> crate::Result<usize> {
                Ok(0)
            }
        }

        #[derive(Debug, Clone)]
        struct ExitTool;

        #[async_trait]
        impl AiTool for ExitTool {
            type Input = serde_json::Value;
            type Output = String;

            fn name(&self) -> &str {
                "exit_tool"
            }

            fn description(&self) -> &str {
                "Required before exit"
            }

            async fn execute(
                &self,
                _params: Self::Input,
                _meta: &ExecutionMeta,
            ) -> crate::Result<Self::Output> {
                Ok("Exit handled".to_string())
            }
        }

        let dbs = test_dbs().await;
        create_test_agent(&dbs, "test_agent").await;

        let memory = Arc::new(MockMemoryStore);
        let messages = MessageStore::new(dbs.constellation.pool().clone(), "test_agent");
        let model = Arc::new(ExitTestModel {
            call_count: Arc::new(AtomicUsize::new(0)),
        }) as Arc<dyn ModelProvider>;
        let (heartbeat_tx, _heartbeat_rx) = heartbeat_channel();

        // Create runtime with exit requirement rule
        let mut runtime_config = crate::runtime::RuntimeConfig::default();
        let model_info = crate::model::ModelInfo {
            id: "test".to_string(),
            name: "Test Model".to_string(),
            provider: "test".to_string(),
            capabilities: vec![],
            context_window: 8000,
            max_output_tokens: Some(1000),
            cost_per_1k_prompt_tokens: None,
            cost_per_1k_completion_tokens: None,
        };
        let response_opts = crate::model::ResponseOptions::new(model_info);
        runtime_config.set_model_options("default", response_opts.clone());
        runtime_config.set_default_options(response_opts); // Use same options as default fallback

        let mut tools = crate::tool::ToolRegistry::new();
        tools.register(ExitTool);

        let exit_rule = ToolRule {
            tool_name: "exit_tool".to_string(),
            rule_type: ToolRuleType::RequiredBeforeExit,
            conditions: vec![],
            priority: 100,
            metadata: None,
        };

        let runtime = AgentRuntime::builder()
            .agent_id("test_agent")
            .memory(memory)
            .messages(messages)
            .tools(tools)
            .dbs(dbs.clone())
            .config(runtime_config)
            .add_tool_rule(exit_rule)
            .build()
            .unwrap();

        let agent = Arc::new(
            DatabaseAgent::builder()
                .id(AgentId::new("test_agent"))
                .name("Test Agent")
                .runtime(Arc::new(runtime))
                .model(model)
                .heartbeat_sender(heartbeat_tx)
                .build()
                .unwrap(),
        );

        // Process a message
        let test_message = Message::user("Test exit requirement");
        let stream = agent.clone().process(test_message).await.unwrap();

        // Collect events
        let events: Vec<_> = stream.collect().await;

        // Should eventually complete after force-executing exit tool
        let has_complete = events
            .iter()
            .any(|e| matches!(e, ResponseEvent::Complete { .. }));

        assert!(has_complete, "Should eventually complete");
        assert_eq!(agent.state().await.0, AgentState::Ready);
    }
}
