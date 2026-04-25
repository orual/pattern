//! Handler for `Pattern.Message`.
//!
//! Send / Reply / Notify: construct a `Message`, push it into
//! `SessionContext::pending_messages`, and dispatch via the
//! [`RouterBridge`](crate::router::RouterBridge). The handler runs
//! on a plain OS eval worker thread (no tokio runtime context), so
//! router dispatch uses the sync `RouterBridge::route_sync` method
//! which sends requests to an async router task via a channel.
//!
//! Ask: stubbed as candidate-for-removal per Phase 5 Task 20.
//! v3 agents don't call LLMs via effects; LLMs drive agent turns
//! via `run_turn`, and within that call the LLM uses the `code`
//! tool to invoke SDK capabilities.

use jiff::Timestamp;
use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
use pattern_core::types::message::Message;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::MessageReq;
use crate::session::SessionContext;
use crate::timeout::HandlerGuard;

/// Handler for `Pattern.Message`. Send/Reply/Notify dispatch through
/// the session's `RouterRegistry`; Ask is stubbed.
#[derive(Default, Clone)]
pub struct MessageHandler;

impl DescribeEffect for MessageHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Message",
            description: "Inter-agent and outbound messaging (Ask/Send/Reply/Notify)",
            constructors: &[
                "Ask    :: Request -> Message (MessageContent, Usage)",
                "Send   :: Recipient -> Body -> Message ()",
                "Reply  :: MessageId -> Body -> Message ()",
                "Notify :: ChannelId -> Body -> Message ()",
            ],
            type_defs: &[
                "type Request = Text",
                "type MessageContent = Text",
                "type Usage = Text",
                "type Recipient = Text",
                "type Body = Text",
                "type MessageId = Text",
                "type ChannelId = Text",
            ],
            helpers: &[
                "ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)\nask r = Freer.send (Ask r)",
                "send :: Member Message effs => Recipient -> Body -> Eff effs ()\nsend r b = Freer.send (Send r b)",
                "reply :: Member Message effs => MessageId -> Body -> Eff effs ()\nreply m b = Freer.send (Reply m b)",
                "notify :: Member Message effs => ChannelId -> Body -> Eff effs ()\nnotify c b = Freer.send (Notify c b)",
            ],
        }
    }
}

/// Handler position of `MessageHandler` in the canonical
/// [`crate::sdk::bundle::SdkBundle`] HList.
const MESSAGE_HANDLER_TAG: u32 = 3;

impl EffectHandler<SessionContext> for MessageHandler {
    type Request = MessageReq;

    fn handle(
        &mut self,
        req: MessageReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        // Soft-cancel check.
        let state = cx.user().cancel_state();
        if state.cancellation.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(EffectError::Handler(format!(
                "{}: message handler cancelled at entry",
                crate::timeout::CANCELLED_SENTINEL,
            )));
        }
        let _guard = HandlerGuard::enter(&state.gate);

        let request_repr = format!("{req:?}");

        let result = match req {
            MessageReq::Ask(_request) => {
                // Ask is stubbed as candidate-for-removal per Phase 5 Task 20.
                return Err(EffectError::Handler(
                    "Pattern.Message.Ask is a candidate for removal in a future plan; \
                     v3 agents don't call LLMs via effects — LLMs drive agent turns \
                     via the `code` tool. Use Memory / Send / Reply for inter-agent \
                     communication instead."
                        .to_string(),
                ));
            }
            MessageReq::Send(recipient, body) => {
                let agent_id = cx.user().agent_id().to_string();
                dispatch_outbound(cx, &agent_id, &recipient, &body, "Send")
            }
            MessageReq::Reply(msg_id, body) => {
                let agent_id = cx.user().agent_id().to_string();
                dispatch_outbound(cx, &agent_id, &msg_id, &body, "Reply")
            }
            MessageReq::Notify(channel_id, body) => {
                let agent_id = cx.user().agent_id().to_string();
                dispatch_outbound(cx, &agent_id, &channel_id, &body, "Notify")
            }
        };

        // Record exchange on success (same pattern as MemoryHandler).
        if let Ok(ref value) = result {
            let log = cx.user().checkpoint_log();
            let turn = cx.user().current_turn();
            crate::session::record_exchange(&log, MESSAGE_HANDLER_TAG, request_repr, value, turn);
        }
        result
    }
}

/// Construct a `Message` from the body, push it into pending_messages,
/// and dispatch via the router bridge (sync, no tokio context required).
fn dispatch_outbound(
    cx: &EffectContext<'_, SessionContext>,
    agent_id: &str,
    recipient: &str,
    body: &str,
    op_name: &str,
) -> Result<Value, EffectError> {
    let msg = Message {
        chat_message: genai::chat::ChatMessage::new(
            genai::chat::ChatRole::Assistant,
            body.to_string(),
        ),
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        owner_id: AgentId::from(agent_id),
        created_at: Timestamp::now(),
        batch: BatchId::from(new_snowflake_id()),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };

    // Push into pending_messages for turn-close drain.
    cx.user()
        .pending_messages()
        .lock()
        .unwrap()
        .push(msg.clone());

    // Dispatch via the router bridge (sync channel to async router task).
    let bridge = cx.user().router_bridge().ok_or_else(|| {
        EffectError::Handler(format!(
            "Pattern.Message.{op_name}: no router bridge configured \
             (session must be opened with a router via with_router)"
        ))
    })?;
    bridge.route_sync(recipient, &msg).map_err(|e| {
        EffectError::Handler(format!("Pattern.Message.{op_name}: routing failed: {e}"))
    })?;

    cx.respond(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NopProviderClient;
    use crate::router::RouterRegistry;
    use crate::router::cli::CliRouter;
    use crate::testing::{InMemoryMemoryStore, standard_datacon_table};
    use pattern_core::ProviderClient;
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::snapshot::PersonaSnapshot;
    use std::sync::Arc;

    fn sctx_with_router(
        registry: RouterRegistry,
        db: Arc<pattern_db::ConstellationDb>,
    ) -> SessionContext {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let persona = PersonaSnapshot::new("agent-a", "A");
        SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        )
        .with_router(Arc::new(registry))
    }

    /// Build a DataConTable that includes the `()` constructor needed by
    /// `cx.respond(())`.
    fn handler_table() -> tidepool_repr::DataConTable {
        let mut table = standard_datacon_table();
        table.insert(tidepool_repr::DataCon {
            id: tidepool_repr::DataConId(100),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    #[tokio::test]
    async fn ask_returns_candidate_for_removal_error() {
        let table = standard_datacon_table();
        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(RouterRegistry::new(), db);
        let cx = EffectContext::with_user(&table, &ctx);
        let mut h = MessageHandler;
        let err = h.handle(MessageReq::Ask("test".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("candidate for removal"), "got: {msg}");
        assert!(
            msg.contains("code"),
            "should mention the code tool; got: {msg}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_dispatches_to_cli_router() {
        let (cli_router, mut rx) = CliRouter::new();
        let mut registry = RouterRegistry::new();
        registry.register(Arc::new(cli_router));
        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(registry, db);

        let table = handler_table();

        let result = tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(
                MessageReq::Send("cli:user".into(), "hello world".into()),
                &cx,
            )
        })
        .await
        .unwrap();

        assert!(result.is_ok(), "Send should succeed; got: {result:?}");

        // Verify the receiver got the message.
        let received = rx.recv().await.expect("should receive routed message");
        let text = received
            .chat_message
            .content
            .first_text()
            .expect("message should have text content");
        assert_eq!(text, "hello world");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_unknown_scheme_returns_error() {
        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(RouterRegistry::new(), db);
        let table = handler_table();

        let result = tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(
                MessageReq::Send("unknown:target".into(), "body".into()),
                &cx,
            )
        })
        .await
        .unwrap();

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("routing failed"), "got: {msg}");
        assert!(msg.contains("unknown"), "got: {msg}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_pushes_into_pending_messages() {
        let (cli_router, _rx) = CliRouter::new();
        let mut registry = RouterRegistry::new();
        registry.register(Arc::new(cli_router));
        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(registry, db);
        let pending = ctx.pending_messages().clone();

        let table = handler_table();

        tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(MessageReq::Send("cli:user".into(), "test body".into()), &cx)
                .unwrap();
        })
        .await
        .unwrap();

        let msgs = pending.lock().unwrap();
        assert_eq!(msgs.len(), 1, "should have 1 pending message");
    }
}
