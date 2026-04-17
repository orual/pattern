//! Stub handler for `Pattern.Message`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::requests::MessageReq;

/// Not-implemented placeholder for the Message effect. Real implementation
/// arrives in Phase 4 (pattern_provider backing).
#[derive(Default)]
pub struct MessageHandler;

impl EffectHandler for MessageHandler {
    type Request = MessageReq;

    fn handle(
        &mut self,
        req: MessageReq,
        _cx: &EffectContext<'_>,
    ) -> Result<Value, EffectError> {
        Err(EffectError::Handler(format!(
            "Pattern.Message.{req:?} is stubbed in Phase 3 — Phase 4 wires real \
             pattern_provider backing. Agent code should not call message effects yet."
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
        let err = h
            .handle(MessageReq::Ask("test".into()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Message"), "got: {msg}");
        assert!(msg.contains("stubbed"), "got: {msg}");
        assert!(msg.contains("Phase 4"), "got: {msg}");
    }
}
