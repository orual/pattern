//! Main processing loop implementation.
//!
//! This module contains the core processing loop extracted from DatabaseAgent,
//! restructured to:
//! - Process content blocks in order with inline tool execution
//! - Use centralized error handling
//! - Support early exit on tool actions

use tokio::sync::mpsc;

use crate::agent::ResponseEvent;
use crate::context::heartbeat::{HeartbeatRequest, HeartbeatSender, check_heartbeat_request};
use crate::id::AgentId;
use crate::messages::{BatchType, Message, ResponseMetadata, ToolCall, ToolResponse};
use crate::model::{ModelVendor, ResponseOptions};
use crate::runtime::{AgentRuntime, ProcessToolState, ToolAction, ToolExecutionError};
use crate::tool::ExecutionMeta;
use crate::{MessageId, ModelProvider, SnowflakePosition, ToolCallId};

use super::content::{ContentItem, iter_content_items};
use super::errors::{ErrorContext, ProcessingError, handle_processing_error};
use super::retry::{RetryConfig, complete_with_retry};

/// Immutable context for the processing loop.
pub struct ProcessingContext<'a> {
    pub agent_id: &'a str,
    pub runtime: &'a AgentRuntime,
    pub model: &'a dyn ModelProvider,
    pub response_options: &'a ResponseOptions,
    pub base_instructions: Option<&'a str>,
    pub batch_id: SnowflakePosition,
    pub batch_type: BatchType,
    pub heartbeat_sender: &'a HeartbeatSender,
}

/// Mutable state that changes during processing.
pub struct ProcessingState {
    pub process_state: ProcessToolState,
    pub sequence_num: u32,
    pub start_constraint_attempts: u8,
    pub exit_requirement_attempts: u8,
}

/// Outcome of the processing loop.
#[derive(Debug, Clone)]
pub enum LoopOutcome {
    /// Processing completed normally
    Completed { metadata: ResponseMetadata },
    /// Heartbeat requested for external continuation
    HeartbeatRequested {
        tool_name: String,
        call_id: String,
        next_sequence_num: u32,
    },
    /// Error occurred but was recovered
    ErrorRecovered,
}

/// Run the main processing loop.
///
/// This is the core agent processing logic extracted from DatabaseAgent.
/// It handles:
/// - Model completion with retry
/// - Content block processing with inline tool execution
/// - Start constraints and exit requirements
/// - Heartbeat continuation
pub async fn run_processing_loop(
    ctx: ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    initial_message: Message,
) -> Result<LoopOutcome, ProcessingError> {
    let retry_config = RetryConfig::default();
    let error_ctx = ErrorContext {
        event_tx,
        runtime: ctx.runtime,
        batch_id: Some(ctx.batch_id),
        agent_id: ctx.agent_id,
    };

    // 1. Build initial request (stores incoming message)
    let mut request = ctx
        .runtime
        .prepare_request(
            vec![initial_message],
            None,
            Some(ctx.batch_id),
            Some(ctx.batch_type),
            ctx.base_instructions,
        )
        .await
        .map_err(|e| ProcessingError::ContextBuild(e.to_string()))?;

    #[allow(unused_assignments)]
    let mut last_metadata: Option<ResponseMetadata> = None;
    let mut heartbeat_tool_info: Option<(String, String)> = None;
    let model_vendor = ModelVendor::from_provider_string(&ctx.response_options.model_info.provider);

    // Main loop
    loop {
        // 2. Call model with retry
        let response =
            match complete_with_retry(ctx.model, ctx.response_options, &mut request, &retry_config)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    handle_processing_error(&error_ctx, &e).await;
                    return Err(e);
                }
            };

        last_metadata = Some(response.metadata.clone());

        // 3. Store response message(s)
        let agent_id_ref = AgentId::new(ctx.agent_id);
        let mut response_messages = Message::from_response(
            &response,
            &agent_id_ref,
            Some(ctx.batch_id),
            Some(ctx.batch_type),
        );

        for msg in &mut response_messages {
            if msg.sequence_num.is_none() {
                msg.sequence_num = Some(state.sequence_num);
                state.sequence_num += 1;
            }

            if let Err(e) = ctx.runtime.store_message(msg).await {
                let err = ProcessingError::MessageStorage(e.to_string());
                handle_processing_error(&error_ctx, &err).await;
                return Err(err);
            }
        }

        // 4. Process content blocks IN ORDER (inline tool execution)
        let mut tool_responses: Vec<ToolResponse> = Vec::new();
        let mut pending_action = ToolAction::Continue;
        let mut needs_continuation = false;

        for item in iter_content_items(&response.content) {
            match item {
                ContentItem::Text(text) => {
                    emit_event(
                        event_tx,
                        ResponseEvent::TextChunk {
                            text: text.to_string(),
                            is_final: true,
                        },
                    )
                    .await;
                }

                ContentItem::Thinking(text) => {
                    emit_event(
                        event_tx,
                        ResponseEvent::ReasoningChunk {
                            text: text.to_string(),
                            is_final: false,
                        },
                    )
                    .await;
                }

                ContentItem::ToolUse { id, name, input } => {
                    let (action, response, continuation) =
                        execute_tool_inline(&ctx, state, event_tx, id, name, input).await;

                    tool_responses.push(response);
                    if continuation {
                        needs_continuation = true;
                    }

                    // Track heartbeat tool info
                    if matches!(action, ToolAction::RequestHeartbeat { .. }) {
                        heartbeat_tool_info = Some((name.to_string(), id.to_string()));
                    }

                    if !matches!(action, ToolAction::Continue) {
                        pending_action = action;
                        break; // Early exit from content processing
                    }
                }

                ContentItem::Other => {}
            }
        }

        // 5. Emit standalone reasoning if present
        if let Some(reasoning) = &response.reasoning {
            emit_event(
                event_tx,
                ResponseEvent::ReasoningChunk {
                    text: reasoning.clone(),
                    is_final: true,
                },
            )
            .await;
        }

        // 6. Emit and store tool responses
        if !tool_responses.is_empty() {
            emit_event(
                event_tx,
                ResponseEvent::ToolResponses {
                    responses: tool_responses.clone(),
                },
            )
            .await;

            let msg = Message::tool_in_batch_typed(
                ctx.batch_id,
                state.sequence_num,
                ctx.batch_type,
                tool_responses,
            );
            state.sequence_num += 1;

            if let Err(e) = ctx.runtime.store_message(&msg).await {
                tracing::warn!(error = %e, "Failed to store tool response");
            }
            needs_continuation = true;
        }

        // 7. Handle heartbeat exit
        if let ToolAction::RequestHeartbeat { tool_name, call_id } = pending_action {
            send_heartbeat(
                ctx.heartbeat_sender,
                ctx.agent_id,
                &tool_name,
                &call_id,
                ctx.batch_id,
                state.sequence_num,
                model_vendor,
            );

            return Ok(LoopOutcome::HeartbeatRequested {
                tool_name,
                call_id,
                next_sequence_num: state.sequence_num,
            });
        }

        // 8. Check exit conditions and requirements
        let should_exit = matches!(pending_action, ToolAction::ExitLoop)
            || ctx.runtime.should_exit_loop(&state.process_state);

        if should_exit || !needs_continuation {
            let pending_exit = ctx
                .runtime
                .get_pending_exit_requirements(&state.process_state);

            if !pending_exit.is_empty() {
                state.exit_requirement_attempts += 1;

                if state.exit_requirement_attempts >= 3 {
                    // Force execute and exit
                    let exit_responses =
                        force_execute_tools(&ctx, state, event_tx, &pending_exit, "exit_force")
                            .await;

                    emit_and_store_responses(&ctx, state, event_tx, &exit_responses).await;
                    ctx.runtime.mark_complete(&mut state.process_state);
                    break;
                } else {
                    // Add reminder and continue
                    add_exit_reminder(&ctx, state, &pending_exit).await;
                    needs_continuation = true;
                }
            } else {
                // No pending exit requirements
                // Check for heartbeat
                if let Some((tool_name, call_id)) = heartbeat_tool_info.take() {
                    send_heartbeat(
                        ctx.heartbeat_sender,
                        ctx.agent_id,
                        &tool_name,
                        &call_id,
                        ctx.batch_id,
                        state.sequence_num,
                        model_vendor,
                    );

                    return Ok(LoopOutcome::HeartbeatRequested {
                        tool_name,
                        call_id,
                        next_sequence_num: state.sequence_num,
                    });
                }

                // Clean exit
                break;
            }
        }

        // 9. Prepare continuation request
        if needs_continuation {
            // Check for heartbeat with exit condition
            if ctx.runtime.should_exit_loop(&state.process_state) {
                if let Some((tool_name, call_id)) = heartbeat_tool_info.take() {
                    send_heartbeat(
                        ctx.heartbeat_sender,
                        ctx.agent_id,
                        &tool_name,
                        &call_id,
                        ctx.batch_id,
                        state.sequence_num,
                        model_vendor,
                    );

                    return Ok(LoopOutcome::HeartbeatRequested {
                        tool_name,
                        call_id,
                        next_sequence_num: state.sequence_num,
                    });
                }
            }

            request = ctx
                .runtime
                .prepare_request(
                    Vec::<Message>::new(),
                    None,
                    Some(ctx.batch_id),
                    Some(ctx.batch_type),
                    ctx.base_instructions,
                )
                .await
                .map_err(|e| ProcessingError::ContextBuild(e.to_string()))?;
        } else {
            break;
        }
    }

    // 10. Complete batch
    ctx.runtime.complete_batch(ctx.batch_id);

    Ok(LoopOutcome::Completed {
        metadata: last_metadata.unwrap_or_else(default_metadata),
    })
}

// ============================================================================
// Helper functions
// ============================================================================

/// Emit an event to the channel.
async fn emit_event(tx: &mpsc::Sender<ResponseEvent>, event: ResponseEvent) {
    let _ = tx.send(event).await;
}

/// Execute a single tool inline and return the action, response, and continuation flag.
async fn execute_tool_inline(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    call_id: &str,
    fn_name: &str,
    fn_arguments: &serde_json::Value,
) -> (ToolAction, ToolResponse, bool) {
    // Emit start event
    emit_event(
        event_tx,
        ResponseEvent::ToolCallStarted {
            call_id: call_id.to_string(),
            fn_name: fn_name.to_string(),
            args: fn_arguments.clone(),
        },
    )
    .await;

    let explicit_heartbeat = check_heartbeat_request(fn_arguments);

    let meta = ExecutionMeta {
        permission_grant: None,
        request_heartbeat: explicit_heartbeat,
        caller_user: None,
        call_id: Some(ToolCallId(call_id.to_string())),
        route_metadata: None,
    };

    let call = ToolCall {
        call_id: call_id.to_string(),
        fn_name: fn_name.to_string(),
        fn_arguments: fn_arguments.clone(),
    };

    match ctx
        .runtime
        .execute_tool_checked(&call, ctx.batch_id, &mut state.process_state, &meta)
        .await
    {
        Ok(outcome) => {
            emit_event(
                event_tx,
                ResponseEvent::ToolCallCompleted {
                    call_id: call_id.to_string(),
                    result: Ok(outcome.response.content.clone()),
                },
            )
            .await;

            let needs_continuation = true; // Tool executed = needs continuation
            (outcome.action, outcome.response, needs_continuation)
        }

        Err(e) => {
            // Handle start constraint violations with retry logic
            if let ToolExecutionError::RuleViolation(
                crate::agent::tool_rules::ToolRuleViolation::StartConstraintsNotMet {
                    ref required_start_tools,
                    ..
                },
            ) = e
            {
                return handle_start_constraint_violation(
                    ctx,
                    state,
                    event_tx,
                    &call,
                    required_start_tools,
                )
                .await;
            }

            // Other errors become error responses, continue processing
            let error_content = format!("Execution error: {}", e);

            emit_event(
                event_tx,
                ResponseEvent::ToolCallCompleted {
                    call_id: call_id.to_string(),
                    result: Err(error_content.clone()),
                },
            )
            .await;

            (
                ToolAction::Continue,
                ToolResponse {
                    call_id: call_id.to_string(),
                    content: error_content,
                    is_error: Some(true),
                },
                true,
            )
        }
    }
}

/// Handle start constraint violation with retry logic.
async fn handle_start_constraint_violation(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    original_call: &ToolCall,
    required_start_tools: &[String],
) -> (ToolAction, ToolResponse, bool) {
    state.start_constraint_attempts += 1;

    if state.start_constraint_attempts >= 3 {
        // Attempt 3: Force execute required tools
        let force_responses =
            force_execute_tools(ctx, state, event_tx, required_start_tools, "force").await;

        emit_and_store_responses(ctx, state, event_tx, &force_responses).await;

        ctx.runtime
            .mark_start_constraints_done(&mut state.process_state);

        let error_content = format!(
            "Start constraint violation: required tools {} force-executed",
            required_start_tools.join(", ")
        );

        emit_event(
            event_tx,
            ResponseEvent::ToolCallCompleted {
                call_id: original_call.call_id.clone(),
                result: Err(error_content.clone()),
            },
        )
        .await;

        (
            ToolAction::Continue,
            ToolResponse {
                call_id: original_call.call_id.clone(),
                content: error_content,
                is_error: Some(true),
            },
            true,
        )
    } else {
        // Attempt 1 or 2: Return error and optionally add reminder
        let error_content = format!(
            "Start constraint violation: must call {} first",
            required_start_tools.join(", ")
        );

        emit_event(
            event_tx,
            ResponseEvent::ToolCallCompleted {
                call_id: original_call.call_id.clone(),
                result: Err(error_content.clone()),
            },
        )
        .await;

        // Attempt 2: Add system reminder
        if state.start_constraint_attempts == 2 {
            let reminder_text = format!(
                "[System Reminder] You must call these tools first before any others: {}",
                required_start_tools.join(", ")
            );
            let reminder_msg = Message::user_in_batch_typed(
                ctx.batch_id,
                state.sequence_num,
                ctx.batch_type,
                reminder_text,
            );
            state.sequence_num += 1;

            if let Err(e) = ctx.runtime.store_message(&reminder_msg).await {
                tracing::warn!(error = %e, "Failed to store start constraint reminder");
            }
        }

        (
            ToolAction::Continue,
            ToolResponse {
                call_id: original_call.call_id.clone(),
                content: error_content,
                is_error: Some(true),
            },
            true,
        )
    }
}

/// Force execute tools with empty arguments.
async fn force_execute_tools(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    _event_tx: &mpsc::Sender<ResponseEvent>,
    tool_names: &[String],
    prefix: &str,
) -> Vec<ToolResponse> {
    let mut responses = Vec::new();

    for tool_name in tool_names {
        let synthetic_id = format!("{}_{}", prefix, MessageId::generate());

        let synthetic_call = ToolCall {
            call_id: synthetic_id.clone(),
            fn_name: tool_name.clone(),
            fn_arguments: serde_json::json!({}),
        };

        let meta = ExecutionMeta {
            permission_grant: None,
            request_heartbeat: false,
            caller_user: None,
            call_id: Some(ToolCallId(synthetic_id.clone())),
            route_metadata: None,
        };

        match ctx
            .runtime
            .execute_tool(
                &synthetic_call,
                ctx.batch_id,
                &mut state.process_state,
                &meta,
            )
            .await
        {
            Ok(result) => {
                responses.push(result.response);
            }
            Err(_) => {
                responses.push(ToolResponse {
                    call_id: synthetic_id,
                    content: format!("Force-executed {} with empty args (failed)", tool_name),
                    is_error: Some(true),
                });
            }
        }
    }

    responses
}

/// Emit and store tool responses.
async fn emit_and_store_responses(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    responses: &[ToolResponse],
) {
    if responses.is_empty() {
        return;
    }

    emit_event(
        event_tx,
        ResponseEvent::ToolResponses {
            responses: responses.to_vec(),
        },
    )
    .await;

    for response in responses {
        let msg = Message::tool_in_batch_typed(
            ctx.batch_id,
            state.sequence_num,
            ctx.batch_type,
            vec![response.clone()],
        );
        state.sequence_num += 1;

        if let Err(e) = ctx.runtime.store_message(&msg).await {
            tracing::warn!(error = %e, "Failed to store tool response");
        }
    }
}

/// Add exit requirement reminder.
async fn add_exit_reminder(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    pending_exit: &[String],
) {
    let reminder_intensity = if state.exit_requirement_attempts == 1 {
        "Reminder"
    } else {
        "IMPORTANT REMINDER"
    };

    let reminder_text = format!(
        "[System {}] You must call these tools before ending the conversation: {}",
        reminder_intensity,
        pending_exit.join(", ")
    );

    let reminder_msg = Message::user_in_batch_typed(
        ctx.batch_id,
        state.sequence_num,
        ctx.batch_type,
        reminder_text,
    );
    state.sequence_num += 1;

    if let Err(e) = ctx.runtime.store_message(&reminder_msg).await {
        tracing::warn!(error = %e, "Failed to store exit reminder");
    }
}

/// Send heartbeat request.
fn send_heartbeat(
    sender: &HeartbeatSender,
    agent_id: &str,
    tool_name: &str,
    call_id: &str,
    batch_id: SnowflakePosition,
    sequence_num: u32,
    model_vendor: ModelVendor,
) {
    let req = HeartbeatRequest {
        agent_id: crate::id::AgentId::new(agent_id),
        tool_name: tool_name.to_string(),
        tool_call_id: call_id.to_string(),
        batch_id: Some(batch_id),
        next_sequence_num: Some(sequence_num),
        model_vendor: Some(model_vendor),
    };

    if let Err(e) = sender.try_send(req) {
        tracing::warn!("Failed to send heartbeat: {:?}", e);
    }
}

/// Create default metadata for error cases.
fn default_metadata() -> ResponseMetadata {
    ResponseMetadata {
        processing_time: None,
        tokens_used: None,
        model_used: None,
        confidence: None,
        model_iden: genai::ModelIden::new(genai::adapter::AdapterKind::Anthropic, "unknown"),
        custom: serde_json::json!({}),
    }
}
