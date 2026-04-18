//! Stub handler for `Pattern.Message`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::MessageReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the Message effect. Real implementation
/// arrives in Phase 4 (pattern_provider backing).
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
                "ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)\nask r = send (Ask r)",
                "send_ :: Member Message effs => Recipient -> Body -> Eff effs ()\nsend_ r b = send (Send r b)",
                "reply :: Member Message effs => MessageId -> Body -> Eff effs ()\nreply m b = send (Reply m b)",
                "notify :: Member Message effs => ChannelId -> Body -> Eff effs ()\nnotify c b = send (Notify c b)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for MessageHandler
where
    U: HasCancelState,
{
    type Request = MessageReq;

    fn handle(&mut self, req: MessageReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        Err(EffectError::Handler(format!(
            "Message handler is stubbed in phase 3 — Phase 4 wires pattern_provider. \
             Request was: Pattern.Message.{req:?}."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn message_stub_reports_not_implemented() {
        let mut h = MessageHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(MessageReq::Ask("test".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Message handler"), "got: {msg}");
        assert!(msg.contains("stubbed in phase 3"), "got: {msg}");
        assert!(msg.contains("Phase 4"), "got: {msg}");
    }
}
