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

use crate::router::{ROUTER_ERROR_PREFIX, RouterError};
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
            description: "Inter-agent and outbound messaging (Ask/Send/Reply/Notify/Delegate)",
            constructors: std::borrow::Cow::Borrowed(&[
                "Ask      :: Request -> Message (MessageContent, Usage)",
                "Send     :: Recipient -> Body -> Message ()",
                "Reply    :: MessageId -> Body -> Message ()",
                "Notify   :: ChannelId -> Body -> Message ()",
                "Delegate :: DelegateReq -> Message ()",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type Request = Text",
                "type MessageContent = Text",
                "type Usage = Text",
                "type Recipient = Text",
                "type Body = Text",
                "type MessageId = Text",
                "type ChannelId = Text",
                "data DelegateReq = DelegateReq { delegateTaskLabel :: Text, \
                 delegateTaskBlockId :: Text, delegateTaskAgentId :: Text, \
                 delegateRecipient :: Text, delegateBody :: Text }",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)\nask r = Freer.send (Ask r)",
                "send :: Member Message effs => Recipient -> Body -> Eff effs ()\nsend r b = Freer.send (Send r b)",
                "reply :: Member Message effs => MessageId -> Body -> Eff effs ()\nreply m b = Freer.send (Reply m b)",
                "notify :: Member Message effs => ChannelId -> Body -> Eff effs ()\nnotify c b = Freer.send (Notify c b)",
                "delegate :: Member Message effs => DelegateReq -> Eff effs ()\ndelegate d = Freer.send (Delegate d)",
            ]),
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

        // Effect-class runtime guard. All Message constructors are
        // RuntimeClassCheck::Skip so this is defensive; it returns Ok(())
        // immediately for all Skip entries regardless of the capset.
        let constructor_name = match &req {
            MessageReq::Ask(_) => "Ask",
            MessageReq::Send(_, _) => "Send",
            MessageReq::Reply(_, _) => "Reply",
            MessageReq::Notify(_, _) => "Notify",
            MessageReq::Delegate(_) => "Delegate",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Message",
            constructor_name,
        )?;

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
            MessageReq::Delegate(wire) => {
                let agent_id = cx.user().agent_id().to_string();
                let task_ref = pattern_core::BlockRef::with_owner(
                    wire.task_label,
                    wire.task_block_id,
                    wire.task_agent_id,
                );
                dispatch_delegate(
                    cx,
                    &agent_id,
                    &wire.recipient,
                    &wire.body,
                    task_ref,
                    "Delegate",
                )
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

/// Construct a delegation `Message` — body plus the task's `BlockRef` pinned
/// into `block_refs` — and dispatch it to `recipient`.
///
/// Inserting the `BlockRef` here causes the snapshot composer at the
/// recipient's session to pin the task into its working-memory selection for
/// the incoming turn (AC6.3).
fn dispatch_delegate(
    cx: &EffectContext<'_, SessionContext>,
    agent_id: &str,
    recipient: &str,
    body: &str,
    task_ref: pattern_core::BlockRef,
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
        // Pin the assigned task — snapshot composer reads this at the
        // recipient's turn entry to include the task block in context.
        block_refs: vec![task_ref],
        attachments: vec![],
    };

    // Push into pending_messages for turn-close drain (same as Send).
    cx.user()
        .pending_messages()
        .lock()
        .unwrap()
        .push(msg.clone());

    let bridge = cx.user().router_bridge().ok_or_else(|| {
        EffectError::Handler(format!(
            "Pattern.Message.{op_name}: no router bridge configured \
             (session must be opened with a router via with_router)"
        ))
    })?;

    let sender = cx.user().current_dispatch_origin().ok_or_else(|| {
        EffectError::Handler(format!(
            "Pattern.Message.{op_name}: no dispatch origin available \
             (handler invoked outside a turn — drive_step is responsible \
             for populating SessionContext::current_dispatch_origin)"
        ))
    })?;

    bridge
        .route_sync(&sender, recipient, &msg)
        .map_err(|e| match &e {
            RouterError::PersonaNotFound(id) => {
                EffectError::Handler(format!("{ROUTER_ERROR_PREFIX}PersonaNotFound: {id}"))
            }
            RouterError::MailboxClosed => {
                EffectError::Handler(format!("{ROUTER_ERROR_PREFIX}MailboxClosed"))
            }
            _ => EffectError::Handler(format!("Pattern.Message.{op_name}: routing failed: {e}")),
        })?;

    cx.respond(())
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

    // Sender attribution: read the immediate-dispatch origin set by
    // `agent_loop::drive_step` per orchestrate iteration. Phase 1's
    // dispatch-origin discipline guarantees this is populated for
    // every handler invocation that runs inside a real turn — see
    // `current_dispatch_origin` on `SessionContext`. A `None` here
    // means the handler is being called outside that discipline
    // (e.g. a test fixture that bypasses `drive_step`); fail loudly
    // rather than silently synthesising a sender that misattributes
    // the message.
    let sender = cx.user().current_dispatch_origin().ok_or_else(|| {
        EffectError::Handler(format!(
            "Pattern.Message.{op_name}: no dispatch origin available \
             (handler invoked outside a turn — drive_step is responsible \
             for populating SessionContext::current_dispatch_origin)"
        ))
    })?;

    bridge.route_sync(&sender, recipient, &msg).map_err(|e| {
        // PersonaNotFound and MailboxClosed carry the ROUTER_ERROR_PREFIX so
        // consumers (tests, TUI, CLI) can discriminate routing failures from
        // other handler errors without parsing free-form prose. All other
        // routing errors surface via the generic "routing failed" wrapper.
        match &e {
            RouterError::PersonaNotFound(id) => {
                EffectError::Handler(format!("{ROUTER_ERROR_PREFIX}PersonaNotFound: {id}"))
            }
            RouterError::MailboxClosed => {
                EffectError::Handler(format!("{ROUTER_ERROR_PREFIX}MailboxClosed"))
            }
            _ => EffectError::Handler(format!("Pattern.Message.{op_name}: routing failed: {e}")),
        }
    })?;

    cx.respond(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NopProviderClient;
    use crate::mailbox::Mailbox;
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
        use pattern_core::types::origin::{AgentAuthor, Author, MessageOrigin, Sphere};

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn ProviderClient> = Arc::new(NopProviderClient);
        let persona = PersonaSnapshot::new("agent-a", "A");
        let ctx = SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        )
        .with_router(Arc::new(registry));

        // Tests bypass `drive_step`, so they must populate the
        // dispatch-origin slot themselves to satisfy the handler's
        // attribution invariant.
        *ctx.current_dispatch_origin_slot().write().unwrap() = Some(MessageOrigin::new(
            Author::Agent(AgentAuthor {
                agent_id: "agent-a".into(),
            }),
            Sphere::Internal,
        ));

        ctx
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

        // Verify the receiver got the routed event with sender attribution.
        let received = rx.recv().await.expect("should receive routed event");
        // Default-fallback sender is Author::Agent(self) — the session's
        // own agent_id ("agent-a" per `sctx_with_router`).
        use pattern_core::types::origin::Author;
        match &received.sender.author {
            Author::Agent(a) => assert_eq!(a.agent_id.as_str(), "agent-a"),
            other => panic!("expected Author::Agent attribution, got: {other:?}"),
        }
        // Target is post-scheme-strip ("user", not "cli:user").
        assert_eq!(received.target, "user");
        let text = received
            .body
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
    async fn send_without_dispatch_origin_returns_error() {
        // Construct a session context that explicitly clears the
        // dispatch-origin slot. The handler must reject the call
        // rather than synthesise an attribution.
        let (cli_router, _rx) = CliRouter::new();
        let mut registry = RouterRegistry::new();
        registry.register(Arc::new(cli_router));
        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(registry, db);
        // Wipe the slot the helper populates.
        *ctx.current_dispatch_origin_slot().write().unwrap() = None;

        let table = handler_table();

        let result = tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(MessageReq::Send("cli:user".into(), "body".into()), &cx)
        })
        .await
        .unwrap();

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no dispatch origin available"),
            "expected dispatch-origin error; got: {msg}"
        );
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

    /// AC6.3: delegate dispatches with the task's BlockRef pinned into
    /// the outgoing message's block_refs. The recipient's snapshot composer
    /// will include the task in the working-memory selection for that turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delegate_pins_task_block_ref_in_message() {
        use crate::agent_registry::{AgentRegistry, SessionStatus};
        use crate::router::agent::AgentRouter;

        // Set up a shared registry with an "active" recipient persona.
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("recipient-r".into());
        reg.register("recipient-r".into(), mailbox.clone(), SessionStatus::Active);

        let agent_router = Arc::new(AgentRouter::new(Arc::clone(&reg)));
        let mut registry = RouterRegistry::new();
        registry.register(agent_router);

        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(registry, db);
        let table = handler_table();

        let result = tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(
                MessageReq::Delegate(crate::sdk::requests::message::WireDelegateReq {
                    task_label: "my-task".into(),
                    task_block_id: "block-123".into(),
                    task_agent_id: "agent-a".into(),
                    recipient: "agent:recipient-r".into(),
                    body: "please handle this".into(),
                }),
                &cx,
            )
        })
        .await
        .unwrap();

        assert!(result.is_ok(), "Delegate should succeed; got: {result:?}");

        // Verify the recipient's mailbox got a message with the task's BlockRef.
        let mailbox_input = mailbox
            .lock_rx()
            .await
            .recv()
            .await
            .expect("recipient should receive a message");
        let block_refs = &mailbox_input.msg.block_refs;
        assert_eq!(block_refs.len(), 1, "message should have 1 block_ref");
        assert_eq!(block_refs[0].block_id, "block-123");
        assert_eq!(block_refs[0].label, "my-task");
        assert_eq!(block_refs[0].agent_id, "agent-a");

        // Body text is correct.
        let text = mailbox_input.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "please handle this");
    }

    /// AC6.4: sending to a nonexistent agent persona produces an error with
    /// the ROUTER_ERROR_PREFIX followed by "PersonaNotFound: <persona-id>".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_to_nonexistent_agent_produces_router_error_prefix() {
        use crate::agent_registry::AgentRegistry;
        use crate::router::ROUTER_ERROR_PREFIX;
        use crate::router::agent::AgentRouter;

        let reg = Arc::new(AgentRegistry::new());
        // No persona registered — any send to agent: scheme is PersonaNotFound.
        let agent_router = Arc::new(AgentRouter::new(Arc::clone(&reg)));
        let mut registry = RouterRegistry::new();
        registry.register(agent_router);

        let db = crate::testing::test_db().await;
        let ctx = sctx_with_router(registry, db);
        let table = handler_table();

        let result = tokio::task::spawn_blocking(move || {
            let cx = EffectContext::with_user(&table, &ctx);
            let mut h = MessageHandler;
            h.handle(
                MessageReq::Send("agent:ghost-persona".into(), "hello?".into()),
                &cx,
            )
        })
        .await
        .unwrap();

        let err = result.unwrap_err();
        let msg = err.to_string();
        // Must start with the well-known prefix.
        assert!(
            msg.contains(ROUTER_ERROR_PREFIX),
            "expected ROUTER_ERROR_PREFIX in error; got: {msg}"
        );
        // Must contain the target persona-id fragment.
        assert!(
            msg.contains("ghost-persona"),
            "expected persona-id in error; got: {msg}"
        );
        assert!(
            msg.contains("PersonaNotFound"),
            "expected PersonaNotFound in error; got: {msg}"
        );
    }
}
